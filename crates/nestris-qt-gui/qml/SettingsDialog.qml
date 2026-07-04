import QtQuick
import QtQuick.Controls
import at.retroverse.nestris.core

// Schema-driven settings dialog covering every persisted knob (mirrors
// the egui settings window / the web GUI's schema form).
Popup {
    id: dialog

    property var cfg: ({})
    property int cfgRev: 0
    property string error: ""

    width: 760
    height: 920
    modal: true
    focus: true
    closePolicy: Popup.CloseOnEscape

    // Load once up front so the form bindings never see an empty config,
    // and refresh on every open to pick up externally changed values.
    Component.onCompleted: cfg = JSON.parse(AppBridge.settingsJson())

    onOpened: {
        cfg = JSON.parse(AppBridge.settingsJson())
        cfgRev++
        error = ""
    }

    function getVal(path) {
        var v = cfg
        var parts = path.split(".")
        for (var i = 0; i < parts.length; i++)
            v = v[parts[i]]
        return v
    }

    function setVal(path, value) {
        var o = cfg
        var parts = path.split(".")
        for (var i = 0; i < parts.length - 1; i++)
            o = o[parts[i]]
        o[parts[parts.length - 1]] = value
    }

    // type: "b" bool, "f" float slider, "i" integer field, "e" enum,
    // "s" string field, "u" the undistort auto/off checkbox.
    readonly property var schema: [
        { section: "CALIBRATION" },
        { path: "engine.calibration.acquire_threshold", label: "Acquire threshold", type: "f", min: 0, max: 1 },
        { path: "engine.calibration.drift_threshold", label: "Drift threshold", type: "f", min: 0, max: 1 },
        { path: "engine.calibration.lost_frames", label: "Lost after weak frames", type: "i" },
        { path: "engine.calibration.acquire_frames", label: "Frames to confirm acquire", type: "i" },
        { path: "engine.calibration.smooth_alpha", label: "Geometry smoothing (EMA)", type: "f", min: 0, max: 1 },
        { path: "engine.calibration.adopt_margin", label: "Re-solve adopt margin", type: "f", min: 0, max: 0.5 },
        { path: "engine.calibration.undistort", label: "Barrel undistortion (auto)", type: "u" },
        { path: "engine.calibration.background_recalibration", label: "Background recalibration", type: "b" },
        { path: "engine.calibration.menu_drift_hold", label: "Hold lock through menus", type: "b" },
        { section: "FUSION" },
        { path: "engine.fusion.vote_window", label: "Vote window (frames)", type: "i" },
        { path: "engine.fusion.confidence_decay", label: "Confidence decay", type: "f", min: 0.5, max: 1 },
        { path: "engine.fusion.min_report_confidence", label: "Min report confidence", type: "f", min: 0, max: 1 },
        { path: "engine.fusion.enforce_monotonic", label: "Enforce monotonic score/lines/level", type: "b" },
        { path: "engine.fusion.new_game_menu_frames", label: "Menu frames to arm new game", type: "i" },
        { section: "PLAUSIBILITY" },
        { path: "engine.plausibility.enabled", label: "Enabled (NES-rules guard)", type: "b" },
        { path: "engine.plausibility.max_score_jump", label: "Max score jump", type: "i" },
        { path: "engine.plausibility.max_lines_step", label: "Max lines step (contiguous)", type: "i" },
        { path: "engine.plausibility.max_lines_skip", label: "Max lines skip (across gaps)", type: "i" },
        { path: "engine.plausibility.level_tolerance", label: "Level tolerance", type: "i" },
        { path: "engine.plausibility.confirm_frames", label: "Self-heal after frames", type: "i" },
        { section: "RECOGNITION" },
        { path: "engine.recognition.score_base", label: "Score base", type: "e", options: ["auto", "dec", "hex"] },
        { path: "engine.recognition.score_base_latch_frames", label: "Base latch frames", type: "i" },
        { path: "engine.recognition.read_statistics", label: "Read STATISTICS rail", type: "b" },
        { path: "engine.recognition.statistics_every_n", label: "STATISTICS every N frames", type: "i" },
        { path: "engine.recognition.read_current_piece", label: "Track current piece", type: "b" },
        { path: "engine.recognition.freeze_on_clear_animation", label: "Freeze during clear animation", type: "b" },
        { path: "engine.recognition.playfield_stabilizer", label: "Playfield stabilizer", type: "b" },
        { section: "TRACKING" },
        { path: "engine.tracking.enabled", label: "Continuous geometry tracking", type: "b" },
        { path: "engine.tracking.search_radius_px", label: "Label search radius (px)", type: "f", min: 4, max: 16, integer: true },
        { path: "engine.tracking.damping", label: "Correction damping", type: "f", min: 0.1, max: 1 },
        { section: "OUTPUT" },
        { path: "record_enabled", label: "Record games (.ngf.gz)", type: "b" },
        { path: "record_dir", label: "Recording directory", type: "s", hint: "Documents\\nestris-recordings" },
        { path: "ws_enabled", label: "WebSocket broadcast", type: "b" },
        { path: "ws_addr", label: "WebSocket address", type: "s", hint: "127.0.0.1:8765" },
        { path: "jsonl_enabled", label: "JSONL file", type: "b" },
        { path: "jsonl_path", label: "JSONL path", type: "s", hint: "out.jsonl" }
    ]

    FontLoader {
        id: pixelFont
        source: "qrc:/nestris/assets/fonts/PressStart2P-Regular.ttf"
    }

    background: Rectangle {
        color: "#0d1016"
        border.color: "#1c2430"
        border.width: 2
        radius: 6
    }

    contentItem: Column {
        spacing: 10

        Text {
            text: "SETTINGS"
            color: "#3cbcfc"
            font.family: pixelFont.name
            font.pixelSize: 18
        }

        Flickable {
            width: parent.width
            height: dialog.height - 150
            contentHeight: form.height
            clip: true

            Column {
                id: form
                width: parent.width - 16
                spacing: 6

                Repeater {
                    model: dialog.schema

                    Loader {
                        required property var modelData
                        width: form.width
                        sourceComponent: modelData.section !== undefined ? sectionRow
                                       : modelData.type === "b" ? boolRow
                                       : modelData.type === "u" ? undistortRow
                                       : modelData.type === "f" ? sliderRow
                                       : modelData.type === "e" ? enumRow
                                       : textRow

                        property var spec: modelData
                    }
                }
            }
        }

        Text {
            visible: dialog.error.length > 0
            text: dialog.error
            color: "#f83800"
            font.family: pixelFont.name
            font.pixelSize: 10
        }

        Row {
            spacing: 12

            RetroButton {
                label: "APPLY & SAVE"
                accent: true
                onClicked: {
                    var err = AppBridge.applySettings(JSON.stringify(dialog.cfg))
                    if (err.length > 0) {
                        dialog.error = err
                    } else {
                        dialog.error = ""
                        dialog.close()
                    }
                }
            }
            RetroButton {
                label: "RESET TO DEFAULTS"
                onClicked: {
                    dialog.cfg = JSON.parse(AppBridge.resetSettings())
                    dialog.cfgRev++
                }
            }
            RetroButton {
                label: "CLOSE"
                onClicked: dialog.close()
            }
        }
    }

    Component {
        id: sectionRow

        Item {
            width: form.width
            height: 34

            Text {
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 4
                text: spec.section
                color: "#ffd700"
                font.family: pixelFont.name
                font.pixelSize: 12
            }
        }
    }

    Component {
        id: boolRow

        Item {
            width: form.width
            height: 26

            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: spec.label
                color: "#9aa3ad"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            Rectangle {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: 22
                height: 22
                radius: 3
                color: "#10141a"
                border.width: 2
                border.color: { dialog.cfgRev; return dialog.getVal(spec.path) ? "#3cbcfc" : "#1c2430" }

                Text {
                    anchors.centerIn: parent
                    text: { dialog.cfgRev; return dialog.getVal(spec.path) ? "x" : "" }
                    color: "#3cbcfc"
                    font.family: pixelFont.name
                    font.pixelSize: 12
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: {
                        dialog.setVal(spec.path, !dialog.getVal(spec.path))
                        dialog.cfgRev++
                    }
                }
            }
        }
    }

    Component {
        id: undistortRow

        Item {
            width: form.width
            height: 26

            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: spec.label
                color: "#9aa3ad"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            Rectangle {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: 22
                height: 22
                radius: 3
                color: "#10141a"
                border.width: 2
                border.color: { dialog.cfgRev; return dialog.getVal(spec.path) === "auto" ? "#3cbcfc" : "#1c2430" }

                Text {
                    anchors.centerIn: parent
                    text: { dialog.cfgRev; return dialog.getVal(spec.path) === "auto" ? "x" : "" }
                    color: "#3cbcfc"
                    font.family: pixelFont.name
                    font.pixelSize: 12
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: {
                        dialog.setVal(spec.path,
                                      dialog.getVal(spec.path) === "auto" ? "off" : "auto")
                        dialog.cfgRev++
                    }
                }
            }
        }
    }

    Component {
        id: sliderRow

        Item {
            width: form.width
            height: 30

            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: spec.label
                color: "#9aa3ad"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            Text {
                anchors.right: slider.left
                anchors.rightMargin: 10
                anchors.verticalCenter: parent.verticalCenter
                text: { dialog.cfgRev; var v = dialog.getVal(spec.path); return spec.integer ? String(v) : Number(v).toFixed(2) }
                color: "#e8ecf0"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            Slider {
                id: slider
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: 220
                from: spec.min
                to: spec.max
                stepSize: spec.integer ? 1 : 0
                value: { dialog.cfgRev; return dialog.getVal(spec.path) }
                onMoved: dialog.setVal(spec.path, spec.integer ? Math.round(value) : value)

                background: Rectangle {
                    x: slider.leftPadding
                    y: slider.topPadding + slider.availableHeight / 2 - height / 2
                    width: slider.availableWidth
                    height: 4
                    radius: 2
                    color: "#1c2430"

                    Rectangle {
                        width: slider.visualPosition * parent.width
                        height: parent.height
                        radius: 2
                        color: "#3cbcfc"
                    }
                }

                handle: Rectangle {
                    x: slider.leftPadding + slider.visualPosition * (slider.availableWidth - width)
                    y: slider.topPadding + slider.availableHeight / 2 - height / 2
                    width: 10
                    height: 16
                    radius: 2
                    color: "#e8ecf0"
                }
            }
        }
    }

    Component {
        id: enumRow

        Item {
            width: form.width
            height: 30

            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: spec.label
                color: "#9aa3ad"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            Row {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                spacing: 6

                Repeater {
                    model: spec.options

                    Rectangle {
                        required property string modelData
                        width: optText.implicitWidth + 16
                        height: 24
                        radius: 3
                        color: "#10141a"
                        border.width: 2
                        border.color: { dialog.cfgRev; return dialog.getVal(spec.path) === modelData ? "#3cbcfc" : "#1c2430" }

                        Text {
                            id: optText
                            anchors.centerIn: parent
                            text: parent.modelData.toUpperCase()
                            color: { dialog.cfgRev; return dialog.getVal(spec.path) === parent.modelData ? "#3cbcfc" : "#9aa3ad" }
                            font.family: pixelFont.name
                            font.pixelSize: 9
                        }

                        MouseArea {
                            anchors.fill: parent
                            onClicked: {
                                dialog.setVal(spec.path, parent.modelData)
                                dialog.cfgRev++
                            }
                        }
                    }
                }
            }
        }
    }

    Component {
        id: textRow

        Item {
            width: form.width
            height: 32

            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: spec.label
                color: "#9aa3ad"
                font.family: pixelFont.name
                font.pixelSize: 10
            }

            TextField {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: 320
                height: 28
                text: { dialog.cfgRev; var v = dialog.getVal(spec.path); return v === null ? "" : String(v) }
                placeholderText: spec.hint || ""
                color: "#e8ecf0"
                placeholderTextColor: "#2a333d"
                font.family: pixelFont.name
                font.pixelSize: 9
                onTextEdited: dialog.setVal(spec.path,
                                            spec.type === "i" ? parseInt(text) || 0 : text)

                background: Rectangle {
                    color: "#10141a"
                    border.color: parent.activeFocus ? "#3cbcfc" : "#1c2430"
                    border.width: 2
                    radius: 3
                }
            }
        }
    }
}
