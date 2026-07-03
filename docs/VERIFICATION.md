# Verification against the Python oracle

The port's central claim is **behavioral equivalence**: given the same video,
the Rust engine emits the same schema-v4 JSON as the Python implementation.
That claim is not asserted once end-to-end — it is enforced *layer by layer*,
so any regression pinpoints the exact stage that diverged.

## The oracle

The Python repo (`NestrisLTM_OCR`) is the reference. Two of its tools feed
this repo's test data:

| Tool | Runs in | Produces |
|---|---|---|
| `tools/gen_cv_goldens.py` (in this repo's `tools/`) | the Python repo's venv | `testdata/cv/` — small OpenCV/numpy input→output pairs, **committed** |
| `tools/dump_stage_artifacts.py` (in the Python repo) | the Python repo's venv | `testdata/stages/<fixture>/` — per-frame stage dumps, **gitignored, regenerable** |

Stage dumps per fixture (1200 frames each, deterministic because background
recalibration is disabled during dumping):

- `canonical/NNNNNN.png` — the rectified 256×240 frame every 30th frame
- `raw_readings.jsonl` — the pre-fusion recognition outputs per frame,
  including the **pre-stabilizer** playfield (the stabilizer is temporal, so
  a sampled replay needs the stateless raw read as its oracle)
- `lock_trace.jsonl` — lock state + homography per frame
- `output.jsonl` — the final wire-format frames (the end-to-end oracle)
- `meta.json` — run parameters

Fixture videos are **never** copied into this repo. Point
`NESTRIS_FIXTURES_DIR` at the Python repo's `fixtures/` directory:

```powershell
$env:NESTRIS_FIXTURES_DIR = "D:\Projekte\Retroverse\NestrisLTM_OCR\fixtures"
```

## The four gates

### Gate 1 — CV primitive golden tests (`cargo test -p nestris-vision`)

Each reimplemented OpenCV/numpy routine is compared against captured cv2
output on real fixture crops and seeded noise:

| Routine | Tolerance |
|---|---|
| `cvtColor` BGR→GRAY | byte-exact |
| BGR→Lab, BGR→HSV (8-bit) | ≤ 1 LSB |
| Lab→BGR | ≤ 2 LSB (float inverse; decision gates downstream are the real check) |
| Otsu threshold | exact value + byte-exact binary |
| morphology (ellipse 3/5, the exact op sequences the engine runs) | byte-exact |
| `matchTemplate` TM_CCOEFF_NORMED | ≤ 1e-4, argmax exact |
| connected components | identical component set (stats-level) |
| `minAreaRect` | center/size ≤ 0.5 px, corners ≤ 0.75 px |
| `getPerspectiveTransform` | ≤ 1e-9 |
| `findHomography` RANSAC | identical inlier set on separable data, H ≤ 1e-3 rel, mean inlier reprojection ≤ 0.1 px |
| `warpPerspective` | ≥ 99% pixels byte-exact, max diff ≤ 2 |
| undistort maps (k1 model) | ≤ 1e-3 px |
| numpy percentile/median/std/argsort | ≤ 1e-12 |

Cross-language RANSAC/Hough equivalence is **decision-level** by design
(different RNGs); within Rust, both are bit-deterministic via seeded PCG32.

### Gate 2 — recognition replay (`cargo test -p nestris-engine --test replay_canonical`)

Python-rectified canonical PNGs are replayed through the Rust recognition
layer and compared against the dumped pre-fusion readings. This isolates
recognition parity from any geometry difference. **Result: 100% on every
field — score, lines, level, next piece, statistics, and every playfield
cell — across all dumped fixtures.**

### Gate 3 — state-machine replay (`cargo test -p nestris-engine --test replay_readings`)

Python's dumped pre-fusion readings are fed through the Rust
fusion → plausibility → stats → OutputFrame chain and compared against
`output.jsonl` **exactly** (integers exact, floats within 1e-9, key order
included). No CV is involved, so zero tolerance applies. **Result: 15
fixtures × 1200 frames = 18 000 frames, all structurally identical** —
including confidences, events, and the STATISTICS key order.

### Gate 4 — full pipeline (`nestris verify`)

The complete Rust pipeline — its own ffmpeg decode, its own RANSAC, its own
warp — runs on the raw fixture videos in oracle-parity mode and is diffed
against `output.jsonl` under this policy:

- **Exact-class fields** (`game_state`, all `fields.*`, discrete `stats.*`)
  must match on ≥ 99.5% of frames, with a ±3-frame transition slack that
  absorbs a different-but-equally-valid RANSAC solve crossing a threshold
  one frame earlier or later.
- **Confidences and pace floats** are reported but not gated: they sit
  directly downstream of the RANSAC geometry, whose RNG legitimately differs
  across languages.

```sh
nestris verify --fixtures $env:NESTRIS_FIXTURES_DIR            # all fixtures
nestris verify --fixtures ... --only tetris_01                 # one fixture
```

Result: **all 15 fixtures PASS**, most fields at 100.000% exactly. One
production detail this gate caught: file decodes must use ffmpeg's
`-fps_mode passthrough` — the default CFR output *duplicates* frames on
variable-frame-rate sources (phone captures), silently shifting frame
indexes against any decode-order consumer (the Python oracle decodes with
PyAV, which yields each encoded frame exactly once; the port's per-frame
comparisons drifted by a full second on the VFR fixtures until aligned).

For single-frame cross-language debugging there is a paired instrument:
`cargo run -p nestris-engine --example solve_debug -- frame.png` prints the
playfield candidates, constellation scores, and the solve confidence for
one frame — byte-comparable against the same probe run in the Python repo.
When extracting probe frames with ffmpeg, remember `-vsync 0`
(passthrough), or the extracted index will not match the decode order.

## Regenerating everything

```sh
# 1. CV goldens (commit the result):
cd ../NestrisLTM_OCR
uv run python ..\nestris-core\tools\gen_cv_goldens.py

# 2. Stage dumps (gitignored):
$env:PYTHONIOENCODING = "utf-8"
uv run python -m tools.dump_stage_artifacts all --force

# 3. Gates:
cd ..\nestris-core
cargo test --workspace --release
cargo run --release -p nestris-cli -- verify --fixtures ..\NestrisLTM_OCR\fixtures
```

## Determinism policy (what makes replays meaningful)

- RANSAC and Hough draw from PCG32 seeded with `splitmix64(frame_index)` —
  a Rust run is bit-identical to any other Rust run on any platform.
- The oracle dumps are produced with the Python engine's background
  recalibration disabled, so its solve cadence is frame-deterministic too.
- The verify harness stamps frames with the oracle's own capture timestamps,
  so pace metrics are compared on identical clocks (PyAV pts vs seq/fps
  would otherwise drift).
