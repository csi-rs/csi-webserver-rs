# csi-webserver

`csi-webserver` is a host-side HTTP and WebSocket service for ESP32 CSI capture.
It receives CSI frames over USB serial and forwards them to WebSocket clients,
dump files, or both.

## Documentation map

- Repository guide: this file
- Crates.io package document: [CRATES.md](CRATES.md)
- HTTP API reference: [API.md](API.md)
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

## Install as a binary

```bash
cargo install csi-webserver
csi-webserver --help
```

## Quick start

```bash
# 1) Start service
csi-webserver

# 2) Verify the device is running esp-csi-cli-rs
curl -sS "http://127.0.0.1:3000/api/info"

# 3) Configure log parser mode
curl -sS -X POST "http://127.0.0.1:3000/api/config/log-mode" \
  -H "Content-Type: application/json" \
  -d '{"mode":"array-list"}'

# 4) Start an indefinite collection
curl -sS -X POST "http://127.0.0.1:3000/api/control/start"

# 5) Check status
curl -sS "http://127.0.0.1:3000/api/control/status"

# 6) Stop the collection
curl -sS -X POST "http://127.0.0.1:3000/api/control/stop"
```

Pass `{"duration": <secs>}` to `/api/control/start` for a timed run that
stops on its own.

WebSocket endpoint: `ws://127.0.0.1:3000/api/ws`

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
| `RUST_LOG` | `csi_webserver=debug` | Tracing log filter |

## License

Apache-2.0. See [LICENSE](LICENSE).
