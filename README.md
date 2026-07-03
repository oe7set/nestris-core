# nestris-core

High-performance Rust port of the [NestrisLTM_OCR](../NestrisLTM_OCR) NES-Tetris
capture/OCR engine. One sans-io engine crate powers three frontends: a native
CLI, a WebAssembly build with a browser GUI, and Android bindings.

Part of the **Retroverse** system: this project captures a NES-Tetris video
signal, extracts the full game state (score, lines, level, next piece, the
10×20 playfield, statistics, game state) via classical CV + template matching,
computes community-standard stats, and streams the result as JSON
(schema v4, wire-compatible with the Python implementation).

## Workspace

| Crate | Role |
|---|---|
| `nestris-vision` | Pure CV primitives (color, NCC, morphology, components, homography/RANSAC, warp). Zero I/O, zero deps, wasm-clean. |
| `nestris-engine` | Layout, recognition, state fusion, stats, the per-frame `Processor`. Sans-io. |
| `nestris-cli` | Native binary: ffmpeg-pipe capture, JSONL/WebSocket sinks, `diff`/`bench`/`replay-*` verification commands. |
| `nestris-wasm` | `wasm-bindgen` exports for the browser GUI in `web/`. |
| `nestris-android` | UniFFI bindings stub. |

Dependency direction is strict: `vision ← engine ← {cli, wasm, android}`.

## Quickstart

```sh
cargo test --workspace                 # unit + CV golden tests
cargo run -p nestris-cli -- run --input path\to\capture.mp4 --jsonl out.jsonl
```

`nestris-cli run` needs `ffmpeg`/`ffprobe` on `PATH` (or set `NESTRIS_FFMPEG`).

## Verification against the Python oracle

The Python implementation is the behavioral oracle. Fixture videos are **not**
copied into this repo — point `NESTRIS_FIXTURES_DIR` at the Python repo's
`fixtures/` directory:

```powershell
$env:NESTRIS_FIXTURES_DIR = "D:\Projekte\Retroverse\NestrisLTM_OCR\fixtures"
```

- `tools/gen_cv_goldens.py` (run with the Python repo's venv) regenerates the
  committed OpenCV/numpy golden pairs under `testdata/cv/`.
- `NestrisLTM_OCR/tools/dump_stage_artifacts.py` dumps per-fixture stage
  artifacts (canonical frames, raw readings, lock trace, output JSONL) into
  `testdata/stages/` (gitignored, regenerable).
- `nestris-cli replay-readings` / `replay-canonical` / `diff` compare the Rust
  stages against those artifacts with a per-field tolerance policy.

## Web GUI

```sh
cd web && npm install && npm run dev   # rebuild wasm first when the engine changed:
wasm-pack build crates/nestris-wasm --target web --release
```

Open the printed URL, then drop a video file / open the camera / share a
screen. The recalibration solver runs in a Web Worker; the engine itself
stays on the main thread with one RGBA copy per frame.

## Live capture (Windows)

```sh
nestris list-devices
nestris run --input "dshow:YOUR CAPTURE DEVICE" --ws 127.0.0.1:8765
```

## Status

Engine, CLI, WASM + web GUI, and the Android UniFFI stub are implemented and
verified against the Python implementation: recognition/state layers replay
byte-exact (18 000 frames across 15 fixtures), the full pipeline scores 100%
on all exact-class fields on the reference fixture, and the native hot path
runs at ~3.4 ms p50 per frame (Python: ~12.8 ms).
