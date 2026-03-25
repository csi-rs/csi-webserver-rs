use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

// ─── Device config (cached state) ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub wifi_mode: Option<String>,
    pub channel: Option<u32>,
    pub sta_ssid: Option<String>,
    pub traffic_hz: Option<u32>,
    pub collection_mode: Option<String>,
    pub log_mode: Option<String>,
}

// ─── HTTP request bodies ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WifiConfig {
    pub mode: String,
    pub sta_ssid: Option<String>,
    pub sta_password: Option<String>,
    pub channel: Option<u32>,
}

impl WifiConfig {
    pub fn to_cli_command(&self) -> String {
        let mut cmd = format!("set-wifi --mode={}", self.mode);
        if let Some(ssid) = &self.sta_ssid {
            cmd.push_str(&format!(" --sta-ssid={}", ssid.replace(' ', "_")));
        }
        if let Some(pass) = &self.sta_password {
            cmd.push_str(&format!(" --sta-password={}", pass.replace(' ', "_")));
        }
        if let Some(ch) = self.channel {
            cmd.push_str(&format!(" --set-channel={}", ch));
        }
        cmd
    }
}

#[derive(Debug, Deserialize)]
pub struct TrafficConfig {
    pub frequency_hz: u32,
}

impl TrafficConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-traffic --frequency-hz={}", self.frequency_hz)
    }
}

/// CSI feature flags — non-C6 and C6-specific options are all optional.
/// Only flags set to `true` are included in the generated command.
#[derive(Debug, Deserialize)]
pub struct CsiConfig {
    // Non-C6
    pub disable_lltf: Option<bool>,
    pub disable_htltf: Option<bool>,
    pub disable_stbc_htltf: Option<bool>,
    pub disable_ltf_merge: Option<bool>,
    // C6-specific
    pub disable_csi: Option<bool>,
    pub disable_csi_legacy: Option<bool>,
    pub disable_csi_ht20: Option<bool>,
    pub disable_csi_ht40: Option<bool>,
    pub disable_csi_su: Option<bool>,
    pub disable_csi_mu: Option<bool>,
    pub disable_csi_dcm: Option<bool>,
    pub disable_csi_beamformed: Option<bool>,
    pub csi_he_stbc: Option<u8>,
    pub val_scale_cfg: Option<u8>,
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
    /// "collector" or "listener"
    pub mode: String,
}

impl CollectionModeConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-collection-mode --mode={}", self.mode)
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
}

impl LogMode {
    pub fn as_cli_value(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::ArrayList => "array-list",
            Self::Serialized => "serialized",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StartConfig {
    /// Collection duration in seconds; omit for indefinite collection.
    pub duration: Option<u32>,
}

impl StartConfig {
    pub fn to_cli_command(&self) -> String {
        match self.duration {
            Some(d) => format!("start --duration={d}"),
            None => "start".to_string(),
        }
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
