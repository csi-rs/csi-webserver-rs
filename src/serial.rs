//! Serial-port discovery, connection lifecycle, and frame forwarding.
//!
//! The serial task reconnects automatically, accepts command strings from
//! route handlers, parses incoming frame boundaries based on log mode, then
//! writes to dump files and/or WebSocket broadcast channels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialPortType};

use crate::models::{DeviceInfo, LogMode, OutputMode};
use crate::state::InfoResponder;

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

/// Known ESP32 USB-UART adapter Vendor IDs.
const ESP_USB_VIDS: &[u16] = &[
    0x10C4, // Silicon Labs CP210x (most common on ESP32 devkits)
    0x1A86, // WCH CH340 / CH341
    0x303A, // Espressif built-in USB (ESP32-S3 / C3 / C6 native USB)
];

/// Detect the first available ESP32 USB serial port.
///
/// Resolution order:
/// 1. `CSI_SERIAL_PORT` environment variable override.
/// 2. First USB port whose name contains `usbserial` / `usbmodem` / `ttyUSB` / `ttyACM`,
///    or whose VID matches a known ESP chip.
/// 3. Any USB port as a last resort.
pub fn detect_esp_port() -> Result<String, String> {
    // Allow the user to pin a specific port without recompiling.
    if let Ok(port) = std::env::var("CSI_SERIAL_PORT") {
        tracing::info!("Using CSI_SERIAL_PORT override: {port}");
        return Ok(port);
    }

    let ports = tokio_serial::available_ports()
        .map_err(|e| format!("Failed to enumerate serial ports: {e}"))?;

    // First pass: match by known VID or recognisable port-name prefix.
    for port in &ports {
        if let SerialPortType::UsbPort(ref info) = port.port_type {
            let name_ok = port.port_name.contains("usbserial")
                || port.port_name.contains("usbmodem")
                || port.port_name.contains("ttyUSB")
                || port.port_name.contains("ttyACM");

            let vid_ok = ESP_USB_VIDS.contains(&info.vid);

            if name_ok || vid_ok {
                let product = info
                    .product
                    .as_deref()
                    .map(|p| format!(", {p}"))
                    .unwrap_or_default();
                tracing::info!(
                    "Auto-detected ESP port: {} (VID:{:04X} PID:{:04X}{product})",
                    port.port_name,
                    info.vid,
                    info.pid,
                );
                return Ok(port.port_name.clone());
            }
        }
    }

    // Second pass: fall back to any USB port.
    for port in &ports {
        if matches!(port.port_type, SerialPortType::UsbPort(_)) {
            tracing::warn!(
                "No known ESP port found — using first USB port: {}",
                port.port_name
            );
            return Ok(port.port_name.clone());
        }
    }

    let names: Vec<&str> = ports.iter().map(|p| p.port_name.as_str()).collect();
    Err(format!(
        "No USB serial ports detected. Available ports: [{}]",
        names.join(", ")
    ))
}

/// Background task: owns the serial port for its lifetime.
///
/// - Continuously reconnects if the ESP32 disconnects.
/// - Reads incoming frames from the serial port and broadcasts the raw bytes
///   to all WebSocket subscribers via `csi_tx`. The frame delimiter adapts to
///   the active log mode: `\0` for serialized, `\n` for text/array-list.
/// - Watches `cmd_rx` for outgoing CLI command strings and writes them to the
///   port, appending a newline.
/// - Does NOT set a log mode on startup — call `POST /api/config/log-mode` to
///   configure the device before collecting data.
pub async fn run_serial_task(
    initial_port_path: String,
    baud: u32,
    mut cmd_rx: mpsc::Receiver<String>,
    csi_tx: broadcast::Sender<Vec<u8>>,
    mut log_mode_rx: watch::Receiver<LogMode>,
    mut output_mode_rx: watch::Receiver<OutputMode>,
    mut session_file_rx: watch::Receiver<Option<String>>,
    mut info_request_rx: mpsc::Receiver<InfoResponder>,
    serial_connected: Arc<AtomicBool>,
    collection_running: Arc<AtomicBool>,
    firmware_verified: Arc<AtomicBool>,
    device_info: Arc<Mutex<Option<DeviceInfo>>>,
    shared_port_path: Arc<Mutex<String>>,
) {
    let mut port_path = initial_port_path;
    const RECONNECT_DELAY: Duration = Duration::from_millis(800);

    loop {
        {
            let mut lock = shared_port_path.lock().await;
            *lock = port_path.clone();
        }

        let mut stream = match tokio_serial::new(&port_path, baud).open_native_async() {
            Ok(s) => s,
            Err(e) => {
                serial_connected.store(false, Ordering::SeqCst);
                collection_running.store(false, Ordering::SeqCst);
                tracing::warn!("Failed to open serial port {port_path}: {e}. Retrying...");
                sleep(RECONNECT_DELAY).await;
                if let Ok(new_path) = detect_esp_port() {
                    port_path = new_path;
                }
                continue;
            }
        };

        #[cfg(unix)]
        {
            // Allow opening a short-lived second handle for RTS reset operations.
            let _ = stream.set_exclusive(false);
        }

        // Auto-reset ESP32 right after a successful serial connection.
        // This matches the devkit EN/RTS wiring used by ESP32 USB-UART boards.
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

        serial_connected.store(true, Ordering::SeqCst);
        tracing::info!("Opened serial port {port_path} @ {baud} baud");

        let exit = run_serial_connection(
            &port_path,
            stream,
            &mut cmd_rx,
            &csi_tx,
            &mut log_mode_rx,
            &mut output_mode_rx,
            &mut session_file_rx,
            &mut info_request_rx,
            &collection_running,
            &firmware_verified,
            &device_info,
        )
        .await;

        serial_connected.store(false, Ordering::SeqCst);
        collection_running.store(false, Ordering::SeqCst);
        // Disconnect invalidates the firmware identity — a different chip
        // may be re-attached on reconnect, so force a fresh verification.
        firmware_verified.store(false, Ordering::SeqCst);
        *device_info.lock().await = None;

        match exit {
            ConnectionExit::Disconnected => {
                tracing::warn!("ESP32 disconnected; waiting for reconnect...");
                sleep(RECONNECT_DELAY).await;
                if let Ok(new_path) = detect_esp_port() {
                    port_path = new_path;
                }
            }
            ConnectionExit::CommandChannelClosed => {
                tracing::info!("Command channel closed — shutting down serial task");
                break;
            }
        }
    }
}

enum ConnectionExit {
    Disconnected,
    CommandChannelClosed,
}

async fn run_serial_connection(
    port_path: &str,
    stream: tokio_serial::SerialStream,
    cmd_rx: &mut mpsc::Receiver<String>,
    csi_tx: &broadcast::Sender<Vec<u8>>,
    log_mode_rx: &mut watch::Receiver<LogMode>,
    output_mode_rx: &mut watch::Receiver<OutputMode>,
    session_file_rx: &mut watch::Receiver<Option<String>>,
    info_request_rx: &mut mpsc::Receiver<InfoResponder>,
    collection_running: &Arc<AtomicBool>,
    firmware_verified: &Arc<AtomicBool>,
    device_info: &Arc<Mutex<Option<DeviceInfo>>>,
) -> ConnectionExit {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();

    // ── Auto-verify firmware on connect ───────────────────────────────────
    // The chip just rebooted via the RTS pulse in run_serial_task. Give it
    // a moment to finish printing its boot banner, then ask `info` and
    // mirror the result into AppState. This is what makes command
    // endpoints unblock without requiring the user to call /api/info first.
    sleep(Duration::from_millis(700)).await;
    match do_info_exchange(&mut writer, &mut reader).await {
        Ok(info) => {
            tracing::info!(
                "Firmware verified: esp-csi-cli-rs/{} ({})",
                info.banner_version,
                info.chip.as_deref().unwrap_or("unknown chip"),
            );
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

    // ── Dump-file state (owned exclusively by this task) ──────────────────
    let mut current_mode = output_mode_rx.borrow().clone();
    let mut current_session_path = session_file_rx.borrow().clone();
    let mut current_log_mode = log_mode_rx.borrow().clone();
    let mut drop_next_serialized_chunk = matches!(current_log_mode, LogMode::Serialized);
    let mut dump_file: Option<tokio::fs::File> = None;

    // Open dump file immediately if mode/session already require it.
    sync_dump_file(&current_mode, &current_session_path, &mut dump_file).await;

    loop {
        // ── React to runtime output-mode or session-file changes ──────────
        let mode_changed = output_mode_rx.has_changed().unwrap_or(false);
        let session_changed = session_file_rx.has_changed().unwrap_or(false);
        let log_mode_changed = log_mode_rx.has_changed().unwrap_or(false);

        if mode_changed {
            current_mode = output_mode_rx.borrow_and_update().clone();
        }
        if session_changed {
            match session_file_rx.borrow_and_update().clone() {
                Some(path) => current_session_path = Some(path),
                None => {
                    dump_file = None;
                    current_session_path = None;
                    tracing::info!("Session ended — dump file closed");
                }
            }
        }
        if log_mode_changed {
            current_log_mode = log_mode_rx.borrow_and_update().clone();
            // Drop partial bytes collected under the previous framing mode.
            buf.clear();
            // Switching into serialized mode can leave CLI echo bytes queued
            // before the first COBS frame terminator.
            if matches!(current_log_mode, LogMode::Serialized) {
                drop_next_serialized_chunk = true;
            }
        }
        if mode_changed || session_changed {
            sync_dump_file(&current_mode, &current_session_path, &mut dump_file).await;
        }

        // Pick the frame delimiter based on the active mode.
        // Serialized mode is COBS-framed; text, array-list and esp-csi-tool
        // are newline-framed.
        let is_text_mode = matches!(current_log_mode, LogMode::Text);
        let is_array_list_mode = matches!(current_log_mode, LogMode::ArrayList);
        let is_esp_csi_tool_mode = matches!(current_log_mode, LogMode::EspCsiTool);
        let delimiter = if matches!(current_log_mode, LogMode::Serialized) {
            b'\0'
        } else {
            b'\n'
        };

        tokio::select! {
            result = reader.read_until(delimiter, &mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("Serial port {port_path} closed (EOF)");
                        return ConnectionExit::Disconnected;
                    }
                    Ok(_) => {
                        if matches!(current_log_mode, LogMode::Serialized) && drop_next_serialized_chunk {
                            // Discard the first null-delimited chunk after mode/command transitions.
                            // It may contain CLI prompt/echo lines buffered before binary frames.
                            drop_next_serialized_chunk = false;
                            buf.clear();
                            continue;
                        }

                        if is_text_mode {
                            // Text mode packets span multiple lines.
                            // The final line contains the actual CSI data array.
                            let text = String::from_utf8_lossy(&buf);
                            if !text.contains("csi raw data:") && buf.len() < 65536 {
                                // Keep accumulating lines for the same packet.
                                continue;
                            }

                            // Ignore command echoes / prompts / startup lines before packet body.
                            if let Some(start) = find_subsequence(&buf, b"mac:") {
                                if start > 0 {
                                    buf.drain(..start);
                                }
                            } else {
                                buf.clear();
                                continue;
                            }

                            // Strip control bytes that can appear when switching modes.
                            buf.retain(|b| {
                                *b == b'\n' || *b == b'\r' || *b == b'\t' || (*b >= 0x20 && *b <= 0x7E)
                            });
                        } else if is_array_list_mode {
                            // Array-list mode should emit one compact bracketed row per packet.
                            // Drop CLI echoes, prompts, and boot logs.
                            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                                buf.pop();
                            }
                            if buf.first() != Some(&b'[') || buf.last() != Some(&b']') {
                                buf.clear();
                                continue;
                            }
                        } else if is_esp_csi_tool_mode {
                            // Hernandez 26-column CSV. Each row begins with the
                            // literal label `CSI_DATA,`; drop everything else
                            // (CLI prompts, boot lines, echoes, headers).
                            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                                buf.pop();
                            }
                            if let Some(start) = find_subsequence(&buf, b"CSI_DATA,") {
                                if start > 0 {
                                    buf.drain(..start);
                                }
                            } else {
                                buf.clear();
                                continue;
                            }
                        }

                        if buf.last() == Some(&delimiter) {
                            buf.pop();
                        }
                        // For multiline text mode we might also want to strip a trailing \r from the last line
                        if is_text_mode && buf.last() == Some(&b'\r') {
                            buf.pop();
                        }

                        // Keep text outputs frame-separated for dump readability.
                        if !matches!(current_log_mode, LogMode::Serialized)
                            && !buf.is_empty()
                            && buf.last() != Some(&b'\n')
                        {
                            buf.push(b'\n');
                        }

                        // Only forward to consumers while a session is
                        // active. After `POST /api/control/stop` flips
                        // `collection_running` to false, this drops any
                        // tail-of-session bytes (in-flight CSI frames,
                        // post-`q` boot text, command echoes) on the floor
                        // instead of leaking them to WebSocket clients or
                        // the dump file. The buffer still gets cleared
                        // below, so the framer keeps draining serial
                        // input rather than back-pressuring it.
                        let still_collecting = collection_running.load(Ordering::SeqCst);

                        if still_collecting && !buf.is_empty() {
                            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                                if let Some(ref mut file) = dump_file {
                                    if matches!(current_log_mode, LogMode::Serialized) {
                                        let len = buf.len() as u32;
                                        if let Err(e) = file.write_all(&len.to_le_bytes()).await {
                                            tracing::error!("Dump write error (len): {e}");
                                        } else if let Err(e) = file.write_all(&buf).await {
                                            tracing::error!("Dump write error (data): {e}");
                                        }
                                    } else if let Err(e) = file.write_all(&buf).await {
                                        tracing::error!("Dump write error (text): {e}");
                                    }
                                }
                            }
                            if matches!(current_mode, OutputMode::Stream | OutputMode::Both) {
                                let _ = csi_tx.send(buf.clone());
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
                        if matches!(current_log_mode, LogMode::Serialized) {
                            // In serialized mode command echoes are text but framing is null-delimited.
                            // Drop the next chunk to avoid mixing those echoes with binary payload.
                            drop_next_serialized_chunk = true;
                        }
                        let line = format!("{cmd}\r\n");
                        if let Err(e) = writer.write_all(line.as_bytes()).await {
                            tracing::error!("Serial write error: {e}");
                            return ConnectionExit::Disconnected;
                        }
                        // Flush so the bytes leave the host buffer
                        // immediately. This matters most for the `q`
                        // stop signal — without an explicit flush, the
                        // OS may sit on the byte while CSI traffic keeps
                        // streaming back, delaying the firmware's
                        // STOP_REQUEST and prolonging the collection.
                        if let Err(e) = writer.flush().await {
                            tracing::error!("Serial flush error: {e}");
                            return ConnectionExit::Disconnected;
                        }
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

                if matches!(current_log_mode, LogMode::Serialized) {
                    // The info block is text — drop any partial COBS chunk
                    // straddling our text exchange.
                    drop_next_serialized_chunk = true;
                }
                // Discard any partial CSI frame the framer was accumulating;
                // the info exchange runs in line-mode below.
                buf.clear();

                match do_info_exchange(&mut writer, &mut reader).await {
                    Ok(info) => {
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
    if let Err(e) = writer.flush().await {
        return Err(InfoExchangeError::Hard(format!("Serial flush error: {e}")));
    }

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

async fn sync_dump_file(
    mode: &OutputMode,
    session_path: &Option<String>,
    dump_file: &mut Option<tokio::fs::File>,
) {
    match mode {
        OutputMode::Dump | OutputMode::Both => {
            if dump_file.is_none() {
                if let Some(path) = session_path {
                    match OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(path)
                        .await
                    {
                        Ok(f) => {
                            tracing::info!("Opened dump file: {path}");
                            *dump_file = Some(f);
                        }
                        Err(e) => {
                            tracing::error!("Failed to open dump file {path}: {e}");
                        }
                    }
                }
            }
        }
        OutputMode::Stream => {
            if dump_file.take().is_some() {
                tracing::info!("Switched to stream mode — dump file closed");
            }
        }
    }
}
