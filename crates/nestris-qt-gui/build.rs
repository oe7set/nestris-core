use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("at.retroverse.nestris.core")
            .qml_files(["qml/Main.qml", "qml/Theme.qml"])
            .depend("QtQuick"),
    )
    .qt_module("Quick")
    .files(["src/bridge/raw_view.rs"])
    .qrc("assets.qrc")
    .build();
}
