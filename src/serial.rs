//! Serial-port discovery, connection lifecycle, and frame forwarding.
//!
//! The serial task reconnects automatically, accepts command strings from
//! route handlers, and splits the incoming stream into COBS frames (the wire
//! is always the firmware's `serialized` mode). Each frame is broadcast raw to
//! WebSocket clients and/or decoded and written to a Parquet session file.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialPortType};

use crate::csi::{self, ChipVariant};
use crate::models::{DeviceConfig, DeviceInfo, OutputMode};
use crate::parquet_sink::ParquetSink;
use crate::state::{DeviceHandle, DeviceRegistry, InfoResponder};

/// Distinguishes "firmware-not-present / parse-failure" from "the link itself
/// died" so the caller can decide whether to surface a `Result` or to
/// reconnect.
#[derive(Debug)]
enum InfoExchangeError {
    /// Logical failure — magic prefix never seen, timed out, or parse error.
    /// Connection is still healthy.
    Soft(String),
    /// I/O failure — connection is broken; the outer loop should reconnect.
    Hard(String),
}

impl InfoExchangeError {
    fn message(&self) -> &str {
        match self {
            Self::Soft(m) | Self::Hard(m) => m,
        }
    }
}

/// How long to wait for the device to emit a complete info block before
/// failing the request. The firmware prints the block synchronously in
/// response to `info`, so anything significantly above the round-trip time
/// signals that the firmware is missing or unresponsive.
const INFO_RESPONSE_TIMEOUT: Duration = Duration::from_millis(2000);

/// How often the connection loop re-attempts firmware verification while the
/// link is up but the device is still unverified (and not collecting). The
/// initial auto-verify on connect can miss — the chip may still be booting,
/// the RTS reset may not have landed on a native-USB board, or the device may
/// have been mid-stream when the server started. Without this retry, a device
/// attached before server start would sit `firmware_verified == false` until a
/// full reconnect, never becoming usable to clients.
const REVERIFY_INTERVAL: Duration = Duration::from_secs(3);

/// Espressif's USB vendor id, used by the built-in USB-Serial-JTAG controller
/// on ESP32-S3 / C3 / C6. These chips reset by re-enumerating their USB
/// endpoint, so the RTS/DTR auto-reset is skipped for them.
const ESPRESSIF_NATIVE_USB_VID: u16 = 0x303A;

/// Known ESP32 USB-UART adapter Vendor IDs.
const ESP_USB_VIDS: &[u16] = &[
    0x10C4,                    // Silicon Labs CP210x (most common on ESP32 devkits)
    0x1A86,                    // WCH CH340 / CH341
    ESPRESSIF_NATIVE_USB_VID, // Espressif built-in USB (ESP32-S3 / C3 / C6 native USB)
];

/// Per-device CSI frame broadcast buffer, in frames. Sized well above a typical
/// burst so a momentarily-slow WebSocket client does not immediately start
/// dropping frames; sustained overruns surface as `Lagged` and are counted in
/// the WebSocket metrics (see [`crate::routes::ws`]).
const CSI_BROADCAST_CAPACITY: usize = 1024;

/// Detect *all* available ESP32 USB serial port paths, sorted so device-id
/// assignment is deterministic across scans.
///
/// Resolution order:
/// 1. `CSI_SERIAL_PORT` environment variable override (pins a single port).
/// 2. Every USB port whose name contains `usbserial` / `usbmodem` / `ttyUSB` /
///    `ttyACM`, or whose VID matches a known ESP chip.
/// 3. If that heuristic pass found nothing *and* exactly one USB port exists,
///    that lone port as a last resort. (When several USB ports are present we
///    refuse to guess, to avoid grabbing unrelated USB serial devices.)
pub fn detect_esp_ports() -> Vec<String> {
    // Allow the user to pin a specific port without recompiling.
    if let Ok(port) = std::env::var("CSI_SERIAL_PORT") {
        tracing::debug!("Using CSI_SERIAL_PORT override: {port}");
        return vec![port];
    }

    let ports = match tokio_serial::available_ports() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to enumerate serial ports: {e}");
            return Vec::new();
        }
    };

    // First pass: match by known VID or recognisable port-name prefix.
    let mut matched: Vec<String> = Vec::new();
    for port in &ports {
        if let SerialPortType::UsbPort(ref info) = port.port_type {
            let name_ok = port.port_name.contains("usbserial")
                || port.port_name.contains("usbmodem")
                || port.port_name.contains("ttyUSB")
                || port.port_name.contains("ttyACM");

            let vid_ok = ESP_USB_VIDS.contains(&info.vid);

            if name_ok || vid_ok {
                matched.push(port.port_name.clone());
            }
        }
    }

    // Fallback: a single lone USB port when the heuristic pass came up empty.
    if matched.is_empty() {
        let usb: Vec<&tokio_serial::SerialPortInfo> = ports
            .iter()
            .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
            .collect();
        if usb.len() == 1 {
            tracing::warn!(
                "No known ESP port found — using the only USB port: {}",
                usb[0].port_name
            );
            matched.push(usb[0].port_name.clone());
        }
    }

    matched.sort();
    matched
}

/// Sanitize an arbitrary string into a URL-safe device id: alphanumerics,
/// `_` and `-` survive, everything else (`/`, `:`, …) becomes `-`. A MAC
/// `D0:CF:13:E2:90:E8` → `D0-CF-13-E2-90-E8`; raw paths can't be ids because
/// `/` breaks Axum path matching.
fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Derive a stable, URL-safe device id, honouring `alias=<port|mac>` overrides.
///
/// Identity preference, most-stable first:
/// 1. An alias whose key matches the port path **or** the MAC.
/// 2. The board MAC (from the USB `iSerialNumber`), sanitized — stable across a
///    `ttyACMx` renumbering, which is the whole point of MAC-pinning.
/// 3. The sanitized port basename (`/dev/ttyUSB0` → `ttyUSB0`) for adapters
///    that expose no serial number; those are UART bridges that don't
///    re-enumerate on reset, so a path-derived id is stable enough for them.
fn device_id(port_path: &str, mac: Option<&str>, aliases: &[(String, String)]) -> String {
    for (alias, key) in aliases {
        if key == port_path || Some(key.as_str()) == mac {
            return alias.clone();
        }
    }
    if let Some(mac) = mac {
        return sanitize_id(mac);
    }
    sanitize_id(port_path.rsplit('/').next().unwrap_or(port_path))
}

/// Build a [`DeviceHandle`] for a port, wire up its channels, and spawn the
/// per-device serial task. Returns the shared handle for registry insertion.
pub fn spawn_device(
    id: String,
    port_path: String,
    baud: u32,
    native_usb: bool,
    mac: Option<String>,
) -> Arc<DeviceHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>(64);
    let (csi_tx, _) = broadcast::channel::<Vec<u8>>(CSI_BROADCAST_CAPACITY);
    let (output_mode_tx, output_mode_rx) = watch::channel(OutputMode::default());
    let (session_file_tx, session_file_rx) = watch::channel::<Option<String>>(None);
    let (info_request_tx, info_request_rx) = mpsc::channel::<InfoResponder>(4);

    let dev = Arc::new(DeviceHandle {
        id,
        mac,
        port_path,
        baud_rate: baud,
        native_usb,
        serial_connected: AtomicBool::new(false),
        collection_running: AtomicBool::new(false),
        firmware_verified: AtomicBool::new(false),
        cmd_tx,
        csi_tx,
        output_mode_tx,
        session_file_tx,
        info_request_tx,
        config: tokio::sync::Mutex::new(DeviceConfig::default()),
        device_info: tokio::sync::Mutex::new(None),
        shutdown: tokio_util::sync::CancellationToken::new(),
    });

    tokio::spawn(run_serial_task(
        dev.clone(),
        cmd_rx,
        output_mode_rx,
        session_file_rx,
        info_request_rx,
    ));

    dev
}

/// One synchronous serial-port scan, intended to run inside
/// [`tokio::task::spawn_blocking`].
///
/// `tokio_serial::available_ports()` is a blocking syscall that, on Linux, can
/// stall for tens to hundreds of milliseconds while CDC-ACM ports are open —
/// long enough to starve the runtime's I/O reactor and stutter active CSI
/// streams if called on a worker thread. Collecting everything the supervisor
/// needs in one off-runtime pass keeps the async loop non-blocking.
///
/// Returns the candidate [`PortCandidate`]s (ESP heuristics plus alias-pinned
/// ports the OS currently lists) and the set of all present port names, used
/// for alias reconciliation.
fn scan_ports(aliases: &[(String, String)]) -> (Vec<PortCandidate>, HashSet<String>) {
    let detected = detect_esp_ports();
    let all_ports = tokio_serial::available_ports().unwrap_or_default();
    let existing: HashSet<String> = all_ports.iter().map(|p| p.port_name.clone()).collect();

    // True if `path` is an Espressif native USB-Serial-JTAG endpoint (VID
    // 0x303A); those re-enumerate on RTS/DTR reset, so the serial task must
    // skip the auto-reset for them.
    let is_native = |path: &str| {
        all_ports.iter().any(|p| {
            p.port_name == path
                && matches!(
                    p.port_type,
                    SerialPortType::UsbPort(ref info) if info.vid == ESPRESSIF_NATIVE_USB_VID
                )
        })
    };

    // The USB `iSerialNumber` descriptor for `path`, if any. For native
    // USB-Serial-JTAG boards this is the eFuse MAC (`AA:BB:CC:DD:EE:FF`); read
    // straight from the enumeration, so it's available before the port is even
    // opened.
    let mac_of = |path: &str| -> Option<String> {
        all_ports.iter().find_map(|p| match &p.port_type {
            SerialPortType::UsbPort(info) if p.port_name == path => info.serial_number.clone(),
            _ => None,
        })
    };

    let mut candidates: Vec<PortCandidate> = detected
        .into_iter()
        .map(|path| {
            let mac = mac_of(&path);
            PortCandidate {
                id: device_id(&path, mac.as_deref(), aliases),
                native_usb: is_native(&path),
                mac,
                path,
            }
        })
        .collect();

    // Honour alias-pinned ports the heuristics missed, if they exist.
    for (alias, path) in aliases {
        if existing.contains(path) && !candidates.iter().any(|c| &c.path == path) {
            candidates.push(PortCandidate {
                id: alias.clone(),
                native_usb: is_native(path),
                mac: mac_of(path),
                path: path.clone(),
            });
        }
    }

    (candidates, existing)
}

/// One port the supervisor may register, resolved off-runtime in [`scan_ports`].
struct PortCandidate {
    /// Stable device id (MAC-derived, alias, or path basename).
    id: String,
    /// Current OS port path (`/dev/ttyACM0`).
    path: String,
    /// Espressif native USB-Serial-JTAG (skips the RTS auto-reset).
    native_usb: bool,
    /// USB `iSerialNumber` (the MAC for native USB), if exposed.
    mac: Option<String>,
}

/// Hotplug supervisor: the single authority on which devices exist.
///
/// Polls the live port set on `scan_interval`, registering newly appeared
/// devices and tearing down ones that have been absent for `DEBOUNCE`
/// consecutive scans (the debounce absorbs transient USB drops, which the
/// per-device reconnect loop handles on its own). Alias-pinned ports are
/// honoured even if they don't match the ESP heuristics, as long as the OS
/// currently lists them.
pub async fn run_supervisor(
    registry: Arc<DeviceRegistry>,
    baud: u32,
    scan_interval: Duration,
    aliases: Vec<(String, String)>,
) {
    /// Consecutive missing scans before a device is removed.
    const DEBOUNCE: u32 = 3;

    let mut missing: HashMap<String, u32> = HashMap::new();

    loop {
        // Enumerate ports off the runtime — `available_ports()` blocks, and
        // running it on a worker thread stalls the I/O reactor that drives the
        // active CSI streams.
        let aliases_scan = aliases.clone();
        let (candidates, _existing) =
            match tokio::task::spawn_blocking(move || scan_ports(&aliases_scan)).await {
                Ok(scan) => scan,
                Err(e) => {
                    tracing::error!("Port enumeration task failed: {e}");
                    sleep(scan_interval).await;
                    continue;
                }
            };

        let present_ids: HashSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();

        // ── Add newly appeared devices, and follow re-enumerated ones ─────
        // Identity is the stable id (MAC-derived), not the port path, so a
        // board that re-enumerates under a different `/dev/ttyACMx` is the
        // *same* device. If its path changed we tear the old task down (it is
        // pinned to the now-stale path) and respawn at the new one; if only
        // the path's occupant changed we leave the healthy task alone.
        for c in &candidates {
            missing.remove(&c.id);
            match registry.get(&c.id) {
                None => {
                    tracing::info!("Device added: {} ({})", c.id, c.path);
                    registry.insert(spawn_device(
                        c.id.clone(),
                        c.path.clone(),
                        baud,
                        c.native_usb,
                        c.mac.clone(),
                    ));
                }
                Some(dev) if dev.port_path != c.path => {
                    tracing::info!(
                        "Device {} re-enumerated: {} → {} (following by MAC)",
                        c.id,
                        dev.port_path,
                        c.path,
                    );
                    dev.shutdown.cancel();
                    registry.insert(spawn_device(
                        c.id.clone(),
                        c.path.clone(),
                        baud,
                        c.native_usb,
                        c.mac.clone(),
                    ));
                }
                Some(_) => {}
            }
        }

        // ── Tear down devices absent past the debounce window ─────────────
        // Keyed on id presence: a device whose id is no longer enumerated is
        // gone (a path change alone is handled above as a re-enumeration).
        for dev in registry.snapshot() {
            if present_ids.contains(dev.id.as_str()) {
                missing.remove(&dev.id);
                continue;
            }
            let count = missing.entry(dev.id.clone()).or_insert(0);
            *count += 1;
            if *count >= DEBOUNCE {
                tracing::info!("Device removed: {} ({})", dev.id, dev.port_path);
                dev.shutdown.cancel();
                registry.remove(&dev.id);
                missing.remove(&dev.id);
            }
        }

        sleep(scan_interval).await;
    }
}

/// Background task: owns the serial port for its lifetime.
///
/// - Continuously reconnects if the ESP32 disconnects.
/// - Reads incoming CSI frames from the serial port. The wire format is always
///   the firmware's `serialized` mode: COBS-framed postcard records delimited
///   by `\0`. Each frame is broadcast verbatim to WebSocket subscribers via
///   `csi_tx` and, when dumping, decoded and written to a Parquet session file.
/// - Watches `cmd_rx` for outgoing CLI command strings and writes them to the
///   port, appending a newline.
pub async fn run_serial_task(
    dev: Arc<DeviceHandle>,
    mut cmd_rx: mpsc::Receiver<String>,
    mut output_mode_rx: watch::Receiver<OutputMode>,
    mut session_file_rx: watch::Receiver<Option<String>>,
    mut info_request_rx: mpsc::Receiver<InfoResponder>,
) {
    let port_path = dev.port_path.clone();
    let baud = dev.baud_rate;
    const RECONNECT_DELAY: Duration = Duration::from_millis(800);

    loop {
        if dev.shutdown.is_cancelled() {
            break;
        }

        let mut stream = match tokio_serial::new(&port_path, baud).open_native_async() {
            Ok(s) => s,
            Err(e) => {
                dev.serial_connected.store(false, Ordering::SeqCst);
                dev.collection_running.store(false, Ordering::SeqCst);
                tracing::warn!("Failed to open serial port {port_path}: {e}. Retrying...");
                tokio::select! {
                    _ = sleep(RECONNECT_DELAY) => continue,
                    _ = dev.shutdown.cancelled() => break,
                }
            }
        };

        #[cfg(unix)]
        {
            // Allow opening a short-lived second handle for RTS reset operations.
            let _ = stream.set_exclusive(false);
        }

        // Auto-reset the ESP32 right after a successful serial connection by
        // pulsing RTS (RTS→EN). This matches the devkit EN/RTS wiring used by
        // ESP32 USB-UART boards (CP210x / CH340) and is what gets them to
        // (re)initialise and start answering `info` after the port opens.
        //
        // Skipped for native USB-Serial-JTAG chips (VID 0x303A). On those the
        // RTS/DTR pulse reboots the USB peripheral itself, so the device
        // re-enumerates and its `/dev/ttyACMx` node can return under a
        // different number — or, on slower hosts like the Raspberry Pi, the
        // re-enumeration races with the pinned-path reconnect and leaves the
        // USB CDC endpoint wedged (writes time out, the board never verifies,
        // and only a physical replug recovers it). These chips already answer
        // `info` without a reset; `quiesce_stale_stream` (a `q` to stop any
        // auto-stream) plus the periodic re-verify loop wakes them instead.
        if dev.native_usb {
            tracing::info!(
                "Skipping RTS auto-reset on {port_path} (native USB-Serial-JTAG; reset would re-enumerate the port)"
            );
        } else {
            let _ = stream.write_data_terminal_ready(false);
            if let Err(e) = stream.write_request_to_send(true) {
                tracing::warn!("Failed to assert RTS on {port_path}: {e}");
            } else {
                sleep(Duration::from_millis(100)).await;
                if let Err(e) = stream.write_request_to_send(false) {
                    tracing::warn!("Failed to deassert RTS on {port_path}: {e}");
                } else {
                    tracing::info!("ESP32 reset on connect via RTS ({port_path})");
                }
            }
        }

        dev.serial_connected.store(true, Ordering::SeqCst);
        tracing::info!("Opened serial port {port_path} @ {baud} baud");

        let exit = run_serial_connection(
            &dev,
            stream,
            &mut cmd_rx,
            &mut output_mode_rx,
            &mut session_file_rx,
            &mut info_request_rx,
        )
        .await;

        dev.serial_connected.store(false, Ordering::SeqCst);
        dev.collection_running.store(false, Ordering::SeqCst);
        // Disconnect invalidates the firmware identity — a different chip
        // may be re-attached on reconnect, so force a fresh verification.
        dev.firmware_verified.store(false, Ordering::SeqCst);
        *dev.device_info.lock().await = None;

        match exit {
            ConnectionExit::Disconnected => {
                // Pinned to dev.port_path — retry the SAME port, never re-detect
                // (re-detecting would let two device tasks race for one port).
                tracing::warn!("ESP32 disconnected on {port_path}; waiting for reconnect...");
                tokio::select! {
                    _ = sleep(RECONNECT_DELAY) => {}
                    _ = dev.shutdown.cancelled() => break,
                }
            }
            ConnectionExit::CommandChannelClosed => {
                tracing::info!("Command channel closed — shutting down serial task ({port_path})");
                break;
            }
            ConnectionExit::Shutdown => {
                tracing::info!("Device unplugged — shutting down serial task ({port_path})");
                break;
            }
        }
    }
}

enum ConnectionExit {
    Disconnected,
    CommandChannelClosed,
    Shutdown,
}

/// The connected chip's identity and its CSI wire layout, derived from the
/// firmware `info` block. Only constructible for chips with a known layout.
struct ChipInfo {
    /// Chip string as reported by the firmware (e.g. `esp32c6`).
    name: String,
    /// Wire layout the Parquet decoder applies.
    variant: ChipVariant,
}

impl ChipInfo {
    fn from_info(info: &DeviceInfo) -> Option<Self> {
        let name = info.chip.clone()?;
        let variant = ChipVariant::from_chip_str(&name)?;
        Some(ChipInfo { name, variant })
    }
}

async fn run_serial_connection(
    dev: &DeviceHandle,
    stream: tokio_serial::SerialStream,
    cmd_rx: &mut mpsc::Receiver<String>,
    output_mode_rx: &mut watch::Receiver<OutputMode>,
    session_file_rx: &mut watch::Receiver<Option<String>>,
    info_request_rx: &mut mpsc::Receiver<InfoResponder>,
) -> ConnectionExit {
    let port_path = dev.port_path.as_str();
    let csi_tx = &dev.csi_tx;
    let collection_running = &dev.collection_running;
    let firmware_verified = &dev.firmware_verified;
    let device_info = &dev.device_info;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();

    // ── Auto-verify firmware on connect ───────────────────────────────────
    // The chip just rebooted via the RTS pulse in run_serial_task. Give it
    // a moment to finish printing its boot banner, then ask `info` and
    // mirror the result into AppState. This is what makes command
    // endpoints unblock without requiring the user to call /api/info first.

    // The chip identity (from the `info` block) selects the wire layout the
    // Parquet decoder uses; refreshed on every successful info exchange.
    let mut chip: Option<ChipInfo> = None;

    sleep(Duration::from_millis(700)).await;
    // Native USB-Serial-JTAG chips skip the RTS reset above, so a device left
    // in a stale collecting state (e.g. an auto-start firmware, or a previous
    // run) keeps flooding binary CSI and would bury the `info` request. Stop
    // and drain it first so the CLI is responsive before we verify.
    if dev.native_usb {
        quiesce_stale_stream(&mut writer, &mut reader, port_path).await;
    }
    match do_info_exchange(&mut writer, &mut reader).await {
        Ok(info) => {
            tracing::info!(
                "Firmware verified: esp-csi-cli-rs/{} ({})",
                info.banner_version,
                info.chip.as_deref().unwrap_or("unknown chip"),
            );
            chip = ChipInfo::from_info(&info);
            firmware_verified.store(true, Ordering::SeqCst);
            *device_info.lock().await = Some(info);
        }
        Err(e) => {
            tracing::warn!(
                "Firmware not verified on {port_path}: {}. Command endpoints will return 412 Precondition Failed until verification succeeds.",
                e.message(),
            );
            firmware_verified.store(false, Ordering::SeqCst);
            *device_info.lock().await = None;
            if matches!(e, InfoExchangeError::Hard(_)) {
                return ConnectionExit::Disconnected;
            }
        }
    }

    // ── Output state (owned exclusively by this task) ─────────────────────
    // The wire is always serialized (COBS-framed postcard), so framing is
    // fixed; `drop_next_chunk` still skips the CLI echo straddling the first
    // COBS terminator after a command/transition.
    let mut current_mode = output_mode_rx.borrow().clone();
    let mut current_session_path = session_file_rx.borrow().clone();
    let mut drop_next_chunk = true;
    let mut sink: Option<ParquetSink> = None;
    let mut decode_errors: u64 = 0;

    // Open the Parquet sink immediately if mode/session already require it.
    sync_parquet_sink(&current_mode, &current_session_path, chip.as_ref(), &mut sink);

    // Per-second throughput counters. Reported on `metrics` ticks to expose
    // where a stream stalls: `frames_in` is what we pull off the serial port,
    // `frames_broadcast` is what reaches the per-device broadcast channel. A
    // gap between them (or asymmetry between two devices) localises the
    // bottleneck — see the matching WebSocket-side metrics in `routes::ws`.
    let mut frames_in: u64 = 0;
    let mut frames_broadcast: u64 = 0;
    let mut metrics = tokio::time::interval(Duration::from_secs(1));
    metrics.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    metrics.tick().await; // consume the immediate first tick

    // Periodically re-attempt firmware verification if the initial auto-verify
    // above did not succeed. The first tick fires immediately, so consume it
    // here to avoid re-verifying on the very next loop iteration.
    let mut reverify = tokio::time::interval(REVERIFY_INTERVAL);
    reverify.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    reverify.tick().await;

    loop {
        // ── React to runtime output-mode or session-file changes ──────────
        let mode_changed = output_mode_rx.has_changed().unwrap_or(false);
        let session_changed = session_file_rx.has_changed().unwrap_or(false);

        if mode_changed {
            current_mode = output_mode_rx.borrow_and_update().clone();
        }
        if session_changed {
            match session_file_rx.borrow_and_update().clone() {
                Some(path) => current_session_path = Some(path),
                None => {
                    // Dropping the sink flushes remaining rows and writes the
                    // Parquet footer (see ParquetSink::Drop).
                    sink = None;
                    current_session_path = None;
                    tracing::info!("Session ended — Parquet file finalized");
                }
            }
        }
        if mode_changed || session_changed {
            sync_parquet_sink(&current_mode, &current_session_path, chip.as_ref(), &mut sink);
        }

        // The wire is always serialized: COBS frames terminated by `\0`.
        const DELIMITER: u8 = b'\0';

        tokio::select! {
            // ── Per-second throughput report (only while collecting) ──────
            _ = metrics.tick() => {
                if collection_running.load(Ordering::SeqCst) {
                    tracing::debug!(
                        target: "csi_metrics",
                        "{port_path}: serial_in={frames_in}/s broadcast_out={frames_broadcast}/s ws_clients={}",
                        csi_tx.receiver_count(),
                    );
                }
                frames_in = 0;
                frames_broadcast = 0;
            }

            _ = dev.shutdown.cancelled() => {
                return ConnectionExit::Shutdown;
            }

            // ── Re-verify firmware while unverified and idle ──────────────
            // The branch is disabled once verified or while collecting (the
            // CLI is locked during collection), so a healthy device stops
            // probing as soon as it identifies itself.
            _ = reverify.tick(), if !firmware_verified.load(Ordering::SeqCst)
                && !collection_running.load(Ordering::SeqCst) =>
            {
                // The info block is text; drop the COBS chunk straddling it.
                drop_next_chunk = true;
                // Drop any partial frame; the info exchange runs in line-mode.
                buf.clear();
                // A native-USB device may still be flooding from a stale
                // session; stop and drain it before re-probing.
                if dev.native_usb {
                    quiesce_stale_stream(&mut writer, &mut reader, port_path).await;
                }

                match do_info_exchange(&mut writer, &mut reader).await {
                    Ok(info) => {
                        tracing::info!(
                            "Firmware verified on retry: esp-csi-cli-rs/{} ({})",
                            info.banner_version,
                            info.chip.as_deref().unwrap_or("unknown chip"),
                        );
                        chip = ChipInfo::from_info(&info);
                        firmware_verified.store(true, Ordering::SeqCst);
                        *device_info.lock().await = Some(info);
                    }
                    Err(InfoExchangeError::Soft(msg)) => {
                        // Still not esp-csi-cli-rs (or not responding yet);
                        // surface periodically so a stuck device is visible
                        // rather than silently retrying forever.
                        tracing::debug!("Re-verify on {port_path} still failing: {msg}");
                    }
                    Err(InfoExchangeError::Hard(msg)) => {
                        tracing::warn!("Serial link error during re-verify on {port_path}: {msg}");
                        return ConnectionExit::Disconnected;
                    }
                }
            }

            result = reader.read_until(DELIMITER, &mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("Serial port {port_path} closed (EOF)");
                        return ConnectionExit::Disconnected;
                    }
                    Ok(_) => {
                        if drop_next_chunk {
                            // Discard the first null-delimited chunk after a
                            // command/transition: it may hold CLI prompt/echo
                            // text buffered before the first binary frame.
                            drop_next_chunk = false;
                            buf.clear();
                            continue;
                        }

                        // Strip the trailing COBS `\0` terminator to leave just
                        // the COBS body.
                        if buf.last() == Some(&DELIMITER) {
                            buf.pop();
                        }

                        // Only forward to consumers while a session is active.
                        // After `POST /api/control/stop` flips
                        // `collection_running` to false, this drops any
                        // tail-of-session bytes (in-flight CSI frames, post-`q`
                        // boot text, command echoes) on the floor instead of
                        // leaking them. The buffer is still cleared below so the
                        // framer keeps draining serial input.
                        let still_collecting = collection_running.load(Ordering::SeqCst);

                        if still_collecting && !buf.is_empty() {
                            frames_in += 1;
                            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                                if let (Some(sink), Some(chip)) = (sink.as_mut(), chip.as_ref()) {
                                    match csi::decode(&buf, chip.variant) {
                                        Ok(decoded) => {
                                            let host_rx = chrono::Utc::now().timestamp_micros();
                                            if let Err(e) = sink.push(decoded, host_rx) {
                                                tracing::error!("Parquet write error: {e}");
                                            }
                                        }
                                        Err(e) => {
                                            decode_errors += 1;
                                            // Hex-dump the first few raw frames to
                                            // diagnose wire mismatches (run with
                                            // RUST_LOG=debug).
                                            if decode_errors <= 3 {
                                                let hex: String = buf
                                                    .iter()
                                                    .map(|b| format!("{b:02x}"))
                                                    .collect();
                                                tracing::warn!(
                                                    "Decode error #{decode_errors} on {port_path}: {e}; cobs_len={} frame_hex={hex}",
                                                    buf.len(),
                                                );
                                            } else if decode_errors.is_power_of_two() {
                                                tracing::warn!(
                                                    "Failed to decode CSI frame on {port_path} ({} total); check firmware/chip wire compatibility",
                                                    decode_errors,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            if matches!(current_mode, OutputMode::Stream | OutputMode::Both)
                                && csi_tx.send(buf.clone()).is_ok()
                            {
                                frames_broadcast += 1;
                            }
                        }
                        buf.clear();
                    }
                    Err(e) => {
                        tracing::error!("Serial read error on {port_path}: {e}");
                        return ConnectionExit::Disconnected;
                    }
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        tracing::debug!("→ ESP32: {cmd}");
                        // Command echoes are text but the wire framing is
                        // null-delimited; drop the next chunk so echoes don't
                        // mix with binary payload.
                        drop_next_chunk = true;
                        let line = format!("{cmd}\r\n");
                        if let Err(e) = writer.write_all(line.as_bytes()).await {
                            tracing::error!("Serial write error: {e}");
                            return ConnectionExit::Disconnected;
                        }
                        // NB: deliberately no `flush()` here. tokio-serial maps
                        // `flush` to a blocking `tcdrain()` that runs on the
                        // tokio worker thread and waits for the UART/USB FIFO to
                        // physically empty. If the device browns out or its USB
                        // endpoint wedges (common with several native-USB boards
                        // on one bus), `tcdrain` blocks forever and takes the
                        // worker with it — enough of them freezes the whole
                        // runtime. `write_all` has already handed the bytes to
                        // the kernel via a non-blocking write; the USB stack
                        // sends them without us draining.
                    }
                    None => {
                        return ConnectionExit::CommandChannelClosed;
                    }
                }
            }

            req = info_request_rx.recv() => {
                let Some(responder) = req else { continue };

                if collection_running.load(Ordering::SeqCst) {
                    let _ = responder.send(Err(
                        "collection is running; CLI is locked until stop".to_string(),
                    ));
                    continue;
                }

                // The info block is text — drop any partial COBS chunk
                // straddling our text exchange.
                drop_next_chunk = true;
                // Discard any partial CSI frame the framer was accumulating;
                // the info exchange runs in line-mode below.
                buf.clear();

                match do_info_exchange(&mut writer, &mut reader).await {
                    Ok(info) => {
                        chip = ChipInfo::from_info(&info);
                        firmware_verified.store(true, Ordering::SeqCst);
                        *device_info.lock().await = Some(info.clone());
                        let _ = responder.send(Ok(info));
                    }
                    Err(InfoExchangeError::Soft(msg)) => {
                        firmware_verified.store(false, Ordering::SeqCst);
                        *device_info.lock().await = None;
                        let _ = responder.send(Err(msg));
                    }
                    Err(InfoExchangeError::Hard(msg)) => {
                        firmware_verified.store(false, Ordering::SeqCst);
                        *device_info.lock().await = None;
                        let _ = responder.send(Err(msg));
                        return ConnectionExit::Disconnected;
                    }
                }
            }
        }
    }
}

/// Bring a possibly-streaming device back to a responsive CLI.
///
/// Sends a stop (`q`) and discards whatever the device emits until the stream
/// goes idle (or a short cap elapses). Used on native USB-Serial-JTAG chips,
/// which skip the RTS reset and so may still be flooding CSI from a previous
/// session — without this, that flood buries the `info` request and the device
/// never verifies.
async fn quiesce_stale_stream<W, R>(writer: &mut W, reader: &mut BufReader<R>, port_path: &str)
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // No `flush()`: it maps to a blocking `tcdrain()` on the worker thread that
    // hangs forever if the device's USB endpoint has wedged. `write_all` already
    // delivered the bytes to the kernel.
    let _ = writer.write_all(b"q\r\n").await;

    let mut scratch = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut drained = 0usize;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, reader.read(&mut scratch)).await {
            Ok(Ok(0)) => break,           // EOF
            Ok(Ok(n)) => drained += n,    // discard backlog and keep draining
            Ok(Err(_)) => break,
            Err(_) => break,              // idle — stream has quiesced
        }
    }
    if drained > 0 {
        tracing::info!("Drained {drained} bytes of stale stream on {port_path} before verify");
    }
}

/// Issue a single `info` command on the link and read until the `END-INFO`
/// sentinel arrives or [`INFO_RESPONSE_TIMEOUT`] elapses. Returns
/// `Soft` errors when the link is healthy but the firmware is not (or not
/// `esp-csi-cli-rs`); `Hard` errors when the I/O itself failed.
async fn do_info_exchange<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
) -> Result<DeviceInfo, InfoExchangeError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    if let Err(e) = writer.write_all(b"info\r\n").await {
        return Err(InfoExchangeError::Hard(format!("Serial write error: {e}")));
    }
    // No `flush()`: tokio-serial's flush is a blocking `tcdrain()` that wedges
    // the worker thread if the device's USB endpoint stalls. The non-blocking
    // `write_all` above is sufficient to send the command.

    let deadline = tokio::time::Instant::now() + INFO_RESPONSE_TIMEOUT;
    let mut info_buf: Vec<u8> = Vec::new();

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(InfoExchangeError::Soft(
                "info command timed out; firmware may not be esp-csi-cli-rs".to_string(),
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        let read_fut = reader.read_until(b'\n', &mut info_buf);
        match tokio::time::timeout(remaining, read_fut).await {
            Ok(Ok(0)) => {
                return Err(InfoExchangeError::Hard(
                    "serial closed during info exchange".to_string(),
                ));
            }
            Ok(Ok(_)) => {
                if find_subsequence(&info_buf, b"END-INFO").is_some() {
                    return parse_info_block(&info_buf).map_err(InfoExchangeError::Soft);
                }
            }
            Ok(Err(e)) => {
                return Err(InfoExchangeError::Hard(format!("Serial read error: {e}")));
            }
            Err(_) => {
                return Err(InfoExchangeError::Soft(
                    "info command timed out; firmware may not be esp-csi-cli-rs".to_string(),
                ));
            }
        }
    }
}

/// Parse the firmware-identification block emitted by the device-side
/// `info` command. The block is delimited by `ESP-CSI-CLI/<version>` (start)
/// and `END-INFO` (end), with `key=value` lines in between.
fn parse_info_block(buf: &[u8]) -> Result<DeviceInfo, String> {
    let text = String::from_utf8_lossy(buf);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();

    let start = lines
        .iter()
        .position(|l| l.starts_with("ESP-CSI-CLI/"))
        .ok_or_else(|| {
            "info magic prefix 'ESP-CSI-CLI/' not seen — firmware is not esp-csi-cli-rs"
                .to_string()
        })?;
    let end = lines
        .iter()
        .skip(start)
        .position(|l| *l == "END-INFO")
        .map(|p| start + p)
        .ok_or_else(|| "END-INFO sentinel not seen in info block".to_string())?;

    let banner_version = lines[start]
        .strip_prefix("ESP-CSI-CLI/")
        .unwrap_or("")
        .to_string();

    let mut info = DeviceInfo {
        banner_version,
        name: None,
        version: None,
        chip: None,
        mac: None,
        protocol: None,
        features: Vec::new(),
    };

    for line in &lines[start + 1..end] {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "name" => info.name = Some(v.to_string()),
            "version" => info.version = Some(v.to_string()),
            "chip" => info.chip = Some(v.to_string()),
            "mac" => info.mac = Some(v.to_string()),
            "protocol" => info.protocol = v.parse().ok(),
            "features" => {
                info.features = v
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    Ok(info)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Reconcile the Parquet sink with the active output mode and session path.
///
/// Opens a sink when dumping is active and a session path is set (requires a
/// known chip so frames can be decoded); drops the sink — finalizing the file —
/// when switching to stream-only.
fn sync_parquet_sink(
    mode: &OutputMode,
    session_path: &Option<String>,
    chip: Option<&ChipInfo>,
    sink: &mut Option<ParquetSink>,
) {
    match mode {
        OutputMode::Dump | OutputMode::Both => {
            if sink.is_none() {
                if let Some(path) = session_path {
                    let Some(chip) = chip else {
                        tracing::error!(
                            "Cannot open Parquet dump {path}: chip not identified or unsupported; \
                             frames cannot be decoded. Streaming (if enabled) is unaffected."
                        );
                        return;
                    };
                    match ParquetSink::open(path, &chip.name) {
                        Ok(s) => {
                            tracing::info!("Opened Parquet dump: {path} (chip {})", chip.name);
                            *sink = Some(s);
                        }
                        Err(e) => {
                            tracing::error!("Failed to open Parquet dump {path}: {e}");
                        }
                    }
                }
            }
        }
        OutputMode::Stream => {
            if sink.take().is_some() {
                // Dropping the sink finalizes the Parquet file.
                tracing::info!("Switched to stream mode — Parquet file finalized");
            }
        }
    }
}
