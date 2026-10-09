# Changelog

The section of a version is the text of its GitHub release
(`.github/scripts/release_notes.py`). NestrisLTM reads the station packages
of a release (`nestris-station_<version>-1_<arch>.deb`) for station updates.

## 0.3.0

- **Station remote configuration**: NestrisLTM sets the station config
  (`station_config` command, retained `config` topic); stored in
  `<state_dir>/remote.json` between `station.toml` and env/`--set`, applied
  by a restart after the running game. Identity, broker, host and paths are
  never remote-settable; an invalid set is rejected and changes nothing.
- **Station performance telemetry**: `perf` in the status (capture and
  processed fps, drop rate, missing frames, engine ms, frame age, live rate,
  CPU) and `seq` / `frame_age_ms` in `live`.
- **`nestris-station bench`**: the capture + engine pipeline on the station
  itself, a file paced in real time (drops like a live device) or the device.
- **Downscaling**: `capture.lowres` (MJPEG decode at 1/2^n size); files are
  decoded like the device (`capture.scale_*`). CLI: `--scale WxH` and
  `--lowres N` for `run`, `bench`, `screens eval`; `bench --json`,
  `--decode-only`; new `nestris compare` (field agreement of two runs).
  Results: `docs/DOWNSCALE.md`.

## 0.2.0

First release of nestris-core: the Rust recognition engine for NES Tetris
and the headless tournament station.

- **Engine**: score, lines, level, next piece, playfield and statistics from
  the console picture (wire-compatible with NestrisLTM_OCR, golden tests),
  cheat detection, validation, NGF recordings.
- **Apps**: command line tool, desktop GUI (egui), Qt GUI, web GUI (wasm);
  Windows builds include ffmpeg.
- **Station 0.2.0** (Debian 12, amd64 and arm64 packages): capture, card
  reader protocol v2 (nestris-rfid-reader 1.0.0), MQTT to NestrisLTM with
  durable delivery, live playfield at 60 Hz, recording upload, self-healing
  under systemd.
- **Station updates from NestrisLTM**: the station checks the release
  signature itself, a root helper installs the package; the card reader is
  flashed with esptool (`sudo apt install esptool`).
