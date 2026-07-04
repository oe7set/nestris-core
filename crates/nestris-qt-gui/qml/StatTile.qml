import QtQuick

// One classic_1080 stat tile: caption on top, big value below.
Rectangle {
    property string label: ""
    property string value: ""
    property bool gold: false
    property bool alert: false
    property real conf: 1.0
    property int valueSize: 18

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    color: "#10141a"
    border.color: "#1c2430"
    border.width: 2
    radius: 4

    Text {
        x: 10
        y: 8
        text: label
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 10
    }

    Text {
        anchors.right: parent.right
        anchors.rightMargin: 10
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 8
        text: value
        color: alert ? "#f83800" : gold ? "#ffd700" : "#e8ecf0"
        opacity: conf < 0.4 ? 0.35 : 1.0
        font.family: pixelFont.name
        font.pixelSize: valueSize
    }
}
