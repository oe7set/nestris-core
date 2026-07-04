//! Qt 6 desktop GUI (cxx-qt + QML): a NestrisChamps-classic_1080-style
//! live dashboard over the shared `nestris-gui-core` pipeline worker.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod frames;
mod worker_glue;

use std::sync::OnceLock;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QQuickStyle, QString, QUrl};

/// Optional source to open immediately: `nestris-qt-gui <source> [--start <s>]`.
pub struct AutoStart {
    pub source: String,
    pub start_s: f64,
}

pub static AUTO_START: OnceLock<AutoStart> = OnceLock::new();

fn parse_args() {
    let mut source: Option<String> = None;
    let mut start_s = 0.0f64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--start" => {
                start_s = args.next().and_then(|v| v.parse().ok()).unwrap_or_default();
            }
            _ if source.is_none() => source = Some(arg),
            _ => {}
        }
    }
    if let Some(source) = source {
        let _ = AUTO_START.set(AutoStart { source, start_s });
    }
}

fn main() {
    parse_args();
    // Hand-rolled retro design: the non-native Basic style keeps controls
    // identical on every platform (and native styles crash on customized
    // control delegates).
    QQuickStyle::set_style(&QString::from("Basic"));

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
