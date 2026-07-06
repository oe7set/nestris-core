# Bundled ffmpeg notice

This distribution bundles unmodified `ffmpeg.exe` and `ffprobe.exe`
binaries (in the `ffmpeg/` directory) so video decoding and DirectShow
capture work without a separate ffmpeg installation.

- **Build**: BtbN FFmpeg-Builds, release 7.1 branch (win64, GPL variant),
  tag `autobuild-2026-07-05-15-08`, asset
  `ffmpeg-n7.1.5-1-g7d0e842004-win64-gpl-7.1.zip`.
- **License**: **GNU General Public License v3** (the binaries are
  invoked as separate executables — mere aggregation; the license of this
  application itself is unaffected). License texts and build provenance:
  <https://github.com/BtbN/FFmpeg-Builds>.
- **Source code**: <https://ffmpeg.org/download.html> and the exact build
  scripts at <https://github.com/BtbN/FFmpeg-Builds>.
- **Replacing the bundled binaries**: point the `NESTRIS_FFMPEG`
  environment variable at a directory containing your own
  `ffmpeg.exe`/`ffprobe.exe` — it always takes precedence over the
  bundled copies — or simply delete the bundled `ffmpeg/` directory to
  fall back to an ffmpeg on your `PATH`.
