# Bundled ffmpeg notice

This distribution bundles unmodified `ffmpeg.exe` and `ffprobe.exe`
binaries (in the `ffmpeg/` directory) so video decoding and DirectShow
capture work without a separate ffmpeg installation.

- **Build**: gyan.dev "essentials" build of FFmpeg 8.1.2 (win64, GPL),
  release `8.1.2` of <https://github.com/GyanD/codexffmpeg>, asset
  `ffmpeg-8.1.2-essentials_build.zip` (SHA-256 checked when building).
- **License**: **GNU General Public License v3** (the binaries are
  invoked as separate executables — mere aggregation; the license of this
  application itself is unaffected). License text: `LICENSE` in the
  archive; build details: <https://www.gyan.dev/ffmpeg/builds/>.
- **Source code**: <https://ffmpeg.org/download.html> (release 8.1.2) and
  <https://github.com/GyanD/codexffmpeg>.
- **Replacing the bundled binaries**: point the `NESTRIS_FFMPEG`
  environment variable at a directory containing your own
  `ffmpeg.exe`/`ffprobe.exe` — it always takes precedence over the
  bundled copies — or simply delete the bundled `ffmpeg/` directory to
  fall back to an ffmpeg on your `PATH`.
