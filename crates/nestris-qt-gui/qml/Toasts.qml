import QtQuick
import at.retroverse.nestris.core

// Transient top-right notifications fed by AppBridge.toast — info and
// success expire after 4 s, errors after 8 s, at most 6 visible.
Column {
    spacing: 8

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    ListModel { id: toastModel }

    Connections {
        target: AppBridge
        function onToast(kind, message) {
            toastModel.append({
                kind: kind,
                message: message,
                deadline: Date.now() + (kind === "error" ? 8000 : 4000)
            })
            if (toastModel.count > 6)
                toastModel.remove(0, toastModel.count - 6)
        }
    }

    Timer {
        interval: 250
        running: toastModel.count > 0
        repeat: true
        onTriggered: {
            for (var i = toastModel.count - 1; i >= 0; i--) {
                if (toastModel.get(i).deadline <= Date.now())
                    toastModel.remove(i)
            }
        }
    }

    Repeater {
        model: toastModel

        Rectangle {
            required property var model
            width: toastText.implicitWidth + 28
            height: toastText.implicitHeight + 18
            radius: 4
            color: "#e010141a"
            border.width: 2
            border.color: model.kind === "error" ? "#f83800"
                          : model.kind === "success" ? "#58d854" : "#3cbcfc"

            Text {
                id: toastText
                anchors.centerIn: parent
                text: model.message
                color: "#e8ecf0"
                font.family: pixelFont.name
                font.pixelSize: 10
                width: Math.min(implicitWidth, 420)
                wrapMode: Text.Wrap
            }
        }
    }
}
