//! Host-side decoder for the firmware's `serialized` CSI wire format.
//!
//! In `serialized` mode `esp-csi-rs` emits each CSI sample as
//! `postcard::to_slice_cobs(&CSIDataPacket)` — a COBS-framed, postcard-encoded
//! record terminated by a `\0` byte. This module mirrors that on-device struct
//! so the host can decode frames into typed fields (for Parquet) without
//! reverse-engineering the byte layout.
//!
//! ## Why the struct is mirrored, not imported
//! `esp-csi-rs` is an `esp-hal` crate and cannot compile for a Linux host, so
//! the wire types are re-declared here. postcard is **not** self-describing and
//! uses varint encoding, so these mirrors must match the firmware field-for-
//! field, in order. **Pinned to `esp-csi-rs` 0.7.0.** When the firmware bumps
//! its protocol/struct, update these definitions in lockstep.
//!
//! ## Chip layouts
//! The on-device `CSIDataPacket` has two shapes selected by `#[cfg]`:
//! - esp32 / esp32c3 / esp32s3 — the full radio-metadata variant ([`PacketA`]).
//! - esp32c5 / esp32c6 — a different field set ([`PacketBc5`] / [`PacketBc6`]),
//!   where c6 additionally carries `he_sigb_len`, `cur_single_mpdu`, `rxmatch0`.
//!
//! The host learns the chip from the firmware `info` exchange and selects the
//! matching layout at decode time.

use serde::{Deserialize, Serialize};

/// Which on-device `CSIDataPacket` layout a connected chip produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    /// esp32, esp32c3, esp32s3 — the full radio-metadata layout ([`PacketA`]).
    Esp32Family,
    /// esp32c5 — the reduced layout without the c6-only fields ([`PacketBc5`]).
    Esp32c5,
    /// esp32c6 — the reduced layout plus `he_sigb_len`/`cur_single_mpdu`/`rxmatch0`.
    Esp32c6,
}

impl ChipVariant {
    /// Map a firmware `chip=` string (case-insensitive) to its wire layout.
    ///
    /// Returns `None` for unrecognized chips so the caller can refuse to decode
    /// rather than guess a layout.
    pub fn from_chip_str(chip: &str) -> Option<Self> {
        match chip.trim().to_ascii_lowercase().as_str() {
            "esp32" | "esp32c3" | "esp32s3" | "esp32s2" => Some(Self::Esp32Family),
            "esp32c5" => Some(Self::Esp32c5),
            "esp32c6" => Some(Self::Esp32c6),
            _ => None,
        }
    }
}

/// Optional NTP-derived calendar timestamp the firmware may attach to a packet.
///
/// Mirror of `esp_csi_rs::time::DateTime` (all fields `u64`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateTime {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hour: u64,
    pub minute: u64,
    pub second: u64,
    pub millisecond: u64,
}

/// Compact CSI data-format descriptor.
///
/// Mirror of `esp_csi_rs::csi::RxCSIFmt` — **variant order is the wire encoding**
/// (postcard encodes the discriminant as a varint of the declaration index), so
/// do not reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RxCsiFmt {
    Bw20,
    HtBw20,
    HtBw20Stbc,
    SecbBw20,
    SecbHtBw20,
    SecbHtBw20Stbc,
    SecbHtBw40,
    SecbHtBw40Stbc,
    SecaBw20,
    SecaHtBw20,
    SecaHtBw20Stbc,
    SecaHtBw40,
    SecaHtBw40Stbc,
    Undefined,
}

impl RxCsiFmt {
    /// Stable lowercase-ish identifier for the Parquet `data_format` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bw20 => "Bw20",
            Self::HtBw20 => "HtBw20",
            Self::HtBw20Stbc => "HtBw20Stbc",
            Self::SecbBw20 => "SecbBw20",
            Self::SecbHtBw20 => "SecbHtBw20",
            Self::SecbHtBw20Stbc => "SecbHtBw20Stbc",
            Self::SecbHtBw40 => "SecbHtBw40",
            Self::SecbHtBw40Stbc => "SecbHtBw40Stbc",
            Self::SecaBw20 => "SecaBw20",
            Self::SecaHtBw20 => "SecaHtBw20",
            Self::SecaHtBw20Stbc => "SecaHtBw20Stbc",
            Self::SecaHtBw40 => "SecaHtBw40",
            Self::SecaHtBw40Stbc => "SecaHtBw40Stbc",
            Self::Undefined => "Undefined",
        }
    }
}

/// esp32 / esp32c3 / esp32s3 layout — mirror of `CSIDataPacket`
/// (`#[cfg(not(any(esp32c5, esp32c6)))]`). Field order is the wire order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketA {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub sgi: u32,
    pub secondary_channel: u32,
    pub channel: u32,
    pub bandwidth: u32,
    pub antenna: u32,
    pub sig_mode: u32,
    pub mcs: u32,
    pub smoothing: u32,
    pub not_sounding: u32,
    pub aggregation: u32,
    pub stbc: u32,
    pub fec_coding: u32,
    pub ampdu_cnt: u32,
    pub noise_floor: i32,
    pub rx_state: u32,
    pub sig_len: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub data_format: RxCsiFmt,
    pub csi_data_len: u16,
    pub csi_data: Vec<i8>,
}

/// esp32c5 layout — mirror of the `#[cfg(any(esp32c5, esp32c6))]` `CSIDataPacket`
/// **without** the `#[cfg(feature = "esp32c6")]` fields. Field order is the wire
/// order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketBc5 {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub dump_len: u32,
    pub cur_bb_format: u32,
    pub rx_channel_estimate_info_vld: u32,
    pub rx_channel_estimate_len: u32,
    pub second: u32,
    pub channel: u32,
    pub is_group: u32,
    pub rxend_state: u32,
    pub rxmatch3: u32,
    pub rxmatch2: u32,
    pub rxmatch1: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub csi_data_len: u16,
    pub data_format: RxCsiFmt,
    pub csi_data: Vec<i8>,
}

/// esp32c6 layout — the c5 layout plus the three `#[cfg(feature = "esp32c6")]`
/// fields (`he_sigb_len`, `cur_single_mpdu`, `rxmatch0`) at their declared
/// positions. Field order is the wire order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketBc6 {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub dump_len: u32,
    pub he_sigb_len: u32,
    pub cur_single_mpdu: u32,
    pub cur_bb_format: u32,
    pub rx_channel_estimate_info_vld: u32,
    pub rx_channel_estimate_len: u32,
    pub second: u32,
    pub channel: u32,
    pub is_group: u32,
    pub rxend_state: u32,
    pub rxmatch3: u32,
    pub rxmatch2: u32,
    pub rxmatch1: u32,
    pub rxmatch0: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub csi_data_len: u16,
    pub data_format: RxCsiFmt,
    pub csi_data: Vec<i8>,
}

/// Chip-agnostic decoded CSI record — the superset of every layout's fields.
///
/// Fields absent on the source chip are `None`. This is what the Parquet sink
/// consumes; its column set is the union of all chip layouts plus the
/// host-supplied receive time.
#[derive(Debug, Clone)]
pub struct DecodedCsi {
    // ── Common to every layout ──────────────────────────────────────────
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub channel: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub data_format: RxCsiFmt,
    pub csi_data_len: u16,
    pub csi_data: Vec<i8>,

    // ── esp32-family only ───────────────────────────────────────────────
    pub sgi: Option<u32>,
    pub secondary_channel: Option<u32>,
    pub bandwidth: Option<u32>,
    pub antenna: Option<u32>,
    pub sig_mode: Option<u32>,
    pub mcs: Option<u32>,
    pub smoothing: Option<u32>,
    pub not_sounding: Option<u32>,
    pub aggregation: Option<u32>,
    pub stbc: Option<u32>,
    pub fec_coding: Option<u32>,
    pub ampdu_cnt: Option<u32>,

    // ── c5 / c6 only ────────────────────────────────────────────────────
    pub dump_len: Option<u32>,
    pub cur_bb_format: Option<u32>,
    pub rx_channel_estimate_info_vld: Option<u32>,
    pub rx_channel_estimate_len: Option<u32>,
    pub second: Option<u32>,
    pub is_group: Option<u32>,
    pub rxend_state: Option<u32>,
    pub rxmatch3: Option<u32>,
    pub rxmatch2: Option<u32>,
    pub rxmatch1: Option<u32>,

    // ── c6 only ─────────────────────────────────────────────────────────
    pub he_sigb_len: Option<u32>,
    pub cur_single_mpdu: Option<u32>,
    pub rxmatch0: Option<u32>,
}

impl From<PacketA> for DecodedCsi {
    fn from(p: PacketA) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: Some(p.sgi),
            secondary_channel: Some(p.secondary_channel),
            bandwidth: Some(p.bandwidth),
            antenna: Some(p.antenna),
            sig_mode: Some(p.sig_mode),
            mcs: Some(p.mcs),
            smoothing: Some(p.smoothing),
            not_sounding: Some(p.not_sounding),
            aggregation: Some(p.aggregation),
            stbc: Some(p.stbc),
            fec_coding: Some(p.fec_coding),
            ampdu_cnt: Some(p.ampdu_cnt),
            dump_len: None,
            cur_bb_format: None,
            rx_channel_estimate_info_vld: None,
            rx_channel_estimate_len: None,
            second: None,
            is_group: None,
            rxend_state: None,
            rxmatch3: None,
            rxmatch2: None,
            rxmatch1: None,
            he_sigb_len: None,
            cur_single_mpdu: None,
            rxmatch0: None,
        }
    }
}

impl From<PacketBc5> for DecodedCsi {
    fn from(p: PacketBc5) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: None,
            secondary_channel: None,
            bandwidth: None,
            antenna: None,
            sig_mode: None,
            mcs: None,
            smoothing: None,
            not_sounding: None,
            aggregation: None,
            stbc: None,
            fec_coding: None,
            ampdu_cnt: None,
            dump_len: Some(p.dump_len),
            cur_bb_format: Some(p.cur_bb_format),
            rx_channel_estimate_info_vld: Some(p.rx_channel_estimate_info_vld),
            rx_channel_estimate_len: Some(p.rx_channel_estimate_len),
            second: Some(p.second),
            is_group: Some(p.is_group),
            rxend_state: Some(p.rxend_state),
            rxmatch3: Some(p.rxmatch3),
            rxmatch2: Some(p.rxmatch2),
            rxmatch1: Some(p.rxmatch1),
            he_sigb_len: None,
            cur_single_mpdu: None,
            rxmatch0: None,
        }
    }
}

impl From<PacketBc6> for DecodedCsi {
    fn from(p: PacketBc6) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: None,
            secondary_channel: None,
            bandwidth: None,
            antenna: None,
            sig_mode: None,
            mcs: None,
            smoothing: None,
            not_sounding: None,
            aggregation: None,
            stbc: None,
            fec_coding: None,
            ampdu_cnt: None,
            dump_len: Some(p.dump_len),
            cur_bb_format: Some(p.cur_bb_format),
            rx_channel_estimate_info_vld: Some(p.rx_channel_estimate_info_vld),
            rx_channel_estimate_len: Some(p.rx_channel_estimate_len),
            second: Some(p.second),
            is_group: Some(p.is_group),
            rxend_state: Some(p.rxend_state),
            rxmatch3: Some(p.rxmatch3),
            rxmatch2: Some(p.rxmatch2),
            rxmatch1: Some(p.rxmatch1),
            he_sigb_len: Some(p.he_sigb_len),
            cur_single_mpdu: Some(p.cur_single_mpdu),
            rxmatch0: Some(p.rxmatch0),
        }
    }
}

/// Failure decoding a serialized CSI frame.
#[derive(Debug)]
pub struct DecodeError(postcard::Error);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to decode CSI frame: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        DecodeError(e)
    }
}

/// Decode one COBS-framed postcard CSI frame into a [`DecodedCsi`].
///
/// `frame` is the buffer the serial task accumulates up to (not including) the
/// `\0` COBS terminator. `take_from_bytes_cobs` decodes in place and tolerates
/// trailing pad bytes after the encoded struct, mirroring the firmware's own
/// length-tolerant decode path. The input is copied because COBS decoding
/// mutates the buffer.
pub fn decode(frame: &[u8], chip: ChipVariant) -> Result<DecodedCsi, DecodeError> {
    let mut owned = frame.to_vec();
    let decoded = match chip {
        ChipVariant::Esp32Family => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketA>(&mut owned)?;
            p.into()
        }
        ChipVariant::Esp32c5 => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketBc5>(&mut owned)?;
            p.into()
        }
        ChipVariant::Esp32c6 => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketBc6>(&mut owned)?;
            p.into()
        }
    };
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a `PacketA` through postcard+COBS exactly as the firmware
    /// emits it (`to_slice_cobs`), then decode it back. Guards against wire
    /// drift in field order/types for the esp32-family layout.
    #[test]
    fn decode_packet_a_roundtrip() {
        let pkt = PacketA {
            mac: [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01],
            rssi: -42,
            timestamp: 123_456,
            rate: 11,
            sgi: 1,
            secondary_channel: 0,
            channel: 6,
            bandwidth: 0,
            antenna: 0,
            sig_mode: 1,
            mcs: 7,
            smoothing: 0,
            not_sounding: 1,
            aggregation: 0,
            stbc: 0,
            fec_coding: 0,
            ampdu_cnt: 0,
            noise_floor: -96,
            rx_state: 0,
            sig_len: 128,
            date_time: Some(DateTime {
                year: 2026,
                month: 6,
                day: 22,
                hour: 12,
                minute: 30,
                second: 15,
                millisecond: 250,
            }),
            sequence_number: 4242,
            data_format: RxCsiFmt::HtBw20,
            csi_data_len: 4,
            csi_data: vec![1, -2, 3, -4],
        };

        // Mirror the firmware: postcard-serialize then COBS-frame.
        let mut buf = vec![0u8; 1024];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        // The serial reader strips the trailing `\0` terminator; mimic that.
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);

        let out = decode(body, ChipVariant::Esp32Family).unwrap();
        assert_eq!(out.mac, pkt.mac);
        assert_eq!(out.rssi, -42);
        assert_eq!(out.channel, 6);
        assert_eq!(out.mcs, Some(7));
        assert_eq!(out.noise_floor, -96);
        assert_eq!(out.sequence_number, 4242);
        assert_eq!(out.data_format, RxCsiFmt::HtBw20);
        assert_eq!(out.csi_data, vec![1, -2, 3, -4]);
        assert_eq!(out.dump_len, None);
        let dt = out.date_time.expect("date_time present");
        assert_eq!((dt.year, dt.month, dt.day), (2026, 6, 22));
    }

    #[test]
    fn decode_packet_bc6_roundtrip() {
        let pkt = PacketBc6 {
            mac: [1, 2, 3, 4, 5, 6],
            rssi: -55,
            timestamp: 9,
            rate: 1,
            noise_floor: -90,
            sig_len: 64,
            rx_state: 0,
            dump_len: 100,
            he_sigb_len: 7,
            cur_single_mpdu: 1,
            cur_bb_format: 2,
            rx_channel_estimate_info_vld: 1,
            rx_channel_estimate_len: 64,
            second: 3,
            channel: 11,
            is_group: 0,
            rxend_state: 0,
            rxmatch3: 0,
            rxmatch2: 0,
            rxmatch1: 1,
            rxmatch0: 1,
            date_time: None,
            sequence_number: 7,
            csi_data_len: 2,
            data_format: RxCsiFmt::Undefined,
            csi_data: vec![-1, 1],
        };
        let mut buf = vec![0u8; 1024];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);

        let out = decode(body, ChipVariant::Esp32c6).unwrap();
        assert_eq!(out.mac, [1, 2, 3, 4, 5, 6]);
        assert_eq!(out.he_sigb_len, Some(7));
        assert_eq!(out.rxmatch0, Some(1));
        assert_eq!(out.sgi, None);
        assert_eq!(out.csi_data, vec![-1, 1]);
        assert!(out.date_time.is_none());
    }

    /// A clean esp32c5 frame round-trips through `to_slice_cobs` (as the
    /// firmware emits it) and back through `decode`.
    #[test]
    fn decode_packet_bc5_roundtrip() {
        let pkt = PacketBc5 {
            mac: [0xde, 0xad, 0xbe, 0xef, 0x00, 0x05],
            rssi: -61, timestamp: 123456, rate: 1, noise_floor: -92, sig_len: 80,
            rx_state: 0, dump_len: 384, cur_bb_format: 2, rx_channel_estimate_info_vld: 1,
            rx_channel_estimate_len: 384, second: 7, channel: 149, is_group: 0,
            rxend_state: 0, rxmatch3: 0, rxmatch2: 0, rxmatch1: 1, date_time: None,
            sequence_number: 99, csi_data_len: 4, data_format: RxCsiFmt::Undefined,
            csi_data: vec![1, -2, 3, -4],
        };
        let mut buf = vec![0u8; 2048];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);
        let out = decode(body, ChipVariant::Esp32c5).unwrap();
        assert_eq!(out.channel, 149);
        assert_eq!(out.mac, [0xde, 0xad, 0xbe, 0xef, 0x00, 0x05]);
        assert_eq!(out.csi_data, vec![1, -2, 3, -4]);
        assert_eq!(out.dump_len, Some(384));
        assert_eq!(out.sgi, None);
    }

    #[test]
    fn chip_string_mapping() {
        assert_eq!(ChipVariant::from_chip_str("ESP32"), Some(ChipVariant::Esp32Family));
        assert_eq!(ChipVariant::from_chip_str("esp32c6"), Some(ChipVariant::Esp32c6));
        assert_eq!(ChipVariant::from_chip_str("esp32c5"), Some(ChipVariant::Esp32c5));
        assert_eq!(ChipVariant::from_chip_str("weird"), None);
    }
}
