use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    let builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("at.retroverse.nestris.core")
            .qml_files([
                "qml/DashRow.qml",
                "qml/Main.qml",
                "qml/PixelPanel.qml",
                "qml/RetroButton.qml",
                "qml/Theme.qml",
                "qml/TransportBar.qml",
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
    .qrc("assets.qrc");

    // MSVC decodes narrow string literals with the local codepage by
    // default; the QML sources embedded by qmlcachegen are UTF-8.
    // SAFETY: only adds a compiler flag, no custom C++ is injected.
    let builder = unsafe {
        builder.cc_builder(|cc| {
            cc.flag_if_supported("/utf-8");
        })
    };

    builder.build();
}
