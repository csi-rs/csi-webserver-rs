# csi-webserver HTTP API

Base URL examples:

- `http://127.0.0.1:3000`
- `http://<host>:<port>`

## Response shape

Most command endpoints return this JSON shape:

```json
{ "success": true, "message": "..." }
```

## Health

### `GET /`

Returns a plain text health string.

Example response body:

```text
CSI Server Active
```

## Config endpoints

### `GET /api/config`

Returns server-side cached device configuration.

Example:

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

### `POST /api/config/reset`

Resets cached device configuration on the server and sends `reset-config` to the device.

Request body: none.

### `POST /api/config/wifi`

Sets Wi-Fi mode and optional station settings.

Request body:

```json
{
  "mode": "sta",
  "sta_ssid": "MyNetwork",
  "sta_password": "secret",
  "channel": 6
}
```

Required field:

- `mode`

Optional fields:

- `sta_ssid`
- `sta_password`
- `channel`

### `POST /api/config/traffic`

Sets traffic frequency used by the firmware.

Request body:

```json
{ "frequency_hz": 100 }
```

### `POST /api/config/csi`

Sets CSI feature flags.

All fields are optional.

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

### `POST /api/config/collection-mode`

Sets collection mode.

Request body:

```json
{ "mode": "collector" }
```

Accepted values:

- `collector`
- `listener`

### `POST /api/config/log-mode`

Sets serial framing mode used by the firmware and parser.

Request body:

```json
{ "mode": "array-list" }
```

Accepted values:

- `text`
- `array-list`
- `serialized`

Mode behavior:

| Mode | Delimiter | Notes |
|------|-----------|-------|
| `text` | `\n` | Multiline packet assembly; waits for `csi raw data:` marker |
| `array-list` | `\n` | One packet per line |
| `serialized` | `\0` | Binary COBS-framed packets |

Invalid mode values return `400 Bad Request`.

### `POST /api/config/output-mode`

Switches CSI output destination at runtime.

Request body:

```json
{ "mode": "stream" }
```

Accepted values:

- `stream`
- `dump`
- `both`

Behavior:

| Value | WebSocket | Dump file | `/api/ws` |
|-------|-----------|-----------|-----------|
| `stream` | yes | no | available |
| `dump` | no | yes | `403 Forbidden` |
| `both` | yes | yes | available |

The new mode applies on the next received frame.

## Control endpoints

### `GET /api/control/status`

Returns runtime serial and collection status.

Example:

```json
{
  "serial_connected": true,
  "collection_running": false,
  "port_path": "/dev/ttyUSB0"
}
```

### `POST /api/control/start`

Starts a collection session.

Request body is optional:

```json
{ "duration": 120 }
```

Notes:

- Omit `duration` for indefinite collection.
- If collection is already running, endpoint returns an error.
- A new session dump filename is generated for each start request.

### `POST /api/control/reset`

Resets ESP32 by pulsing RTS (EN low, then release).

Request body: none.

Notes:

- Works on typical ESP32 devkits where RTS is wired to EN.
- Returns error when adapter or board wiring does not support RTS reset.

## WebSocket endpoint

### `GET /api/ws`

Upgrades to a WebSocket and streams raw CSI frames as binary messages.

Notes:

- Returns `403 Forbidden` when output mode is `dump`.
- Each message is one unmodified serial frame.
- Slow clients may drop frames but remain connected.

JavaScript example:

```js
const ws = new WebSocket("ws://127.0.0.1:3000/api/ws");
ws.binaryType = "arraybuffer";
ws.onmessage = (event) => {
  const frame = new Uint8Array(event.data);
  // decode frame according to active log mode
};
```

## Dump file format

Session dump output uses one of two layouts, based on log mode:

- `serialized` mode: length-prefixed binary records
- `text` and `array-list` modes: newline-terminated text packet blocks

Serialized frame layout:

```text
u32 little-endian length (4 bytes) + frame bytes (N bytes)
```

A new dump file is created per session, using:

- `csi_dump_YYYYMMDD_HHmmss.bin`
