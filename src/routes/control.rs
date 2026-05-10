//! Handlers for collection control endpoints under `/api/control/*`.

use axum::{Json, extract::State, http::StatusCode};
use chrono::Local;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;
use tokio::time::{Duration, sleep, timeout};
use tokio_serial::{SerialPort, SerialPortBuilderExt};

use crate::{
    models::{ApiResponse, CollectionStatusResponse, OutputMode, StartConfig},
    state::AppState,
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
    State(state): State<AppState>,
) -> (StatusCode, Json<CollectionStatusResponse>) {
    let port_path = state.port_path.lock().await.clone();
    (
        StatusCode::OK,
        Json(CollectionStatusResponse::from_state(
            &state.serial_connected,
            &state.collection_running,
            port_path,
        )),
    )
}

// ─── POST /api/control/reset ───────────────────────────────────────────────

/// Reset the ESP32 by pulsing the RTS line (asserting EN low for 100 ms).
///
/// Works on all standard ESP32 devkits where the USB-UART adapter's RTS pin
/// is wired through a transistor to the chip's EN (enable/reset) pin.
/// Opens a short-lived second file descriptor on the serial port, pulses RTS,
/// then drops the handle immediately so the main serial task is unaffected.
pub async fn reset_esp32(State(state): State<AppState>) -> (StatusCode, Json<ApiResponse>) {
    // End any active session immediately so the serial task closes dump handles.
    state.collection_running.store(false, Ordering::SeqCst);
    let _ = state.session_file_tx.send(None);

    // The chip is about to reboot. Whatever firmware ran a moment ago may or
    // may not be running afterwards (the user might have re-flashed the
    // device); invalidate the cached identity so command endpoints stay
    // blocked until the post-reset re-verification confirms it.
    state.firmware_verified.store(false, Ordering::SeqCst);
    *state.device_info.lock().await = None;

    if !state.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }

    let current_port = state.port_path.lock().await.clone();

    let mut port = match tokio_serial::new(current_port.as_str(), state.baud_rate).open_native_async() {
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
    if state.info_request_tx.send(resp_tx).await.is_err() {
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
pub async fn stop_collection(State(state): State<AppState>) -> (StatusCode, Json<ApiResponse>) {
    if !state.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }
    if let Some(blocked) = state.require_firmware() {
        return blocked;
    }

    if !state.collection_running.load(Ordering::SeqCst) {
        return (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: "Collection not running".to_string(),
            }),
        );
    }

    match state.cmd_tx.send("q".to_string()).await {
        Ok(_) => {
            state.collection_running.store(false, Ordering::SeqCst);
            let _ = state.session_file_tx.send(None);
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
    State(state): State<AppState>,
    body: Option<Json<StartConfig>>,
) -> (StatusCode, Json<ApiResponse>) {
    if !state.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }
    if let Some(blocked) = state.require_firmware() {
        return blocked;
    }

    if state
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

    match state.cmd_tx.send(cmd.clone()).await {
        Ok(_) => {
            // Generate a timestamped session dump file path and notify the
            // serial task. The file is only opened if the output mode includes
            // Dump; otherwise the path is remembered and used if the mode
            // switches later during the same session.
            let path = format!("csi_dump_{}.bin", Local::now().format("%Y%m%d_%H%M%S"));
            let current_mode = state.output_mode_tx.borrow().clone();
            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                tracing::info!("New session dump file: {path}");
            }
            let _ = state.session_file_tx.send(Some(path));

            (
                StatusCode::OK,
                Json(ApiResponse {
                    success: true,
                    message: format!("Collection started: {cmd}"),
                }),
            )
        }
        Err(e) => {
            state.collection_running.store(false, Ordering::SeqCst);
            let (status, message) = if !state.serial_connected.load(Ordering::SeqCst) {
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
