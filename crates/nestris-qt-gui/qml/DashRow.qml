import QtQuick

// One dashboard label/value row with confidence graying.
Item {
    property string label: ""
    property string value: ""
    property real conf: 1.0
    property bool gold: false

    width: parent ? parent.width : 300
    height: 26

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    Text {
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: label
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 13
    }

    Text {
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: value
        color: gold ? "#ffd700" : "#e8ecf0"
        opacity: conf < 0.4 ? 0.35 : 1.0
        font.family: pixelFont.name
        font.pixelSize: 14
    }
}
