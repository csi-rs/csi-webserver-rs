# csi-webserver

`csi-webserver` is a host-side HTTP and WebSocket service for ESP32 CSI capture.
It receives CSI frames over USB serial (always the compact `serialized`
COBS+postcard format), streams the raw frames to WebSocket clients and/or
decodes them into **Parquet** dump files. It supports **multiple devices at
once** — a hotplug supervisor discovers attached boards, and every endpoint is
scoped to a device id under `/api/devices/{id}/...`.

## Documentation map

- Repository guide: this file
- Crates.io package document: [CRATES.md](CRATES.md)
- HTTP API reference: [API.md](API.md)
- Multi-device migration guide (breaking changes + client integration): [MIGRATION.md](MIGRATION.md)
- Rust API docs: <https://docs.rs/csi-webserver>

## API reference

For complete endpoint documentation, request/response payloads, and runtime
behavior details, see [API.md](API.md).

## Prerequisites

Flash your ESP32 with `esp-csi-cli-rs` before running this server:

- <https://github.com/csi-rs/esp-csi-cli-rs>

Supported board families:

- ESP32
- ESP32-C3
- ESP32-C5
- ESP32-C6
- ESP32-S3

## Build and run from source

```bash
cargo run
```

Or run with explicit bind / link parameters:

```bash
cargo run -- --interface 127.0.0.1 --port 3000 --baud-rate 921600
```

The baud rate also accepts `CSI_BAUD_RATE` as an environment-variable
fallback when `--baud-rate` is omitted.

Pin stable device ids to specific ports with repeatable `--device` flags, and
tune how often the supervisor rescans for plugged/unplugged boards:

```bash
cargo run -- --device lab1=/dev/ttyUSB0 --device lab2=/dev/ttyUSB1 \
  --scan-interval-ms 1000
```

Without a `--device` override, a device's id is its sanitized port basename
(e.g. `/dev/ttyUSB0` → `ttyUSB0`).

## Install as a binary

```bash
cargo install csi-webserver
csi-webserver --help
```

## Quick start

```bash
# 1) Start service
csi-webserver

# 2) Discover attached devices and pick an id (e.g. "ttyUSB0")
curl -sS "http://127.0.0.1:3000/api/devices"

# 3) Verify the device is running esp-csi-cli-rs
curl -sS "http://127.0.0.1:3000/api/devices/ttyUSB0/info"

# 4) (Optional) write dumps to disk as well as streaming
curl -sS -X POST "http://127.0.0.1:3000/api/devices/ttyUSB0/config/output-mode" \
  -H "Content-Type: application/json" \
  -d '{"mode":"both"}'

# 5) Start an indefinite collection (always serialized; dumps are Parquet)
curl -sS -X POST "http://127.0.0.1:3000/api/devices/ttyUSB0/control/start"

# 6) Check status
curl -sS "http://127.0.0.1:3000/api/devices/ttyUSB0/control/status"

# 7) Stop the collection (finalizes the Parquet file)
curl -sS -X POST "http://127.0.0.1:3000/api/devices/ttyUSB0/control/stop"
```

Pass `{"duration": <secs>}` to `/api/devices/{id}/control/start` for a timed run
that stops on its own.

WebSocket endpoint: `ws://127.0.0.1:3000/api/devices/{id}/ws`

## Output modes

| Mode | WebSocket stream | Dump file |
|------|------------------|-----------|
| `stream` (default) | yes | no |
| `dump` | no | yes |
| `both` | yes | yes |

Switch at runtime with `POST /api/devices/{id}/config/output-mode`.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CSI_SERIAL_PORT` | auto-detect | Pin a single serial port (disables multi-device discovery) |
| `CSI_BAUD_RATE` | `115200` | Override serial baud rate (also `--baud-rate`) |
| `RUST_LOG` | `csi_webserver=debug` | Tracing log filter |

## License

Apache-2.0. See [LICENSE](LICENSE).
