# csi-webserver

Host-side HTTP and WebSocket server for ESP32 CSI collection. Reads CSI frames
from boards running [`esp-csi-cli-rs`](https://github.com/csi-rs/esp-csi-cli-rs)
over USB serial (always the compact `serialized` COBS+postcard format) and
forwards them to:

- WebSocket clients (`/api/devices/{id}/ws`) — raw serialized frames
- Session dump files decoded to Parquet (`csi_dump_<id>_YYYYMMDD_HHmmss.parquet`)
- Or both at the same time

Built on the [`csi-webserver-core`](https://docs.rs/csi-webserver-core) library.

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
      --interface <INTERFACE>       Network interface to bind to [default: 0.0.0.0]
      --port <PORT>                   TCP port to listen on [default: 3000]
      --baud-rate <BAUD_RATE>         UART baud rate [env: CSI_BAUD_RATE] [default: 115200]
      --device <ALIAS=PORT_OR_MAC>    Pin a stable device id (repeatable)
      --scan-interval-ms <MS>         Hotplug rescan interval [default: 2000]
  -h, --help                          Print help
  -V, --version                       Print version
```

## Firmware requirement

Flash the ESP32 with `esp-csi-cli-rs` first:

- https://github.com/csi-rs/esp-csi-cli-rs

Supported board families: ESP32, ESP32-C3, ESP32-C5, ESP32-C6, ESP32-S3.

## Quick start

```bash
csi-webserver
curl -sS "http://127.0.0.1:3000/api/devices"
curl -sS "http://127.0.0.1:3000/api/devices/<id>/info"
curl -sS -X POST "http://127.0.0.1:3000/api/devices/<id>/control/start"
```

Pass `{"duration": <secs>}` to `/api/devices/{id}/control/start` for a timed run.

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
| `CSI_SERIAL_PORT` | auto-detect | Override serial port path |
| `CSI_BAUD_RATE` | `115200` | Override serial baud rate (also `--baud-rate`) |
| `RUST_LOG` | `csi_webserver_core=debug` | Tracing filter |

## Documentation

| Resource | Link |
|----------|------|
| HTTP API reference | [API.md](https://github.com/csi-rs/csi-webserver-rs/blob/main/crates/csi-webserver/API.md) |
| Service README | [README.md](https://github.com/csi-rs/csi-webserver-rs/blob/main/crates/csi-webserver/README.md) |
| Library (embedding) | [csi-webserver-core on docs.rs](https://docs.rs/csi-webserver-core)

## License

Apache-2.0.
