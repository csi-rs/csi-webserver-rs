//! Shared application state used by Axum route handlers.
//!
//! This state object wraps channels and runtime flags that coordinate route
//! requests with the long-running serial background task.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::models::{DeviceConfig, DeviceInfo, LogMode, OutputMode};

/// One-shot reply channel for an in-flight `info` exchange.
pub type InfoResponder = oneshot::Sender<Result<DeviceInfo, String>>;

/// Shared application state, cheaply cloned into every route handler via Axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// USB serial port path used to reach the ESP32 (e.g. `/dev/ttyUSB0`).
    /// Stored so route handlers can open a short-lived second fd for control
    /// operations such as RTS-triggered reset.
    pub port_path: Arc<Mutex<String>>,
    /// Baud rate negotiated at startup. The serial task and the RTS-reset
    /// handler both read this so a single source of truth governs the link.
    pub baud_rate: u32,
    /// Whether the serial task currently has an open and healthy ESP32 link.
    pub serial_connected: Arc<AtomicBool>,
    /// Best-effort flag: true after successful `start`, false after reset/disconnect.
    pub collection_running: Arc<AtomicBool>,
    /// Send CLI command strings to the serial background task.
    pub cmd_tx: mpsc::Sender<String>,
    /// Broadcast raw CSI frame bytes to all connected WebSocket clients.
    pub csi_tx: broadcast::Sender<Vec<u8>>,
    /// Notify the serial task of log-mode changes (affects the frame delimiter).
    pub log_mode_tx: Arc<watch::Sender<LogMode>>,
    /// Notify the serial task of output-mode changes (stream / dump / both).
    pub output_mode_tx: Arc<watch::Sender<OutputMode>>,
    /// Signal the serial task of the current session's dump file path.
    /// `Some(path)` → open/reuse that file; `None` → session ended, close file.
    pub session_file_tx: Arc<watch::Sender<Option<String>>>,
    /// Cached view of the current device configuration.
    pub config: Arc<Mutex<DeviceConfig>>,
    /// Issue an `info` command on the device and capture the magic block.
    /// The serial task synchronously consumes the responder.
    pub info_request_tx: mpsc::Sender<InfoResponder>,
    /// `true` once the serial task (or an explicit `/api/info` call) has
    /// observed a valid `ESP-CSI-CLI/<version>` magic block from the device.
    /// Cleared on disconnect, on `POST /api/control/reset` (until the
    /// post-reset re-verification completes), and on a failed verification.
    /// Command endpoints refuse to send while this is `false`.
    pub firmware_verified: Arc<AtomicBool>,
    /// Last successfully parsed firmware identification block. `None` until
    /// the first verification succeeds; cleared alongside `firmware_verified`.
    pub device_info: Arc<Mutex<Option<DeviceInfo>>>,
}

impl AppState {
    /// Returns an early-return tuple suitable for handlers when the firmware
    /// has not yet been verified as `esp-csi-cli-rs`. Use this to short-circuit
    /// any endpoint that issues a CLI command — sending commands to an
    /// unverified device may interact with whatever bootloader/firmware is
    /// listening in unintended ways.
    pub fn require_firmware(
        &self,
    ) -> Option<(axum::http::StatusCode, axum::Json<crate::models::ApiResponse>)> {
        if self
            .firmware_verified
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            None
        } else {
            Some((
                axum::http::StatusCode::PRECONDITION_FAILED,
                axum::Json(crate::models::ApiResponse {
                    success: false,
                    message:
                        "Firmware not verified as esp-csi-cli-rs. Call GET /api/info to verify, \
                         or POST /api/control/reset to power-cycle and re-check."
                            .to_string(),
                }),
            ))
        }
    }
}
