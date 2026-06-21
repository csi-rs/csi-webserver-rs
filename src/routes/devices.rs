//! Device registry listing at `GET /api/devices`.

use std::sync::atomic::Ordering;

use axum::{Json, extract::State};
use serde::Serialize;

use crate::{models::DeviceInfo, state::AppState};

/// One row of `GET /api/devices` — a snapshot of an attached device.
#[derive(Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub port_path: String,
    pub baud_rate: u32,
    pub serial_connected: bool,
    pub collection_running: bool,
    pub firmware_verified: bool,
    pub device_info: Option<DeviceInfo>,
}

/// `GET /api/devices` — list all currently attached devices and their status.
///
/// The set is maintained by the hotplug supervisor, so newly plugged-in boards
/// appear here within one scan interval and unplugged ones drop off after the
/// debounce window.
pub async fn list_devices(State(state): State<AppState>) -> Json<Vec<DeviceSummary>> {
    let mut out = Vec::new();
    for dev in state.devices.snapshot() {
        out.push(DeviceSummary {
            id: dev.id.clone(),
            port_path: dev.port_path.clone(),
            baud_rate: dev.baud_rate,
            serial_connected: dev.serial_connected.load(Ordering::SeqCst),
            collection_running: dev.collection_running.load(Ordering::SeqCst),
            firmware_verified: dev.firmware_verified.load(Ordering::SeqCst),
            device_info: dev.device_info.lock().await.clone(),
        });
    }
    Json(out)
}
