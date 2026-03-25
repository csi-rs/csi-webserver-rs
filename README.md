# csi-webserver

`csi-webserver` is an HTTP/WebSocket server that runs on your host machine and talks to an ESP32 over USB. It receives **Wi-Fi Channel State Information (CSI)** frames from the device in real time and makes them available to any application that can open a WebSocket connection or read a binary file — no serial port code required on your end.

You point it at your device, start a collection session via a simple REST API, and the server handles everything else: auto-detecting the serial port, framing, and forwarding the raw CSI data wherever you need it.

### What can you do with CSI data?
CSI captures how Wi-Fi radio signals change as they travel through the air. This can be used for things like motion detection, occupancy sensing, gesture recognition, and indoor positioning — all without any dedicated sensors, just a Wi-Fi chip.

---

## Prerequisites

Your ESP32 must have **[esp-csi-cli-rs](https://github.com/csi-rs/esp-csi-cli-rs)** flashed onto it. This is the firmware that runs on the device, collects CSI data, and communicates with this server over USB serial. Supported boards:

- ESP32
- ESP32-C3
- ESP32-C6 *(WiFi 6)*
- ESP32-S3

Follow the flashing instructions in the [esp-csi-cli-rs README](https://github.com/csi-rs/esp-csi-cli-rs#usage) — it takes a single `cargo` command per board.

---

## How it works

Once the firmware is running and you start this server, the flow is straightforward:

1. The server finds the ESP32's USB serial port automatically.
2. You configure the device (Wi-Fi mode, channel, log format, etc.) via REST API calls.
3. You call `POST /api/control/start` to begin collection.
4. CSI frames stream in from the device and are forwarded to WebSocket clients, written to a dump file on disk, or both — your choice, changeable at any time without restarting.

### Output modes

| Mode | WebSocket (`/api/ws`) | Dump file |
|------|-----------------------|-----------|
| `stream` *(default)* | ✅ live frames | ❌ |
| `dump` | `403 Forbidden` | ✅ written to disk |
| `both` | ✅ live frames | ✅ written to disk |

Switch modes at runtime via `POST /api/config/output-mode` — takes effect on the next received frame.

---

## Building

```bash
cargo build --release
```

Requires Rust 1.85+ (edition 2024) and an ESP32 with [esp-csi-cli-rs](https://github.com/csi-rs/esp-csi-cli-rs) flashed.

---

## Usage

```
csi-webserver [OPTIONS]

Options:
      --interface <INTERFACE>  Network interface to bind to [default: 0.0.0.0]
      --port <PORT>            TCP port to listen on [default: 3000]
  -h, --help                   Print help
  -V, --version                Print version
```

### Examples

```bash
# Default — all interfaces, port 3000
cargo run

# Custom interface and port
cargo run -- --interface 127.0.0.1 --port 8080

# Override the serial port (skips auto-detection)
CSI_SERIAL_PORT=/dev/ttyUSB1 cargo run

# Override baud rate (default 115200)
CSI_BAUD_RATE=921600 cargo run
```

Auto-detection checks USB VIDs for Silicon Labs CP210x (`0x10C4`), WCH CH340/341 (`0x1A86`), and Espressif native USB (`0x303A`), then falls back to any USB port.

---

## REST API

All JSON responses follow the `ApiResponse` shape unless noted:

```json
{ "success": true, "message": "..." }
```

---

### Config

#### `GET /api/config`
Returns the server-side cached device configuration.

**Response**
```json
{
  "wifi_mode": "sta",
  "channel": 6,
  "sta_ssid": "MyNetwork",
  "traffic_hz": 100,
  "collection_mode": "collector",
  "log_mode": "array-list"
}
```

---

#### `POST /api/config/reset`
Resets the device configuration to defaults.

---

#### `POST /api/config/wifi`
Set Wi-Fi mode, SSID, password, and channel.

**Body**
```json
{
  "mode": "sta",
  "sta_ssid": "MyNetwork",
  "sta_password": "secret",
  "channel": 6
}
```
`mode` is required; all other fields are optional.

---

#### `POST /api/config/traffic`
Set the traffic generation frequency.

**Body**
```json
{ "frequency_hz": 100 }
```

---

#### `POST /api/config/csi`
Configure CSI feature flags. All fields are optional booleans (`true` to disable the feature) except `csi_he_stbc` and `val_scale_cfg` which are `u8`.

**Body**
```json
{
  "disable_lltf": true,
  "disable_htltf": false,
  "disable_stbc_htltf": false,
  "disable_ltf_merge": false,
  "disable_csi": false,
  "disable_csi_legacy": false,
  "disable_csi_ht20": false,
  "disable_csi_ht40": false,
  "disable_csi_su": false,
  "disable_csi_mu": false,
  "disable_csi_dcm": false,
  "disable_csi_beamformed": false,
  "csi_he_stbc": 0,
  "val_scale_cfg": 0
}
```

---

#### `POST /api/config/collection-mode`
Switch between collector and listener roles.

**Body**
```json
{ "mode": "collector" }
```
Accepted values: `collector`, `listener`.

---

#### `POST /api/config/log-mode`
Set the serial framing format used by the ESP32 firmware. The server forwards the value to the device and immediately updates its serial frame parsing behavior.

**Body**
```json
{ "mode": "array-list" }
```

| Mode value | Server behavior | Notes |
|------------|-----------------|-------|
| `cobs` (or any value containing `cobs`) | Uses `\0` as frame delimiter | COBS-encoded binary framing |
| `text` (or any value containing `text`) | Uses `\n` delimiter and multiline text-packet assembly | Waits for a line containing `csi raw data:` before emitting a frame |
| Any other value (for example `array-list`, `none`) | Uses `\n` delimiter | Value is still sent to firmware as-is |

The server does not enforce a fixed enum for `mode`; it passes your value through to `set-log-mode --mode=<value>`.

---

#### `POST /api/config/output-mode`
Switch the CSI output destination at runtime. Takes effect on the next received frame.

**Body**
```json
{ "mode": "stream" }
```

| Value | WebSocket | Dump file | `/api/ws` |
|-------|-----------|-----------|-----------|
| `stream` | ✅ | ❌ | Available |
| `dump` | ❌ | ✅ | `403 Forbidden` |
| `both` | ✅ | ✅ | Available |

If switched to `dump` or `both` before a session is started, the file is opened as soon as `POST /api/control/start` is called.

---

### Control

#### `POST /api/control/start`
Start CSI data collection on the device. Creates a new session and generates a timestamped dump file name (`csi_dump_YYYYMMDD_HHmmss.bin`). The file is only written to if the output mode includes `dump`.

**Body** *(optional)*
```json
{ "duration": 120 }
```
Omit the body (or `duration`) for indefinite collection.

> ⚠️ **Indefinite collection warning** — if `duration` is omitted, the ESP32 will collect CSI data indefinitely until it is reset. While the device is actively collecting, **new configuration commands (Wi-Fi mode, channel, log format, etc.) will be ignored**. To reconfigure the device you must first reset it via `POST /api/control/reset`, wait for it to boot, and then send your new configuration before starting collection again.

---

#### `POST /api/control/reset`
Reset the ESP32 by pulsing the RTS line (asserts EN low for 100 ms, then releases it). The chip reboots and is ready to accept new configuration commands.

No request body required.

> ⚠️ **Adapter support** — this relies on the USB-UART adapter's RTS pin being wired to the ESP32's EN pin, which is the case on all standard devkits (CP210x, CH340, Espressif native USB). Custom or bare-module boards without this circuit will receive a `500` response; in that case reset the device manually by pressing the EN/RST button.

---

### WebSocket

#### `GET /api/ws`
Upgrades to a WebSocket connection and streams raw CSI frames as **binary messages**.

- Returns `403 Forbidden` (JSON body) when the output mode is `dump`.
- Each message is one unmodified frame exactly as received from the serial port.
- Slow clients that fall behind the broadcast buffer have packets dropped but remain connected.
- The client must decode frames according to the active log mode (`array-list` text or COBS binary).

**Example (JavaScript)**
```js
const ws = new WebSocket("ws://localhost:3000/api/ws");
ws.binaryType = "arraybuffer";
ws.onmessage = (event) => {
  const frame = new Uint8Array(event.data);
  // decode frame according to active log-mode
};
```

---

## Dump File Format

Each frame is stored as a length-prefixed record:

```
┌─────────────┬─────────────────────────┐
│  u32 LE (4B)│  frame bytes (N bytes)  │
│   length N  │                         │
└─────────────┴─────────────────────────┘
```

Files are always truncated on open. A new file is created for each collection session (`POST /api/control/start`).

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CSI_SERIAL_PORT` | *(auto-detect)* | Override the serial port path |
| `CSI_BAUD_RATE` | `115200` | Override the serial baud rate |
| `RUST_LOG` | `csi_webserver=debug` | Tracing log filter |
