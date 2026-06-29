//! Handlers for collection control endpoints under `/api/control/*`.

use axum::{Json, http::StatusCode};
use chrono::Local;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;
use tokio::time::{Duration, sleep, timeout};
use tokio_serial::{SerialPort, SerialPortBuilderExt};

use crate::{
    models::{ApiResponse, CollectionStatusResponse, OutputMode, StartConfig},
    routes::Device,
};

/// Gives the chip enough time to finish bootloader + early init after the
/// RTS pulse before the post-reset `info` re-verification fires.
const POST_RESET_BOOT_DELAY: Duration = Duration::from_millis(800);
/// Cap on how long the reset endpoint will wait for the re-verification
/// reply. Longer than the serial-side info timeout to leave headroom for
/// channel hop-on-hop-off latency.
const POST_RESET_VERIFY_TIMEOUT: Duration = Duration::from_millis(3000);

// ─── GET /api/control/status ──────────────────────────────────────────────

pub async fn get_collection_status(
    Device(dev): Device,
) -> (StatusCode, Json<CollectionStatusResponse>) {
    (
        StatusCode::OK,
        Json(CollectionStatusResponse::from_state(
            &dev.serial_connected,
            &dev.collection_running,
            dev.port_path.clone(),
        )),
    )
}

// ─── POST /api/control/reset ───────────────────────────────────────────────

/// Reset the ESP32, then re-verify its firmware identity.
///
/// Two paths, chosen by adapter type:
/// - **UART adapters (CP210x / CH340):** pulse the RTS line (EN low for 100 ms)
///   on a short-lived second fd, then synchronously re-verify over the existing
///   link. This matches the devkit EN/RTS auto-reset wiring.
/// - **Native USB-Serial-JTAG (VID 0x303A):** send the firmware `restart`
///   command instead — pulsing RTS/DTR on these re-enumerates (and can wedge)
///   the USB device. Re-verification then happens asynchronously on reconnect,
///   so this path returns immediately.
pub async fn reset_esp32(Device(dev): Device) -> (StatusCode, Json<ApiResponse>) {
    // End any active session immediately so the serial task closes dump handles.
    dev.collection_running.store(false, Ordering::SeqCst);
    let _ = dev.session_file_tx.send(None);

    // The chip is about to reboot. Whatever firmware ran a moment ago may or
    // may not be running afterwards (the user might have re-flashed the
    // device); invalidate the cached identity so command endpoints stay
    // blocked until the post-reset re-verification confirms it.
    dev.firmware_verified.store(false, Ordering::SeqCst);
    *dev.device_info.lock().await = None;

    if !dev.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }

    // ── Native USB-Serial-JTAG: software reset over the CLI ───────────────
    // Pulsing RTS/DTR on these chips re-enumerates the USB device (and can
    // wedge it). Instead ask the firmware to reset itself via the `restart`
    // command (esp_hal::system::software_reset). The chip still re-enumerates,
    // but it always reboots cleanly into the app — and on a new `/dev/ttyACMx`
    // the supervisor follows it by MAC. We don't drive a synchronous re-verify
    // here because the re-enumeration may move the device onto a freshly
    // spawned task; the auto-verify-on-(re)connect path handles it.
    if dev.native_usb {
        if let Err(e) = dev.cmd_tx.send("restart".to_string()).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    message: format!("Failed to queue restart command: {e}"),
                }),
            );
        }
        tracing::info!(
            "ESP32 restart command sent on {} (native USB-Serial-JTAG; re-verifies on reconnect)",
            dev.port_path,
        );
        return (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: "ESP32 restart issued via firmware (native USB-Serial-JTAG). The device \
                          will re-enumerate and re-verify automatically; poll GET /api/devices or \
                          GET /api/info to confirm."
                    .to_string(),
            }),
        );
    }

    let current_port = dev.port_path.clone();

    let mut port = match tokio_serial::new(current_port.as_str(), dev.baud_rate).open_native_async() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    message: format!("Failed to open serial port for reset: {e}"),
                }),
            );
        }
    };

    #[cfg(unix)]
    {
        let _ = port.set_exclusive(false);
    }

    // Assert RTS → EN pulled low (chip in reset)
    let _ = port.write_data_terminal_ready(false);
    if let Err(e) = port.write_request_to_send(true) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                message: format!("RTS assert failed (adapter may not support it): {e}"),
            }),
        );
    }
    sleep(Duration::from_millis(100)).await;
    // Deassert RTS → EN released, chip boots
    let _ = port.write_request_to_send(false);
    // Drop the temporary handle; the main serial task is unaffected.
    drop(port);

    tracing::info!("ESP32 reset via RTS on {}", current_port);

    // Wait for the chip to boot, then re-verify firmware identity. The auto-
    // verify-on-connect path in run_serial_connection only fires when the
    // serial task itself reconnects; an in-band RTS reset keeps the same fd
    // open, so the re-verification has to be driven from here.
    sleep(POST_RESET_BOOT_DELAY).await;

    let (resp_tx, resp_rx) = oneshot::channel();
    if dev.info_request_tx.send(resp_tx).await.is_err() {
        return (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message:
                    "ESP32 reset triggered via RTS, but post-reset re-verification could not be \
                     queued (serial task is shutting down). Call GET /api/info to retry."
                        .to_string(),
            }),
        );
    }

    match timeout(POST_RESET_VERIFY_TIMEOUT, resp_rx).await {
        Ok(Ok(Ok(info))) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!(
                    "ESP32 reset; firmware re-verified: esp-csi-cli-rs/{} ({})",
                    info.banner_version,
                    info.chip.as_deref().unwrap_or("unknown chip"),
                ),
            }),
        ),
        Ok(Ok(Err(reason))) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!(
                    "ESP32 reset; firmware identity could NOT be re-verified \
                     (esp-csi-cli-rs may not be flashed): {reason}. Command endpoints will \
                     return 412 Precondition Failed until verification succeeds."
                ),
            }),
        ),
        Ok(Err(_)) | Err(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: "ESP32 reset; post-reset re-verification timed out. Call GET /api/info \
                          to retry."
                    .to_string(),
            }),
        ),
    }
}

// ─── POST /api/control/stop ────────────────────────────────────────────────

/// Stop an in-progress collection by sending a `q` byte over the serial port.
///
/// While the firmware has `IS_COLLECTING == true` the CLI is locked: only
/// `q`/`Q` is acted on, every other byte is discarded. The `q` byte triggers
/// `STOP_REQUEST` on the device, which unwinds both `run_duration` and `run`.
/// The trailing `\r\n` appended by the serial task is harmlessly discarded
/// during collection lock.
pub async fn stop_collection(Device(dev): Device) -> (StatusCode, Json<ApiResponse>) {
    if !dev.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }
    if let Some(blocked) = dev.require_firmware() {
        return blocked;
    }

    if !dev.collection_running.load(Ordering::SeqCst) {
        return (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: "Collection not running".to_string(),
            }),
        );
    }

    match dev.cmd_tx.send("q".to_string()).await {
        Ok(_) => {
            dev.collection_running.store(false, Ordering::SeqCst);
            let _ = dev.session_file_tx.send(None);
            (
                StatusCode::OK,
                Json(ApiResponse {
                    success: true,
                    message: "Collection stop requested".to_string(),
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to send stop: {e}"),
            }),
        ),
    }
}

// ─── POST /api/control/start ────────────────────────────────────────────────
///
/// Body (all fields optional):
/// ```json
/// { "duration": 120 }   // omit for indefinite collection
/// ```
pub async fn start_collection(
    Device(dev): Device,
    body: Option<Json<StartConfig>>,
) -> (StatusCode, Json<ApiResponse>) {
    if !dev.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }
    if let Some(blocked) = dev.require_firmware() {
        return blocked;
    }

    if dev
        .collection_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "Collection already running".to_string(),
            }),
        );
    }

    let cmd = body
        .map(|Json(b)| b.to_cli_command())
        .unwrap_or_else(|| "start".to_string());

    // Force the device into `serialized` (COBS-framed postcard) mode — the only
    // format this server consumes. The CLI is locked once collection runs, so
    // this must precede `start`; the ordered command channel guarantees the
    // device applies it first. Best-effort: if it fails, the `start` send below
    // surfaces the same channel error.
    let _ = dev
        .cmd_tx
        .send("set-log-mode --mode=serialized".to_string())
        .await;

    match dev.cmd_tx.send(cmd.clone()).await {
        Ok(_) => {
            // Generate a timestamped, per-device session dump file path and
            // notify the serial task. The id keeps concurrent devices from
            // colliding on one file. The file is only opened if the output
            // mode includes Dump; otherwise the path is remembered and used if
            // the mode switches later during the same session.
            let path = format!(
                "csi_dump_{}_{}.parquet",
                dev.id,
                Local::now().format("%Y%m%d_%H%M%S")
            );
            let current_mode = dev.output_mode_tx.borrow().clone();
            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                tracing::info!("New session dump file: {path}");
            }
            let _ = dev.session_file_tx.send(Some(path));

            (
                StatusCode::OK,
                Json(ApiResponse {
                    success: true,
                    message: format!("Collection started: {cmd}"),
                }),
            )
        }
        Err(e) => {
            dev.collection_running.store(false, Ordering::SeqCst);
            let (status, message) = if !dev.serial_connected.load(Ordering::SeqCst) {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ESP32 disconnected; serial command unavailable".to_string(),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to start collection: {e}"),
                )
            };
            (
                status,
                Json(ApiResponse {
                    success: false,
                    message,
                }),
            )
        }
    }
}
