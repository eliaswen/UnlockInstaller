// Builds the Qt side: the Rust/C++ bridge in src/ui.rs and the QML files,
// compiled into the binary as the `UnlockInstaller` QML module.
//
// Qt is found through `qmake6` (or `qmake`) on PATH, or the QMAKE variable.

use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("UnlockInstaller").qml_files([
            "qml/Main.qml",
            "qml/SettingsWindow.qml",
            "qml/GameIcon.qml",
        ]),
    )
    .qt_module("Quick")
    .qrc_resources(["assets/fonts/NotoSans-Regular.ttf", "assets/fonts/NotoSans-Bold.ttf"])
    .files(["src/ui.rs"])
    .build();
}
