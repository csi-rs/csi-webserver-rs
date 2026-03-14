use axum::{Json, extract::State, http::StatusCode};
use std::sync::atomic::Ordering;

use crate::{
    models::{
        ApiResponse, CollectionModeConfig, CsiConfig, DeviceConfig, LogModeConfig, OutputMode,
        OutputModeConfig, TrafficConfig, WifiConfig,
    },
    state::AppState,
};

// ─── GET /api/config ────────────────────────────────────────────────────────

/// Return the server-side cached device configuration as JSON.
pub async fn get_config(State(state): State<AppState>) -> Json<DeviceConfig> {
    let config = state.config.lock().await;
    Json(config.clone())
}

// ─── POST /api/config/reset ─────────────────────────────────────────────────

pub async fn reset_config(State(state): State<AppState>) -> (StatusCode, Json<ApiResponse>) {
    let result = send_cmd(&state, "reset-config".to_string()).await;
    if result.0 == StatusCode::OK {
        *state.config.lock().await = DeviceConfig::default();
    }
    result
}

// ─── POST /api/config/wifi ──────────────────────────────────────────────────

pub async fn set_wifi(
    State(state): State<AppState>,
    Json(body): Json<WifiConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&state, cmd).await;
    if result.0 == StatusCode::OK {
        let mut cfg = state.config.lock().await;
        cfg.wifi_mode = Some(body.mode);
        cfg.channel = body.channel;
        cfg.sta_ssid = body.sta_ssid;
    }
    result
}

// ─── POST /api/config/traffic ───────────────────────────────────────────────

pub async fn set_traffic(
    State(state): State<AppState>,
    Json(body): Json<TrafficConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&state, cmd).await;
    if result.0 == StatusCode::OK {
        state.config.lock().await.traffic_hz = Some(body.frequency_hz);
    }
    result
}

// ─── POST /api/config/csi ───────────────────────────────────────────────────

pub async fn set_csi(
    State(state): State<AppState>,
    Json(body): Json<CsiConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    send_cmd(&state, body.to_cli_command()).await
}

// ─── POST /api/config/collection-mode ──────────────────────────────────────

pub async fn set_collection_mode(
    State(state): State<AppState>,
    Json(body): Json<CollectionModeConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&state, cmd).await;
    if result.0 == StatusCode::OK {
        state.config.lock().await.collection_mode = Some(body.mode);
    }
    result
}

// ─── POST /api/config/log-mode ─────────────────────────────────────────────

/// Set the log mode on the device and update the serial task's frame delimiter.
///
/// Known modes (passed through verbatim to `esp-csi-cli-rs`):
/// - `"array-list"` — newline-delimited text packets
/// - `"cobs"`       — COBS-encoded binary frames, null-byte delimited
/// - `"none"`       — disable CSI output
pub async fn set_log_mode(
    State(state): State<AppState>,
    Json(body): Json<LogModeConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&state, cmd).await;
    if result.0 == StatusCode::OK {
        let mut cfg = state.config.lock().await;
        cfg.log_mode = Some(body.mode.clone());
        // Notify the serial task to switch its frame delimiter immediately.
        let _ = state.log_mode_tx.send(body.mode);
    }
    result
}

// ─── POST /api/config/output-mode ───────────────────────────────────────────

/// Switch the server's CSI output mode at runtime.
///
/// Body:
/// ```json
/// { "mode": "stream" }   // default — broadcast via WebSocket
/// { "mode": "dump" }     // write to session dump file; /api/ws returns 403
/// { "mode": "both" }     // write to file AND broadcast
/// ```
///
/// The change takes effect for the very next CSI frame received from the
/// serial port. If no session has been started yet the dump destination will
/// be set as soon as `POST /api/control/start` is called.
pub async fn set_output_mode(
    State(state): State<AppState>,
    Json(body): Json<OutputModeConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let mode = match body.mode.to_ascii_lowercase().as_str() {
        "stream" => OutputMode::Stream,
        "dump" => OutputMode::Dump,
        "both" => OutputMode::Both,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    success: false,
                    message: format!(
                        "Unknown output mode '{other}'; expected stream, dump, or both"
                    ),
                }),
            );
        }
    };
    let _ = state.output_mode_tx.send(mode);
    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: format!("Output mode set to {}", body.mode),
        }),
    )
}

// ─── Shared helper ──────────────────────────────────────────────────────────

async fn send_cmd(state: &AppState, cmd: String) -> (StatusCode, Json<ApiResponse>) {
    if !state.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            }),
        );
    }

    match state.cmd_tx.send(cmd.clone()).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Sent: {cmd}"),
            }),
        ),
        Err(e) => {
            let (status, message) = if !state.serial_connected.load(Ordering::SeqCst) {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ESP32 disconnected; serial command unavailable".to_string(),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to send command: {e}"),
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
