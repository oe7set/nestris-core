# Releasing

Releases are built by `.github/workflows/release.yml` and published as a
GitHub Release with one zip per component and OS plus a combined bundle
per OS.

## Artifact inventory

| Artifact | Contents |
|---|---|
| `nestris-cli-vX.Y.Z-<os>.zip` | `nestris` CLI executable |
| `nestris-gui-vX.Y.Z-<os>.zip` | `nestris-gui` (egui desktop app) |
| `nestris-qt-gui-vX.Y.Z-<os>.zip` | Qt GUI with the Qt runtime: windeployqt folder (Windows), `Nestris Core.app` (macOS, ad-hoc signed), AppImage (Linux) + `QT-NOTICE.md` |
| `nestris-vX.Y.Z-<os>-bundle.zip` | Everything above + `README.md`, `LICENSE`, `docs/` |
| `nestris-web-vX.Y.Z.zip` | Static web app (`web/dist`, SIMD wasm) — serve from any web server |
| `nestris-station_<v>-1_amd64.deb`, `..._arm64.deb` | the headless station for Debian 12 (built in a `debian:bookworm` container: matching glibc) |
| `SHA256SUMS.txt` | checksums of all zips and packages |
| `SHA256SUMS.txt.sig` | Ed25519 signature of `SHA256SUMS.txt` (secret `RELEASE_SIGNING_KEY`; the station updater in NestrisLTM checks it, see `nestris-ltm/docs/UPDATES.md`) |

OS suffixes: `windows-x64`, `linux-x64`, `macos-arm64`.

ffmpeg/ffprobe are runtime prerequisites for video/capture input and are
NOT bundled.

## Cutting a release

The repository secret `RELEASE_SIGNING_KEY` must be set (Settings → Secrets
and variables → Actions); without it the release job fails rather than
publishing an unsigned release. The station's version is its own
(`crates/nestris-station/Cargo.toml`), the tag is the workspace version.

1. Bump the version in **both** places (the Qt crate is an excluded
   standalone workspace and does not inherit the root version):
   - `Cargo.toml` → `[workspace.package] version`
   - `crates/nestris-qt-gui/Cargo.toml` → `[package] version`
2. Commit, push, and let CI go green.
3. Dry-run: trigger the **Release** workflow via *Run workflow*
   (workflow_dispatch). This builds every artifact (named
   `v0.0.0-dev.<run>`) but skips the release step. Download and
   spot-check the artifacts.
4. Tag and push:

   ```
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   The workflow builds everything again and publishes the GitHub Release
   with auto-generated notes.

## Notes

- macOS artifacts are ad-hoc signed (no notarization). First launch
  needs right-click → Open, or `xattr -dr com.apple.quarantine`.
- The Linux Qt GUI is an AppImage; the plain binaries in `cli`/`gui`
  zips need the usual X11/Wayland/GL runtime libraries.
- Qt version in CI is pinned in both workflow files (`ci.yml` qt job and
  `release.yml`); bump them together.
