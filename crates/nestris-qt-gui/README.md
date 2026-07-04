# nestris-qt-gui

Qt 6 desktop GUI (cxx-qt + QML): a NestrisChamps-`classic_1080`-style live
dashboard over the same `nestris-gui-core` pipeline worker the egui GUI
uses.

## Building

This crate is **excluded from the repository root workspace** so that
`cargo build --workspace` never needs a Qt SDK. Build it from this
directory instead:

```powershell
# Windows (Qt installed via the online installer or aqt):
$env:QMAKE = "C:\Qt\6.10.0\msvc2022_64\bin\qmake.exe"
cargo build --release
```

```bash
# Linux/macOS: have qmake on PATH (qt6-base-dev / brew qt) or set QMAKE.
cargo build --release
```

Requirements:

- Qt 6.8+ (Quick, Qml, Gui; dev headers). Local dev uses 6.10, CI uses the
  same version via `jurplel/install-qt-action`.
- A C++17 compiler matching the Rust toolchain (MSVC on Windows).

## Running (dev)

The Qt DLLs/solibs must be findable at runtime:

```powershell
$env:PATH = "C:\Qt\6.10.0\msvc2022_64\bin;$env:PATH"
cargo run --release
```

Release zips bundle the Qt runtime via windeployqt / macdeployqt /
linuxdeploy, so end users need none of this.

## Layout

- `src/bridge/` — cxx-qt bridges (QML-visible QObjects, painted items)
- `qml/` — the QML UI (module `at.retroverse.nestris.core`)
- `assets/fonts/` — Press Start 2P (OFL 1.1, license file alongside)
