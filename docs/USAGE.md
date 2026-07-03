# Usage

## Prerequisites

- **Rust** (pinned by `rust-toolchain.toml`; rustup installs it on first build)
- **ffmpeg + ffprobe** for the native CLI — install via `winget install
  Gyan.FFmpeg` (Windows) or your package manager. The CLI finds them on
  `PATH`, in the winget links directory, or via the `NESTRIS_FFMPEG`
  environment variable (a directory containing the executables).
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
| `--config <toml>` | engine tuning (see below) |
| `--oracle-parity` | deterministic verification mode: inline geometry solves instead of the background thread |

### Live capture (Windows capture card / UVC device)

```sh
nestris list-devices                     # ffmpeg's DirectShow device list
nestris run --input "dshow:Game Capture HD60" --ws 127.0.0.1:8765
```

Live inputs are normalized to 1280×720 and timestamped with the wall clock.

### Benchmark

```sh
nestris bench --input fixture.mp4 --start 60 --frames 300
# ms/frame: p50=3.44 p90=5.57 ...  (p50 fps=290.7)
```

Measures the production hot path (geometry re-solves on the worker thread).

### Verify against the Python oracle

See [VERIFICATION.md](VERIFICATION.md).

## Engine configuration

`--config engine.toml` mirrors the Python `AppConfig` structure and defaults.
Everything is optional; omitted values use the defaults shown:

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
```

## Web GUI

```sh
# 1. Build the wasm package (repeat after engine changes):
wasm-pack build crates/nestris-wasm --target web --release

# 2. Run the dev server:
cd web
npm install
npm run dev            # → http://localhost:5173
```

In the page: **Open video…** (or drag & drop a file), **Camera** (point a
phone/webcam at a CRT), or **Screen capture** (share the window of an
emulator). The header shows the lock state and the processing rate; panes
show the raw source with the detected-playfield overlay, the rectified
canonical frame, and the tracked playfield rendered in the authentic NES
level palette; the side panel has the dashboard tiles and the event stream
(line clears, plausibility rejections, new-game boundaries).

Implementation notes:

- Exactly **one frame copy** crosses JS→WASM per frame: the RGBA
  `ImageData` bytes are written straight into the engine's linear memory.
- Geometry re-solves run in a **Web Worker** holding its own wasm instance,
  so the main thread never blocks; results are adopted under the same
  never-regress rule as everywhere else.
- `npm run build` produces a fully static `dist/` (508 KB wasm, ~220 KB
  gzipped) that any static host can serve.

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
