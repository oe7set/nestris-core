# Downscaling the engine input

Does a smaller engine input (`capture.scale_width/height`) or a reduced
MJPEG decode (`capture.lowres`) make a station faster, and how much
recognition does it cost? Measured on the two station captures
(`tetrisvideo/aufnahme_20260930-*.mkv`, MS2109, PAL 720×576 MJPEG at 50 fps,
muxed as 25 fps) with nestris-core 0.3.0, October 2026.

**Result: keep the capture size (720×576 for PAL) and `lowres = 0`.**
Downscaling saves no engine time, makes decoding slightly slower, and below
480 px wide the playfield is no longer found.

## Recognition

Screen accuracy is `nestris screens eval` against the hand labels
(`testdata/screens/*.labels.tsv`, gate ≥ 99.5 %). Field agreement is
`nestris compare` against the native run (in-game frames, ±3 frames slack).
Engine defaults (inline acquisition: deterministic).

Short capture (2.4 min, 2 games):

| Engine input | Screens | Starts | score / lines / level | next | playfield cells |
|---|---|---|---|---|---|
| 720×576 (native) | 100 % | 2/2 | reference | | |
| 640×512 | 100 % | 2/2 | 100 % | 100 % | 99.9 % |
| 560×448 | 100 % | 2/2 | 100 % | 100 % | 99.9 % |
| 480×384 | 100 % | 2/2 | 100 % | 100 % | 99.9 % |
| 400×320 | 54.5 % | 1 late | 0 % level | 100 % | 88.8 % |
| 360×288 | 99.96 % | 2/2 | 100 % | 98.4 % | **82.7 %** |
| `lowres 1` (360×288) | 44.1 % | 0 | 0 % | 0 % | 0 % |
| 320×256 | 44.1 % | 0 | 0 % | 0 % | 0 % |
| 256×240 | 99.98 % | 2/2 | 100 % | 100 % | 99.9 % |
| `lowres 2` (180×144) | 100 % | 2/2 | 38–89 % | 100 % | 99.3 % |

Long capture (19 min, 5 games, 4 starts after menus):

| Engine input | Screens | Starts | score / lines / level | next | playfield cells |
|---|---|---|---|---|---|
| 720×576 (native) | 100 % | 4/4 | reference | | |
| 640×512 | 100 % | 4/4 | 100 % | 98.3 % | 98.5 % |
| 560×448 | 100 % | 4/4 | 100 % | 99.3 % | 99.2 % |
| 480×384 | 100 % | 4/4 | 100 % | 99.3 % | 99.0 % |
| 360×288 | **9.8 %** | **0/4** | 0 % | 0 % | 0 % |

(Sizes that already failed on the short capture were not run on the long
one: unlocked frames cost up to 50 ms each.)

Why it breaks: the playfield search (`geometry_cal/anchors.rs`) thresholds
the dark playfield and cleans the mask with fixed-size morphology (3×3 close
twice, 5×5 open). On a small picture the playfield border is only one or two
pixels wide, so the mask merges the playfield with the dark area around it
and no candidate has the playfield's shape. Whether a size works is then
luck (256×240 happens to work, 320×256 and 400×320 do not, the softer
`lowres` JPEG decode fails at a size the scaler passes). Without a lock the
engine keeps searching, which costs 10-50 ms per frame instead of 3.

## Speed

`nestris bench` on the long capture from 250 s (in game, 5700 timed frames)
with the station's engine settings (background acquisition, 640-wide
candidate detection); decode-only = ffmpeg alone. Development PC: Intel Xeon
E3-1505M v6 (4 cores, up to 4 GHz).

| Engine input | engine p50 | p95 | p99 | decode (ms/frame) |
|---|---|---|---|---|
| 720×576 | 2.27 | 3.43 | 4.17 | 1.84 |
| 640×512 | 2.71 | 3.73 | 4.44 | 1.98 |
| 480×384 | 2.45 | 3.19 | 3.66 | 2.08 |
| 720×576, 1 engine thread | 3.44 | 4.59 | 5.07 | |

The engine time does not depend on the input size: every locked frame is
warped onto the 256×240 NES raster first, and everything after that is the
same work. Scaling the frame in ffmpeg adds work instead of saving it.
MJPEG `lowres` would save about 15-35 % of the decode
(1.7 / 1.45 / 1.1 ms per frame at lowres 0 / 1 / 2), but at 720×576 it
produces sizes the playfield search cannot handle.

## The stations (AMD GX-415GA)

The Debian stations have a 4-core AMD GX-415GA (Jaguar, 1.5 GHz, no AVX2).
A Jaguar core is roughly 4-5× slower than the development PC. Rough
estimate: engine p50 10-15 ms, p95 15-25 ms per frame against a budget of
20 ms at 50 fps, plus ffmpeg's decode (≈ 8-10 ms per frame) on another core.
That is close to the limit; occasional dropped frames at busy moments are
possible. Measure on the station itself (no broker needed):

```sh
nestris-station bench -c /etc/nestris-station/station.toml \
    --input aufnahme_20260930-231217.mkv --fps 50 --start 250 --seconds 60
# the live device (stop the service first):
sudo systemctl stop nestris-station
nestris-station bench -c /etc/nestris-station/station.toml --seconds 60
sudo systemctl start nestris-station
```

It prints processed / delivered fps, the drop rate, engine ms p50/p95 and
the CPU load every 2 seconds. In operation, NestrisLTM's *Stationen &
Geräte → Leistung* shows the same numbers live. If a station drops frames:
check `capture_fps` first (the camera/USB), then the CPU; reducing the
engine input size does not help.

## Reproduce

```sh
cargo build --release -p nestris-cli
V=tetrisvideo   # directory with the captures
# accuracy against the labels
nestris screens eval --input $V/aufnahme_20260930-233153.mkv --fps 50 --scale 480x384 \
    --labels testdata/screens/aufnahme_20260930-233153.labels.tsv
# field agreement against the native run
nestris run --input $V/aufnahme_20260930-233153.mkv --fps 50 --no-record --jsonl native.jsonl
nestris run --input $V/aufnahme_20260930-233153.mkv --fps 50 --scale 480x384 --no-record --jsonl small.jsonl
nestris compare --a native.jsonl --b small.jsonl
# speed
nestris bench --input $V/aufnahme_20260930-231217.mkv --fps 50 --start 250 --frames 6000 \
    --warmup 300 --scale 480x384 --json \
    --set calibration.background_acquisition=true --set calibration.acquire_downscale_width=640
nestris bench --input $V/aufnahme_20260930-231217.mkv --fps 50 --start 250 --frames 3000 --decode-only
# the gate test for one size
NESTRIS_SCREEN_VIDEOS=$PWD/$V NESTRIS_SCREEN_SCALE=480x384 \
    cargo test --release -p nestris-cli -- --ignored station_captures
```
