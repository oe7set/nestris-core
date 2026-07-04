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

    // Extended stats and PB tables arrive as JSON (throttled bridge props).
    property var ext: {
        try { return JSON.parse(AppBridge.extJson) } catch (e) { return {} }
    }
    property var pb: {
        try { return JSON.parse(AppBridge.pbJson) } catch (e) { return {} }
    }

    function fmtOpt(v, pad) {
        return v === null || v === undefined ? "—" : String(v).padStart(pad, "0")
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

        // ================= header =================
        Row {
            id: header
            x: 64
            y: 24
            spacing: 14

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
                width: 230
                height: 40
                anchors.verticalCenter: parent.verticalCenter
                model: AppBridge.devices
                displayText: currentIndex < 0 ? "camera…" : currentText
                font.family: pixelFont.name
                font.pixelSize: 11
                onActivated: AppBridge.openDevice(currentText)
            }

            RetroButton {
                label: AppBridge.running ? "■ STOP" : "▶ START"
                accent: !AppBridge.running
                onClicked: AppBridge.running ? AppBridge.stopSource() : AppBridge.startSource()
            }
            RetroButton { label: "RESET LOCK"; onClicked: AppBridge.resetLock() }
            RetroButton { label: "⚙ SETTINGS"; onClicked: settingsDialog.open() }
        }

        // ================= status cluster =================
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
            y: 80
            width: 1500
            visible: AppBridge.lastError.length > 0
            text: AppBridge.lastError
            color: theme.bad
            elide: Text.ElideRight
            font.family: pixelFont.name
            font.pixelSize: 12
        }

        // ================= left column =================
        PixelPanel {
            id: sourceZone
            x: 64
            y: 110
            width: 960
            height: 560

            property bool showCanon: false

            RawFrameView {
                id: rawView
                anchors.fill: parent
                anchors.margins: 8
                visible: !sourceZone.showCanon
            }

            CanonFrameView {
                id: canonView
                anchors.fill: parent
                anchors.margins: 8
                visible: sourceZone.showCanon
            }

            // RAW / CANON switch.
            Row {
                anchors.top: parent.top
                anchors.right: parent.right
                anchors.margins: 10
                spacing: 4
                visible: AppBridge.running && !AppBridge.isReplay()

                Repeater {
                    model: [{ t: "RAW", c: false }, { t: "CANON", c: true }]

                    Rectangle {
                        required property var modelData
                        width: chipText.implicitWidth + 14
                        height: 24
                        radius: 3
                        color: "#c010141a"
                        border.width: 2
                        border.color: sourceZone.showCanon === modelData.c ? theme.accent : theme.panelEdge

                        Text {
                            id: chipText
                            anchors.centerIn: parent
                            text: parent.modelData.t
                            color: sourceZone.showCanon === parent.modelData.c ? theme.accent : theme.dim
                            font.family: pixelFont.name
                            font.pixelSize: 9
                        }

                        MouseArea {
                            anchors.fill: parent
                            onClicked: sourceZone.showCanon = parent.modelData.c
                        }
                    }
                }
            }

            // Idle drop zone / replay placeholder.
            Column {
                anchors.centerIn: parent
                spacing: 24
                visible: !AppBridge.running || AppBridge.isReplay()

                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: AppBridge.running ? "▶ NGF REPLAY" : "DROP VIDEO / .NGF FILE"
                    color: AppBridge.running ? theme.good : theme.dim
                    font.family: pixelFont.name
                    font.pixelSize: 22
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: AppBridge.running && AppBridge.isReplay()
                          ? "RECORDED GAME PLAYBACK — NO SOURCE VIDEO"
                          : "OR USE OPEN VIDEO / OPEN REPLAY ABOVE"
                    color: AppBridge.running ? theme.dim : theme.grid
                    font.family: pixelFont.name
                    font.pixelSize: 12
                }
            }

            // ⚠ CHECK CAPTURE alarm.
            Rectangle {
                anchors.fill: parent
                color: "#a0100404"
                visible: AppBridge.alarm
                radius: 4

                Text {
                    anchors.centerIn: parent
                    text: "⚠ CHECK CAPTURE"
                    color: theme.bad
                    font.family: pixelFont.name
                    font.pixelSize: 26
                }
            }
        }

        HeightChart {
            x: 64
            y: 682
            width: 960
            height: 96
            timeline: root.ext.height_timeline || []
        }

        TransportBar {
            x: 64
            y: 790
            width: 960
            height: 40
        }

        PixelPanel {
            x: 64
            y: 844
            width: 960
            height: 212
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

        // ================= middle column: tiles =================
        Rectangle {
            x: 1056
            y: 110
            width: 400
            height: 40
            radius: 4
            color: "#10141a"
            border.width: 2
            border.color: AppBridge.gameState === "IN GAME" ? theme.good
                        : AppBridge.gameState === "GAME OVER" ? theme.bad
                        : AppBridge.gameState === "PAUSED" ? theme.warn : theme.panelEdge

            Text {
                anchors.centerIn: parent
                text: AppBridge.gameState.length > 0 ? AppBridge.gameState : "NO SOURCE"
                color: AppBridge.gameState === "IN GAME" ? theme.good
                     : AppBridge.gameState === "GAME OVER" ? theme.bad
                     : AppBridge.gameState === "PAUSED" ? theme.warn : theme.dim
                font.family: pixelFont.name
                font.pixelSize: 16
            }
        }

        StatTile {
            x: 1056
            y: 158
            width: 400
            height: 64
            label: "SCORE"
            valueSize: 26
            value: AppBridge.score < 0 ? "—" : String(AppBridge.score).padStart(7, "0")
            conf: AppBridge.confScore
        }

        Grid {
            x: 1056
            y: 230
            columns: 2
            columnSpacing: 12
            rowSpacing: 8

            StatTile {
                width: 194; height: 56
                label: "PACE"
                value: root.ext.pace_score === null || root.ext.pace_score === undefined
                       ? "—" : String(root.ext.pace_score).padStart(7, "0")
            }
            StatTile {
                width: 194; height: 56
                label: "LINES"
                value: AppBridge.lines < 0 ? "—" : String(AppBridge.lines).padStart(3, "0")
                conf: AppBridge.confLines
            }
            StatTile {
                width: 194; height: 56
                label: "LEVEL"
                value: AppBridge.level < 0 ? "—" : String(AppBridge.level).padStart(2, "0")
                conf: AppBridge.confLevel
            }
            StatTile {
                width: 194; height: 56
                label: "EFF"
                // Clear points per line; 300 = tetris-only.
                value: root.ext.efficiency === null || root.ext.efficiency === undefined
                       ? "—" : root.ext.efficiency.toFixed(0)
            }
            StatTile {
                width: 194; height: 56
                label: "BRN"
                value: String(AppBridge.burn)
            }
            StatTile {
                width: 194; height: 56
                label: "TRT"
                gold: true
                value: AppBridge.tetrisRate < 0 ? "—" : (AppBridge.tetrisRate * 100).toFixed(0) + "%"
            }
            StatTile {
                width: 194; height: 56
                label: "I-DRT"
                valueSize: 13
                alert: (root.ext.i_drought ? root.ext.i_drought.current : 0) >= 13
                value: root.ext.i_drought
                       ? root.ext.i_drought.current + "/" + root.ext.i_drought.last + "/" + root.ext.i_drought.max
                       : "—"
            }
            StatTile {
                width: 194; height: 56
                label: "PIECES"
                value: String(AppBridge.pieces)
            }
            StatTile {
                width: 194; height: 56
                label: "PPS"
                value: AppBridge.pps < 0 ? "—" : AppBridge.pps.toFixed(2)
            }
            StatTile {
                width: 194; height: 56
                label: "DROUGHT"
                alert: AppBridge.drought >= 13
                value: String(AppBridge.drought)
            }
        }

        StatBreakdown {
            x: 1056
            y: 560
            width: 400
            height: 168
            title: "LINES"
            rows: {
                var L = Math.max(AppBridge.lines, 0)
                function pct(n) { return L > 0 ? Math.round(n / L * 100) + "%" : "" }
                return [
                    { k: "SINGLES", v: String(AppBridge.clearsSingle), p: pct(AppBridge.clearsSingle) },
                    { k: "DOUBLES", v: String(AppBridge.clearsDouble), p: pct(AppBridge.clearsDouble * 2) },
                    { k: "TRIPLES", v: String(AppBridge.clearsTriple), p: pct(AppBridge.clearsTriple * 3) },
                    { k: "TETRIS", v: String(AppBridge.clearsTetris), p: pct(AppBridge.clearsTetris * 4), gold: true }
                ]
            }
        }

        StatBreakdown {
            x: 1056
            y: 736
            width: 400
            height: 192
            title: "POINTS"
            rows: {
                var p = root.ext.points || {}
                var S = Math.max(AppBridge.score, 0)
                function pct(n) { return S > 0 && n !== undefined ? Math.round(n / S * 100) + "%" : "" }
                function v(n) { return n === undefined ? "—" : String(n) }
                return [
                    { k: "DROPS", v: v(p.drops), p: pct(p.drops) },
                    { k: "SINGLES", v: v(p.singles), p: pct(p.singles) },
                    { k: "DOUBLES", v: v(p.doubles), p: pct(p.doubles) },
                    { k: "TRIPLES", v: v(p.triples), p: pct(p.triples) },
                    { k: "TETRISES", v: v(p.tetrises), p: pct(p.tetrises), gold: true }
                ]
            }
        }

        StatBreakdown {
            x: 1056
            y: 936
            width: 400
            height: 120
            title: "BOARD"
            rows: {
                var b = root.ext.board || {}
                var state = (b.tetris_ready ? "READY " : "") + (b.double_well ? "WELL " : "")
                          + (b.clean_slope ? "SLOPE" : "")
                return [
                    { k: "MAX HEIGHT", v: b.max_height === undefined ? "—" : String(b.max_height), p: "" },
                    { k: "HOLES", v: b.holes === undefined ? "—" : String(b.holes), p: "" },
                    { k: "STATE", v: state.length > 0 ? state : "—", p: "", gold: b.tetris_ready === true }
                ]
            }
        }

        // ================= right column =================
        PixelPanel {
            x: 1488
            y: 110
            width: 244
            height: 470
            label: "FIELD"

            PlayfieldView {
                id: fieldView
                anchors.fill: parent
                anchors.margins: 10
                anchors.topMargin: 30
            }
        }

        PixelPanel {
            x: 1744
            y: 110
            width: 112
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

        PieceDistribution {
            x: 1488
            y: 592
            width: 368
            height: 208
            counts: root.ext.piece_dist ? root.ext.piece_dist.counts : []
            droughts: root.ext.piece_dist ? root.ext.piece_dist.drought : []
            deviation: root.ext.piece_dist ? root.ext.piece_dist.deviation : 0
        }

        TrtChart {
            x: 1488
            y: 812
            width: 368
            height: 108
            trend: root.ext.trt_trend || []
        }

        PBTable {
            x: 1488
            y: 936
            width: 178
            height: 120
            title: "TODAY"
            rows: root.pb.today || []
        }

        PBTable {
            x: 1678
            y: 936
            width: 178
            height: 120
            title: "OVERALL"
            rows: root.pb.overall || []
        }

        SettingsDialog {
            id: settingsDialog
            parent: design
            x: (design.width - width) / 2
            y: 70
        }
    }

    Toasts {
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.margins: 18
        z: 100
    }

    // ---- keyboard shortcuts (mirror the egui GUI) ----
    Shortcut { sequence: "Space"; onActivated: AppBridge.togglePause() }
    Shortcut { sequence: "Left"; onActivated: AppBridge.seekBy(-5) }
    Shortcut { sequence: "Right"; onActivated: AppBridge.seekBy(5) }
    Shortcut { sequence: ","; onActivated: AppBridge.stepBack() }
    Shortcut { sequence: "."; onActivated: AppBridge.stepFrame() }
    Shortcut { sequence: "Up"; onActivated: AppBridge.cycleSpeed(true) }
    Shortcut { sequence: "Down"; onActivated: AppBridge.cycleSpeed(false) }
    Shortcut { sequence: "R"; onActivated: AppBridge.resetLock() }
    Shortcut { sequence: "O"; onActivated: AppBridge.openVideoDialog() }
    Shortcut {
        sequence: "F11"
        onActivated: root.visibility = root.visibility === Window.FullScreen
                     ? Window.Windowed : Window.FullScreen
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
