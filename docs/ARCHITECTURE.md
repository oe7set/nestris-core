# Architecture

`nestris-core` is a Rust port of the Python [`NestrisLTM_OCR`](../../NestrisLTM_OCR)
engine. It watches a NES-Tetris video signal (file, capture card, camera,
screen share), extracts the full game state with classical computer vision —
no ML — and streams it as JSON. One engine crate powers three frontends:
a native CLI, a WebAssembly build with a browser GUI, and Android bindings.

```
             ┌─────────────────────────────────────────────┐
             │                nestris-vision               │
             │  pure CV primitives, zero deps, wasm-clean  │
             └──────────────────────┬──────────────────────┘
                                    │
             ┌──────────────────────▼──────────────────────┐
             │                nestris-engine               │
             │   layout · recognition · state · stats ·    │
             │   geometry lock · processor   (sans-io)     │
             └──────┬───────────────┬───────────────┬──────┘
                    │               │               │
          ┌─────────▼────┐  ┌───────▼──────┐  ┌─────▼─────────┐
          │ nestris-cli  │  │ nestris-wasm │  │nestris-android│
          │ ffmpeg pipe, │  │ browser GUI  │  │ UniFFI/Kotlin │
          │ JSONL, WS    │  │ (web/)       │  │ bindings      │
          └──────────────┘  └──────────────┘  └───────────────┘
```

The dependency direction is strict and acyclic: `vision ← engine ← frontends`.
The headless tournament daemon `nestris-station` is one more native
frontend (see [STATION.md](STATION.md)).
The engine performs **no I/O whatsoever** — no threads, no sockets, no files,
no clocks. That single property is what lets the identical code run natively,
in a Web Worker, and on Android.

The two desktop GUIs (egui `nestris-gui` and Qt/QML `nestris-qt-gui`)
share one GUI-agnostic layer, **`nestris-gui-core`**: the pipeline worker
thread (decode → process → recalibrate → publish, plus transport control
and NGF recording), the persisted `GuiSettings` (each frontend passes its
own TOML file name), the session PB store (`session_pbs.json`, shared, so
games recorded in either GUI appear in both), event-row formatting, and
game-end detection. A frontend is only rendering plus a message pump over
`worker::{Cmd, WorkerMsg, GuiUpdate}`. The Qt crate is excluded from the
root workspace so building the workspace never requires a Qt SDK; it
consumes the same crates by path and paints frames through
QQuickPaintedItem subclasses implemented in Rust (cxx-qt).

## Per-frame data flow

`FrameProcessor::process(&mut self, &Frame) -> OutputFrame` is the one
synchronous entry point. For each BGR frame:

1. **Calibration lock** (`geometry_cal/lock.rs`) — a state machine
   (`UNLOCKED → ACQUIRING → LOCKED ⇄ DRIFT → LOST`) that finds and tracks the
   NES picture inside the source frame. While unlocked it runs a full
   geometry solve every frame; while locked it only runs a cheap per-frame
   sanity check (playfield dark-fraction) and defers expensive re-solves to
   the background protocol (below). Geometry updates are EMA-smoothed to
   absorb camera shake; a jump ≥ 24 px replaces the geometry outright.

   With `tracking.enabled` (the GUI default, for handheld sources), a
   **continuous micro-tracker** (`geometry_cal/tracker.rs`) additionally
   re-matches the four HUD labels plus playfield-edge probes on every
   locked frame, fits a damped similarity correction with a velocity
   feed-forward term, and composes it onto the homography — so the next
   frame is rectified where the camera actually points. A deadband keeps
   stable capture-card sources bit-identical (no rectifier rebuild); misses
   and sustained motion raise a solve-urgency hint that speeds up the
   background solves and relaxes the never-regress adoption margin.

2. **Rectification** (`geometry_cal/calibration.rs::Rectifier`) — the source
   frame is warped onto the canonical 256×240 NES raster. On every adopted
   geometry the rectifier precomputes a **sampling map**: for each of the
   61 440 destination pixels, the source tap position and bilinear weights
   (with the optional barrel-undistortion composed in). Per frame this makes
   rectification a single gather pass.

3. **Screen classification** (`state/screen.rs`) — cheap, explainable image
   signatures decide `title / type_select / level_select / in_game / paused /
   game_over / highscore_entry / no_signal`. Two evidence paths are fused:
   menu signatures on a ~160×120 subsample of the *raw* frame (so menus stay
   reachable while the geometry lock is held between games), and gameplay
   signatures on the canonical frame (HUD label re-match, playfield
   structure, both pause styles, the game-over curtain).

4. **Recognition** (`recognition/`) — only on gameplay frames:
   - `digits.rs` — SCORE/LINES/LEVEL by normalized-correlation template
     matching of 8×8 digit glyphs, with ±2 px re-centering and an
     auto/dec/hex score-base latch for modded ROMs.
   - `next_piece.rs` — the NEXT box is Otsu-binarized, cleaned, cropped to
     the piece bounding box, resampled onto a grid sized to the piece, and
     classified by rotation-tolerant footprint matching. Template-free and
     color-invariant.
   - `playfield.rs` — the 10×20 grid. Occupancy is *relative* contrast
     (cell center vs an 8-sample median of its own border) plus a luma-gated
     chroma assist for dark saturated blocks; the margin adapts to frame
     brightness. Filled cells get one of three stable color ids (white /
     accent A / accent B) by nearest-CIELAB matching against the level's real
     palette targets, exposure-scaled; without a known level a deterministic
     2-means fallback clusters the accents.
   - `stabilizer.rs` — a per-cell Schmitt trigger: cells hovering on the
     occupancy boundary keep their previous state, killing flicker with zero
     lag for genuine changes.
   - `clear_anim.rs` — detects the line-clear animation (row retraction,
     tetris flash) so those garbage frames are withheld from fusion.
   - `current_piece.rs` — the falling piece, separated from the settled stack
     by temporal subtraction (preferred) or per-column surface analysis.
   - `statistics.rs` — the seven STATISTICS counts, read every 6th frame.

5. **Temporal fusion** (`state/fusion.rs`) — the robustness core. Per-field
   confidence-weighted majority voting over a sliding window, monotonic
   guards with a challenge/override recovery (a persistent lower reading
   eventually wins, a one-frame glitch never does), last-good holding with
   confidence decay, a full freeze while PAUSED, and the single authoritative
   new-game boundary signal.

6. **Plausibility** (`state/plausibility.rs`) — NES-rules validation: score
   never decreases and can't jump more than a cap, lines rise by 1–4 per
   clear, level must stay consistent with the line count given the inferred
   start level. Rejected-but-persistent values self-heal after N frames.

7. **Stats** (`stats.rs`) — community metrics, O(1) per frame: line-clear
   attribution cross-checked against the score delta (the NES base scores
   40/100/300/1200 × (level+1) disambiguate merged clears), tetris rate,
   burn, drought, piece count reconciled against the STATISTICS rail, and
   pace metrics on an active-play clock (pauses and menus don't dilute PPS).
   An extended block (`stats_ext.rs`, [STATS.md](STATS.md)) adds the
   dashboard metrics — points breakdown, EFF, PACE, per-piece distribution
   and droughts, board-shape flags, TRT/height time series — attached to
   the wire format only when `output.extended_stats` is on.

8. **Output** (`output.rs`) — everything is assembled into an `OutputFrame`
   and serialized as **schema v4** JSON, field-for-field identical to the
   Python implementation's wire format (`Option`s serialize as `null`, enum
   values and key order match pydantic's output).

## Recording & replay (`nestris-ngf`)

A separate sans-io, wasm-clean crate owns the NGF (NestrisChamps Game
Format) surface: the bit-exact v3 encoder / v1–v3 decoder, the
`GameRecorder` (one `.ngf.gz` per detected game, crash-safe `.part`
streaming on native hosts), and the `ReplayEngine` that re-derives full
output frames — statistics included — from a recording via the same
`StatsEngine` as live analysis. See [NGF.md](NGF.md). Hosts wire it up:
the CLI and desktop GUI record to `Documents\nestris-recordings`; the
browser records in memory and downloads finished games.

## Integrity checks (`integrity/`)

`nestris-engine/src/integrity/` holds two per-game consumers of the output
stream: the `CheatDetector` (score gains no legal NES scoring event
explains, counted as Select-cheat inputs) and the `GameValidator` (an
end-of-game report: partial game, score reconciliation, lines vs. clears,
level progression, confidence, lost signal). Both are sans-io and
deterministic, and neither is called from `FrameProcessor::process`, so the
verified schema-v4 path is untouched; hosts run them next to the processor.

## Tournament station (`nestris-station`)

The headless Debian daemon composes the native pieces for an unattended
station: `nestris-host`'s `CaptureSupervisor` (ffmpeg on its own thread with
stall detection, restart backoff, and waiting for a re-plugged device), the
processor with the background-recalibration thread, a session layer that
turns output frames into games (start, player from the ESP32 RFID reader,
cheat events, validated end), the NGF recorder, and an MQTT link with an
on-disk spool for must-deliver results. systemd provides auto-start,
restarts and a watchdog. See [STATION.md](STATION.md).

## The web architecture (worker + binary snapshots)

The browser runs the **entire engine in a dedicated Web Worker** that owns
the wasm instance and spawns the recalibration worker itself. The page
thread captures `ImageBitmap`s (transferred, newest-wins back-pressure)
and paints the UI. Per-frame results cross the JS boundary as **versioned
little-endian binary snapshots** written into a persistent wasm buffer
(`nestris-wasm/src/snapshot.rs` ↔ `web/src/snapshot.ts`, cross-tested
against the actual Rust encoder output) — no per-frame JSON, no per-frame
`Vec` returns. The wasm package is built with SIMD (`simd128`) and
`wasm-opt -O4` via `tools/build-wasm.ps1`.

## The background-recalibration protocol (sans-io)

The Python engine runs periodic geometry re-solves on a daemon thread. The
Rust engine replaces the thread with a **protocol**, so the same code works
on native threads, Web Workers, and Android coroutines:

```
loop per frame:
    output = processor.process(frame)
    if processor.lock().wants_background_solve():        # lock is tracking,
        snapshot = frame.clone()                         # gameplay frame
        # ... run estimate_geometry(snapshot) ANYWHERE ...
        processor.lock().offer_solution(result)          # adopted next frame
                                                         # only if better
```

Adoption follows a never-regress hysteresis: a candidate must clear the
drift floor *and* beat the current confidence by a margin. Hosts pace the
solves (≥ 0.5 s apart, newest frame wins):

- **CLI** — `recalib_thread.rs`, a `std::thread` worker.
- **Web** — `web/src/worker/recalib.ts`, a Web Worker holding its own wasm
  instance (`Solver`), fed transferable frame copies.
- **Android** — `solve_frame()` called from a Kotlin coroutine.

The CLI's `--oracle-parity` flag disables the protocol and runs solves
inline on a fixed cadence instead — the deterministic mode used for
verification against the Python oracle.

## Determinism

- All randomness (RANSAC sampling, probabilistic Hough) comes from an
  in-crate PCG32 seeded via `splitmix64(frame.seq)`. Runs are bit-identical
  across platforms and repeatable from a frame number.
- No `HashMap` iteration in any output-affecting path; Python `dict`
  insertion-order semantics (the STATISTICS map) are replicated with ordered
  vectors and `serde_json`'s `preserve_order` feature.
- Candidate orderings, tie-breaks, and float accumulation orders mirror the
  Python/numpy implementations (documented at each site).
- The native `parallel` feature (rayon) splits only **independent output
  rows** of the warp gather and NCC response — bit-exact by construction
  and proven by the golden tests and the full oracle verify running with
  the feature enabled. The `estimate_geometry` candidate loop shares one
  sequential PCG32 and must never be parallelized.
- New engine behaviors (tracking, color refinements, clear prediction,
  extended output) all sit behind **default-off config flags**, so the
  default path stays byte-identical to the verified oracle port.

## Why the CV layer is hand-written

`nestris-vision` reimplements exactly the OpenCV/numpy routines the engine
uses — nothing more. This is deliberate:

- **WASM** — OpenCV builds for the browser are ~8 MB; this whole engine is
  0.5 MB, and the pure-Rust primitives compile to wasm untouched.
- **Exactness** — every threshold in the engine was tuned against OpenCV's
  numeric behavior. The primitives replicate OpenCV's *fixed-point
  implementations* (not the idealized formulas), verified by golden tests
  (see [VERIFICATION.md](VERIFICATION.md)). Notable findings baked into the
  code: `warpPerspective` silently substitutes INTER_LINEAR for INTER_AREA;
  OpenCV 4.x BGR→GRAY uses the 15-bit coefficient set (not the widely-cited
  14-bit one); `resize INTER_AREA` is three different algorithms depending
  on the scale direction; `getStructuringElement` ellipse rasterization is
  quirky enough that the masks are hard-coded from captured output.
- **Pixel model** — plain `Vec<u8>` interleaved buffers with a thin `Image`
  type, no ndarray. Every hot routine is a hand-written loop over bytes,
  which is what OpenCV-exact integer arithmetic needs anyway, and it keeps
  the wasm binary small.

## Frame input contract

Hosts hand the engine `Frame { image: Image /* BGR8 */, seq, ts,
discontinuity }`. `Frame::from_rgba` converts browser/Android RGBA in place.
`discontinuity = true` (a seek, a source switch) resets temporal tracking
but deliberately keeps the geometry lock — a video's geometry is constant,
so re-acquiring on every scrub would waste it.

## Performance

Measured with `nestris bench` (production configuration, solve off-thread),
mid-game on the reference fixture, on the development machine:

| | per frame (p50) | throughput |
|---|---|---|
| Python engine | ~12.8 ms | ~78 fps |
| Rust engine (scalar) | ~3.6 ms | ~280 fps |
| Rust engine (`parallel` row splits) | **~2.9 ms** | **~350 fps** |

The `parallel` feature (on by default for the native binaries) runs the
rectification gather and NCC response rows on the rayon pool; p99 drops
from ~6.2 ms to ~4.6 ms. The continuous tracker adds ≈ 0.1 ms on stable
sources (deadband path) and ≈ 0.8 ms while actually following motion.

The remaining hot path is dominated by rectification (the precomputed gather)
and the playfield read. The sliding NCC uses integral-image statistics with
an exact integer cross term, and the morphology kernels run as exact
separable passes (row-parallel under `parallel`, like the undistort remap
and BGR→gray).

### Acquisition

Acquisition solves (full geometry estimation while *unlocked*) are handled
by two mechanisms, both **on by default in the GUIs and the web app** and
off in the engine/CLI defaults (oracle byte-identity):

- `calibration.background_acquisition` routes acquisition through the same
  background solver that maintains the lock (native `RecalibThread`, web
  recalib Worker), solving back-to-back on newest-wins snapshots while the
  pipeline thread keeps flowing frames — a live preview stays at capture
  fps during acquisition instead of stuttering at the solve rate. Adoption
  mirrors the inline semantics (`acquire_threshold`, `acquire_frames`,
  streak reset on failed solves).
- `calibration.acquire_downscale_width` (GUI/web default 640) runs
  playfield-candidate detection — the threshold/morphology/connected-
  components sweep, the bulk of a solve — on an INTER_AREA-downscaled
  frame. Label anchors, RANSAC, and validation stay at full resolution, so
  lock quality and OCR are unaffected; `nestris bench --acquire N` measures
  both time-to-lock and unlocked per-frame latency.

With both off (the deterministic default), acquisition runs inline per
frame and costs ~1 s at 1080p; OpenCV's FFT-based `matchTemplate` is still
faster on very large search windows.

Web-worker parallelism note: wasm builds stay single-threaded (SIMD128
only). wasm-threads/rayon (`+atomics`) would require cross-origin-isolation
headers (COOP/COEP) on every host serving the web build and a separate
artifact; deferred until a real need appears — after off-thread
acquisition, the engine worker's steady-state cost is the small canonical
raster.
