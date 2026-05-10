//! Data models used by HTTP handlers and runtime control flow.
//!
//! This module contains:
//! - request-body structs for config/control endpoints,
//! - runtime enums used by watch channels,
//! - common API response payloads.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

// ─── Device config (cached state) ─────────────────────────────────────────

/// Server-side cached view of device-side `UserConfig`.
///
/// Fields are best-effort and updated when the corresponding HTTP endpoint
/// is hit successfully. They may drift if the firmware is re-flashed or if
/// commands are sent over a parallel channel.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub wifi_mode: Option<String>,
    pub channel: Option<u8>,
    pub sta_ssid: Option<String>,
    pub traffic_hz: Option<u64>,
    pub collection_mode: Option<String>,
    pub log_mode: Option<String>,
    pub phy_rate: Option<String>,
    pub io_tx_enabled: Option<bool>,
    pub io_rx_enabled: Option<bool>,
    pub csi_delivery_mode: Option<String>,
    pub csi_logging_enabled: Option<bool>,
}

// ─── Quoting helpers ──────────────────────────────────────────────────────

/// Quote a free-form string argument for `esp-csi-cli-rs`.
///
/// The CLI accepts both `'…'` and `"…"`; the opening quote style is
/// matched by the same style and the other quote is treated literally.
/// Spaces inside quotes are forwarded as `0x1F` and decoded back to `' '`
/// in the device-side handler. Underscores are passed through literally
/// (no shorthand substitution).
fn quote_cli_arg(s: &str) -> Result<String, String> {
    if s.contains('\n') || s.contains('\r') {
        return Err("value cannot contain newline characters".to_string());
    }
    if !s.contains('\'') {
        Ok(format!("'{s}'"))
    } else if !s.contains('"') {
        Ok(format!("\"{s}\""))
    } else {
        Err("value cannot contain both single and double quote characters".to_string())
    }
}

// ─── HTTP request bodies ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WifiConfig {
    /// `station` | `sniffer` | `esp-now-central` | `esp-now-peripheral`.
    pub mode: String,
    pub sta_ssid: Option<String>,
    pub sta_password: Option<String>,
    pub channel: Option<u8>,
}

impl WifiConfig {
    /// Validate values and emit the matching `set-wifi …` line.
    pub fn to_cli_command(&self) -> Result<String, String> {
        match self.mode.as_str() {
            "station" | "sniffer" | "esp-now-central" | "esp-now-peripheral" => {}
            other => {
                return Err(format!(
                    "Unknown wifi mode '{other}'; expected station, sniffer, esp-now-central, or esp-now-peripheral"
                ));
            }
        }

        let mut cmd = format!("set-wifi --mode={}", self.mode);

        if let Some(ssid) = &self.sta_ssid {
            if ssid.len() > 32 {
                return Err(format!(
                    "sta_ssid is {} bytes; firmware limit is 32 bytes",
                    ssid.len()
                ));
            }
            cmd.push_str(&format!(" --sta-ssid={}", quote_cli_arg(ssid)?));
        }

        if let Some(pass) = &self.sta_password {
            if pass.len() > 32 {
                return Err(format!(
                    "sta_password is {} bytes; firmware limit is 32 bytes",
                    pass.len()
                ));
            }
            cmd.push_str(&format!(" --sta-password={}", quote_cli_arg(pass)?));
        }

        if let Some(ch) = self.channel {
            cmd.push_str(&format!(" --set-channel={ch}"));
        }

        Ok(cmd)
    }
}

#[derive(Debug, Deserialize)]
pub struct TrafficConfig {
    /// Traffic generation frequency in Hz; `0` disables generation.
    pub frequency_hz: u64,
}

impl TrafficConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-traffic --frequency-hz={}", self.frequency_hz)
    }
}

/// CSI feature flags. Classic (ESP32 / ESP32-C3 / ESP32-S3) and HE
/// (ESP32-C5 / ESP32-C6) parameters are merged here; the firmware will
/// silently ignore flags that are not part of its compiled-in variant.
/// Only flags set to `true` are forwarded.
#[derive(Debug, Deserialize)]
pub struct CsiConfig {
    // ── Classic (non-C5/C6) ────────────────────────────────────────────
    pub disable_lltf: Option<bool>,
    pub disable_htltf: Option<bool>,
    pub disable_stbc_htltf: Option<bool>,
    pub disable_ltf_merge: Option<bool>,
    // ── HE (C5/C6) ─────────────────────────────────────────────────────
    pub disable_csi: Option<bool>,
    pub disable_csi_legacy: Option<bool>,
    pub disable_csi_ht20: Option<bool>,
    pub disable_csi_ht40: Option<bool>,
    pub disable_csi_su: Option<bool>,
    pub disable_csi_mu: Option<bool>,
    pub disable_csi_dcm: Option<bool>,
    pub disable_csi_beamformed: Option<bool>,
    /// `0` HE-LTF1, `1` HE-LTF2, `2` even sample (default).
    pub csi_he_stbc: Option<u32>,
    /// `0..=3`; default `2`.
    pub val_scale_cfg: Option<u32>,
}

impl CsiConfig {
    pub fn to_cli_command(&self) -> String {
        let mut cmd = "set-csi".to_string();
        if self.disable_lltf.unwrap_or(false) {
            cmd.push_str(" --disable-lltf");
        }
        if self.disable_htltf.unwrap_or(false) {
            cmd.push_str(" --disable-htltf");
        }
        if self.disable_stbc_htltf.unwrap_or(false) {
            cmd.push_str(" --disable-stbc-htltf");
        }
        if self.disable_ltf_merge.unwrap_or(false) {
            cmd.push_str(" --disable-ltf-merge");
        }
        if self.disable_csi.unwrap_or(false) {
            cmd.push_str(" --disable-csi");
        }
        if self.disable_csi_legacy.unwrap_or(false) {
            cmd.push_str(" --disable-csi-legacy");
        }
        if self.disable_csi_ht20.unwrap_or(false) {
            cmd.push_str(" --disable-csi-ht20");
        }
        if self.disable_csi_ht40.unwrap_or(false) {
            cmd.push_str(" --disable-csi-ht40");
        }
        if self.disable_csi_su.unwrap_or(false) {
            cmd.push_str(" --disable-csi-su");
        }
        if self.disable_csi_mu.unwrap_or(false) {
            cmd.push_str(" --disable-csi-mu");
        }
        if self.disable_csi_dcm.unwrap_or(false) {
            cmd.push_str(" --disable-csi-dcm");
        }
        if self.disable_csi_beamformed.unwrap_or(false) {
            cmd.push_str(" --disable-csi-beamformed");
        }
        if let Some(stbc) = self.csi_he_stbc {
            cmd.push_str(&format!(" --csi-he-stbc={stbc}"));
        }
        if let Some(scale) = self.val_scale_cfg {
            cmd.push_str(&format!(" --val-scale-cfg={scale}"));
        }
        cmd
    }
}

#[derive(Debug, Deserialize)]
pub struct CollectionModeConfig {
    /// `collector` or `listener`.
    pub mode: String,
}

impl CollectionModeConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        match self.mode.as_str() {
            "collector" | "listener" => {
                Ok(format!("set-collection-mode --mode={}", self.mode))
            }
            other => Err(format!(
                "Unknown collection mode '{other}'; expected collector or listener"
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LogModeConfig {
    pub mode: LogMode,
}

impl LogModeConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-log-mode --mode={}", self.mode.as_cli_value())
    }
}

/// Supported CSI log formats exposed by `esp-csi-cli-rs set-log-mode`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogMode {
    /// Verbose human-readable output with metadata.
    Text,
    /// Compact one-line text output per packet.
    #[default]
    ArrayList,
    /// Binary COBS-framed postcard output.
    Serialized,
    /// Hernandez-style 26-column CSV (compatible with the ESP32-CSI-Tool collector).
    EspCsiTool,
}

impl LogMode {
    pub fn as_cli_value(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::ArrayList => "array-list",
            Self::Serialized => "serialized",
            Self::EspCsiTool => "esp-csi-tool",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StartConfig {
    /// Collection duration in seconds; omit for indefinite collection.
    pub duration: Option<u64>,
}

impl StartConfig {
    pub fn to_cli_command(&self) -> String {
        match self.duration {
            Some(d) => format!("start --duration={d}"),
            None => "start".to_string(),
        }
    }
}

/// `POST /api/config/rate` — pin the Wi-Fi PHY rate (only honored in ESP-NOW
/// modes).
#[derive(Debug, Deserialize)]
pub struct RateConfig {
    /// e.g. `1m`, `2m`, `5m5`, `11m`, `6m`..`54m`, `mcs0-lgi`..`mcs7-lgi`,
    /// `mcs0-sgi`.
    pub rate: String,
}

impl RateConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-rate --rate={}", self.rate)
    }
}

/// `POST /api/config/io-tasks` — toggle the per-direction TX/RX Embassy tasks.
/// Both fields are independently optional; omitted fields keep their current
/// device-side value.
#[derive(Debug, Deserialize)]
pub struct IoTasksConfig {
    pub tx: Option<bool>,
    pub rx: Option<bool>,
}

impl IoTasksConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        if self.tx.is_none() && self.rx.is_none() {
            return Err("at least one of tx or rx must be provided".to_string());
        }
        let mut cmd = "set-io-tasks".to_string();
        if let Some(tx) = self.tx {
            cmd.push_str(&format!(" --tx={}", if tx { "on" } else { "off" }));
        }
        if let Some(rx) = self.rx {
            cmd.push_str(&format!(" --rx={}", if rx { "on" } else { "off" }));
        }
        Ok(cmd)
    }
}

/// `POST /api/config/csi-delivery` — switch the CSI delivery path and
/// inline log gate. Both fields are independent; either or both may be set.
#[derive(Debug, Deserialize)]
pub struct CsiDeliveryConfig {
    /// `off` | `callback` | `async`.
    pub mode: Option<String>,
    /// Toggle for the per-packet UART/JTAG inline log path.
    pub logging: Option<bool>,
}

impl CsiDeliveryConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        if self.mode.is_none() && self.logging.is_none() {
            return Err("at least one of mode or logging must be provided".to_string());
        }
        let mut cmd = "set-csi-delivery".to_string();
        if let Some(mode) = &self.mode {
            match mode.as_str() {
                "off" | "callback" | "async" => {}
                other => {
                    return Err(format!(
                        "Unknown csi-delivery mode '{other}'; expected off, callback, or async"
                    ));
                }
            }
            cmd.push_str(&format!(" --mode={mode}"));
        }
        if let Some(logging) = self.logging {
            cmd.push_str(&format!(
                " --logging={}",
                if logging { "on" } else { "off" }
            ));
        }
        Ok(cmd)
    }
}

// ─── Output mode ──────────────────────────────────────────────────────────

/// Controls where CSI frames are sent after being read from the serial port.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// Stream frames to WebSocket clients only (default).
    #[default]
    Stream,
    /// Write frames to a session dump file only; /api/ws returns 403.
    Dump,
    /// Both stream to WebSocket clients and write to the dump file.
    Both,
}

#[derive(Debug, Deserialize)]
pub struct OutputModeConfig {
    pub mode: String,
}

// ─── API response ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: String,
}

// ─── Device identification ─────────────────────────────────────────────────

/// Parsed result of the `info` command on `esp-csi-cli-rs`.
///
/// The magic prefix `ESP-CSI-CLI/<version>` is what proves the firmware is
/// `esp-csi-cli-rs`; if the prefix line never arrives, the device is either
/// running unrelated firmware, an older `esp-csi-cli-rs` build that predates
/// the `info` command, or no firmware at all.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// The version string from the `ESP-CSI-CLI/<version>` magic line.
    pub banner_version: String,
    /// `name=` line, expected to be `esp-csi-cli-rs`.
    pub name: Option<String>,
    /// `version=` line; should match `banner_version`.
    pub version: Option<String>,
    /// `chip=` line: `esp32` | `esp32c3` | `esp32c5` | `esp32c6` | `esp32s3` | `unknown`.
    pub chip: Option<String>,
    /// `protocol=` line — a wire-format version number bumped on
    /// incompatible grammar changes. Host tooling should refuse unknown
    /// protocol values.
    pub protocol: Option<u32>,
    /// `features=` list (compile-time enabled Cargo features).
    pub features: Vec<String>,
}

// ─── Runtime status ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CollectionStatusResponse {
    pub serial_connected: bool,
    pub collection_running: bool,
    pub port_path: String,
}

impl CollectionStatusResponse {
    pub fn from_state(
        serial_connected: &AtomicBool,
        collection_running: &AtomicBool,
        port_path: String,
    ) -> Self {
        Self {
            serial_connected: serial_connected.load(Ordering::SeqCst),
            collection_running: collection_running.load(Ordering::SeqCst),
            port_path,
        }
    }
}
