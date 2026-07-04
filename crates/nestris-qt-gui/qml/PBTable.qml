import QtQuick

// Compact top-5 high-score table (score / lines / level / TRT).
Item {
    property string title: ""
    /// [{ score, lines, end_level, tetris_rate, date, time }]
    property var rows: []

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    Text {
        id: header
        text: title
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 10
    }

    Column {
        y: 20
        width: parent.width
        spacing: 4

        Text {
            visible: !rows || rows.length === 0
            text: "NO GAMES YET"
            color: "#2a333d"
            font.family: pixelFont.name
            font.pixelSize: 9
        }

        Repeater {
            model: rows

            Item {
                required property var modelData
                required property int index
                width: parent.width
                height: 14

                Text {
                    anchors.left: parent.left
                    text: String(modelData.score).padStart(7, "0")
                    color: index === 0 ? "#ffd700" : "#e8ecf0"
                    font.family: pixelFont.name
                    font.pixelSize: 9
                }
                Text {
                    anchors.right: parent.right
                    anchors.rightMargin: 62
                    text: String(modelData.lines).padStart(3, "0")
                    color: "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 9
                }
                Text {
                    anchors.right: parent.right
                    anchors.rightMargin: 34
                    text: modelData.end_level === null ? "--"
                          : String(modelData.end_level).padStart(2, "0")
                    color: "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 9
                }
                Text {
                    anchors.right: parent.right
                    text: modelData.tetris_rate === null ? "  -"
                          : (modelData.tetris_rate * 100).toFixed(0).padStart(2, " ") + "%"
                    color: "#ffd700"
                    font.family: pixelFont.name
                    font.pixelSize: 9
                }
            }
        }
    }
}
