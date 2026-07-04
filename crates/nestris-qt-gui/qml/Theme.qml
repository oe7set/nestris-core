import QtQuick

// Retro dashboard palette shared by every component. Instantiate once per
// window: `Theme { id: theme }`.
QtObject {
    readonly property color bg: "#0a0c10"
    readonly property color panel: "#10141a"
    readonly property color panelEdge: "#1c2430"
    readonly property color text: "#e8ecf0"
    readonly property color dim: "#9aa3ad"
    readonly property color grid: "#2a333d"
    readonly property color accent: "#3cbcfc"
    readonly property color gold: "#ffd700"
    readonly property color good: "#58d854"
    readonly property color warn: "#fc9838"
    readonly property color bad: "#f83800"
}
