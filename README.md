# nestris-core

High-performance Rust port of the [NestrisLTM_OCR](../NestrisLTM_OCR) NES-Tetris
capture/OCR engine. One sans-io engine crate powers three frontends: a native
CLI, a WebAssembly build with a browser GUI, and Android bindings.

Part of the **Retroverse** system: this project captures a NES-Tetris video
signal, extracts the full game state (score, lines, level, next piece, the
10×20 playfield, statistics, game state) via classical CV + template matching,
computes community-standard stats, and streams the result as JSON
(schema v4, wire-compatible with the Python implementation).

Highlights beyond the original port:

- **Continuous geometry tracking** for handheld/phone footage: the lock
  follows per-frame camera motion instead of holding a fixed quad
  (on by default in both GUIs; `--preset handheld` for the CLI).
- **Automatic game recording**: every detected game is saved as a
  NestrisChamps-format `.ngf.gz` (crash-safe on native hosts) and can be
  **replayed** in both GUIs and via `nestris replay` — see
  [docs/NGF.md](docs/NGF.md).
- **NestrisChamps-style statistics** in both GUIs: PACE/EFF/TRT/I-drought
  tiles, LINES & POINTS breakdowns, trend charts, piece distribution,
  board-state timeline, persistent high-score tables — see
  [docs/STATS.md](docs/STATS.md).
- **Fast**: rectification/NCC row-parallelism natively (~2.9 ms p50 per
  frame), SIMD wasm build, and a Web-Worker engine so the browser UI never
  janks.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — crate layout, the
  per-frame data flow, the sans-io background-recalibration protocol,
  determinism policy, and why the CV layer is hand-written.
- **[docs/VERIFICATION.md](docs/VERIFICATION.md)** — how equivalence with the
  Python implementation is proven layer by layer (golden tests, stage
  replays, the full-pipeline diff policy) and how to regenerate everything.
- **[docs/USAGE.md](docs/USAGE.md)** — CLI reference, engine configuration
  (TOML/JSON/YAML), desktop & web GUIs, live capture, Android bindings,
  output schema.
- **[docs/NGF.md](docs/NGF.md)** — the NGF recording format, the recording
  lifecycle, and replay.
- **[docs/STATION.md](docs/STATION.md)** — the Debian tournament station:
  install, configuration, MQTT contract, cheat detection, validation,
  self-healing.
- **[docs/STATS.md](docs/STATS.md)** — exact definitions of every computed
  statistic.

## Workspace

| Crate | Role |
|---|---|
| `nestris-vision` | Pure CV primitives (color, NCC, morphology, components, homography/RANSAC, warp). Zero I/O, wasm-clean; optional `parallel` row-parallelism. |
| `nestris-engine` | Layout, recognition, state fusion, stats, geometry lock + tracker, the per-frame `FrameProcessor`. Sans-io. |
| `nestris-ngf` | NGF (NestrisChamps Game Format) codec, game recorder, replay engine. Sans-io, wasm-clean. |
| `nestris-host` | Shared native glue: ffmpeg-pipe capture, output sinks, recalibration worker thread, crash-safe recording sink. |
| `nestris-cli` | Native binary: `run`/`replay`/`bench`/`verify`/`list-devices`. |
| `nestris-station` | Headless tournament-station daemon for Debian: self-healing v4l2 capture, ESP32 RFID player login, Select-cheat detection, per-game validation, MQTT with a durable spool, systemd/`.deb` packaging. |
| `nestris-gui-core` | GUI-agnostic desktop core shared by the frontends: pipeline worker thread, persisted settings, session PB store. |
| `nestris-gui` | Native desktop GUI (egui): previews, dashboard, stats window, transport, replay viewer, full settings dialog. |
| `nestris-qt-gui` | Native desktop GUI (Qt 6 + QML via cxx-qt): NestrisChamps-`classic_1080`-style dashboard shell. Excluded from the root workspace — building it needs a Qt SDK ([docs/USAGE.md](docs/USAGE.md)). |
| `nestris-wasm` | `wasm-bindgen` exports (binary snapshot boundary) for the browser GUI in `web/`. |
| `nestris-android` | UniFFI (Kotlin) bindings. |

Dependency direction is strict: `vision ← engine ← ngf ← gui-core ← {cli, gui, qt-gui, wasm}`
(`nestris-station` sits beside the CLI on `engine ← ngf ← host`).

## Quickstart

```sh
cargo test --workspace                 # unit + CV golden tests
cargo run --release -p nestris-cli -- run --input path\to\capture.mp4 --jsonl out.jsonl
```

`nestris run` needs `ffmpeg`/`ffprobe`. The Windows release zips bundle
them (no install needed); for source builds put them on `PATH` (Windows:
`winget install Gyan.FFmpeg`) or set `NESTRIS_FFMPEG`.

## Desktop GUI

```sh
cargo run --release -p nestris-gui
```

Open a video file, a DirectShow capture device, or an `.ngf` replay
(dialogs or drag & drop). Live previews (raw + lock overlay, canonical,
tracked field), a confidence-aware dashboard with a capture alarm, the 📊
stats window with persistent high scores, full transport (pause, speed,
frame stepping, seek with hover preview) plus keyboard shortcuts, automatic
`.ngf.gz` game recording, and a settings dialog for every engine knob
(persisted to `%APPDATA%\nestris-core\gui-settings.toml`).

There is also a **Qt 6 desktop GUI** (`crates/nestris-qt-gui`, cxx-qt +
QML) with the same features presented as an always-on
NestrisChamps-`classic_1080` stats dashboard; it needs a Qt SDK to build
(see [docs/USAGE.md](docs/USAGE.md)) and ships with the Qt runtime
bundled in [Releases](../../releases).

## Web GUI

```sh
tools/build-wasm.ps1                    # SIMD wasm build (after engine changes)
cd web && npm install && npm run dev    # → http://localhost:5173
```

Open the printed URL, then open a video file, an `.ngf` replay, the camera,
or a screen share (or drag & drop). The full engine runs in a Web Worker
with a binary snapshot boundary, so the page never janks; files and replays
get a transport bar, the statistics section mirrors the desktop stats
window, and finished games download automatically as `.ngf.gz`.

## Live capture

```sh
nestris list-devices
nestris run --input "dshow:YOUR CAPTURE DEVICE" --ws 127.0.0.1:8765      # Windows
nestris run --input "v4l2:/dev/v4l/by-id/...-video-index0" --ws 127.0.0.1:8765   # Linux
```

## Tournament station (Debian, headless)

```sh
cargo deb -p nestris-station && sudo apt install ./target/debian/nestris-station_*.deb
```

Starts at boot, captures from the USB stick, reads the player from the ESP32
RFID reader, detects the Select cheat and publishes live state and validated
results over MQTT — see [docs/STATION.md](docs/STATION.md).

## Verification against the Python oracle

Fixture videos are **not** copied into this repo — point
`NESTRIS_FIXTURES_DIR` at the Python repo's `fixtures/` directory. See
[docs/VERIFICATION.md](docs/VERIFICATION.md) for the full methodology.

## Status

All layers are implemented and verified against the Python implementation:

- CV primitives match OpenCV/numpy at documented tolerances (golden tests),
  including with the `parallel` row-splits enabled.
- Recognition replays Python-rectified frames at **100%** field parity,
  including every playfield cell.
- The state layers (fusion/plausibility/stats) reproduce Python's output
  **byte-exactly** over 18 000 replayed frames (15 fixtures).
- The full pipeline (own decode, own RANSAC, own warp) passes the verify
  policy on **all 15 fixtures**, most exact-class fields at 100.000% —
  every new engine behavior ships behind a default-off config flag so the
  verified default path stays byte-identical (see the divergence table in
  [docs/VERIFICATION.md](docs/VERIFICATION.md)).
- Native hot path: **~2.9 ms p50** per frame (~350 fps, row-parallel
  rectify/NCC) vs ~12.8 ms in Python; the wasm build is SIMD-enabled,
  ~630 KB (~270 KB gzipped).
- NGF recordings produced by the recorder parse correctly in the
  NestrisLTM reference importer; record → replay round trips are
  deterministic.
