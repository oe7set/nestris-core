# Tournament station (`nestris-station`)

`nestris-station` is the headless daemon for a Retroverse tournament
station: a small Debian PC with a USB capture stick on the NES and the
Retroverse card reader (ESP32, firmware `nestris-rfid-reader`, protocol v2)
on USB. It

- captures the console picture and runs the recognition engine,
- attributes every game to the player whose card is on the reader,
- detects the **Select score cheat** (+10 000 points) and counts it per game,
- validates every result (`valid` + a list of issues),
- publishes live state, events and results to the host over **MQTT**,
- records every game as a NestrisChamps `.ngf.gz` (evidence for disputes),
- starts at boot and heals itself (see [Self-healing](#self-healing)).

One process serves one capture device and one reader; multiple stations are
told apart by `station.id`.

```
NES ─► USB capture ─► ffmpeg (v4l2) ─► engine ─► session ─┬─► MQTT ─► host
ESP32 RFID ─(USB serial JSON)─► player ────────────────────┤   (spool on disk
                                                           └─► .ngf.gz   until PUBACK)
```

## Install on Debian

German step-by-step guide (build, copy, configure, go2rtc, checklist):
[STATION_ANLEITUNG.de.md](STATION_ANLEITUNG.de.md).

Build the package once and copy only the `.deb` to every station. On
Windows with Docker Desktop:

```powershell
./tools/build-station-deb.ps1        # → dist/nestris-station_*_amd64.deb (-Tls for MQTT over TLS)
```

It builds in a Debian bookworm container, so the package runs on Debian 12
and 13. On any Debian/Ubuntu machine (or WSL) instead:

```sh
sudo apt install build-essential pkg-config curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
cargo install cargo-deb
cargo deb -p nestris-station          # → target/debian/nestris-station_*.deb
```

On the station:

```sh
sudo apt install ./nestris-station_*.deb     # pulls in ffmpeg
```

The package installs `/usr/bin/nestris-station`, the systemd unit (enabled
at boot, not started), `/etc/nestris-station/station.toml` and
`/etc/nestris-station/env` (both kept on upgrades), and creates the
`nestris` service user (groups `video`, `dialout`). For MQTT over TLS build
with `--features tls`.

Then:

```sh
nestris-station list-devices                 # find capture.device / rfid.port
sudoedit /etc/nestris-station/station.toml   # station.id, devices, mqtt.host
nestris-station check-config                 # validates, prints the resolved config
nestris-station test-mqtt                    # broker reachable?
sudo systemctl start nestris-station
journalctl -u nestris-station -f
```

After a reboot the station starts on its own. Keep the clock synced
(`systemd-timesyncd` or `chrony`): all timestamps in the payloads are wall
clock UTC.

### Device names

Use the stable links instead of `/dev/video0` / `/dev/ttyUSB0`, which can
swap between boots:

- capture: `/dev/v4l/by-id/usb-<vendor>_<product>-video-index0`
- reader: `/dev/serial/by-id/usb-<chip>-if00-port0`

or install the example udev rules
(`/usr/share/doc/nestris-station/99-nestris-station.rules`), which create
`/dev/nestris-capture` / `/dev/nestris-rfid` and disable USB autosuspend for
both devices.

Most cheap USB capture sticks only deliver 1280×720 at 60 fps as MJPEG
(`capture.input_format = "mjpeg"`, the default). `v4l2-ctl --list-formats-ext
-d <device>` (package `v4l-utils`) shows what a device supports.

### Capture through go2rtc

A capture device can only be opened by one process. To keep a browser
preview for debugging, let go2rtc own the device and point the station at
the local restream:

```toml
[capture]
device = "http://127.0.0.1:1984/api/stream.mjpeg?src=nes"   # or rtsp://127.0.0.1:8554/nes
```

Network URLs (`http(s)://`, `rtsp(s)://`, `rtmp://`, `tcp://`, `udp://`,
`srt://`) are live sources: no probing, wall-clock timestamps, frames
dropped rather than queued, and a dropped stream is reopened with backoff.
`input_format`/`width`/`height`/`fps` are ignored for them;
`scale_width`/`scale_height` still apply. HTTP MJPEG passes the device's JPEGs through
unchanged; RTSP is read over TCP. For tournament use prefer the device
directly (stop go2rtc, since it grabs the device as soon as someone
opens the stream).

## Configuration

`/etc/nestris-station/station.toml` — the annotated template is
[`crates/nestris-station/packaging/station.example.toml`](../crates/nestris-station/packaging/station.example.toml).
Only `capture.device`, `rfid.port` and usually `mqtt.host` / `station.id`
need editing.

Precedence (later wins):

1. built-in defaults,
2. the config file (deep-merged: a partial `[engine.fusion]` table keeps
   every other default),
3. the remote configuration set from NestrisLTM
   (`/var/lib/nestris-station/remote.json`, see *Remote configuration*),
4. environment `NESTRIS_STATION__<SECTION>__<KEY>=value` (e.g. from
   `/etc/nestris-station/env`),
5. `--set section.key=value` on the command line.

With NestrisLTM managing the stations, keep only the station's identity and
secrets local (`station.id`, `mqtt.host`, passwords/tokens, `rfid.port` if
it differs) and set everything else from NestrisLTM: an env or `--set` value
always wins and is shown there as locked.

Unknown keys are errors, so typos never pass silently. Secrets: put the MQTT
password in `mqtt.password_file` (mode 0600) or as
`NESTRIS_STATION__MQTT__PASSWORD` in the env file (mode 0640, `root:nestris`);
`check-config` masks it.

| Section | Key settings |
|---|---|
| `station` | `id` (topic + game-id prefix), `name`, `state_dir` (default: systemd `StateDirectory`, `/var/lib/nestris-station`) |
| `capture` | `device` (device path or stream URL), `input_format`, `width`/`height`/`fps`, `scale_width`/`scale_height`, `lowres` (0: MJPEG decode at 1/2^n size, see [DOWNSCALE.md](DOWNSCALE.md)), `stall_timeout_s` (5), `backoff_max_s` (30) |
| `rfid` | `enabled`, `port`, `baud` (115200), `stale_after_s` (6: no line for this long = reopen the port), `player_grace_s` (60) |
| `mqtt` | `host`, `port`, `username`, `password_file`, `tls`/`ca_file`, `topic_prefix` (`retroverse/nestris`), `live_max_hz` (60), `live_playfield` (true), `status_interval_s` (10) |
| `recording` | `enabled`, `dir`, `keep_days` (30), `max_gb` (20) |
| `host` | `url` (NestrisLTM, empty = no upload), `token` / `token_file` (API token, scope `stations`), `retry_max_s` (300), `max_age_h` (48), `timeout_s` (30) |
| `spool` | `dir`, `max_files` |
| `session` | `end_confirm_frames` (30), `min_game_frames` (120), `signal_lost_end_s` (30) |
| `integrity` | cheat detection and validation thresholds (see below) |
| `engine` | the full engine configuration ([USAGE.md](USAGE.md#engine-configuration)); station defaults enable background acquisition and downscaled candidate detection |
| `log` | `level` (`RUST_LOG` overrides) |

## MQTT contract

All topics live below `<topic_prefix>/<station.id>/`, e.g.
`retroverse/nestris/station-1/`. Payloads are compact JSON; timestamps are
RFC 3339 UTC with milliseconds.

| Topic | QoS / retain | When |
|---|---|---|
| `status` | 1, retained | on change and every `status_interval_s`; last will = offline |
| `player` | 1, retained | card placed / removed, reader connected / lost |
| `live` | 0 | on change, at most `live_max_hz` |
| `event/game_start` | 1, durable | a game was recognized (after `min_game_frames`) |
| `event/cheat` | 1, durable | a cheat was confirmed |
| `event/game_end` | 1, durable | a game ended — the result |
| `cmd` | subscribed | commands forwarded to the RFID reader; `update` (see *Updates*) |
| `update` | 1, retained | progress of an update started from NestrisLTM |
| `config` | 1, retained | remote configuration state (since 0.3.0, see *Remote configuration*) |

**Durable** messages are written to the on-disk spool before publishing and
deleted only after the broker's PUBACK. They survive network outages, broker
restarts and station restarts, and are delivered **at least once** — the
host must deduplicate by `game_id` (+ topic).

### `status`

```json
{"state":"online","station":"station-1","name":"Station 1","version":"0.2.0",
 "capture":"ok","capture_detail":"1280x720","lock":"locked","game_state":"in_game",
 "rfid":"ok","reader_fw":"1.0.0","reader_serial":"A4CF12B3C4D5",
 "game_id":"station-1-1790241008228","fps":50.0,"dropped_frames":0,
 "perf":{"capture_fps":50.0,"fps":50.0,"target_fps":50.0,"drop_rate":0.0,
         "missing":0,"dropped_total":0,"missing_total":3,
         "engine_ms_p50":6.1,"engine_ms_p95":11.8,"frame_age_ms_p95":13.2,
         "live_hz":31.5,"size":"720x576","cpu_pct":62.0,"load1":1.9},
 "uptime_s":3605,"ts":"2026-09-24T09:10:08.228Z"}
```

`perf` (since 0.3.0, absent until the first 2-second window closed) is the
pipeline's performance over the last 2 seconds:

| Field | Meaning |
|---|---|
| `capture_fps` | frames ffmpeg delivered per second |
| `fps` | frames the engine processed per second (same as top-level `fps`) |
| `target_fps` | `capture.fps`, `null` when the driver chooses |
| `drop_rate` | share of delivered frames dropped because the engine fell behind (0-1) |
| `missing` / `missing_total` | frames the source never delivered: gaps longer than 1.5 frame periods in the capture clock (drops inside the device, driver or ffmpeg), this window / since start |
| `dropped_total` | frames dropped since start (as `dropped_frames`) |
| `engine_ms_p50` / `engine_ms_p95` | engine time per frame |
| `frame_age_ms_p95` | from reading a frame off ffmpeg until its result is out (queue wait + engine) |
| `live_hz` | `live` messages sent per second (only changes are sent) |
| `size` | engine input size (`capture.scale_*`) |
| `cpu_pct` / `load1` | whole-machine CPU use and 1-minute load (Linux) |

`capture_fps` below `target_fps`, or `missing` above 0, points at the
camera, USB or decode; `drop_rate` above 0 means the CPU is too slow for the
engine input size (lower `capture.scale_*` or set `capture.lowres`).

`capture`: `ok`, `opening`, `waiting_for_device` (unplugged), `reconnecting`
(`capture_detail` has ffmpeg's error and the retry delay). `rfid`: `ok`,
`offline`, `outdated` (a reader answers but with another protocol: flash the
current `nestris-rfid-reader` firmware), `disabled`. `reader_fw` /
`reader_serial` identify the connected reader (`null` without one). On a clean shutdown and as the broker's last will:
`{"state":"offline","station":"station-1","ts":...}`.

### `player`

```json
{"present":true,"player":{"uid":"A1B2C3D4","name":"Erv"},"rfid":"ok","ts":"..."}
```

`name` is `null` for a blank card (uid known, no name written).

### `live`

```json
{"game_id":"station-1-1790241008228","player":{"uid":"A1B2C3D4","name":"Erv"},
 "game_state":"in_game","score":22800,"lines":4,"level":18,"next_piece":"I",
 "tetris_rate":1.0,"burn":0,"drought":3,"max_drought":9,"pps":0.9876,"pieces":12,
 "cheated":0,"confidence":0.95,
 "playfield":["0000000000","0000000000","...","0000000110","2221103311"],
 "seq":18822,"frame_age_ms":7.4,"ts":"..."}
```

`game_id` is `null` between games (menus still update `game_state`).

`seq` (since 0.3.0) counts the `live` messages since the station started:
a gap at the receiver means messages were lost (QoS 0), a lower value means
the station restarted. `frame_age_ms` is the time from reading the frame off
the capture to publishing it.

`playfield` is the stack, top row first: 20 strings of 10 cell ids — `0`
empty, `1` white, `2`/`3` the two accent colors of the current level's
palette (the color itself follows from `level`, as on the console). It
includes the falling piece and is held through line-clear animations. It is
`null` outside `in_game` — in particular while paused, because the console
hides the board during a pause and a spectator view must not reveal it — and
when `mqtt.live_playfield = false`. With the board, `live` changes with every
piece move and is published at up to `live_max_hz` (default 60/s, i.e. every
NES frame, about 18 KB/s per station).

### `event/game_start`

```json
{"game_id":"station-1-1790241008228","station":"station-1",
 "player":{"uid":"A1B2C3D4","name":"Erv"},"started_at":"...","start_level":18}
```

### `event/cheat`

```json
{"game_id":"station-1-1790241008228","station":"station-1","player":{...},
 "cheated":1,"count":1,"points":10000,"score_before":45600,"score_after":55612,
 "lines_delta":0,"ts":"..."}
```

`cheated` is the running total for the game, `count` this detection.

### `event/game_end`

```json
{"schema":1,"game_id":"station-1-1790241008228","station":"station-1",
 "player":{"uid":"A1B2C3D4","name":"Erv"},
 "started_at":"...","ended_at":"...","duration_s":431.2,"active_seconds":402.9,
 "end_reason":"game_over","start_level":18,"end_level":21,
 "score":216560,"lines":134,"clears":{"single":31,"double":11,"triple":3,"tetris":18},
 "tetris_rate":0.5373,"burn":64,"max_drought":21,"pieces":527,"pps":1.31,
 "cheated":0,"cheat_points":0,
 "valid":true,
 "validation":{"issues":[],"metrics":{"frames":25876,"ingame_frames":24190,
   "mean_confidence":0.93,"low_confidence_ratio":0.0,"signal_lost_s":0.0,
   "plausibility_rejects":2,"plausibility_corrections":0,"start_score":0,
   "start_lines":0,"start_level":18,"clear_points":214820,"cheat_points":0,
   "unexplained_points":0,"score_anomalies":0,"level_offset_steps":0}}}
```

- `cheated`: number of cheat inputs this game (`0` = clean). The score is
  reported as shown on screen; the host decides what to do with cheats.
- `end_reason`: `game_over`, `reset` (console reset / new game without a
  game-over screen), `signal_lost` (no picture for `signal_lost_end_s`),
  `shutdown` (station stopped mid-game).
- `valid`: `false` when a validation **error** was found — review the result
  before it counts. Warnings keep `valid: true`.

### `cmd`

Commands for the card reader (protocol v2, see
`nestris-rfid-reader/docs/PROTOCOL.md`), forwarded as one line once the
reader has introduced itself:

```sh
# text on the reader's display while this card lies on it
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"show","uid":"04A1B2C3","lines":["Erv","Bestwert 159.867"]}'
# write a name onto the card on the reader (optionally only card "uid")
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"write","id":1,"name":"Erv","timeout_ms":15000}'
# reader display settings (stored in the reader)
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"config","display":"128x64"}'
```

Only `show`, `write` and `config` go to the reader. The reader's answers
(`result`) appear in the station log; failed commands as warnings. The
station itself handles `update` (next section) and `station_config`
(*Remote configuration*).

## Remote configuration (from NestrisLTM)

Since 0.3.0 NestrisLTM's *Stationen → Konfiguration* sets the station
config: a template for all stations plus per-station overrides. NestrisLTM
always sends the complete set; its `rev` is a hash of the values.

```sh
# store and apply (after the running game)
mosquitto_pub -t retroverse/nestris/station-1/cmd -m   '{"type":"station_config","op":"set","rev":42,"values":{"capture":{"scale_width":480,"scale_height":384}}}'
# re-publish the config topic / scan capture devices and serial ports
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"station_config","op":"get"}'
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"station_config","op":"list_devices"}'
```

On `set` the station checks the keys against its allowlist and validates
the merged config. A valid set is written to
`<state_dir>/remote.json` (atomically; the previous one stays as
`remote.json.bak`) and the station exits after the running game;
systemd (`Restart=always`) starts it again with the new config about
3 seconds later. A set it rejects changes nothing. A `remote.json` that no
longer validates at startup (e.g. after a downgrade) is renamed to
`remote.json.rejected` and the station runs on its local config.

Remote-settable: `station.name`, `capture.*`, `rfid.enabled`, `rfid.port`,
`mqtt.live_max_hz`, `mqtt.live_playfield`, `mqtt.status_interval_s`,
`recording.enabled/gzip/keep_days/max_gb`, `session.*`, `integrity.*`,
`engine.*`, `log.level`. Never remote: the station id, broker, host URL and
token, paths and updates, so a bad set can always be corrected remotely.

`<base>/config` (retained) after every change, on start and on `get`:

```json
{"station":"station-1","version":"0.3.0","rev":42,"state":"applied","error":null,
 "values":{"capture":{"scale_width":480,"scale_height":384}},
 "effective":{"station":{"id":"station-1","...":"..."},"capture":{"...":"..."}},
 "locked":["station.id"],"allowed":["station.name","capture.","..."],
 "devices":{"capture":[{"path":"/dev/v4l/by-id/usb-MACROSILICON_..-video-index0",
   "formats":[{"format":"mjpeg","sizes":["1920x1080","720x576"]}]}],
   "serial":["/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0"]},
 "ts":"..."}
```

`state`: `none` (no remote set), `applied`, `pending` (stored, applies after
the running game), `restarting`, `rejected` (`error` says why; `rev` is the
rejected set's, `values` the set still in effect). `effective` is the config
the process runs with, secrets masked. `locked` lists the keys the
environment or `--set` pin. `devices` is only present after `list_devices`.

## Updates (from NestrisLTM)

NestrisLTM's page *Geräte* updates the station package and the reader
firmware. Stations need no internet: the host downloads the GitHub release,
verifies it and keeps it in its release cache; the station fetches the files
from the host **and verifies the release signature again itself** (Ed25519
key compiled into the station, the same as in every Retroverse app), so a
compromised host cannot install foreign packages.

```sh
# what NestrisLTM publishes on <base>/cmd:
{"type":"update","target":"station","release":"v0.3.0","version":"0.2.1"}
{"type":"update","target":"reader","release":"v1.1.0","version":"1.1.0","mode":"app"}
```

- `release` is the GitHub tag; `version` the station package version
  (`nestris-station_<version>[-<rev>]_<arch>.deb`, the station picks its
  architecture) or the reader firmware version. `mode: "factory"` writes the
  reader's factory image (readers still on the v1 firmware; resets the
  reader's settings).
- Refused (reported as `failed`) while a game runs, while another update
  runs, with `update.enabled = false`, without `host.url`/`host.token`, and
  for the reader without `rfid.port`.
- Files: `GET <host.url>/api/stations/<station.id>/updates/<repo>/<release>/<file>`
  with the host token (the same as for the recording upload); `SHA256SUMS.txt`
  and `SHA256SUMS.txt.sig` first, then the package or the reader manifest and
  image. Everything lands in `<state_dir>/updates/`.
- **Station package**: the station (user `nestris`, never root) writes
  `<state_dir>/updates/request`. The path unit `nestris-station-update.path`
  starts the oneshot `nestris-station-update.service`, which runs
  `/usr/lib/nestris-station/update-helper` as root: it copies the files into
  the root-only `/var/lib/nestris-station-update/`, checks them again with
  `nestris-station verify-update` (the installed binary), checks the package
  name, runs `apt-get install` (downgrades allowed, config files kept) and
  restarts the station. Its outcome (`result`) is published after the restart.
- **Reader firmware**: the station closes the reader's port and runs
  `esptool` (Debian package `esptool`, recommended by the station package;
  `update.esptool`) with the image and offset from the signed release
  manifest, then waits up to 25 s for the reader's `hello` with the new
  version.

`<base>/update` (retained):

```json
{"station":"station-1","target":"reader","version":"1.1.0","state":"flashing",
 "detail":null,"progress":0.4,"ts":"2026-10-04T18:00:00.000Z"}
```

`state`: `downloading`, `verifying`, `installing` (the station restarts),
`flashing`, `waiting`, `done`, `failed` (`detail` says why).

By hand on a station: `sudo journalctl -u nestris-station -u nestris-station-update`,
`nestris-station verify-update /var/lib/nestris-station-update <file.deb>`.

## Recording upload (NestrisLTM)

With `host.url` set, every saved recording is uploaded to the host after the
game ended, so the host has the complete game (every NES frame) for replays
and disputes:

```
PUT <host.url>/api/stations/<station.id>/games/<game_id>/ngf
Authorization: Bearer <host token>
Content-Type: application/gzip
X-NGF-SHA256: <hex>
```

- The recording is linked to the session game that started while it ran
  (`game_id` as in `event/game_end`). Recording starts on the first in-game
  frame too (`record_partial`), so games the station joined mid-way are kept.
- Jobs live in `<state_dir>/uploads` (one small JSON per recording) and
  survive restarts. `404` from the host means its `game_end` is not processed
  yet and is retried; network errors and `5xx` back off up to
  `retry_max_s`; `400`/`409`/`413` drop the job (logged as error); jobs
  older than `max_age_h` are dropped. A clean shutdown waits up to 10 s for
  pending uploads.
- The host stores the file, verifies it decodes, and deletes its live frames
  of that game (the recording replaces them).

## Cheat detection

Legal score gains in NES Tetris come from line clears — base
40/100/300/1200 × (level + 1) — plus a few push-down (soft-drop) points. The
Select trick adds a flat 10 000. Every settled score gain is explained as

```
ΔSCORE = clear points for ΔLINES  +  k × 10 000  +  push-down
```

and `k` is counted as cheat inputs. The detector (sans-io, in
`nestris-engine/src/integrity/cheat.rs`) is built to never flag a clean game:

- a SCORE/LINES value only counts after holding `confirm_frames` (10) in-game
  frames, so recognition glitches never form a scoring step;
- a score gain waits up to `pair_window_frames` for its LINES change (the two
  counters settle on different frames);
- both the level before and after a step are tried as multiplier (level-ups),
  and a level misread by one is tolerated (reported as `level_score_mismatch`)
  — the multiplier shift is at most 1 200 per tetris, far below a cheat, so
  this never hides one;
- a detected cheat stays provisional for 1.5 s and is retracted if the score
  falls back (a misread ten-thousands digit is exactly ±10 000);
- more than two cheat inputs in one step, or a gain that is neither legal nor
  whole cheat multiples, is reported as a `score_unexplained` anomaly, never
  as a cheat.

Tuning (`[integrity]`): `cheat_points` (10000), `cheat_slack` (250),
`confirm_frames` (10), `pair_window_frames` (30), `softdrop_slack` (200),
`anomaly_threshold` (2000).

## Validation

At the end of every game (`nestris-engine/src/integrity/validate.rs`):

| Code | Severity | Meaning |
|---|---|---|
| `partial_game` | error | capture joined a running game (first score/lines > 0) |
| `score_reconcile` | error | final score is off the recognized clears + cheats by more than the push-down budget (`pieces × softdrop_per_piece`) + `reconcile_tolerance` |
| `lines_clears_mismatch` | error | singles + 2·doubles + 3·triples + 4·tetrises ≠ lines played (± `lines_tolerance`) |
| `value_range` | error | impossible value (level > 255, lines > 9999, …) |
| `signal_lost` | warning ≥ 2 s, error ≥ 10 s | no usable picture during the game |
| `low_confidence` | warning ≥ 10 %, error ≥ 30 % | in-game frames with `confidence.overall` < 0.5 |
| `engine_restart` | error | the recognition engine was rebuilt mid-game |
| `level_mismatch` | warning | final level ≠ NES level progression for the lines played |
| `level_score_mismatch` | warning | scoring only fits with the level off by one — the LEVEL digit is probably misread (5↔6, 8↔9); the score itself is consistent |
| `score_unexplained` | warning | score gains that matched no scoring event |
| `plausibility_corrections` | warning | ≥ 20 rejected readings in the game |

## Self-healing

| Failure | Reaction |
|---|---|
| process crash / panic outside the engine | systemd restarts it (`Restart=always`, 3 s) |
| main loop wedged | systemd watchdog (`WatchdogSec=30`) kills and restarts it |
| engine panic on a frame | engine rebuilt in-process; game flagged `engine_restart` |
| ffmpeg exits / errors | restarted with exponential backoff (1 s → `backoff_max_s`) |
| capture delivers no frames | after `stall_timeout_s` ffmpeg is killed and restarted |
| capture stick unplugged | `status.capture = waiting_for_device`, resumes when it is back |
| no picture for `signal_lost_end_s` mid-game | game closed with `end_reason: signal_lost` |
| RFID reader unplugged / silent | reconnect loop; `status.rfid = offline` meanwhile |
| broker / network down | reconnect loop; results wait in the spool and are resent |
| station reboot mid-game | open game is closed as `shutdown` (clean stop) or lost (power cut); spool is resent after boot |

## Operations

```sh
systemctl status nestris-station
journalctl -u nestris-station -f                 # live log
journalctl -u nestris-station --since today | grep -E 'WARN|ERROR'
mosquitto_sub -v -t 'retroverse/nestris/#'       # everything the stations publish
ls /var/lib/nestris-station/recordings           # .ngf.gz per game
ls /var/lib/nestris-station/spool                # undelivered results (normally empty)
ls /var/lib/nestris-station/uploads              # recordings waiting for upload (normally empty)
```

Debug a single component with `RUST_LOG`, e.g.
`RUST_LOG=info,nestris_station::rfid=debug` in `/etc/nestris-station/env`.

## Testing without hardware

```sh
# A recorded game through sessions, cheat detection and MQTT:
nestris-station run -c station.toml --set rfid.enabled=false \
    --replay game.ngf.gz --fast

# A video file through the full capture pipeline:
nestris-station run -c station.toml --set rfid.enabled=false \
    --set capture.device=file:/path/capture.mp4 --set capture.pace_files=true

# Capture + engine performance on this machine (no broker needed), e.g.
# with a downscaled engine input; prints fps, drops, engine ms, CPU:
nestris-station bench -c station.toml --input /path/capture.mkv --fps 50     --start 250 --seconds 60 --set capture.lowres=1
# ... or the live device (stop the service first, it holds the device):
sudo systemctl stop nestris-station && nestris-station bench -c station.toml

# Real recorded game with an injected +10 000 (unit test, skipped when unset):
NESTRIS_SAMPLE_NGF=game.ngf cargo test -p nestris-station recorded_game
```
