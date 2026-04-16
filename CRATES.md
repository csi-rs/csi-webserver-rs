# csi-webserver

`csi-webserver` is a host-side HTTP and WebSocket server for ESP32 CSI collection.
It reads CSI frames from an ESP32 over USB serial and forwards them to:

- WebSocket clients (`/api/ws`)
- Session dump files (`csi_dump_YYYYMMDD_HHmmss.bin`)
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
  -h, --help                   Print help
  -V, --version                Print version
```

## Firmware requirement

Flash the ESP32 with `esp-csi-cli-rs` first:

- https://github.com/csi-rs/esp-csi-cli-rs

Supported board families:

- ESP32
- ESP32-C3
- ESP32-C6
- ESP32-S3

## Quick start

```bash
# 1) Start the server
csi-webserver

# 2) Set log mode
curl -sS -X POST "http://127.0.0.1:3000/api/config/log-mode" \
  -H "Content-Type: application/json" \
  -d '{"mode":"array-list"}'

# 3) Start a timed collection session
curl -sS -X POST "http://127.0.0.1:3000/api/control/start" \
  -H "Content-Type: application/json" \
  -d '{"duration":60}'

# 4) Read runtime status
curl -sS "http://127.0.0.1:3000/api/control/status"
```

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
| `CSI_BAUD_RATE` | `115200` | Override serial baud rate |
| `RUST_LOG` | `csi_webserver=debug` | Tracing filter |

## Full references

- HTTP API reference: https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md
- Repository README: https://github.com/csi-rs/csi-webserver-rs/blob/main/README.md
- Generated Rust docs: https://docs.rs/csi-webserver

## License

Apache-2.0.
