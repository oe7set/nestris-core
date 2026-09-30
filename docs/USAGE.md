# Usage

## Prerequisites

- **Rust** (pinned by `rust-toolchain.toml`; rustup installs it on first build)
- **ffmpeg + ffprobe** for the native CLI and GUIs. The **Windows release
  zips bundle both** (in the `ffmpeg/` directory next to each executable) —
  nothing to install. For source builds and on Linux/macOS install them via
  `winget install Gyan.FFmpeg` (Windows) or your package manager. Search
  order: the `NESTRIS_FFMPEG` environment variable (a directory containing
  the executables, always wins), bundled binaries next to the executable,
  the winget links directory, then `PATH`.
- **Node 20+** and **wasm-pack** for the web GUI.

## Native CLI (`nestris`)

Build once:

```sh
cargo build --release -p nestris-cli
# binary: target/release/nestris(.exe)
```

### Process a video file

```sh
nestris run --input path\to\capture.mp4 --jsonl out.jsonl
```

Every processed frame is written as one schema-v4 JSON line. Add sinks as
needed — they can be combined:

| Flag | Effect |
|---|---|
| `--jsonl <path>` | per-frame JSONL file |
| `--ws <addr>` | WebSocket broadcast server, e.g. `--ws 127.0.0.1:8765`; every connected client receives each frame as a text message (slow clients drop oldest frames, never stalling the engine) |
| `--ndjson` | also print NDJSON to stdout (default sink when nothing else is set) |
| `--start <s>` / `--frames <n>` | window into the file |
| `--config <file>` | engine tuning — `.toml`, `.json`, or `.yaml`/`.yml` by extension (see below) |
| `--preset handheld` | enable continuous geometry tracking for shaky phone footage |
| `--set path.to.field=value` | override a single config field (repeatable, applied after file + preset, field names validated), e.g. `--set fusion.vote_window=7` |
| `--no-record` | disable the automatic per-game NGF recording (on by default) |
| `--record-dir <dir>` | recording directory (default `Documents\nestris-recordings`) |
| `--record-raw` | save plain `.ngf` instead of gzipped `.ngf.gz` |
| `--record-partial` | also record games already in progress when capture starts |
| `--oracle-parity` | deterministic verification mode: inline geometry solves instead of the background thread |

### Replay a recording

```sh
nestris replay recordings\20260703-193000_g001.ngf.gz --jsonl replay.jsonl
nestris replay game.ngf --speed 1.0 --ws 127.0.0.1:8765   # real-time re-emit
```

Re-derives full schema-v4 frames (statistics included) from a recorded
game — see [NGF.md](NGF.md) for the format, recording lifecycle, and the
GUI replay viewers.

### Live capture (capture card / UVC device)

```sh
# Windows (DirectShow)
nestris list-devices                     # ffmpeg's DirectShow device list
nestris run --input "dshow:Game Capture HD60" --ws 127.0.0.1:8765

# Linux (Video4Linux2)
nestris list-devices                     # /dev/v4l/by-id/* and /dev/video*
nestris run --input "v4l2:/dev/v4l/by-id/usb-...-video-index0" --ws 127.0.0.1:8765
```

Live inputs are normalized to 1280×720 and timestamped with the wall clock.
For an unattended tournament station (auto-start, reconnects, RFID player
login, cheat detection, MQTT) use the `nestris-station` daemon instead —
see [STATION.md](STATION.md).

### Benchmark

```sh
nestris bench --input fixture.mp4 --start 60 --frames 300
# ms/frame: p50=3.44 p90=5.57 ...  (p50 fps=290.7)

nestris bench --input fixture.mp4 --acquire 5
# run 0: locked after 4 frames (0.31s) ...
# unlocked pipeline ms/frame: p50=... p99=...
# time to lock: mean=...s
```

The default mode measures the production hot path (geometry re-solves on
the worker thread). `--acquire N` performs N cold starts with the GUI
acquisition config (background solver + 640-wide candidate detection) and
reports time-to-lock plus the per-frame latency while unlocked — the
number that decides whether a live preview stutters during acquisition.

### Verify against the Python oracle

See [VERIFICATION.md](VERIFICATION.md).

## Engine configuration

`--config` accepts **TOML, JSON, or YAML** (dispatched by file extension);
the structure mirrors the Python `AppConfig` and adds the `tracking` and
`output` groups. Everything is optional; omitted values use the defaults
shown (TOML reference):

```toml
region = "NTSC"                    # advisory tag: "NTSC" | "PAL"

[calibration]
revalidate_every_n = 3             # inline re-solve cadence (oracle-parity mode)
acquire_threshold = 0.55           # confidence to (re)acquire a lock
drift_threshold = 0.40             # below = drifting
lost_frames = 12                   # weak frames before the lock is dropped
acquire_frames = 2                 # good frames to confirm acquisition
smooth_alpha = 0.5                 # geometry EMA factor
undistort = "auto"                 # "auto" | "off"  (barrel correction)
adopt_margin = 0.05                # re-solve must beat current lock by this
background_recalibration = true    # host-driven solve protocol on/off
menu_drift_hold = true             # keep the lock through menus/pause/curtain
background_acquisition = false     # acquisition on the background solver too
                                   # (GUIs/web default ON: smooth preview while
                                   # searching; needs background_recalibration)
acquire_downscale_width = 0        # candidate detection at this width, 0=full
                                   # res (GUIs/web default 640; labels/RANSAC/
                                   # validation always run at full resolution)

[fusion]
vote_window = 5                    # frames in the majority-vote window
confidence_decay = 0.9             # per-frame decay while holding a value
min_report_confidence = 0.4        # below = field reported as null
enforce_monotonic = true           # score/lines/level never decrease in-game
new_game_menu_frames = 10          # menu frames required to arm a new game

[plausibility]
enabled = true
max_score_jump = 60000             # max single-step score increase
max_lines_step = 4                 # max lines per contiguous step (a tetris)
max_lines_skip = 8                 # max lines across a read gap
level_tolerance = 1                # allowed |level - expected_from_lines|
confirm_frames = 6                 # persistent rejected value self-heals after

[recognition]
score_base = "auto"                # "auto" | "dec" | "hex" (modded ROMs)
score_base_latch_frames = 60       # auto-base latches after N agreeing frames
read_statistics = true
statistics_every_n = 6             # STATISTICS rail read cadence
read_current_piece = true
freeze_on_clear_animation = true   # hold the grid during line-clear frames
playfield_stabilizer = true        # per-cell Schmitt hysteresis
# Color-discrimination refinements (default off = oracle-exact classic path):
color_voting = false               # per-cell temporal color voting (5 frames)
color_hue_weight = 0.0             # 0..1: blend a hue-angle term into accent
                                   # assignment (exposure-invariant)
adaptive_ambiguity = false         # tighten the ambiguity ratio on palettes
                                   # with close accent pairs
white_balance = false              # per-channel gains from white cells
piece_color_uniform = false        # force the falling piece to one color
# Clear-animation / game-over robustness (default off):
clear_prediction = false           # predict the post-clear board, validate it,
                                   # detect the game-over curtain, and block
                                   # false animation entry while paused
level_hint_on_clear = false        # next level's palette right after a
                                   # level-crossing clear

[tracking]                         # continuous geometry micro-tracking for
enabled = false                    # handheld/shaky sources (GUIs default ON;
                                   # CLI via --preset handheld). Idles on
                                   # stable capture-card sources (deadband).
search_radius_px = 8               # HUD-label search window (canonical px)
min_label_score = 0.4              # NCC floor for a label match
deadband_px = 0.35                 # corrections below this are ignored
damping = 0.6                      # blend factor toward the fitted correction
max_correction_px = 12.0           # larger label offsets = mismatch, not motion
miss_escalate = 4                  # tracker misses before the drift path
motion_adopt_threshold_px = 1.0    # sustained motion above this relaxes the
                                   # never-regress solve adoption margin
drift_solve_interval_s = 0.15      # fast background-solve pacing under motion

[output]
extended_stats = false             # attach the stats_ext block to every frame
                                   # (see docs/STATS.md; GUIs always show it)
```

## Desktop GUI (`nestris-gui`)

```sh
cargo run --release -p nestris-gui
```

The native desktop app (egui — pure Rust, no Qt SDK required; it fills the
role of the Python PySide6 GUI) drives the same engine as the CLI:

- **Source picker** — `Open video…` / `Open replay…` file dialogs, drag &
  drop (videos and `.ngf` replays alike), or refresh the DirectShow device
  list with `Devices ⟳` and pick a capture card / webcam.
- **Previews** — the raw source with the detected-playfield overlay, the
  rectified 256×240 canonical frame, and the tracked field rendered in the
  authentic NES level palette with the falling piece on top. The layout
  wraps on narrow windows; the side panel is resizable.
- **Dashboard + events** — score/lines/level/next, pieces, tetris rate, PPS,
  burn, drought, clear distribution. Values gray out when their fused
  confidence drops below 0.4, and a **⚠ CHECK CAPTURE** alarm appears after
  2 s of lock loss or low overall confidence. The event stream shows line
  clears (TETRIS in gold), plausibility rejections, and new-game boundaries.
- **📊 Stats window** — the NestrisChamps-style dashboard: SCORE / PACE /
  LINES / LEVEL / EFF / BRN / TRT / I-DRT tiles, LINES and POINTS
  breakdowns, the TRT trend chart, per-piece distribution with drought
  bars, the HEIGHT & STATE timeline, and the persistent TODAY/OVERALL
  high-score tables ([STATS.md](STATS.md)).
- **Transport bar** (files + replays) — play/pause, frame stepping
  (`|◀` / `▶|`), playback speed (0.25×–4× or Max), and a seek slider with a
  hover time preview. Video seeks reopen the ffmpeg pipe (a spinner shows
  while buffering); replay seeks are instant.
- **Keyboard shortcuts** — `Space` pause, `←`/`→` seek ∓5 s, `,`/`.` frame
  step, `↑`/`↓` speed, `R` reset lock, `O` open video, `F11` fullscreen.
- **Recording** — every detected game is saved as `.ngf.gz` by default
  (`● REC` shows while a game is being recorded; a toast confirms each
  save). Toggle and output directory live in Settings → Output.
- **Settings dialog** (⚙) — every `EngineConfig` knob grouped like the TOML
  reference above (including Tracking, on by default in the GUI), plus
  recording and output sinks (WebSocket broadcast, JSONL file).
  `Apply & save` persists to `%APPDATA%\nestris-core\gui-settings.toml` and
  restarts the pipeline at the current position with the new configuration.

## Qt desktop GUI (`nestris-qt-gui`)

The second desktop frontend: Qt 6 + QML (via cxx-qt) with a
NestrisChamps-`classic_1080`-style dashboard as the whole app shell —
the stats layout is always visible and the big top-left zone switches
between the drop-zone/source picker and the live preview (RAW/CANON
chips). It shares `nestris-gui-core` with the egui GUI, so features,
recording behavior, and the PB store (`session_pbs.json`) are identical;
only the settings file differs (`qt-gui-settings.toml`).

The crate is **excluded from the root workspace** so `cargo build
--workspace` never needs a Qt SDK. Building it requires Qt 6.8+
(`qmake` on PATH or the `QMAKE` env var):

```powershell
# Windows (Qt via the online installer or `pip install aqtinstall`):
#   aqt install-qt windows desktop 6.8.3 win64_msvc2022_64 -O C:\Qt
$env:QMAKE = "C:\Qt\6.8.3\msvc2022_64\bin\qmake.exe"
cd crates/nestris-qt-gui
cargo build --release
# Running from a dev tree needs the Qt DLLs on PATH:
$env:PATH = "C:\Qt\6.8.3\msvc2022_64\bin;$env:PATH"
cargo run --release -- "video.mp4" --start 30   # optional auto-open source
```

Release zips bundle the Qt runtime (windeployqt / macdeployqt /
AppImage), so end users need no Qt install. See
[RELEASING.md](RELEASING.md).

## Web GUI

```sh
# 1. Build the wasm package (repeat after engine changes; enables SIMD):
tools/build-wasm.ps1        # or: wasm-pack build crates/nestris-wasm --target web --release

# 2. Run the dev server:
cd web
npm install
npm run dev            # → http://localhost:5173
npm test               # snapshot-codec unit tests (vitest)
```

In the page: **Open video…**, **Open replay…** (`.ngf` / `.ngf.gz`),
**Camera** (point a phone/webcam at a CRT), or **Screen capture** (share
the window of an emulator) — or drag & drop any of them. The header shows
the lock state, the processing rate, a perf HUD (worker engine time p50/p95
and dropped frames), the recording toggle (`● REC`, on by default —
finished games download as `.ngf.gz`), and the settings button.

Files and replays get a **transport bar**: play/pause, frame stepping,
speed, a seek bar with buffered ranges and a hover time bubble, and a loop
toggle. Keyboard shortcuts mirror the desktop app (`Space`, `,`/`.`,
`↑`/`↓`, `R`, `O`). The **Statistics** section is the NestrisChamps-style
dashboard (tiles, LINES/POINTS breakdowns, TRT trend, piece distribution,
HEIGHT & STATE, TODAY/OVERALL high scores in `localStorage` —
[STATS.md](STATS.md)). The layout collapses to a single column under
900 px.

The **⚙ Settings** dialog exposes the same calibration / fusion /
plausibility / recognition / tracking / output knobs as the TOML reference,
persisted in `localStorage`. Web defaults enable continuous tracking and
extended stats. `Apply & save` rebuilds the wasm engine (the lock
re-acquires within a couple of frames).

Implementation notes:

- The **full engine runs in a Web Worker**: the page thread only captures
  `ImageBitmap`s (transferred, newest-wins back-pressure) and paints the
  UI, so heavy frames never jank the page.
- Results cross the JS boundary as **compact binary snapshots** written
  into a persistent wasm buffer (no per-frame JSON); the decoder is tested
  against the actual Rust encoder output (`npm test`).
- Geometry re-solves run in a nested Web Worker with its own wasm instance,
  paced faster automatically while the geometry tracker reports motion.
- The wasm package is built with **SIMD (simd128)** and `wasm-opt -O4`
  (`tools/build-wasm.ps1`; a `-Compat` switch produces a non-SIMD fallback
  package for very old browsers).
- `npm run build` produces a fully static `dist/` that any host can serve.

## Android bindings

`nestris-android` exposes the engine through [UniFFI](https://mozilla.github.io/uniffi-rs/)
proc-macros. The intended Kotlin shape:

```kotlin
val engine = NestrisEngine("")                      // config JSON, "" = defaults
// camera / MediaProjection frame loop:
val json = engine.pushFrame(rgbaBytes, w, h, tsSeconds)
// on a background coroutine, when engine.wantsBackgroundSolve():
engine.solveFrame(rgbaSnapshot, w, h, seed)
```

Generate bindings + build the ARM library with
[cargo-ndk](https://github.com/bbqsrc/cargo-ndk):

```sh
cargo install cargo-ndk
cargo ndk -t arm64-v8a build --release -p nestris-android
cargo run -p uniffi-bindgen ... # or uniffi-bindgen-cli against the cdylib
```

The full Android app (camera integration, NV21 fast path) is future work;
the interface, threading model, and CI target-check are in place.

## Output schema

The JSON contract is **schema v4**, field-for-field identical to the Python
implementation — see `crates/nestris-engine/src/output.rs` (Rust) or
`src/nestris_ocr/output/schema.py` (Python) for the authoritative field
list. Highlights per frame: `game_state`, `fields` (score/lines/level/next/
current piece + cells/10×20 playfield with color ids/STATISTICS), `stats`
(pps, tetris_rate, burn, drought, clears, active_seconds), per-field
`confidence`, and `events` (line clears, plausibility rejections/corrections,
new-game boundaries).
