# csi-webserver HTTP API

Base URL examples:

- `http://127.0.0.1:3000`
- `http://<host>:<port>`

This server is a thin HTTP/WebSocket bridge over the `esp-csi-cli-rs` on-device
CLI. Each `set-*` endpoint forwards a corresponding command to the firmware
over USB serial. Most settings are snapshotted by the firmware on the next
`start`; a few (`log-mode`, `csi-delivery`) take effect immediately.

## Firmware gate

Before the server will dispatch *any* CLI command to the device, the
firmware must be verified as `esp-csi-cli-rs`. Verification works like
this:

- On every successful serial connect (and after the auto-RTS reset that
  follows), the server runs an internal `info` exchange. If the
  `ESP-CSI-CLI/<version>` magic prefix and `END-INFO` sentinel are observed,
  the firmware is marked verified and the parsed [`DeviceInfo`](#get-apiinfo)
  is cached.
- A successful `GET /api/info` call refreshes the cached identity.
- `POST /api/control/reset` *invalidates* the cached identity, pulses RTS,
  waits for the chip to boot, and re-runs the `info` exchange. The HTTP
  response includes whether re-verification succeeded.
- A USB disconnect clears the verified state — a different chip may be
  attached on reconnect.

While the firmware is **not** verified, every command-dispatching endpoint
returns **`412 Precondition Failed`** with a message pointing at
`GET /api/info` and `POST /api/control/reset` as recovery paths. The
exceptions — endpoints that are *always* reachable — are:

- `GET /` — health
- `GET /api/info` — the verification mechanism itself
- `GET /api/config` — read-only cache
- `GET /api/control/status` — read-only runtime status
- `POST /api/control/reset` — recovery path (clears + re-verifies)

## Response shape

Most command endpoints return this JSON shape:

```json
{ "success": true, "message": "..." }
```

Validation errors return `400 Bad Request` with the same shape and
`success: false`. When the ESP32 is not connected, command endpoints return
`503 Service Unavailable`. When the firmware has not been verified as
`esp-csi-cli-rs`, command endpoints return `412 Precondition Failed`
(see [Firmware gate](#firmware-gate)).

## Health

### `GET /`

Returns a plain text health string.

Example response body:

```text
CSI Server Active
```

## Firmware identification

### `GET /api/info`

Verify whether the attached ESP is running `esp-csi-cli-rs` and learn which
build of it. Issues the device-side `info` command, parses the magic block,
and returns it as JSON.

Successful response (200 OK):

```json
{
  "banner_version": "0.4.0",
  "name": "esp-csi-cli-rs",
  "version": "0.4.0",
  "chip": "esp32c6",
  "protocol": 1,
  "features": ["statistics", "println", "auto"]
}
```

Status codes:

| Status | When |
|--------|------|
| `200 OK` | Magic block received and parsed. |
| `502 Bad Gateway` | Device responded but the block could not be parsed. |
| `503 Service Unavailable` | ESP32 disconnected, or a collection is currently running (the firmware CLI is locked while collecting; stop it first). |
| `504 Gateway Timeout` | No `END-INFO` block received within the timeout. Most commonly the firmware is **not** `esp-csi-cli-rs` (or it predates the `info` command). |

Notes:

- `banner_version` is parsed from the `ESP-CSI-CLI/<version>` magic prefix
  emitted on every reset and at the top of the `info` block.
- `protocol` is a wire-format version number from the firmware
  (`CLI_PROTOCOL_VERSION`); a host should refuse to operate against unknown
  protocol values.
- `features` is informational — for example, the presence of `statistics`
  means `POST /api/control/stats` is available on the device side. Treat it
  as an unordered set.
- This endpoint is the **only reliable way** to confirm firmware presence;
  failed `set-*` commands could otherwise be misread as transient errors.

## Config endpoints

### `GET /api/config`

Returns the server-side cached view of device configuration, structured to
mirror the firmware's `show-config` output (`[WiFi]`, `[Collection]`,
`[CSI Config]`). Best-effort — each field is populated when the matching
`POST /api/config/*` endpoint succeeds, and reset to firmware defaults by
`POST /api/config/reset`. Values may drift if the device is re-flashed or
commands are sent out-of-band.

Example:

```json
{
  "wifi": {
    "mode": "sniffer",
    "channel": 6,
    "sta_ssid": "MyNetwork"
  },
  "collection": {
    "mode": "collector",
    "traffic_hz": 100,
    "phy_rate": "mcs0-lgi",
    "io_tx_enabled": true,
    "io_rx_enabled": true
  },
  "csi_config": {
    "lltf_enabled": true,
    "htltf_enabled": true,
    "stbc_htltf_enabled": true,
    "ltf_merge_enabled": true,
    "channel_filter_enabled": false,
    "manual_scale": false,
    "shift": 0,
    "dump_ack_enabled": false,
    "acquire_csi": 1,
    "acquire_csi_legacy": 1,
    "acquire_csi_ht20": 1,
    "acquire_csi_ht40": 1,
    "acquire_csi_su": 1,
    "acquire_csi_mu": 1,
    "acquire_csi_dcm": 1,
    "acquire_csi_beamformed": 1,
    "csi_he_stbc": 2,
    "val_scale_cfg": 2
  },
  "log_mode": "array-list",
  "csi_delivery_mode": "async",
  "csi_logging_enabled": true
}
```

Notes:

- `wifi`, `collection`, `csi_config` mirror the `show-config` sections.
- `csi_config` carries both classic (ESP32 / ESP32-C3 / ESP32-S3) and HE
  (ESP32-C5 / ESP32-C6) fields. The ones applicable to the connected chip
  are populated; the others stay `null`. Check `chip` from `GET /api/info`
  to know which side to read.
- The classic fields `channel_filter_enabled`, `manual_scale`, `shift`,
  `dump_ack_enabled` are **read-only on the device** — they have no
  `POST /api/config/csi` flag, so they only become non-null after
  `POST /api/config/reset` (which loads firmware defaults).
- `sta_password` is intentionally **not cached**; round-tripping plaintext
  passwords through a GET endpoint would defeat the point.
- `log_mode`, `csi_delivery_mode`, `csi_logging_enabled` are server-tracked
  extras not part of `show-config` — they're set via `set-log-mode` /
  `set-csi-delivery` and surfaced here for convenience.
- All fields are nullable (`Option<…>`). Absent fields mean "the
  corresponding endpoint has not been hit since startup / reset-config".
- After `POST /api/config/reset`, the cache is replaced with the firmware
  defaults documented in the `show-config` spec (e.g.
  `wifi.mode = "sniffer"`, `collection.traffic_hz = 100`,
  `csi_config.csi_he_stbc = 2`).

### `POST /api/config/reset`

Sends `reset-config` to the device and clears the server-side cache. Restores
every device field to its compiled-in default.

Request body: none.

### `POST /api/config/wifi`

Sets Wi-Fi mode and optional station / channel parameters. Forwards
`set-wifi`.

Request body:

```json
{
  "mode": "station",
  "sta_ssid": "MyNetwork",
  "sta_password": "secret",
  "channel": 6
}
```

Required field:

- `mode` — one of `station`, `sniffer`, `esp-now-central`, `esp-now-peripheral`

Optional fields:

- `sta_ssid` — UTF-8, ≤ 32 bytes (firmware limit)
- `sta_password` — UTF-8, ≤ 32 bytes (firmware limit)
- `channel` — `u8`; valid Wi-Fi channels are 1–14. Ignored by `station` mode
  (the channel comes from the AP it associates with).

Notes:

- Spaces and special characters in `sta_ssid` / `sta_password` are wrapped in
  CLI quotes by the server. A value containing both `'` and `"` returns
  `400 Bad Request`.
- Values > 32 bytes return `400 Bad Request` (the firmware would otherwise
  panic).
- Mode-to-feature applicability:
  - `--set-channel` — sniffer / esp-now-* only
  - `sta_ssid` / `sta_password` — `station` mode only
  - PHY rate (`/api/config/rate`) — `esp-now-*` only

### `POST /api/config/traffic`

Sets traffic generation frequency. Forwards `set-traffic`.

Request body:

```json
{ "frequency_hz": 100 }
```

- `frequency_hz` — `u64` Hz; `0` disables traffic generation entirely.
- Values > `65535` are silently truncated by the firmware (cast to `u16`
  before being passed to the radio driver).

### `POST /api/config/csi`

Sets CSI feature flags. Forwards `set-csi`. The body merges classic and HE
options — only flags supported by the firmware's compiled-in variant take
effect; the others are silently ignored on the device side.

All fields are optional. Boolean flags are forwarded only when set to `true`.

```json
{
  "disable_lltf": false,
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
  "csi_he_stbc": 2,
  "val_scale_cfg": 2
}
```

Field groups:

- Classic (ESP32 / ESP32-C3 / ESP32-S3): `disable_lltf`, `disable_htltf`,
  `disable_stbc_htltf`, `disable_ltf_merge`.
- HE (ESP32-C5 / ESP32-C6): `disable_csi`, `disable_csi_legacy`,
  `disable_csi_ht20`, `disable_csi_ht40`, `disable_csi_su`, `disable_csi_mu`,
  `disable_csi_dcm`, `disable_csi_beamformed`, `csi_he_stbc` (`u32`),
  `val_scale_cfg` (`u32`).

`csi_he_stbc` semantics: `0` complete HE-LTF1, `1` complete HE-LTF2, `2`
sample evenly across both (default).

`csi_he_stbc` and `val_scale_cfg` ranges are documented in firmware help but
**not enforced** — out-of-range values are passed through.

### `POST /api/config/collection-mode`

Sets the node role. Forwards `set-collection-mode`.

Request body:

```json
{ "mode": "collector" }
```

Accepted values:

- `collector` — active generation + collection
- `listener` — passive receive only

Invalid values return `400 Bad Request`.

### `POST /api/config/log-mode`

Sets the CSI packet output format on the device and updates the server's
serial framer accordingly. Takes effect on the next received packet (no
restart required).

Request body:

```json
{ "mode": "array-list" }
```

Accepted values:

- `text`         — verbose human-readable output with metadata
- `array-list`   — compact one-line text output per packet (default)
- `serialized`   — binary COBS-framed postcard output
- `esp-csi-tool` — Hernandez 26-column CSV (compatible with the
  ESP32-CSI-Tool collector)

| Mode | Delimiter | Notes |
|------|-----------|-------|
| `text` | `\n` | Multiline packet assembly; waits for `csi raw data:` marker |
| `array-list` | `\n` | One bracketed `[…]` row per packet |
| `serialized` | `\0` | Binary COBS-framed packets |
| `esp-csi-tool` | `\n` | One `CSI_DATA,…` CSV row per packet |

Invalid mode values return `400 Bad Request`.

### `POST /api/config/output-mode`

Switches CSI output destination at runtime.

Request body:

```json
{ "mode": "stream" }
```

Accepted values: `stream`, `dump`, `both`.

| Value | WebSocket | Dump file | `/api/ws` |
|-------|-----------|-----------|-----------|
| `stream` | yes | no | available |
| `dump` | no | yes | `403 Forbidden` |
| `both` | yes | yes | available |

The new mode applies on the next received frame.

### `POST /api/config/rate`

Pin the Wi-Fi PHY rate. Forwards `set-rate`.

Request body:

```json
{ "rate": "mcs0-lgi" }
```

- `rate` — one of: `1m`/`1m-l`, `2m`, `5m5`/`5m5-l`, `11m`/`11m-l`, `6m`,
  `9m`, `12m`, `18m`, `24m`, `36m`, `48m`, `54m`, `mcs0-lgi`..`mcs7-lgi`,
  `mcs0-sgi`. Default on the device is `mcs0-lgi`.

Notes:

- Only consumed by ESP-NOW central / peripheral modes on the firmware side.
- Sniffer and station modes ignore the setting.
- Unknown rate values are caught by the firmware (no mutation), not by the
  server.

### `POST /api/config/io-tasks`

Toggle per-direction TX/RX Embassy tasks. Forwards `set-io-tasks`. Either or
both fields may be set; omitted fields keep their current device-side value.

Request body:

```json
{ "tx": true, "rx": true }
```

- `tx`, `rx` — booleans. The server translates `true → on`, `false → off`.
- Disabling RX = "pure transmitter" — skips the Wi-Fi-callback CSI path.
- Disabling TX = "pure receiver" — no traffic generation, regardless of
  `/api/config/traffic`.
- A body with neither field returns `400 Bad Request`.

### `POST /api/config/csi-delivery`

Switch the CSI delivery path and/or toggle the per-packet inline log gate at
runtime. Forwards `set-csi-delivery`. Takes effect immediately on the
firmware (next CSI packet).

Request body:

```json
{ "mode": "async", "logging": true }
```

- `mode` — optional. One of `off`, `callback`, `async`.
  - `off`      — drop user-side dispatch (inline `log_csi` may still run).
  - `callback` — dispatch synchronously to the registered hook.
  - `async`    — enqueue for the async client.
- `logging` — optional boolean; gates the per-packet UART/JTAG inline log
  path. Independent of `mode`.
- A body with neither field returns `400 Bad Request`.

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

Starts a collection session. Forwards `start`.

Request body is optional:

```json
{ "duration": 120 }
```

- `duration` — `u64` seconds. Omit for indefinite collection.
- If a collection is already running, the endpoint returns `503 Service Unavailable`.
- A new session dump filename is generated for each start request.
- Stop conditions: timed run elapsing, `POST /api/control/stop`, or
  `POST /api/control/reset`.

### `POST /api/control/stop`

Gracefully stops an in-progress collection without resetting the device.

Request body: none.

Notes:

- Sends the `q` byte over serial. While collection is running, the firmware
  CLI is locked and only `q`/`Q` is acted on (everything else is discarded),
  so this is the only way to stop without a hard reset.
- Returns `200 OK` with `"Collection not running"` when no session is active.
- Closes the active session dump file immediately.
- Use `POST /api/control/reset` instead when you also need to hard-reset the
  chip (e.g. to recover from a wedged radio).

### `POST /api/control/reset`

Resets the ESP32 by pulsing RTS (EN low, then release), then re-verifies
that the firmware is `esp-csi-cli-rs`. The cached firmware identity is
cleared *before* the pulse, so command endpoints stay blocked until the
post-reset `info` exchange confirms the chip.

Request body: none.

Behavior:

1. Clear `firmware_verified` and the cached `DeviceInfo`.
2. Close any active session dump file.
3. Pulse RTS.
4. Wait ~800 ms for the chip to boot.
5. Re-run the `info` exchange and update the cache.

The HTTP response always returns `200 OK` if the RTS pulse itself
succeeded, with the body's `message` field describing the re-verification
outcome:

- `"…firmware re-verified: esp-csi-cli-rs/<version> (<chip>)"` — happy path.
- `"…firmware identity could NOT be re-verified…"` — RTS worked but the
  device is not running `esp-csi-cli-rs` (or an older build without `info`).
  Command endpoints will keep returning `412 Precondition Failed`.
- `"…post-reset re-verification timed out…"` — no `END-INFO` block within
  the timeout. Try `GET /api/info` again later.

Returns `500 Internal Server Error` if the adapter or board wiring does
not support RTS reset (verification is not attempted in that case).

### `POST /api/control/stats`

Triggers `show-stats` on the device. Requires the firmware to be built with
the `statistics` feature (default-on).

Request body: none.

Notes:

- The actual counter snapshot is printed by the firmware over the same UART
  used for CSI output, so it appears in the stream consumed by `/api/ws` (or
  the dump file). The HTTP response only acknowledges that the command was
  delivered.
- Counters reset on each new `start` collection.

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
- `text`, `array-list`, `esp-csi-tool` modes: newline-terminated text packet blocks

Serialized frame layout:

```text
u32 little-endian length (4 bytes) + frame bytes (N bytes)
```

A new dump file is created per session, using:

- `csi_dump_YYYYMMDD_HHmmss.bin`
