import QtQuick

// Flat retro push button in the dashboard style.
Rectangle {
    id: button

    property string label: ""
    property bool accent: false
    signal clicked

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    width: labelText.implicitWidth + 28
    height: 40
    radius: 4
    color: mouse.pressed ? "#1c2430" : mouse.containsMouse ? "#161d26" : "#10141a"
    border.color: button.accent ? "#3cbcfc" : "#1c2430"
    border.width: 2

    Text {
        id: labelText
        anchors.centerIn: parent
        text: button.label
        color: button.accent ? "#3cbcfc" : "#e8ecf0"
        font.family: pixelFont.name
        font.pixelSize: 12
    }

    MouseArea {
        id: mouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
