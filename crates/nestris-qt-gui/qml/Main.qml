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

    Connections {
        target: AppBridge
        function onFrameSerialChanged() {
            rawView.refresh()
            canonView.refresh()
            fieldView.refresh()
        }
        function onEventAdded(text, severity) {
            eventModel.append({ text: text, severity: severity })
            if (eventModel.count > 300)
                eventModel.remove(0, eventModel.count - 300)
            eventList.positionViewAtEnd()
        }
    }

    ListModel { id: eventModel }

    // Fixed 1920×1080 design space, uniformly scaled and letterboxed.
    Item {
        id: design
        width: 1920
        height: 1080
        transformOrigin: Item.TopLeft
        scale: Math.min(root.width / width, root.height / height)
        x: (root.width - width * scale) / 2
        y: (root.height - height * scale) / 2

        // ---- header bar ----
        Row {
            id: header
            x: 64
            y: 24
            spacing: 16

            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "NESTRIS CORE"
                color: theme.accent
                font.family: pixelFont.name
                font.pixelSize: 24
            }

            RetroButton { label: "OPEN VIDEO"; onClicked: AppBridge.openVideoDialog() }
            RetroButton { label: "OPEN REPLAY"; onClicked: AppBridge.openReplayDialog() }

            RetroButton {
                visible: AppBridge.supportsCapture
                label: "DEVICES ⟳"
                onClicked: AppBridge.refreshDevices()
            }

            ComboBox {
                id: deviceBox
                visible: AppBridge.supportsCapture
                width: 260
                height: 40
                anchors.verticalCenter: parent.verticalCenter
                model: AppBridge.devices
                displayText: currentIndex < 0 ? "camera…" : currentText
                font.family: pixelFont.name
                font.pixelSize: 11
                onActivated: AppBridge.openDevice(currentText)
            }

            RetroButton {
                label: AppBridge.running ? "⏹ STOP" : "▶ START"
                accent: !AppBridge.running
                onClicked: AppBridge.running ? AppBridge.stopSource() : AppBridge.startSource()
            }
            RetroButton { label: "RESET LOCK"; onClicked: AppBridge.resetLock() }
        }

        // ---- status cluster (right) ----
        Row {
            x: design.width - 64 - width
            y: 36
            spacing: 20

            Text {
                visible: AppBridge.recording
                text: "● REC"
                color: theme.bad
                font.family: pixelFont.name
                font.pixelSize: 16
            }
            Text {
                text: AppBridge.lockState
                color: AppBridge.lockState === "LOCKED" || AppBridge.lockState === "REPLAY"
                       ? theme.good
                       : AppBridge.lockState === "DRIFT" ? theme.warn : theme.dim
                font.family: pixelFont.name
                font.pixelSize: 16
            }
            Text {
                text: AppBridge.fps.toFixed(0) + " FPS"
                color: theme.dim
                font.family: pixelFont.name
                font.pixelSize: 16
            }
            BusyIndicator {
                width: 24
                height: 24
                anchors.verticalCenter: parent.verticalCenter
                running: AppBridge.buffering
                visible: AppBridge.buffering
            }
        }

        Text {
            x: 64
            y: 78
            width: 1400
            visible: AppBridge.lastError.length > 0
            text: AppBridge.lastError
            color: theme.bad
            elide: Text.ElideRight
            font.family: pixelFont.name
            font.pixelSize: 12
        }

        // ---- raw preview / source zone ----
        PixelPanel {
            id: sourceZone
            x: 64
            y: 110
            width: 960
            height: 720

            RawFrameView {
                id: rawView
                anchors.fill: parent
                anchors.margins: 8
            }

            // Idle drop zone / replay placeholder.
            Column {
                anchors.centerIn: parent
                spacing: 24
                visible: !AppBridge.running || (AppBridge.isReplay() && AppBridge.running)

                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: AppBridge.running ? "▶ NGF REPLAY" : "DROP VIDEO / .NGF FILE"
                    color: AppBridge.running ? theme.good : theme.dim
                    font.family: pixelFont.name
                    font.pixelSize: 22
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    visible: !AppBridge.running
                    text: "OR USE OPEN VIDEO / OPEN REPLAY ABOVE"
                    color: theme.grid
                    font.family: pixelFont.name
                    font.pixelSize: 12
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    visible: AppBridge.running && AppBridge.isReplay()
                    text: "RECORDED GAME PLAYBACK — NO SOURCE VIDEO"
                    color: theme.dim
                    font.family: pixelFont.name
                    font.pixelSize: 12
                }
            }
        }

        // ---- canonical preview ----
        PixelPanel {
            x: 1056
            y: 110
            width: 400
            height: 400
            label: "CANONICAL"

            CanonFrameView {
                id: canonView
                anchors.fill: parent
                anchors.margins: 8
                anchors.topMargin: 30
            }
        }

        // ---- tracked playfield ----
        PixelPanel {
            x: 1488
            y: 110
            width: 250
            height: 540
            label: "FIELD"

            PlayfieldView {
                id: fieldView
                anchors.fill: parent
                anchors.margins: 8
                anchors.topMargin: 30
            }
        }

        // ---- NEXT box ----
        PixelPanel {
            x: 1770
            y: 110
            width: 86
            height: 100
            label: "NEXT"

            Text {
                anchors.centerIn: parent
                anchors.verticalCenterOffset: 10
                text: AppBridge.nextPiece
                color: theme.text
                opacity: AppBridge.confNext < 0.4 ? 0.35 : 1.0
                font.family: pixelFont.name
                font.pixelSize: 34
            }
        }

        // ---- game-state banner + dashboard values ----
        PixelPanel {
            x: 1056
            y: 540
            width: 400
            height: 400

            Column {
                anchors.fill: parent
                anchors.margins: 16
                spacing: 10

                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: AppBridge.gameState.length > 0 ? AppBridge.gameState : "NO SOURCE"
                    color: AppBridge.gameState === "IN GAME" ? theme.good
                         : AppBridge.gameState === "GAME OVER" ? theme.bad
                         : AppBridge.gameState === "PAUSED" ? theme.warn : theme.dim
                    font.family: pixelFont.name
                    font.pixelSize: 18
                }

                Rectangle { width: parent.width; height: 2; color: theme.grid }

                DashRow { label: "SCORE"; value: AppBridge.score < 0 ? "—" : String(AppBridge.score).padStart(7, "0"); conf: AppBridge.confScore }
                DashRow { label: "LINES"; value: AppBridge.lines < 0 ? "—" : String(AppBridge.lines).padStart(3, "0"); conf: AppBridge.confLines }
                DashRow { label: "LEVEL"; value: AppBridge.level < 0 ? "—" : String(AppBridge.level).padStart(2, "0"); conf: AppBridge.confLevel }
                DashRow { label: "PIECES"; value: String(AppBridge.pieces) }
                DashRow { label: "TRT"; value: AppBridge.tetrisRate < 0 ? "—" : (AppBridge.tetrisRate * 100).toFixed(0) + "%"; gold: true }
                DashRow { label: "PPS"; value: AppBridge.pps < 0 ? "—" : AppBridge.pps.toFixed(2) }
                DashRow { label: "BURN"; value: String(AppBridge.burn) }
                DashRow { label: "DROUGHT"; value: String(AppBridge.drought) }
                DashRow {
                    label: "CLEARS"
                    value: AppBridge.clearsSingle + "/" + AppBridge.clearsDouble + "/"
                         + AppBridge.clearsTriple + "/" + AppBridge.clearsTetris
                }
            }

            // ⚠ CHECK CAPTURE alarm overlay
            Rectangle {
                anchors.fill: parent
                color: "#c0100404"
                visible: AppBridge.alarm
                radius: 4

                Text {
                    anchors.centerIn: parent
                    text: "⚠ CHECK CAPTURE"
                    color: theme.bad
                    font.family: pixelFont.name
                    font.pixelSize: 22
                }
            }
        }

        // ---- event stream ----
        PixelPanel {
            x: 64
            y: 850
            width: 960
            height: 200
            label: "EVENTS"

            ListView {
                id: eventList
                anchors.fill: parent
                anchors.margins: 10
                anchors.topMargin: 30
                model: eventModel
                clip: true
                delegate: Text {
                    text: model.text
                    color: model.severity === "gold" ? theme.gold
                         : model.severity === "info" ? theme.accent
                         : model.severity === "warn" ? theme.warn : theme.bad
                    font.family: pixelFont.name
                    font.pixelSize: 11
                }
            }
        }
    }

    // ---- drag & drop (videos and .ngf replays both open) ----
    DropArea {
        anchors.fill: parent
        onDropped: function (drop) {
            if (drop.hasUrls && drop.urls.length > 0)
                AppBridge.openUrl(drop.urls[0])
        }

        Rectangle {
            anchors.fill: parent
            color: "#a00a0c10"
            visible: parent.containsDrag
            border.color: theme.accent
            border.width: 4

            Text {
                anchors.centerIn: parent
                text: "DROP TO OPEN"
                color: theme.accent
                font.family: pixelFont.name
                font.pixelSize: 32
            }
        }
    }
}
