# csi-webserver HTTP API

Base URL examples:

- `http://127.0.0.1:3000`
- `http://<host>:<port>`

This server bridges the `esp-csi-cli-rs` on-device CLI to HTTP/WebSocket. Each
`set-*` endpoint forwards a corresponding command to the firmware over USB
serial. Most settings are snapshotted by the firmware on the next `start`;
`csi-delivery` takes effect immediately. The device is always driven in
`serialized` mode; the server decodes frames and writes
[Parquet dumps](#dump-file-format).

## Device addressing

The server supports **multiple ESP32 devices at once**. A hotplug supervisor
scans for attached boards (default every 2 s, `--scan-interval-ms`), assigns
each a stable **device id**, spawns a dedicated serial worker, and tears it down
when the board is unplugged (after a short debounce). The server starts and
serves even when no device is attached.

- **Per-device endpoints** live under `/api/devices/{id}/...` — replace `{id}`
  with a real device id. Every endpoint below is documented with the `{id}`
  placeholder.
- **The device id** defaults to the sanitized port basename (e.g. `/dev/ttyUSB0`
  → `ttyUSB0`). Pin a stable alias with `--device <alias>=<port>` (repeatable),
  e.g. `--device lab1=/dev/ttyUSB0` makes the id `lab1`.
- **List devices** with [`GET /api/devices`](#get-apidevices) to discover the
  current ids and their connection / firmware status.
- An unknown `{id}` returns **`404 Not Found`** with the standard response shape.
- State is **per device**: connection, firmware verification, collection
  status, config cache, and the CSI stream are all independent. Starting a
  collection on one device does not affect another, and each device's
  WebSocket carries only its own frames.

## Devices

### `GET /api/devices`

List all currently attached devices and their runtime status. Always reachable
(does not require firmware verification). The set reflects the live hotplug
state, so newly plugged-in boards appear within one scan interval and unplugged
ones drop off after the debounce window.

Example response body:

```json
[
  {
    "id": "D0-CF-13-E2-90-E8",
    "mac": "D0:CF:13:E2:90:E8",
    "port_path": "/dev/ttyACM0",
    "baud_rate": 115200,
    "serial_connected": true,
    "collection_running": false,
    "firmware_verified": true,
    "device_info": {
      "banner_version": "0.5.0",
      "name": "esp-csi-cli-rs",
      "version": "0.5.0",
      "chip": "esp32c6",
      "mac": "D0:CF:13:E2:90:E8",
      "protocol": 2,
      "features": ["statistics"]
    }
  }
]
```

`device_info` is `null` until the firmware identity has been verified for that
device (see [Firmware gate](#firmware-gate)).

**Device identity.** `id` is the value used in `/api/devices/{id}/...` URLs. It
is derived from the board's `mac` (its USB `iSerialNumber`, the eFuse base MAC)
when available, so it stays stable even if the OS renumbers the port — e.g. a
native USB-Serial-JTAG board that re-enumerates from `/dev/ttyACM0` to
`/dev/ttyACM2` keeps the same `id`, and the server transparently follows it to
the new `port_path`. Boards exposing no serial number (most CP210x/CH340 UART
bridges) fall back to an `id` derived from the port basename. A
`--device <alias>=<port|mac>` launch flag overrides the `id` with a friendly
name (matched against either the port path or the MAC).

## Firmware gate

Before the server will dispatch *any* CLI command to the device, the
firmware must be verified as `esp-csi-cli-rs`. Verification works like
this:

- On every successful serial connect (and after the auto-RTS reset that
  follows), the server runs an internal `info` exchange. If the
  `ESP-CSI-CLI/<version>` magic prefix and `END-INFO` sentinel are observed,
  the firmware is marked verified and the parsed [`DeviceInfo`](#get-apidevicesidinfo)
  is cached.
- A successful `GET /api/devices/{id}/info` call refreshes the cached identity.
- `POST /api/devices/{id}/control/reset` *invalidates* the cached identity and
  resets the chip. For UART adapters (CP210x/CH340) it pulses RTS, waits for the
  chip to boot, and synchronously re-runs the `info` exchange — the HTTP
  response reports whether re-verification succeeded. For native USB-Serial-JTAG
  boards it sends the firmware `restart` command instead (pulsing RTS there
  re-enumerates/wedges the USB port) and returns immediately; the board
  re-verifies automatically on reconnect, so poll `GET /api/devices` to confirm.
- A USB disconnect clears the verified state — a different chip may be
  attached on reconnect.

While the firmware is **not** verified, every command-dispatching endpoint
returns **`412 Precondition Failed`** with a message pointing at
`GET /api/devices/{id}/info` and `POST /api/devices/{id}/control/reset` as recovery paths. The
exceptions — endpoints that are *always* reachable — are:

- `GET /` — health
- `GET /api/devices` — device listing
- `GET /api/devices/{id}/info` — the verification mechanism itself
- `GET /api/devices/{id}/config` — read-only cache
- `GET /api/devices/{id}/control/status` — read-only runtime status
- `POST /api/devices/{id}/control/reset` — recovery path (clears + re-verifies)

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

### `GET /api/devices/{id}/info`

Verify whether the attached ESP is running `esp-csi-cli-rs` and learn which
build of it. Issues the device-side `info` command, parses the magic block,
and returns it as JSON.

Successful response (200 OK):

```json
{
  "banner_version": "0.5.0",
  "name": "esp-csi-cli-rs",
  "version": "0.5.0",
  "chip": "esp32c6",
  "mac": "D0:CF:13:E2:90:E8",
  "protocol": 2,
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
- `mac` is the factory eFuse base MAC (`AA:BB:CC:DD:EE:FF`), present from
  CLI protocol 2 onward (`null` on older firmware). It matches the USB
  `iSerialNumber` and is the stable identity the server pins each device to
  (see [`GET /api/devices`](#get-apidevices)).
- `protocol` is a wire-format version number from the firmware
  (`CLI_PROTOCOL_VERSION`); a host should refuse to operate against unknown
  protocol values. `2` adds the `mac=` line.
- `features` is informational — for example, the presence of `statistics`
  means `POST /api/devices/{id}/control/stats` is available on the device side. Treat it
  as an unordered set.
- This endpoint is the **only reliable way** to confirm firmware presence;
  failed `set-*` commands could otherwise be misread as transient errors.

## Config endpoints

### `GET /api/devices/{id}/config`

Returns the server-side cached view of device configuration, structured to
mirror the firmware's `show-config` output (`[WiFi]`, `[Collection]`,
`[CSI Config]`). Best-effort — each field is populated when the matching
`POST /api/devices/{id}/config/*` endpoint succeeds, and reset to firmware defaults by
`POST /api/devices/{id}/config/reset`. Values may drift if the device is re-flashed or
commands are sent out-of-band.

Example:

```json
{
  "wifi": {
    "mode": "sniffer",
    "channel": 6,
    "sta_ssid": "MyNetwork",
    "peer_mac": "auto",
    "ht40": "none"
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
  "csi_delivery_mode": "async",
  "csi_logging_enabled": true
}
```

Notes:

- `wifi`, `collection`, `csi_config` mirror the `show-config` sections.
- `csi_config` carries both classic (ESP32 / ESP32-C3 / ESP32-S3) and HE
  (ESP32-C5 / ESP32-C6) fields. The ones applicable to the connected chip
  are populated; the others stay `null`. Check `chip` from `GET /api/devices/{id}/info`
  to know which side to read.
- The classic fields `channel_filter_enabled`, `manual_scale`, `shift`,
  `dump_ack_enabled` are **read-only on the device** — they have no
  `POST /api/devices/{id}/config/csi` flag, so they only become non-null after
  `POST /api/devices/{id}/config/reset` (which loads firmware defaults).
- `sta_password` is intentionally **not cached**; round-tripping plaintext
  passwords through a GET endpoint would defeat the point.
- `csi_delivery_mode`, `csi_logging_enabled` are server-tracked extras not part
  of `show-config` — they're set via `set-csi-delivery` and surfaced here for
  convenience. (The log mode is fixed to `serialized` and is no longer reported.)
- All fields are nullable (`Option<…>`). Absent fields mean "the
  corresponding endpoint has not been hit since startup / reset-config".
- `wifi.peer_mac` reads `"auto"` for the default magic-prefix pairing, or an
  explicit `aa:bb:cc:dd:ee:ff` once set. `wifi.ht40` is `none` / `above` /
  `below`. Both are ESP-NOW concerns.
- After `POST /api/devices/{id}/config/reset`, the cache is replaced with the firmware
  defaults documented in the `show-config` spec (e.g.
  `wifi.mode = "sniffer"`, `wifi.peer_mac = "auto"`, `wifi.ht40 = "none"`,
  `collection.traffic_hz = 100`, `csi_config.csi_he_stbc = 2`).

### `POST /api/devices/{id}/config/reset`

Sends `reset-config` to the device and clears the server-side cache. Restores
every device field to its compiled-in default.

Request body: none.

### `POST /api/devices/{id}/config/wifi`

Sets Wi-Fi mode and optional station / channel parameters. Forwards
`set-wifi`.

Request body:

```json
{
  "mode": "esp-now-central",
  "sta_ssid": "MyNetwork",
  "sta_password": "secret",
  "channel": 6,
  "peer_mac": "aa:bb:cc:dd:ee:ff",
  "ht40": "above"
}
```

Required field:

- `mode` — one of `station`, `sniffer`, `esp-now-central`, `esp-now-peripheral`

Optional fields:

- `sta_ssid` — UTF-8, ≤ 32 bytes (firmware limit)
- `sta_password` — UTF-8, ≤ 32 bytes (firmware limit)
- `channel` — `u8`; valid Wi-Fi channels are 1–14. Ignored by `station` mode
  (the channel comes from the AP it associates with).
- `peer_mac` — ESP-NOW peer source MAC, `aa:bb:cc:dd:ee:ff` or `aa-bb-...`
  (case-insensitive). An **empty string** clears the filter back to automatic
  magic-prefix pairing. A malformed value returns `400 Bad Request`. ESP-NOW
  modes only.
- `ht40` — forced ESP-NOW TX HT40 secondary channel: `above`, `below`,
  `none`, or `off` (an alias for `none`). ESP-NOW modes only. Any other value
  returns `400 Bad Request`.

Notes:

- Spaces and special characters in `sta_ssid` / `sta_password` are wrapped in
  CLI quotes by the server. A value containing both `'` and `"` returns
  `400 Bad Request`.
- Values > 32 bytes return `400 Bad Request` (the firmware would otherwise
  panic).
- Mode-to-feature applicability:
  - `--set-channel` — sniffer / esp-now-* only
  - `sta_ssid` / `sta_password` — `station` mode only
  - `peer_mac` / `ht40` — `esp-now-*` only (silently ignored by the firmware
    in other modes)
  - PHY rate (`/api/devices/{id}/config/rate`) — `esp-now-*` only

### `POST /api/devices/{id}/config/traffic`

Sets traffic generation frequency. Forwards `set-traffic`.

Request body:

```json
{ "frequency_hz": 100 }
```

- `frequency_hz` — `u64` Hz; `0` disables traffic generation entirely.
- Values > `65535` are silently truncated by the firmware (cast to `u16`
  before being passed to the radio driver).

### `POST /api/devices/{id}/config/csi`

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

### `POST /api/devices/{id}/config/collection-mode`

Sets the node role. Forwards `set-collection-mode`.

Request body:

```json
{ "mode": "collector" }
```

Accepted values:

- `collector` — active generation + collection
- `listener` — passive receive only

Invalid values return `400 Bad Request`.

### Log mode (removed)

The server no longer exposes a log-mode/output-format selector. The device is
always driven in `serialized` mode (COBS-framed postcard), the most compact and
fastest wire format. On `start` the server issues `set-log-mode --mode=serialized`
to the device automatically. The `text`, `array-list`, and `esp-csi-tool`
formats are no longer supported, and `POST /api/devices/{id}/config/log-mode`
has been removed (returns `404`).

The server decodes the serialized frames itself: dump files are written as
[Parquet](#dump-file-format) (typed columns, no reverse-engineering needed),
and the WebSocket carries the [raw serialized frames](#websocket-frame-schema)
for clients that want to decode live.

### `POST /api/devices/{id}/config/output-mode`

Switches CSI output destination at runtime.

Request body:

```json
{ "mode": "stream" }
```

Accepted values: `stream`, `dump`, `both`.

| Value | WebSocket | Dump file | `/api/devices/{id}/ws` |
|-------|-----------|-----------|-----------|
| `stream` | yes | no | available |
| `dump` | no | yes | `403 Forbidden` |
| `both` | yes | yes | available |

The new mode applies on the next received frame.

### `POST /api/devices/{id}/config/rate`

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

### `POST /api/devices/{id}/config/protocol`

Set the Wi-Fi PHY protocol applied to the node at the start of each collection
run. Forwards `set-protocol`.

Request body:

```json
{ "protocol": "lr" }
```

- `protocol` — one of: `b` (802.11b), `g` (802.11g), `n` (802.11n),
  `lr` (Espressif Long-Range), `a` (802.11a), `ac` (802.11ac), `ax` (802.11ax).
  Case-insensitive. Default on the device is `lr`.

Notes:

- Applied at the **start** of each run (read from config), not on command
  entry — set it before `POST .../control/start`.
- Independent of the PHY rate (`/api/devices/{id}/config/rate`); both are
  separate knobs.
- The protocol is **not** auto-derived from the Wi-Fi mode. To associate with a
  standard AP in station mode you must set `n` (or `ax`) explicitly — the
  default `lr` is Espressif-proprietary and won't associate.
- Unknown values return `400 Bad Request` (validated by the server). A protocol
  the chip/band can't support is rejected by the radio at `start`, not here.
- Appears in the cached config under `collection.protocol`.

### `POST /api/devices/{id}/config/io-tasks`

Toggle per-direction TX/RX Embassy tasks. Forwards `set-io-tasks`. Either or
both fields may be set; omitted fields keep their current device-side value.

Request body:

```json
{ "tx": true, "rx": true }
```

- `tx`, `rx` — booleans. The server translates `true → on`, `false → off`.
- Disabling RX = "pure transmitter" — skips the Wi-Fi-callback CSI path.
- Disabling TX = "pure receiver" — no traffic generation, regardless of
  `/api/devices/{id}/config/traffic`.
- A body with neither field returns `400 Bad Request`.

### `POST /api/devices/{id}/config/csi-delivery`

Switch the CSI delivery path and/or toggle the per-packet inline log gate at
runtime. Forwards `set-csi-delivery`. Takes effect immediately on the
firmware (next CSI packet).

Request body:

```json
{ "mode": "async", "logging": true }
```

- `mode` — optional. One of `off`, `callback`, `async`, `raw`.
  - `off`      — drop user-side dispatch (inline `log_csi` may still run).
  - `callback` — dispatch synchronously to the registered hook.
  - `async`    — enqueue for the async client.
  - `raw`      — zero-copy fast-path. Unlike the other modes this is stored as
    a flag on the device and only takes effect on the **next `start`**; while
    active no CSI data is delivered or logged and the `q`-key stop peek is
    skipped (the run is duration-bound or reset-driven). `off`/`callback`/
    `async` clear it again.
- `logging` — optional boolean; gates the per-packet UART/JTAG inline log
  path. Independent of `mode`.
- A body with neither field returns `400 Bad Request`.

## Control endpoints

### `GET /api/devices/{id}/control/status`

Returns runtime serial and collection status.

Example:

```json
{
  "serial_connected": true,
  "collection_running": false,
  "port_path": "/dev/ttyUSB0"
}
```

### `POST /api/devices/{id}/control/start`

Starts a collection session. Forwards `start`.

Request body is optional:

```json
{ "duration": 120 }
```

- `duration` — `u64` seconds. Omit for indefinite collection.
- If a collection is already running, the endpoint returns `503 Service Unavailable`.
- A new session dump filename is generated for each start request.
- Stop conditions: timed run elapsing, `POST /api/devices/{id}/control/stop`, or
  `POST /api/devices/{id}/control/reset`.

### `POST /api/devices/{id}/control/stop`

Gracefully stops an in-progress collection without resetting the device.

Request body: none.

Notes:

- Sends the `q` byte over serial. While collection is running, the firmware
  CLI is locked and only `q`/`Q` is acted on (everything else is discarded),
  so this is the only way to stop without a hard reset.
- Returns `200 OK` with `"Collection not running"` when no session is active.
- Closes the active session dump file immediately.
- Use `POST /api/devices/{id}/control/reset` instead when you also need to hard-reset the
  chip (e.g. to recover from a wedged radio).

### `POST /api/devices/{id}/control/reset`

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
  the timeout. Try `GET /api/devices/{id}/info` again later.

Returns `500 Internal Server Error` if the adapter or board wiring does
not support RTS reset (verification is not attempted in that case).

### `POST /api/devices/{id}/control/stats`

Triggers `show-stats` on the device. Requires the firmware to be built with
the `statistics` feature (default-on).

Request body: none.

Notes:

- The actual counter snapshot is printed by the firmware over the same UART
  used for CSI output, so it appears in the stream consumed by `/api/devices/{id}/ws` (or
  the dump file). The HTTP response only acknowledges that the command was
  delivered.
- Counters reset on each new `start` collection.

## WebSocket endpoint

### `GET /api/devices/{id}/ws`

Upgrades to a WebSocket and streams raw CSI frames as binary messages.

Notes:

- Returns `403 Forbidden` when output mode is `dump`.
- Each message is one unmodified serialized frame — a COBS-encoded postcard
  record (the trailing `\0` COBS terminator is stripped). See
  [WebSocket frame schema](#websocket-frame-schema) to decode.
- Slow clients may drop frames but remain connected.
- The stream carries only this device's frames. If the device is unplugged, the
  server sends a WebSocket Close frame and the socket is closed.

JavaScript example:

```js
const ws = new WebSocket("ws://127.0.0.1:3000/api/devices/{id}/ws");
ws.binaryType = "arraybuffer";
ws.onmessage = (event) => {
  const frame = new Uint8Array(event.data);
  // COBS-decode, then postcard-decode per the WebSocket frame schema below.
};
```

## Dump file format

Session dumps are written as **Apache Parquet** — one file per session,
named with the device id so concurrent devices never collide:

- `csi_dump_<id>_YYYYMMDD_HHmmss.parquet`
  (e.g. `csi_dump_ttyUSB0_20260621_120000.parquet`)

The server decodes the device's serialized frames into typed columns, so the
file is directly consumable by pandas / polars / pyarrow / DuckDB with no
format knowledge:

```python
import pyarrow.parquet as pq
t = pq.read_table("csi_dump_ttyUSB0_20260621_120000.parquet")
print(t.schema)
```

### Schema

One **superset** schema covers all chips; columns that only exist on some chips
are nullable and left null otherwise. Check the `chip` column (or
`GET /api/devices/{id}/info`) to know which apply.

| Column | Type | Chips | Notes |
|--------|------|-------|-------|
| `host_rx_time` | timestamp(µs, UTC) | all | **Server** wall-clock receive time. |
| `chip` | string | all | Source chip (e.g. `esp32`, `esp32c6`). |
| `mac` | string | all | Sender MAC, `aa:bb:cc:dd:ee:ff`. |
| `rssi` | int32 | all | dBm. |
| `timestamp` | uint32 | all | **Device** local time, microseconds since controller start. |
| `rate` | uint32 | all | PHY rate code. |
| `noise_floor` | int32 | all | dBm. |
| `sig_len` | uint32 | all | Packet length incl. FCS. |
| `rx_state` | uint32 | all | 0 = no error. |
| `channel` | uint32 | all | Primary channel. |
| `sequence_number` | uint16 | all | Packet sequence number. |
| `data_format` | string | all | `RxCSIFmt` variant name (e.g. `HtBw20`). |
| `csi_data_len` | uint16 | all | Length of `csi_data`. |
| `csi_data` | list&lt;int8&gt; | all | Raw CSI samples (variable length, ≤ 612). |
| `dt_year`…`dt_millisecond` | uint64 (nullable) | all | NTP calendar time, null unless the device set it. |
| `sgi`, `secondary_channel`, `bandwidth`, `antenna`, `sig_mode`, `mcs`, `smoothing`, `not_sounding`, `aggregation`, `stbc`, `fec_coding`, `ampdu_cnt` | uint32 (nullable) | esp32 / c3 / s3 | Radio metadata; null on c5/c6. |
| `dump_len`, `cur_bb_format`, `rx_channel_estimate_info_vld`, `rx_channel_estimate_len`, `second`, `is_group`, `rxend_state`, `rxmatch3`, `rxmatch2`, `rxmatch1` | uint32 (nullable) | c5 / c6 | Null on esp32-family. |
| `he_sigb_len`, `cur_single_mpdu`, `rxmatch0` | uint32 (nullable) | c6 only | Null elsewhere. |

`host_rx_time` is the host's wall clock; `timestamp` is the device's
microseconds-since-boot counter — use `host_rx_time` to correlate across
devices.

### Durability

The Parquet footer is written when the session ends (`stop`, output-mode switch
to `stream`, device disconnect, or server shutdown). A hard crash/power loss
leaves the in-progress file without a footer and any unflushed rows lost — that
file will not open. Stop collection cleanly to finalize.

## WebSocket frame schema

WebSocket frames are the device's `serialized` records: `postcard`-encoded,
COBS-framed (the server strips the trailing `\0`). To decode: COBS-decode, then
`postcard`-decode against the on-device `CSIDataPacket` struct for the chip.

- **Encoding**: [postcard](https://docs.rs/postcard) (non-self-describing,
  varint integers) inside [COBS](https://docs.rs/cobs) framing — pinned to
  `esp-csi-rs` **0.7.0**.
- **Layout differs by chip.** The field set and order match `CSIDataPacket` in
  `esp-csi-rs` 0.7.0:
  - **esp32 / esp32c3 / esp32s3**: `mac[6], rssi:i32, timestamp:u32, rate:u32,
    sgi:u32, secondary_channel:u32, channel:u32, bandwidth:u32, antenna:u32,
    sig_mode:u32, mcs:u32, smoothing:u32, not_sounding:u32, aggregation:u32,
    stbc:u32, fec_coding:u32, ampdu_cnt:u32, noise_floor:i32, rx_state:u32,
    sig_len:u32, date_time:Option<DateTime>, sequence_number:u16,
    data_format:RxCSIFmt, csi_data_len:u16, csi_data:Vec<i8>`.
  - **esp32c5 / esp32c6**: `mac[6], rssi:i32, timestamp:u32, rate:u32,
    noise_floor:i32, sig_len:u32, rx_state:u32, dump_len:u32,
    [he_sigb_len:u32, cur_single_mpdu:u32 — c6 only], cur_bb_format:u32,
    rx_channel_estimate_info_vld:u32, rx_channel_estimate_len:u32, second:u32,
    channel:u32, is_group:u32, rxend_state:u32, rxmatch3:u32, rxmatch2:u32,
    rxmatch1:u32, [rxmatch0:u32 — c6 only], date_time:Option<DateTime>,
    sequence_number:u16, csi_data_len:u16, data_format:RxCSIFmt,
    csi_data:Vec<i8>`.
- `DateTime` = `{ year, month, day, hour, minute, second, millisecond }`, all `u64`.
- `RxCSIFmt` is a `#[repr(u8)]` enum encoded as a varint of its declaration
  index: `Bw20, HtBw20, HtBw20Stbc, SecbBw20, SecbHtBw20, SecbHtBw20Stbc,
  SecbHtBw40, SecbHtBw40Stbc, SecaBw20, SecaHtBw20, SecaHtBw20Stbc, SecaHtBw40,
  SecaHtBw40Stbc, Undefined`.

Clients that don't want to track this layout should consume the Parquet dump
instead (the server already does this decode). If the firmware's protocol
version changes, update the decoder in lockstep.
