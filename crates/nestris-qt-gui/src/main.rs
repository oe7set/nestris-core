//! Qt 6 desktop GUI (cxx-qt + QML): a NestrisChamps-classic_1080-style
//! live dashboard over the shared `nestris-gui-core` pipeline worker.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

fn main() {
    // Hand-rolled retro design: the non-native Basic style keeps controls
    // identical on every platform.
    // SAFETY: before QGuiApplication::new and any thread spawns.
    unsafe {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/at/retroverse/nestris/core/qml/Main.qml",
        ));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
