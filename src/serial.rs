use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialPortType};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::models::OutputMode;

const DEFAULT_BAUD_RATE: u32 = 115_200;

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
///   the active log mode: `\0` for COBS, `\n` for all text-based modes.
/// - Watches `cmd_rx` for outgoing CLI command strings and writes them to the
///   port, appending a newline.
/// - Does NOT set a log mode on startup — call `POST /api/config/log-mode` to
///   configure the device before collecting data.
pub async fn run_serial_task(
    initial_port_path: String,
    mut cmd_rx: mpsc::Receiver<String>,
    csi_tx: broadcast::Sender<Vec<u8>>,
    log_mode_rx: watch::Receiver<String>,
    mut output_mode_rx: watch::Receiver<OutputMode>,
    mut session_file_rx: watch::Receiver<Option<String>>,
    serial_connected: Arc<AtomicBool>,
    collection_running: Arc<AtomicBool>,
    shared_port_path: Arc<Mutex<String>>,
) {
    let baud = std::env::var("CSI_BAUD_RATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_BAUD_RATE);

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
            &log_mode_rx,
            &mut output_mode_rx,
            &mut session_file_rx,
        )
        .await;

        serial_connected.store(false, Ordering::SeqCst);
        collection_running.store(false, Ordering::SeqCst);

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
    log_mode_rx: &watch::Receiver<String>,
    output_mode_rx: &mut watch::Receiver<OutputMode>,
    session_file_rx: &mut watch::Receiver<Option<String>>,
) -> ConnectionExit {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();

    // ── Dump-file state (owned exclusively by this task) ──────────────────
    let mut current_mode = output_mode_rx.borrow().clone();
    let mut current_session_path = session_file_rx.borrow().clone();
    let mut dump_file: Option<tokio::fs::File> = None;

    // Open dump file immediately if mode/session already require it.
    sync_dump_file(&current_mode, &current_session_path, &mut dump_file).await;

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
                    dump_file = None;
                    current_session_path = None;
                    tracing::info!("Session ended — dump file closed");
                }
            }
        }
        if mode_changed || session_changed {
            sync_dump_file(&current_mode, &current_session_path, &mut dump_file).await;
        }

        // Pick the frame delimiter based on the current log mode.
        // COBS uses null-byte (0x00) framing; all text modes use newline.
        let mode_str = log_mode_rx.borrow().to_ascii_lowercase();
        let is_text_mode = mode_str.contains("text");
        let delimiter = if mode_str.contains("cobs") {
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
                        if is_text_mode {
                            // Text mode packets span multiple lines.
                            // The final line contains the actual CSI data array.
                            let text = String::from_utf8_lossy(&buf);
                            if !text.contains("csi raw data:") && buf.len() < 65536 {
                                // Keep accumulating lines for the same packet.
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
                        if !buf.is_empty() {
                            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                                if let Some(ref mut file) = dump_file {
                                    let len = buf.len() as u32;
                                    if let Err(e) = file.write_all(&len.to_le_bytes()).await {
                                        tracing::error!("Dump write error (len): {e}");
                                    } else if let Err(e) = file.write_all(&buf).await {
                                        tracing::error!("Dump write error (data): {e}");
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
                        let line = format!("{cmd}\r\n");
                        if let Err(e) = writer.write_all(line.as_bytes()).await {
                            tracing::error!("Serial write error: {e}");
                            return ConnectionExit::Disconnected;
                        }
                    }
                    None => {
                        return ConnectionExit::CommandChannelClosed;
                    }
                }
            }
        }
    }
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
