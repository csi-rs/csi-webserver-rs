# csi-webserver

`csi-webserver` is a host-side HTTP and WebSocket server for ESP32 CSI collection.
It reads CSI frames from an ESP32 over USB serial (always the compact
`serialized` COBS+postcard format) and forwards them to:

- WebSocket clients (`/api/ws`) — raw serialized frames
- Session dump files decoded to Parquet (`csi_dump_<id>_YYYYMMDD_HHmmss.parquet`)
- Or both at the same time

This crate is intended to be run as an executable service.

## Install

```bash
cargo install csi-webserver
```

## Run

```bash
csi-webserver --interface 0.0.0.0 --port 3000
```

### CLI options

```text
csi-webserver [OPTIONS]

Options:
      --interface <INTERFACE>  Network interface to bind to [default: 0.0.0.0]
      --port <PORT>            TCP port to listen on [default: 3000]
      --baud-rate <BAUD_RATE>  UART baud rate to the ESP32 [env: CSI_BAUD_RATE]
                               [default: 115200]
  -h, --help                   Print help
  -V, --version                Print version
```

## Firmware requirement

Flash the ESP32 with `esp-csi-cli-rs` first:

- https://github.com/csi-rs/esp-csi-cli-rs

Supported board families:

- ESP32
- ESP32-C3
- ESP32-C5
- ESP32-C6
- ESP32-S3

## Quick start

```bash
# 1) Start the server
csi-webserver

# 2) Verify the device is running esp-csi-cli-rs
curl -sS "http://127.0.0.1:3000/api/info"

# 3) (Optional) write dumps to disk as well as streaming
curl -sS -X POST "http://127.0.0.1:3000/api/config/output-mode" \
  -H "Content-Type: application/json" \
  -d '{"mode":"both"}'

# 4) Start an indefinite collection session (always serialized; dumps are Parquet)
curl -sS -X POST "http://127.0.0.1:3000/api/control/start"

# 5) Read runtime status
curl -sS "http://127.0.0.1:3000/api/control/status"

# 6) Stop the collection (finalizes the Parquet file)
curl -sS -X POST "http://127.0.0.1:3000/api/control/stop"
```

Pass `{"duration": <secs>}` to `/api/control/start` for a timed run.

## Output modes

| Mode | WebSocket stream | Dump file |
|------|------------------|-----------|
| `stream` (default) | yes | no |
| `dump` | no | yes |
| `both` | yes | yes |

Switch at runtime with `POST /api/config/output-mode`.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CSI_SERIAL_PORT` | auto-detect | Override serial port path |
| `CSI_BAUD_RATE` | `115200` | Override serial baud rate (also `--baud-rate`) |
| `RUST_LOG` | `csi_webserver=debug` | Tracing filter |

## Full references

- HTTP API reference: https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md
- Repository README: https://github.com/csi-rs/csi-webserver-rs/blob/main/README.md
- Generated Rust docs: https://docs.rs/csi-webserver

## License

Apache-2.0.
