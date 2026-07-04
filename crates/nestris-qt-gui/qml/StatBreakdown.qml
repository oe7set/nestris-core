import QtQuick

// Titled rows of name / value / percent (LINES and POINTS breakdowns).
Rectangle {
    property string title: ""
    /// rows: [{ k, v, p }] — p is a preformatted percent string or "".
    property var rows: []

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    color: "#10141a"
    border.color: "#1c2430"
    border.width: 2
    radius: 4

    Text {
        x: 12
        y: 10
        text: title
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 12
    }

    Column {
        x: 12
        y: 36
        width: parent.width - 24
        spacing: 6

        Repeater {
            model: rows

            Item {
                required property var modelData
                width: parent.width
                height: 18

                Text {
                    anchors.left: parent.left
                    text: modelData.k
                    color: modelData.gold ? "#ffd700" : "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 11
                }
                Text {
                    anchors.right: parent.right
                    anchors.rightMargin: 70
                    text: modelData.v
                    color: modelData.gold ? "#ffd700" : "#e8ecf0"
                    font.family: pixelFont.name
                    font.pixelSize: 11
                }
                Text {
                    anchors.right: parent.right
                    text: modelData.p
                    color: "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 11
                }
            }
        }
    }
}
