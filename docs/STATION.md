# Tournament station (`nestris-station`)

`nestris-station` is the headless daemon for a Retroverse tournament
station: a small Debian PC with a USB capture stick on the NES and the
ESP32 RFID card reader on USB. It

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

Build on the station itself (or any Debian/Ubuntu machine of the same
architecture):

```sh
sudo apt install build-essential pkg-config ffmpeg curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
cargo install cargo-deb

git clone https://github.com/ErwinSpitaler/nestris-core && cd nestris-core
cargo deb -p nestris-station          # → target/debian/nestris-station_*.deb
sudo apt install ./target/debian/nestris-station_*.deb
```

The package installs `/usr/bin/nestris-station`, the systemd unit (enabled
at boot, not started), `/etc/nestris-station/station.toml` and
`/etc/nestris-station/env` (both kept on upgrades), and creates the
`nestris` service user (groups `video`, `dialout`). For MQTT over TLS build
with `cargo deb -p nestris-station --features tls`.

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

## Configuration

`/etc/nestris-station/station.toml` — the annotated template is
[`crates/nestris-station/packaging/station.example.toml`](../crates/nestris-station/packaging/station.example.toml).
Only `capture.device`, `rfid.port` and usually `mqtt.host` / `station.id`
need editing.

Precedence (later wins):

1. built-in defaults,
2. the config file (deep-merged: a partial `[engine.fusion]` table keeps
   every other default),
3. environment `NESTRIS_STATION__<SECTION>__<KEY>=value` (e.g. from
   `/etc/nestris-station/env`),
4. `--set section.key=value` on the command line.

Unknown keys are errors, so typos never pass silently. Secrets: put the MQTT
password in `mqtt.password_file` (mode 0600) or as
`NESTRIS_STATION__MQTT__PASSWORD` in the env file (mode 0640, `root:nestris`);
`check-config` masks it.

| Section | Key settings |
|---|---|
| `station` | `id` (topic + game-id prefix), `name`, `state_dir` (default: systemd `StateDirectory`, `/var/lib/nestris-station`) |
| `capture` | `device`, `input_format`, `width`/`height`/`fps`, `stall_timeout_s` (5), `backoff_max_s` (30) |
| `rfid` | `enabled`, `port`, `baud` (115200), `stale_after_s` (3), `player_grace_s` (60) |
| `mqtt` | `host`, `port`, `username`, `password_file`, `tls`/`ca_file`, `topic_prefix` (`retroverse/nestris`), `live_max_hz` (5), `status_interval_s` (10) |
| `recording` | `enabled`, `dir`, `keep_days` (30), `max_gb` (20) |
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
| `cmd` | subscribed | commands forwarded to the RFID reader |

**Durable** messages are written to the on-disk spool before publishing and
deleted only after the broker's PUBACK. They survive network outages, broker
restarts and station restarts, and are delivered **at least once** — the
host must deduplicate by `game_id` (+ topic).

### `status`

```json
{"state":"online","station":"station-1","name":"Station 1","version":"0.1.0",
 "capture":"ok","capture_detail":"1280x720","lock":"locked","game_state":"in_game",
 "rfid":"ok","game_id":"station-1-1790241008228","fps":60.0,"dropped_frames":0,
 "uptime_s":3605,"ts":"2026-09-24T09:10:08.228Z"}
```

`capture`: `ok`, `opening`, `waiting_for_device` (unplugged), `reconnecting`
(`capture_detail` has ffmpeg's error and the retry delay). `rfid`: `ok`,
`offline`, `disabled`. On a clean shutdown and as the broker's last will:
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
 "cheated":0,"confidence":0.95,"ts":"..."}
```

`game_id` is `null` between games (menus still update `game_state`).

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

Commands for the RFID reader's display, forwarded verbatim as one line:

```sh
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"highscore","value":"159867"}'
mosquitto_pub -t retroverse/nestris/station-1/cmd -m '{"type":"setname","value":"Erv"}'  # next card
```

Only `highscore` and `setname` are accepted (the reader's Wi-Fi `config`
command is deliberately not reachable over MQTT).

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

# Real recorded game with an injected +10 000 (unit test, skipped when unset):
NESTRIS_SAMPLE_NGF=game.ngf cargo test -p nestris-station recorded_game
```
