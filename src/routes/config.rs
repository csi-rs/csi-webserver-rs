//! Handlers for configuration endpoints under `/api/config/*`.

use axum::{Json, http::StatusCode};
use std::sync::atomic::Ordering;

use crate::{
    models::{
        ApiResponse, CollectionModeConfig, CsiConfig, CsiDeliveryConfig, DeviceConfig,
        IoTasksConfig, OutputMode, OutputModeConfig, ProtocolConfig, RateConfig, TrafficConfig,
        WifiConfig,
    },
    routes::Device,
    state::DeviceHandle,
};

// ─── GET /api/config ────────────────────────────────────────────────────────

/// Return the server-side cached device configuration as JSON.
pub async fn get_config(Device(dev): Device) -> Json<DeviceConfig> {
    let config = dev.config.lock().await;
    Json(config.clone())
}

// ─── POST /api/config/reset ─────────────────────────────────────────────────

pub async fn reset_config(Device(dev): Device) -> (StatusCode, Json<ApiResponse>) {
    let result = send_cmd(&dev, "reset-config".to_string()).await;
    if result.0 == StatusCode::OK {
        // Mirror the firmware's UserConfig::new() / CsiConfig::default()
        // snapshot so the cache reflects what's actually live on the chip,
        // even before the user re-sends any `set-*` commands.
        *dev.config.lock().await = DeviceConfig::firmware_defaults();
    }
    result
}

// ─── POST /api/config/wifi ──────────────────────────────────────────────────

pub async fn set_wifi(
    Device(dev): Device,
    Json(body): Json<WifiConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = match body.to_cli_command() {
        Ok(c) => c,
        Err(message) => return bad_request(message),
    };
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        let mut cfg = dev.config.lock().await;
        cfg.wifi.mode = Some(body.mode);
        if body.channel.is_some() {
            cfg.wifi.channel = body.channel;
        }
        if body.sta_ssid.is_some() {
            cfg.wifi.sta_ssid = body.sta_ssid;
        }
        if let Some(mac) = body.peer_mac {
            // Empty clears the filter back to auto on the device; mirror that
            // in the cache so the displayed value matches `show-config`.
            cfg.wifi.peer_mac = Some(if mac.is_empty() { "auto".to_string() } else { mac });
        }
        if let Some(ht40) = body.ht40 {
            // `off` is an alias for `none` on the device side.
            cfg.wifi.ht40 = Some(if ht40 == "off" { "none".to_string() } else { ht40 });
        }
        // sta_password is intentionally not cached.
    }
    result
}

// ─── POST /api/config/traffic ───────────────────────────────────────────────

pub async fn set_traffic(
    Device(dev): Device,
    Json(body): Json<TrafficConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        dev.config.lock().await.collection.traffic_hz = Some(body.frequency_hz);
    }
    result
}

// ─── POST /api/config/csi ───────────────────────────────────────────────────

pub async fn set_csi(
    Device(dev): Device,
    Json(body): Json<CsiConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let result = send_cmd(&dev, body.to_cli_command()).await;
    if result.0 == StatusCode::OK {
        let mut cfg = dev.config.lock().await;
        // Classic flags — each `disable_*=true` toggles the corresponding
        // `*_enabled` cached value to false. There is no `enable-*` flag
        // (the firmware exposes only disables); the only way to revert is
        // POST /api/config/reset.
        if body.disable_lltf == Some(true) {
            cfg.csi_config.lltf_enabled = Some(false);
        }
        if body.disable_htltf == Some(true) {
            cfg.csi_config.htltf_enabled = Some(false);
        }
        if body.disable_stbc_htltf == Some(true) {
            cfg.csi_config.stbc_htltf_enabled = Some(false);
        }
        if body.disable_ltf_merge == Some(true) {
            cfg.csi_config.ltf_merge_enabled = Some(false);
        }
        // HE (C5/C6) flags — same disable-only pattern, but stored as the
        // underlying `acquire_csi_*` u32 (0 = disabled, non-zero = enabled).
        if body.disable_csi == Some(true) {
            cfg.csi_config.acquire_csi = Some(0);
        }
        if body.disable_csi_legacy == Some(true) {
            cfg.csi_config.acquire_csi_legacy = Some(0);
        }
        if body.disable_csi_ht20 == Some(true) {
            cfg.csi_config.acquire_csi_ht20 = Some(0);
        }
        if body.disable_csi_ht40 == Some(true) {
            cfg.csi_config.acquire_csi_ht40 = Some(0);
        }
        if body.disable_csi_su == Some(true) {
            cfg.csi_config.acquire_csi_su = Some(0);
        }
        if body.disable_csi_mu == Some(true) {
            cfg.csi_config.acquire_csi_mu = Some(0);
        }
        if body.disable_csi_dcm == Some(true) {
            cfg.csi_config.acquire_csi_dcm = Some(0);
        }
        if body.disable_csi_beamformed == Some(true) {
            cfg.csi_config.acquire_csi_beamformed = Some(0);
        }
        if let Some(stbc) = body.csi_he_stbc {
            cfg.csi_config.csi_he_stbc = Some(stbc);
        }
        if let Some(scale) = body.val_scale_cfg {
            cfg.csi_config.val_scale_cfg = Some(scale);
        }
    }
    result
}

// ─── POST /api/config/collection-mode ──────────────────────────────────────

pub async fn set_collection_mode(
    Device(dev): Device,
    Json(body): Json<CollectionModeConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = match body.to_cli_command() {
        Ok(c) => c,
        Err(message) => return bad_request(message),
    };
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        dev.config.lock().await.collection.mode = Some(body.mode);
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
    Device(dev): Device,
    Json(body): Json<OutputModeConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let mode = match body.mode.to_ascii_lowercase().as_str() {
        "stream" => OutputMode::Stream,
        "dump" => OutputMode::Dump,
        "both" => OutputMode::Both,
        other => {
            return bad_request(format!(
                "Unknown output mode '{other}'; expected stream, dump, or both"
            ));
        }
    };
    let _ = dev.output_mode_tx.send(mode);
    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: format!("Output mode set to {}", body.mode),
        }),
    )
}

// ─── POST /api/config/rate ──────────────────────────────────────────────────

/// Pin the Wi-Fi PHY rate (only honored by ESP-NOW central / peripheral modes
/// on the firmware side; sniffer and station ignore it).
pub async fn set_rate(
    Device(dev): Device,
    Json(body): Json<RateConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = body.to_cli_command();
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        dev.config.lock().await.collection.phy_rate = Some(body.rate);
    }
    result
}

// ─── POST /api/config/protocol ──────────────────────────────────────────────

/// Set the Wi-Fi PHY protocol applied at the start of each collection run.
/// Forwards `set-protocol`. The setting is read at `start`, so change it
/// before starting a run.
pub async fn set_protocol(
    Device(dev): Device,
    Json(body): Json<ProtocolConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = match body.to_cli_command() {
        Ok(c) => c,
        Err(message) => return bad_request(message),
    };
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        // Cache the normalized (lowercased) value so it matches `show-config`.
        dev.config.lock().await.collection.protocol = Some(body.protocol.to_ascii_lowercase());
    }
    result
}

// ─── POST /api/config/io-tasks ──────────────────────────────────────────────

/// Toggle per-direction TX/RX Embassy tasks. Either or both fields may be set;
/// omitted fields preserve the current device-side value.
pub async fn set_io_tasks(
    Device(dev): Device,
    Json(body): Json<IoTasksConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = match body.to_cli_command() {
        Ok(c) => c,
        Err(message) => return bad_request(message),
    };
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        let mut cfg = dev.config.lock().await;
        if let Some(tx) = body.tx {
            cfg.collection.io_tx_enabled = Some(tx);
        }
        if let Some(rx) = body.rx {
            cfg.collection.io_rx_enabled = Some(rx);
        }
    }
    result
}

// ─── POST /api/config/csi-delivery ──────────────────────────────────────────

/// Switch the CSI delivery path and/or toggle the inline log gate. Either or
/// both fields may be set; omitted fields preserve the current device-side
/// value. Takes effect immediately on the firmware (next CSI packet).
pub async fn set_csi_delivery(
    Device(dev): Device,
    Json(body): Json<CsiDeliveryConfig>,
) -> (StatusCode, Json<ApiResponse>) {
    let cmd = match body.to_cli_command() {
        Ok(c) => c,
        Err(message) => return bad_request(message),
    };
    let result = send_cmd(&dev, cmd).await;
    if result.0 == StatusCode::OK {
        let mut cfg = dev.config.lock().await;
        if let Some(mode) = body.mode {
            cfg.csi_delivery_mode = Some(mode);
        }
        if let Some(logging) = body.logging {
            cfg.csi_logging_enabled = Some(logging);
        }
    }
    result
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

fn bad_request(message: String) -> (StatusCode, Json<ApiResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            success: false,
            message,
        }),
    )
}

async fn send_cmd(dev: &DeviceHandle, cmd: String) -> (StatusCode, Json<ApiResponse>) {
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

    match dev.cmd_tx.send(cmd.clone()).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Sent: {cmd}"),
            }),
        ),
        Err(e) => {
            let (status, message) = if !dev.serial_connected.load(Ordering::SeqCst) {
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

/// Trigger `show-stats` on the device. The actual counter snapshot is printed
/// over the serial UART by the firmware; on the host side it appears in the
/// regular CSI output stream (WebSocket / dump file). Requires the firmware
/// to be built with the `statistics` feature (default-on).
pub async fn show_stats(Device(dev): Device) -> (StatusCode, Json<ApiResponse>) {
    send_cmd(&dev, "show-stats".to_string()).await
}
