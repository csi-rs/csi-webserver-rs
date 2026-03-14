use axum::{Json, extract::State, http::StatusCode};
use chrono::Local;
use std::sync::atomic::Ordering;
use tokio::time::{Duration, sleep};
use tokio_serial::{SerialPort, SerialPortBuilderExt};

use crate::{
    models::{ApiResponse, CollectionStatusResponse, OutputMode, StartConfig},
    state::AppState,
};

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
    if !state.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }

    let baud: u32 = std::env::var("CSI_BAUD_RATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(115_200);

    let current_port = state.port_path.lock().await.clone();

    let mut port = match tokio_serial::new(current_port.as_str(), baud).open_native_async() {
        Ok(p) => p,
        Err(e) => {
            state.serial_connected.store(false, Ordering::SeqCst);
            state.collection_running.store(false, Ordering::SeqCst);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse {
                    success: false,
                    message: format!("Failed to open serial port for reset: {e}"),
                }),
            );
        }
    };

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

    state.collection_running.store(false, Ordering::SeqCst);
    let _ = state.session_file_tx.send(None);

    tracing::info!("ESP32 reset via RTS on {}", current_port);
    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: "ESP32 reset triggered via RTS".to_string(),
        }),
    )
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

    if state
        .collection_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (
            StatusCode::CONFLICT,
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
            let path = format!(
                "csi_dump_{}.bin",
                Local::now().format("%Y%m%d_%H%M%S")
            );
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
