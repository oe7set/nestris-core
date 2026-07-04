import QtQuick
import QtQuick.Controls
import at.retroverse.nestris.core

// Playback transport for files and replays (hidden while live): pause,
// frame steps, speed, loop, and a seek slider with hover time preview.
Item {
    id: bar

    visible: AppBridge.running && !AppBridge.live && AppBridge.durationS > 0

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    function fmtTime(t) {
        if (t < 0)
            t = 0
        var m = Math.floor(t / 60)
        var s = t - m * 60
        return String(m).padStart(2, "0") + ":" + (s < 10 ? "0" : "") + s.toFixed(1)
    }

    Row {
        anchors.fill: parent
        spacing: 12

        RetroButton {
            anchors.verticalCenter: parent.verticalCenter
            label: AppBridge.paused ? "▶" : "⏸"
            onClicked: AppBridge.togglePause()
        }
        RetroButton {
            anchors.verticalCenter: parent.verticalCenter
            label: "|◀"
            onClicked: AppBridge.stepBack()
        }
        RetroButton {
            anchors.verticalCenter: parent.verticalCenter
            label: "▶|"
            onClicked: AppBridge.stepFrame()
        }

        // Speed steps.
        Row {
            anchors.verticalCenter: parent.verticalCenter
            spacing: 4

            Repeater {
                model: [{ t: "¼", v: 0.25 }, { t: "½", v: 0.5 }, { t: "1×", v: 1.0 },
                        { t: "2×", v: 2.0 }, { t: "4×", v: 4.0 }, { t: "MAX", v: -1.0 }]

                Rectangle {
                    required property var modelData
                    width: speedText.implicitWidth + 14
                    height: 32
                    radius: 4
                    color: AppBridge.speed === modelData.v ? "#1c2430" : "#10141a"
                    border.color: AppBridge.speed === modelData.v ? "#3cbcfc" : "#1c2430"
                    border.width: 2

                    Text {
                        id: speedText
                        anchors.centerIn: parent
                        text: parent.modelData.t
                        color: AppBridge.speed === parent.modelData.v ? "#3cbcfc" : "#9aa3ad"
                        font.family: pixelFont.name
                        font.pixelSize: 11
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: AppBridge.applySpeed(parent.modelData.v)
                    }
                }
            }
        }

        // Loop toggle.
        Rectangle {
            anchors.verticalCenter: parent.verticalCenter
            width: 40
            height: 32
            radius: 4
            color: AppBridge.loopEnabled ? "#1c2430" : "#10141a"
            border.color: AppBridge.loopEnabled ? "#ffd700" : "#1c2430"
            border.width: 2

            Text {
                anchors.centerIn: parent
                text: "⟳"
                color: AppBridge.loopEnabled ? "#ffd700" : "#9aa3ad"
                font.pixelSize: 18
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: AppBridge.loopEnabled = !AppBridge.loopEnabled
            }
        }

        // Seek slider.
        Slider {
            id: seek
            anchors.verticalCenter: parent.verticalCenter
            width: bar.width - x - timeText.implicitWidth - 24
            from: 0
            to: AppBridge.durationS
            value: pressed ? value : AppBridge.positionS
            onPressedChanged: {
                if (!pressed)
                    AppBridge.seekTo(value)
            }

            background: Rectangle {
                x: seek.leftPadding
                y: seek.topPadding + seek.availableHeight / 2 - height / 2
                width: seek.availableWidth
                height: 6
                radius: 3
                color: "#1c2430"

                Rectangle {
                    width: seek.visualPosition * parent.width
                    height: parent.height
                    radius: 3
                    color: "#3cbcfc"
                }
            }

            handle: Rectangle {
                x: seek.leftPadding + seek.visualPosition * (seek.availableWidth - width)
                y: seek.topPadding + seek.availableHeight / 2 - height / 2
                width: 14
                height: 20
                radius: 2
                color: seek.pressed ? "#3cbcfc" : "#e8ecf0"
            }

            MouseArea {
                id: hoverArea
                anchors.fill: parent
                hoverEnabled: true
                acceptedButtons: Qt.NoButton
            }

            ToolTip {
                parent: seek.handle
                visible: hoverArea.containsMouse || seek.pressed
                delay: 0
                background: Rectangle {
                    color: "#10141a"
                    border.color: "#1c2430"
                    border.width: 1
                }
                contentItem: Text {
                    text: seek.pressed
                          ? bar.fmtTime(seek.value)
                          : bar.fmtTime(hoverArea.mouseX / seek.width * AppBridge.durationS)
                    color: "#e8ecf0"
                    font.family: pixelFont.name
                    font.pixelSize: 10
                }
            }
        }

        Text {
            id: timeText
            anchors.verticalCenter: parent.verticalCenter
            text: bar.fmtTime(AppBridge.positionS) + " / " + bar.fmtTime(AppBridge.durationS)
            color: "#9aa3ad"
            font.family: pixelFont.name
            font.pixelSize: 12
        }
    }
}
