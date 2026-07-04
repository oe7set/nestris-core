import QtQuick

// Per-piece counts as bars (T J Z O S L I, the I bar in gold) plus the
// per-piece drought and the distribution deviation.
Rectangle {
    /// counts[7], drought[7] in T J Z O S L I order, deviation 0..n.
    property var counts: []
    property var droughts: []
    property real deviation: 0

    readonly property var letters: ["T", "J", "Z", "O", "S", "L", "I"]

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
        text: "PIECES"
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 12
    }

    Text {
        anchors.right: parent.right
        anchors.rightMargin: 12
        y: 10
        text: "DEV " + (deviation * 100).toFixed(0) + "%"
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 10
    }

    Column {
        x: 12
        y: 34
        width: parent.width - 24
        spacing: 5

        Repeater {
            model: 7

            Item {
                required property int index
                width: parent.width
                height: 16

                Text {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: letters[index]
                    color: index === 6 ? "#ffd700" : "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 11
                }

                Rectangle {
                    x: 22
                    anchors.verticalCenter: parent.verticalCenter
                    height: 8
                    radius: 2
                    color: index === 6 ? "#ffd700" : "#3cbcfc"
                    width: {
                        var c = counts && counts.length === 7 ? counts[index] : 0
                        var m = 1
                        for (var i = 0; i < 7; i++)
                            m = Math.max(m, counts && counts.length === 7 ? counts[i] : 0)
                        return Math.max(2, c / m * (parent.width - 130))
                    }
                }

                Text {
                    anchors.right: parent.right
                    anchors.rightMargin: 56
                    anchors.verticalCenter: parent.verticalCenter
                    text: counts && counts.length === 7 ? counts[index] : 0
                    color: "#e8ecf0"
                    font.family: pixelFont.name
                    font.pixelSize: 10
                }

                Text {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: "d" + (droughts && droughts.length === 7 ? droughts[index] : 0)
                    color: (droughts && droughts.length === 7 ? droughts[index] : 0) >= 13
                           ? "#f83800" : "#9aa3ad"
                    font.family: pixelFont.name
                    font.pixelSize: 10
                }
            }
        }
    }
}
