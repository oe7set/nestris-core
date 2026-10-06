# Changelog

The section of a version is the text of its GitHub release
(`.github/scripts/release_notes.py`). NestrisLTM reads the station packages
of a release (`nestris-station_<version>-1_<arch>.deb`) for station updates.

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
