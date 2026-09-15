# csi-webserver

Default host-side CSI server executable. Discovers ESP32 boards over USB,
bridges `esp-csi-cli-rs` firmware to HTTP/WebSocket clients, and optionally
writes Parquet session dumps.

Built on the [`csi-webserver-core`](https://docs.rs/csi-webserver-core) library.

## Documentation

| Document | Contents |
|----------|----------|
| [CRATES.md](CRATES.md) | crates.io package summary |
| [API.md](API.md) | Full HTTP/WebSocket API reference |
| [MIGRATION.md](MIGRATION.md) | Upgrade notes from older layouts |
| [Library README](../csi-webserver-core/README.md) | Embedding the server in your own app |

## Prerequisites

Flash the ESP32 with [`esp-csi-cli-rs`](https://github.com/csi-rs/esp-csi-cli-rs)
before starting the server.

Supported chips: ESP32, ESP32-C3, ESP32-C5, ESP32-C6, ESP32-S3.

## Install

```bash
cargo install csi-webserver
csi-webserver --help
```

## Run from source

From the workspace root:

```bash
cargo run -p csi-webserver
cargo run -p csi-webserver -- --interface 127.0.0.1 --port 3000 --baud-rate 921600
cargo run -p csi-webserver -- --device lab1=/dev/ttyUSB0 --scan-interval-ms 1000
```

## Quick start

```bash
csi-webserver

curl -sS "http://127.0.0.1:3000/api/devices"
curl -sS "http://127.0.0.1:3000/api/devices/<id>/info"
curl -sS -X POST "http://127.0.0.1:3000/api/devices/<id>/control/start"
```

WebSocket: `ws://127.0.0.1:3000/api/devices/<id>/ws`

See [API.md](API.md) for every endpoint, payload, and status code.

## CLI options

```text
csi-webserver [OPTIONS]

      --interface <INTERFACE>       Bind address [default: 0.0.0.0]
      --port <PORT>                 TCP port [default: 3000]
      --baud-rate <BAUD_RATE>       UART baud [env: CSI_BAUD_RATE] [default: 115200]
      --device <ALIAS=PORT_OR_MAC>  Stable device id override (repeatable)
      --scan-interval-ms <MS>       Hotplug rescan interval [default: 2000]
```

## Output modes

| Output mode | WebSocket | Parquet dump |
|------|-----------|--------------|
| `stream` (default) | yes | no |
| `dump` | no | yes |
| `both` | yes | yes |

Set via `POST /api/devices/{id}/config/output-mode` — see [API.md](API.md). This is where the CSI
goes, not how the node reaches the channel; that is the node's **operational mode**, set with
`POST /api/devices/{id}/config/wifi`.

## The node model

A node is described by four independent attributes — what it contributes to the network, whether
its measurements leave it, how it reaches the channel, and what part it plays in the session. This
server sets the third and validates against the rest.

The model is documented once, in
[`esp-csi-rs/docs/network-model.md`](https://github.com/csi-rs/esp-csi-rs/blob/main/docs/network-model.md).
The accepted `wifi` mode tokens are listed in
[`csi-webserver-core`'s README](https://github.com/csi-rs/csi-webserver-core-rs#node-modes).

## Flashing a board

```sh
csi-webserver flash --port /dev/ttyACM0 --chip c6
```

Writes a merged image with `espflash write-bin`, defaulting to
`<firmware-dir>/esp-csi-cli-rs-<chip>.bin` (`--firmware-dir`, or `CSI_FIRMWARE_DIR`). **Stop the
server first** — a serial port has a single holder, and the supervisor owns every port it has
discovered. Needs `espflash` on `PATH`.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CSI_SERIAL_PORT` | auto-detect | Pin one serial port |
| `CSI_BAUD_RATE` | `115200` | Serial baud rate |
| `RUST_LOG` | `csi_webserver_core=debug` | Tracing filter |

## License

Apache-2.0. See [LICENSE](LICENSE).
