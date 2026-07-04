use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("at.retroverse.nestris.core")
            .qml_files([
                "qml/DashRow.qml",
                "qml/Main.qml",
                "qml/PixelPanel.qml",
                "qml/RetroButton.qml",
                "qml/Theme.qml",
            ])
            .depend("QtQuick"),
    )
    .qt_module("Quick")
    .files([
        "src/bridge/app_bridge.rs",
        "src/bridge/canon_view.rs",
        "src/bridge/field_view.rs",
        "src/bridge/raw_view.rs",
    ])
    .qrc("assets.qrc")
    .build();
}
