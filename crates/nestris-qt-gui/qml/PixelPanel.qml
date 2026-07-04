import QtQuick

// Dark dashboard panel with an optional pixel-font caption.
Rectangle {
    property string label: ""

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    color: "#10141a"
    border.color: "#1c2430"
    border.width: 2
    radius: 6

    Text {
        visible: label.length > 0
        x: 12
        y: 10
        text: label
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 12
    }
}
