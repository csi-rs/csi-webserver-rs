//! Data models used by HTTP handlers and runtime control flow.
//!
//! This module contains:
//! - request-body structs for config/control endpoints,
//! - runtime enums used by watch channels,
//! - common API response payloads.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

// ─── Device config (cached state) ─────────────────────────────────────────

/// Server-side cached view of device-side `UserConfig`, structured to mirror
/// the firmware's `show-config` output (sections `[WiFi]`, `[Collection]`,
/// `[CSI Config]`). Fields are best-effort: each is populated when the
/// matching `POST /api/config/*` endpoint succeeds, and reset to firmware
/// defaults by `POST /api/config/reset`. Values can drift if the device is
/// re-flashed or commands are sent out-of-band.
///
/// `sta_password` is intentionally *not* cached even though `show-config`
/// echoes it — round-tripping plaintext passwords through a GET endpoint
/// would defeat the point of having one.
///
/// The trailing fields (`log_mode`, `csi_delivery_mode`,
/// `csi_logging_enabled`) live alongside the show-config sections because
/// they're set via separate CLI commands (`set-log-mode`, `set-csi-delivery`)
/// and are useful to surface here even though they aren't part of the
/// `show-config` block.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub wifi: WifiSection,
    pub collection: CollectionSection,
    pub csi_config: CsiConfigSection,
    pub log_mode: Option<String>,
    pub csi_delivery_mode: Option<String>,
    pub csi_logging_enabled: Option<bool>,
}

/// `[WiFi]` section in `show-config`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WifiSection {
    /// `node_mode` — `sniffer` | `station` | `esp-now-central` | `esp-now-peripheral`.
    pub mode: Option<String>,
    /// `channel` — `u8`. Valid Wi-Fi 2.4 GHz: 1..=14.
    pub channel: Option<u8>,
    /// `sta_ssid` — UTF-8, ≤ 32 B.
    pub sta_ssid: Option<String>,
    /// `peer_mac` — explicit ESP-NOW source-MAC filter, or `auto` for the
    /// default magic-prefix pairing. ESP-NOW modes only.
    pub peer_mac: Option<String>,
    /// `ht40_secondary` — forced ESP-NOW TX secondary channel:
    /// `above` | `below` | `none` (HT20/legacy). ESP-NOW modes only.
    pub ht40: Option<String>,
}

/// `[Collection]` section in `show-config`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionSection {
    /// `collection_mode` — `collector` | `listener`.
    pub mode: Option<String>,
    /// `trigger_freq` Hz. `0` disables traffic generation.
    pub traffic_hz: Option<u64>,
    /// `phy_rate` enum — e.g. `mcs0-lgi`. Only honored by ESP-NOW modes.
    pub phy_rate: Option<String>,
    /// `io_tasks.tx_enabled`.
    pub io_tx_enabled: Option<bool>,
    /// `io_tasks.rx_enabled`.
    pub io_rx_enabled: Option<bool>,
}

/// `[CSI Config]` section in `show-config`. Both classic (ESP32 / C3 / S3)
/// and HE (ESP32-C5 / C6) fields are merged into a single struct so the
/// JSON shape is stable across chip variants. The fields applicable to
/// the active firmware are populated; the others remain `None`.
///
/// The classic block also includes four read-only fields
/// (`channel_filter_enabled`, `manual_scale`, `shift`, `dump_ack_enabled`)
/// that have no `set-csi` flag; they are populated by
/// `POST /api/config/reset` from firmware defaults but otherwise stay
/// fixed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CsiConfigSection {
    // ── Classic (ESP32 / C3 / S3) ─────────────────────────────────────
    /// `lltf_en`.
    pub lltf_enabled: Option<bool>,
    /// `htltf_en`.
    pub htltf_enabled: Option<bool>,
    /// `stbc_htltf2_en`.
    pub stbc_htltf_enabled: Option<bool>,
    /// `ltf_merge_en`.
    pub ltf_merge_enabled: Option<bool>,
    /// `channel_filter_en` — **read-only**; only restored by `reset-config`.
    pub channel_filter_enabled: Option<bool>,
    /// `manu_scale` — **read-only**; only restored by `reset-config`.
    pub manual_scale: Option<bool>,
    /// `shift` — **read-only**; only restored by `reset-config`.
    pub shift: Option<u8>,
    /// `dump_ack_en` — **read-only**; only restored by `reset-config`.
    pub dump_ack_enabled: Option<bool>,

    // ── HE (ESP32-C5 / C6) ────────────────────────────────────────────
    /// `enable` (acquire CSI overall).
    pub acquire_csi: Option<u32>,
    /// `acquire_csi_legacy` — L-LTF / 11g.
    pub acquire_csi_legacy: Option<u32>,
    /// `acquire_csi_ht20`.
    pub acquire_csi_ht20: Option<u32>,
    /// `acquire_csi_ht40`.
    pub acquire_csi_ht40: Option<u32>,
    /// `acquire_csi_su` — HE20 single-user.
    pub acquire_csi_su: Option<u32>,
    /// `acquire_csi_mu` — HE20 multi-user.
    pub acquire_csi_mu: Option<u32>,
    /// `acquire_csi_dcm` — HE20 dual carrier modulation.
    pub acquire_csi_dcm: Option<u32>,
    /// `acquire_csi_beamformed`.
    pub acquire_csi_beamformed: Option<u32>,
    /// `acquire_csi_he_stbc` — `0` HE-LTF1, `1` HE-LTF2, `2` even sample.
    pub csi_he_stbc: Option<u32>,
    /// `val_scale_cfg`.
    pub val_scale_cfg: Option<u32>,
}

impl DeviceConfig {
    /// Snapshot of `UserConfig::new()` / `CsiConfig::default()` on the
    /// device, as documented in the `show-config` spec. Populated into
    /// the cache by `POST /api/config/reset` so the response after a
    /// reset reflects what the firmware actually holds, even before the
    /// user re-sends any `set-*` commands.
    ///
    /// Both the classic and the HE CSI defaults are populated — the
    /// caller can ignore the fields irrelevant to the connected chip
    /// (consult `GET /api/info` for the `chip` field).
    pub fn firmware_defaults() -> Self {
        Self {
            wifi: WifiSection {
                mode: Some("sniffer".to_string()),
                channel: Some(1),
                sta_ssid: Some(String::new()),
                peer_mac: Some("auto".to_string()),
                ht40: Some("none".to_string()),
            },
            collection: CollectionSection {
                mode: Some("collector".to_string()),
                traffic_hz: Some(100),
                phy_rate: Some("mcs0-lgi".to_string()),
                io_tx_enabled: Some(true),
                io_rx_enabled: Some(true),
            },
            csi_config: CsiConfigSection {
                // Classic
                lltf_enabled: Some(true),
                htltf_enabled: Some(true),
                stbc_htltf_enabled: Some(true),
                ltf_merge_enabled: Some(true),
                channel_filter_enabled: Some(false),
                manual_scale: Some(false),
                shift: Some(0),
                dump_ack_enabled: Some(false),
                // HE
                acquire_csi: Some(1),
                acquire_csi_legacy: Some(1),
                acquire_csi_ht20: Some(1),
                acquire_csi_ht40: Some(1),
                acquire_csi_su: Some(1),
                acquire_csi_mu: Some(1),
                acquire_csi_dcm: Some(1),
                acquire_csi_beamformed: Some(1),
                csi_he_stbc: Some(2),
                val_scale_cfg: Some(2),
            },
            log_mode: None,
            csi_delivery_mode: None,
            csi_logging_enabled: None,
        }
    }
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

/// Validate an ESP-NOW peer MAC the way the firmware does: six hex octets
/// separated by `:` or `-` (case-insensitive). An empty string is *valid* and
/// means "clear back to auto".
fn validate_peer_mac(mac: &str) -> Result<(), String> {
    if mac.is_empty() {
        return Ok(());
    }
    let sep = if mac.contains(':') {
        ':'
    } else if mac.contains('-') {
        '-'
    } else {
        return Err("peer_mac must use ':' or '-' separators (aa:bb:cc:dd:ee:ff)".to_string());
    };
    let octets: Vec<&str> = mac.split(sep).collect();
    if octets.len() != 6 || octets.iter().any(|o| o.len() != 2 || !o.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(format!("Invalid peer_mac '{mac}' (use aa:bb:cc:dd:ee:ff)"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct WifiConfig {
    /// `station` | `sniffer` | `esp-now-central` | `esp-now-peripheral`.
    pub mode: String,
    pub sta_ssid: Option<String>,
    pub sta_password: Option<String>,
    pub channel: Option<u8>,
    /// ESP-NOW peer source MAC (`aa:bb:cc:dd:ee:ff` or `aa-bb-...`). An empty
    /// string clears the filter back to automatic magic-prefix pairing.
    /// ESP-NOW modes only; ignored by the firmware in other modes.
    pub peer_mac: Option<String>,
    /// Forced ESP-NOW TX HT40 secondary channel: `above` | `below` |
    /// `none` | `off`. ESP-NOW modes only.
    pub ht40: Option<String>,
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

        if let Some(mac) = &self.peer_mac {
            validate_peer_mac(mac)?;
            // An empty value is forwarded verbatim (`--peer-mac=`) so the
            // firmware clears the filter back to auto.
            cmd.push_str(&format!(" --peer-mac={mac}"));
        }

        if let Some(ht40) = &self.ht40 {
            match ht40.as_str() {
                "above" | "below" | "none" | "off" => {}
                other => {
                    return Err(format!("Invalid ht40 '{other}' (use above, below, none, or off)"));
                }
            }
            cmd.push_str(&format!(" --ht40={ht40}"));
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
    /// `off` | `callback` | `async` | `raw`.
    ///
    /// `raw` enables the zero-copy fast-path; unlike the other modes it is
    /// stored as a flag on the device and only takes effect on the next
    /// `start` (no CSI data is delivered or logged in that mode).
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
                "off" | "callback" | "async" | "raw" => {}
                other => {
                    return Err(format!(
                        "Unknown csi-delivery mode '{other}'; expected off, callback, async, or raw"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(mode: &str, peer_mac: Option<&str>, ht40: Option<&str>) -> WifiConfig {
        WifiConfig {
            mode: mode.to_string(),
            sta_ssid: None,
            sta_password: None,
            channel: None,
            peer_mac: peer_mac.map(str::to_string),
            ht40: ht40.map(str::to_string),
        }
    }

    #[test]
    fn wifi_emits_peer_mac_and_ht40() {
        let cmd = wifi("esp-now-central", Some("AA:BB:CC:DD:EE:FF"), Some("above"))
            .to_cli_command()
            .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=esp-now-central --peer-mac=AA:BB:CC:DD:EE:FF --ht40=above"
        );
    }

    #[test]
    fn wifi_empty_peer_mac_clears_to_auto() {
        let cmd = wifi("esp-now-peripheral", Some(""), None)
            .to_cli_command()
            .unwrap();
        assert_eq!(cmd, "set-wifi --mode=esp-now-peripheral --peer-mac=");
    }

    #[test]
    fn wifi_rejects_malformed_peer_mac() {
        assert!(wifi("esp-now-central", Some("not-a-mac"), None)
            .to_cli_command()
            .is_err());
    }

    #[test]
    fn wifi_rejects_bad_ht40() {
        assert!(wifi("esp-now-central", None, Some("sideways"))
            .to_cli_command()
            .is_err());
    }

    #[test]
    fn csi_delivery_accepts_raw() {
        let cmd = CsiDeliveryConfig {
            mode: Some("raw".to_string()),
            logging: None,
        }
        .to_cli_command()
        .unwrap();
        assert_eq!(cmd, "set-csi-delivery --mode=raw");
    }

    #[test]
    fn csi_delivery_rejects_unknown_mode() {
        assert!(CsiDeliveryConfig {
            mode: Some("bogus".to_string()),
            logging: None,
        }
        .to_cli_command()
        .is_err());
    }
}
