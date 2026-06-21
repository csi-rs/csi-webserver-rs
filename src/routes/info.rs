//! Firmware-identification endpoint at `GET /api/info`.
//!
//! This endpoint sends the device-side `info` command and surfaces the parsed
//! magic block as JSON. It exists primarily so a host can verify whether the
//! attached ESP is actually running `esp-csi-cli-rs`, and which build of it.

use axum::{Json, http::StatusCode};
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};

use crate::{
    models::{ApiResponse, DeviceInfo},
    routes::Device,
};

/// Slightly longer than the serial-side info timeout to allow for the
/// in-flight command to complete and reply over the oneshot.
const INFO_HTTP_TIMEOUT: Duration = Duration::from_millis(3000);

#[derive(serde::Serialize)]
#[serde(untagged)]
pub enum InfoResult {
    Ok(DeviceInfo),
    Err(ApiResponse),
}

/// `GET /api/info` — issue an `info` command on the device and return the
/// parsed identification block.
///
/// Status codes:
/// - `200 OK`              — valid `esp-csi-cli-rs` info block returned.
/// - `503 Service Unavailable` — ESP32 disconnected, or a collection is
///   running (the firmware CLI is locked while collecting; `q`/`Q` is the
///   only accepted byte until the run ends).
/// - `504 Gateway Timeout` — the device did not produce an info block within
///   the timeout. Most commonly this means the firmware is *not*
///   `esp-csi-cli-rs` (or it's a build that predates the `info` command).
/// - `502 Bad Gateway`     — the device responded with garbled output that
///   could not be parsed.
pub async fn get_info(Device(dev): Device) -> (StatusCode, Json<InfoResult>) {
    if !dev.serial_connected.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(InfoResult::Err(ApiResponse {
                success: false,
                message: "ESP32 disconnected; serial command unavailable".to_string(),
            })),
        );
    }

    if dev.collection_running.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(InfoResult::Err(ApiResponse {
                success: false,
                message: "Collection is running; stop it first to query info".to_string(),
            })),
        );
    }

    let (resp_tx, resp_rx) = oneshot::channel();
    if dev.info_request_tx.send(resp_tx).await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(InfoResult::Err(ApiResponse {
                success: false,
                message: "Serial task is shutting down".to_string(),
            })),
        );
    }

    match timeout(INFO_HTTP_TIMEOUT, resp_rx).await {
        Ok(Ok(Ok(info))) => (StatusCode::OK, Json(InfoResult::Ok(info))),
        Ok(Ok(Err(message))) => {
            // Heuristic mapping between failure mode and HTTP status:
            // - timeouts and missing magic prefix → 504 (firmware not present)
            // - parse / serial-link errors → 502 (bad gateway)
            let status = if message.contains("timed out")
                || message.contains("not esp-csi-cli-rs")
                || message.contains("magic prefix")
            {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            (
                status,
                Json(InfoResult::Err(ApiResponse {
                    success: false,
                    message,
                })),
            )
        }
        Ok(Err(_)) | Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(InfoResult::Err(ApiResponse {
                success: false,
                message: "info request timed out".to_string(),
            })),
        ),
    }
}
