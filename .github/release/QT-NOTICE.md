# Third-party notices — nestris-qt-gui

## Qt 6

The Qt GUI dynamically links the Qt 6 libraries (Core, Gui, Qml, Quick,
QuickControls2 and their dependencies), which are bundled with this
distribution and licensed under the **GNU Lesser General Public License
v3** (LGPLv3). See <https://www.qt.io/licensing/> and
<https://www.gnu.org/licenses/lgpl-3.0.html>.

Qt source code is available at <https://download.qt.io/official_releases/qt/>.
Because Qt is dynamically linked, you may replace the bundled Qt
libraries with your own builds of the same major version.

## Press Start 2P

The bundled pixel font "Press Start 2P" (© CodeMan38) is licensed under
the **SIL Open Font License 1.1**; the license text ships alongside the
font file in the application resources and at
`crates/nestris-qt-gui/assets/fonts/OFL.txt` in the source tree.

## ffmpeg

Video decoding and DirectShow capture invoke `ffmpeg`/`ffprobe`
executables at runtime.

- **Windows** release archives bundle these binaries — see
  `FFMPEG-NOTICE.md` alongside this file for provenance, the GPLv3
  license, and how to replace them.
- **Linux/macOS** distributions do not bundle ffmpeg — install it
  separately (e.g. `apt install ffmpeg`, `brew install ffmpeg`).
