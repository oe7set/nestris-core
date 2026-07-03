# nestris-core

High-performance Rust port of the [NestrisLTM_OCR](../NestrisLTM_OCR) NES-Tetris
capture/OCR engine. One sans-io engine crate powers three frontends: a native
CLI, a WebAssembly build with a browser GUI, and Android bindings.

Part of the **Retroverse** system: this project captures a NES-Tetris video
signal, extracts the full game state (score, lines, level, next piece, the
10×20 playfield, statistics, game state) via classical CV + template matching,
computes community-standard stats, and streams the result as JSON
(schema v4, wire-compatible with the Python implementation).

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — crate layout, the
  per-frame data flow, the sans-io background-recalibration protocol,
  determinism policy, and why the CV layer is hand-written.
- **[docs/VERIFICATION.md](docs/VERIFICATION.md)** — how equivalence with the
  Python implementation is proven layer by layer (golden tests, stage
  replays, the full-pipeline diff policy) and how to regenerate everything.
- **[docs/USAGE.md](docs/USAGE.md)** — CLI reference, engine configuration
  (TOML), web GUI, live capture, Android bindings, output schema.

## Workspace

| Crate | Role |
|---|---|
| `nestris-vision` | Pure CV primitives (color, NCC, morphology, components, homography/RANSAC, warp). Zero I/O, zero deps, wasm-clean. |
| `nestris-engine` | Layout, recognition, state fusion, stats, geometry lock, the per-frame `FrameProcessor`. Sans-io. |
| `nestris-host` | Shared native glue: ffmpeg-pipe capture, output sinks, recalibration worker thread. |
| `nestris-cli` | Native binary: `run`/`bench`/`verify`/`list-devices`. |
| `nestris-gui` | Native desktop GUI (egui): previews, dashboard, transport bar, full settings dialog. |
| `nestris-wasm` | `wasm-bindgen` exports for the browser GUI in `web/`. |
| `nestris-android` | UniFFI (Kotlin) bindings. |

Dependency direction is strict: `vision ← engine ← {cli, wasm, android}`.

## Quickstart

```sh
cargo test --workspace                 # unit + CV golden tests
cargo run --release -p nestris-cli -- run --input path\to\capture.mp4 --jsonl out.jsonl
```

`nestris run` needs `ffmpeg`/`ffprobe` on `PATH` (Windows:
`winget install Gyan.FFmpeg`, or set `NESTRIS_FFMPEG`).

## Desktop GUI

```sh
cargo run --release -p nestris-gui
```

Open a video file or a DirectShow capture device, watch the live previews
(raw + lock overlay, canonical, tracked field), scrub/pause/speed files via
the transport bar, and tune every engine knob in the settings dialog
(persisted to `%APPDATA%\nestris-core\gui-settings.toml`).

## Web GUI

```sh
wasm-pack build crates/nestris-wasm --target web --release   # after engine changes
cd web && npm install && npm run dev                          # → http://localhost:5173
```

Open the printed URL, then open a video file (or drag & drop), the camera, or
a screen share. The recalibration solver runs in a Web Worker; the engine
stays on the main thread with one RGBA copy per frame.

## Live capture (Windows)

```sh
nestris list-devices
nestris run --input "dshow:YOUR CAPTURE DEVICE" --ws 127.0.0.1:8765
```

## Verification against the Python oracle

Fixture videos are **not** copied into this repo — point
`NESTRIS_FIXTURES_DIR` at the Python repo's `fixtures/` directory. See
[docs/VERIFICATION.md](docs/VERIFICATION.md) for the full methodology.

## Status

All layers are implemented and verified against the Python implementation:

- CV primitives match OpenCV/numpy at documented tolerances (golden tests).
- Recognition replays Python-rectified frames at **100%** field parity,
  including every playfield cell.
- The state layers (fusion/plausibility/stats) reproduce Python's output
  **byte-exactly** over 18 000 replayed frames (15 fixtures).
- The full pipeline (own decode, own RANSAC, own warp) scores **100.000%**
  on all exact-class fields on the reference fixture.
- Native hot path: **~3.4 ms p50** per frame (~290 fps) vs ~12.8 ms in
  Python; the wasm build is 508 KB (~220 KB gzipped).
