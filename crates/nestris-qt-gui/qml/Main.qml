import QtQuick
import QtQuick.Controls
import at.retroverse.nestris.core

ApplicationWindow {
    id: root
    visible: true
    width: 1280
    height: 720
    minimumWidth: 840
    minimumHeight: 560
    title: "nestris-core"
    color: theme.bg

    Theme { id: theme }

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    // Fixed 1920×1080 design space, uniformly scaled into the window and
    // letterboxed (the NestrisChamps approach — the pixel layout never
    // reflows, it only scales).
    Item {
        id: design
        width: 1920
        height: 1080
        transformOrigin: Item.TopLeft
        scale: Math.min(root.width / width, root.height / height)
        x: (root.width - width * scale) / 2
        y: (root.height - height * scale) / 2

        Text {
            x: 64
            y: 48
            text: "NESTRIS CORE"
            color: theme.accent
            font.family: pixelFont.name
            font.pixelSize: 40
        }

        Rectangle {
            x: 64
            y: 140
            width: 960
            height: 720
            color: theme.panel
            border.color: theme.panelEdge
            border.width: 2
            radius: 6

            RawFrameView {
                id: rawView
                anchors.fill: parent
                anchors.margins: 10
            }
        }

        Text {
            x: 64
            y: 900
            text: "P1 SPIKE — PAINTED ITEM + PIXEL FONT"
            color: theme.gold
            font.family: pixelFont.name
            font.pixelSize: 18
        }

        Timer {
            interval: 500
            running: true
            repeat: true
            onTriggered: rawView.refresh()
        }
    }
}
