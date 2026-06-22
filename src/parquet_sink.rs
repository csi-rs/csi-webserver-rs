//! Parquet writer for decoded CSI sessions.
//!
//! Each collection session writes one Parquet file. Rows are buffered and
//! flushed as row groups; the file footer is written on [`ParquetSink::close`].
//!
//! ## Schema
//! A single **superset** schema covers all chip layouts so consumers see one
//! stable column set regardless of the source chip. Columns that only exist on
//! some chips are nullable and left null otherwise. `csi_data` is a
//! variable-length `List<Int8>`. `host_rx_time` is the server's wall-clock
//! receive time (UTC, microseconds) — distinct from the device `timestamp`
//! field, which is microseconds since the device's controller start.
//!
//! ## Durability
//! Parquet is only readable once its footer is written by [`ParquetSink::close`].
//! A clean session stop closes the file. An abrupt device unplug or crash leaves
//! the in-progress file without a footer (and any unflushed rows lost) — that
//! file will not open. This is an accepted limitation.

use std::fs::File;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Int32Array, Int8Builder, ListBuilder, StringArray, TimestampMicrosecondArray,
    UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::csi::DecodedCsi;

/// Number of buffered rows that triggers a row-group flush.
const ROW_GROUP_SIZE: usize = 256;

/// A buffered row: the host receive time (UTC microseconds) and the packet.
struct Row {
    host_rx_micros: i64,
    csi: DecodedCsi,
}

/// Writes decoded CSI packets to a Parquet file for one session.
///
/// The file footer is written by [`ParquetSink::close`] *or* automatically on
/// drop (so a dropped sink — session end, device disconnect, shutdown — still
/// produces a readable file). Only a hard crash/panic skips finalization.
pub struct ParquetSink {
    /// `None` once finalized; `Some` while open.
    writer: Option<ArrowWriter<File>>,
    schema: Arc<Schema>,
    chip: String,
    path: String,
    buffer: Vec<Row>,
}

impl ParquetSink {
    /// Open a new Parquet file at `path` for a session on the given `chip`.
    pub fn open(path: &str, chip: &str) -> Result<Self, ParquetSinkError> {
        let schema = build_schema();
        let file = File::create(path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;
        Ok(Self {
            writer: Some(writer),
            schema,
            chip: chip.to_string(),
            path: path.to_string(),
            buffer: Vec::with_capacity(ROW_GROUP_SIZE),
        })
    }

    /// Append one decoded packet stamped with the host receive time
    /// (UTC microseconds). Flushes a row group once the buffer is full.
    pub fn push(&mut self, csi: DecodedCsi, host_rx_micros: i64) -> Result<(), ParquetSinkError> {
        self.buffer.push(Row { host_rx_micros, csi });
        if self.buffer.len() >= ROW_GROUP_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    /// Write any buffered rows as a row group.
    pub fn flush(&mut self) -> Result<(), ParquetSinkError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let batch = self.build_batch()?;
        if let Some(writer) = self.writer.as_mut() {
            writer.write(&batch)?;
        }
        self.buffer.clear();
        Ok(())
    }

    /// Flush remaining rows and write the Parquet footer. Idempotent.
    ///
    /// Called automatically on drop; the file is unreadable until this runs.
    fn finish(&mut self) -> Result<(), ParquetSinkError> {
        if self.writer.is_none() {
            return Ok(());
        }
        self.flush()?;
        if let Some(writer) = self.writer.take() {
            writer.close()?;
        }
        Ok(())
    }

    fn build_batch(&self) -> Result<RecordBatch, ParquetSinkError> {
        let rows = &self.buffer;

        // Helper closures to project the buffered rows into Arrow arrays.
        let u32_req = |f: &dyn Fn(&DecodedCsi) -> u32| -> ArrayRef {
            Arc::new(UInt32Array::from(
                rows.iter().map(|r| f(&r.csi)).collect::<Vec<_>>(),
            ))
        };
        let u32_opt = |f: &dyn Fn(&DecodedCsi) -> Option<u32>| -> ArrayRef {
            Arc::new(UInt32Array::from(
                rows.iter().map(|r| f(&r.csi)).collect::<Vec<_>>(),
            ))
        };
        let i32_req = |f: &dyn Fn(&DecodedCsi) -> i32| -> ArrayRef {
            Arc::new(Int32Array::from(
                rows.iter().map(|r| f(&r.csi)).collect::<Vec<_>>(),
            ))
        };
        let dt_opt = |f: &dyn Fn(&super::csi::DateTime) -> u64| -> ArrayRef {
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|r| r.csi.date_time.as_ref().map(f))
                    .collect::<Vec<_>>(),
            ))
        };

        // host_rx_time (UTC microseconds).
        let host_rx: ArrayRef = Arc::new(
            TimestampMicrosecondArray::from(
                rows.iter().map(|r| r.host_rx_micros).collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        );

        // chip (constant per session).
        let chip: ArrayRef = Arc::new(StringArray::from(
            rows.iter().map(|_| self.chip.as_str()).collect::<Vec<_>>(),
        ));

        // mac as colon-separated hex.
        let mac: ArrayRef = Arc::new(StringArray::from(
            rows.iter().map(|r| format_mac(&r.csi.mac)).collect::<Vec<_>>(),
        ));

        let sequence_number: ArrayRef = Arc::new(UInt16Array::from(
            rows.iter().map(|r| r.csi.sequence_number).collect::<Vec<_>>(),
        ));
        let csi_data_len: ArrayRef = Arc::new(UInt16Array::from(
            rows.iter().map(|r| r.csi.csi_data_len).collect::<Vec<_>>(),
        ));
        let data_format: ArrayRef = Arc::new(StringArray::from(
            rows.iter().map(|r| r.csi.data_format.as_str()).collect::<Vec<_>>(),
        ));

        // csi_data: List<Int8>.
        let mut list_builder = ListBuilder::new(Int8Builder::new());
        for r in rows {
            list_builder.values().append_slice(&r.csi.csi_data);
            list_builder.append(true);
        }
        let csi_data: ArrayRef = Arc::new(list_builder.finish());

        // Column order MUST match `build_schema()`.
        let columns: Vec<ArrayRef> = vec![
            host_rx,
            chip,
            mac,
            i32_req(&|c| c.rssi),
            u32_req(&|c| c.timestamp),
            u32_req(&|c| c.rate),
            i32_req(&|c| c.noise_floor),
            u32_req(&|c| c.sig_len),
            u32_req(&|c| c.rx_state),
            u32_req(&|c| c.channel),
            sequence_number,
            data_format,
            csi_data_len,
            csi_data,
            // date_time flattened
            dt_opt(&|d| d.year),
            dt_opt(&|d| d.month),
            dt_opt(&|d| d.day),
            dt_opt(&|d| d.hour),
            dt_opt(&|d| d.minute),
            dt_opt(&|d| d.second),
            dt_opt(&|d| d.millisecond),
            // esp32-family only
            u32_opt(&|c| c.sgi),
            u32_opt(&|c| c.secondary_channel),
            u32_opt(&|c| c.bandwidth),
            u32_opt(&|c| c.antenna),
            u32_opt(&|c| c.sig_mode),
            u32_opt(&|c| c.mcs),
            u32_opt(&|c| c.smoothing),
            u32_opt(&|c| c.not_sounding),
            u32_opt(&|c| c.aggregation),
            u32_opt(&|c| c.stbc),
            u32_opt(&|c| c.fec_coding),
            u32_opt(&|c| c.ampdu_cnt),
            // c5 / c6 only
            u32_opt(&|c| c.dump_len),
            u32_opt(&|c| c.cur_bb_format),
            u32_opt(&|c| c.rx_channel_estimate_info_vld),
            u32_opt(&|c| c.rx_channel_estimate_len),
            u32_opt(&|c| c.second),
            u32_opt(&|c| c.is_group),
            u32_opt(&|c| c.rxend_state),
            u32_opt(&|c| c.rxmatch3),
            u32_opt(&|c| c.rxmatch2),
            u32_opt(&|c| c.rxmatch1),
            // c6 only
            u32_opt(&|c| c.he_sigb_len),
            u32_opt(&|c| c.cur_single_mpdu),
            u32_opt(&|c| c.rxmatch0),
        ];

        Ok(RecordBatch::try_new(self.schema.clone(), columns)?)
    }
}

impl Drop for ParquetSink {
    fn drop(&mut self) {
        // Finalize on drop so a sink abandoned via session-end / disconnect /
        // shutdown still yields a readable file. Errors can only be logged here.
        if self.writer.is_some() {
            if let Err(e) = self.finish() {
                tracing::error!("Failed to finalize Parquet file {}: {e}", self.path);
            }
        }
    }
}

/// Format a 6-byte MAC as `aa:bb:cc:dd:ee:ff`.
fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Build the superset Arrow schema. Column order must match `build_batch`.
fn build_schema() -> Arc<Schema> {
    let req_u32 = |name: &str| Field::new(name, DataType::UInt32, false);
    let opt_u32 = |name: &str| Field::new(name, DataType::UInt32, true);
    let opt_u64 = |name: &str| Field::new(name, DataType::UInt64, true);

    let fields = vec![
        Field::new(
            "host_rx_time",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("chip", DataType::Utf8, false),
        Field::new("mac", DataType::Utf8, false),
        Field::new("rssi", DataType::Int32, false),
        req_u32("timestamp"),
        req_u32("rate"),
        Field::new("noise_floor", DataType::Int32, false),
        req_u32("sig_len"),
        req_u32("rx_state"),
        req_u32("channel"),
        Field::new("sequence_number", DataType::UInt16, false),
        Field::new("data_format", DataType::Utf8, false),
        Field::new("csi_data_len", DataType::UInt16, false),
        Field::new(
            "csi_data",
            DataType::List(Arc::new(Field::new("item", DataType::Int8, true))),
            false,
        ),
        // date_time flattened (nullable — only present when NTP time is set)
        opt_u64("dt_year"),
        opt_u64("dt_month"),
        opt_u64("dt_day"),
        opt_u64("dt_hour"),
        opt_u64("dt_minute"),
        opt_u64("dt_second"),
        opt_u64("dt_millisecond"),
        // esp32-family only
        opt_u32("sgi"),
        opt_u32("secondary_channel"),
        opt_u32("bandwidth"),
        opt_u32("antenna"),
        opt_u32("sig_mode"),
        opt_u32("mcs"),
        opt_u32("smoothing"),
        opt_u32("not_sounding"),
        opt_u32("aggregation"),
        opt_u32("stbc"),
        opt_u32("fec_coding"),
        opt_u32("ampdu_cnt"),
        // c5 / c6 only
        opt_u32("dump_len"),
        opt_u32("cur_bb_format"),
        opt_u32("rx_channel_estimate_info_vld"),
        opt_u32("rx_channel_estimate_len"),
        opt_u32("second"),
        opt_u32("is_group"),
        opt_u32("rxend_state"),
        opt_u32("rxmatch3"),
        opt_u32("rxmatch2"),
        opt_u32("rxmatch1"),
        // c6 only
        opt_u32("he_sigb_len"),
        opt_u32("cur_single_mpdu"),
        opt_u32("rxmatch0"),
    ];
    Arc::new(Schema::new(fields))
}

/// Error opening, writing, or closing a Parquet session file.
#[derive(Debug)]
pub enum ParquetSinkError {
    Io(std::io::Error),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
}

impl std::fmt::Display for ParquetSinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "parquet sink io error: {e}"),
            Self::Arrow(e) => write!(f, "parquet sink arrow error: {e}"),
            Self::Parquet(e) => write!(f, "parquet sink error: {e}"),
        }
    }
}

impl std::error::Error for ParquetSinkError {}

impl From<std::io::Error> for ParquetSinkError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<arrow::error::ArrowError> for ParquetSinkError {
    fn from(e: arrow::error::ArrowError) -> Self {
        Self::Arrow(e)
    }
}
impl From<parquet::errors::ParquetError> for ParquetSinkError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        Self::Parquet(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csi::{decode, ChipVariant, DateTime, PacketA, RxCsiFmt};

    #[test]
    fn writes_readable_parquet() {
        // Build a packet, encode it like the firmware, decode it, write it.
        let pkt = PacketA {
            mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            rssi: -50,
            timestamp: 1000,
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
            noise_floor: -95,
            rx_state: 0,
            sig_len: 100,
            date_time: Some(DateTime {
                year: 2026,
                month: 6,
                day: 22,
                hour: 1,
                minute: 2,
                second: 3,
                millisecond: 4,
            }),
            sequence_number: 1,
            data_format: RxCsiFmt::HtBw20,
            csi_data_len: 3,
            csi_data: vec![1, 2, 3],
        };
        let mut buf = vec![0u8; 1024];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);
        let decoded = decode(body, ChipVariant::Esp32Family).unwrap();

        let dir = std::env::temp_dir();
        let path = dir.join("csi_sink_test.parquet");
        let path_str = path.to_str().unwrap();

        {
            let mut sink = ParquetSink::open(path_str, "esp32").unwrap();
            sink.push(decoded, 1_700_000_000_000_000).unwrap();
            // Dropping the sink at end of scope finalizes the file.
        }

        // Read it back with the parquet reader.
        let file = File::open(path_str).unwrap();
        let builder =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), build_schema().fields().len());
        let _ = std::fs::remove_file(path_str);
    }
}
