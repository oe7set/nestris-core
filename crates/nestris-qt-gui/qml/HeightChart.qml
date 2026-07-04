import QtQuick

// Stack-height timeline with the state flag strips below: tetris-ready
// (gold), in-drought (red), double-well / clean-slope (blue).
Rectangle {
    /// [[ts, height, flags], ...] — flags bit0 ready, bit1 double well,
    /// bit2 clean slope, bit3 drought.
    property var timeline: []

    onTimelineChanged: canvas.requestPaint()

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
        y: 8
        text: "HEIGHT & STATE"
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 11
    }

    Canvas {
        id: canvas
        anchors.fill: parent
        anchors.margins: 10
        anchors.topMargin: 26
        // Synchronous GUI-thread painting: the default cooperative mode
        // races the threaded scene-graph loop (access violations).
        renderStrategy: Canvas.Immediate
        renderTarget: Canvas.Image

        onPaint: {
            var ctx = getContext("2d")
            ctx.reset()
            var t = timeline
            if (!t || t.length < 2)
                return
            var w = width
            var strips = 12 // three 4-px flag strips at the bottom
            var h = height - strips
            var t0 = t[0][0]
            var t1 = t[t.length - 1][0]
            var span = Math.max(t1 - t0, 1e-6)

            // Height curve (0..20 rows).
            ctx.strokeStyle = "#e8ecf0"
            ctx.lineWidth = 1.5
            ctx.beginPath()
            for (var i = 0; i < t.length; i++) {
                var x = (t[i][0] - t0) / span * w
                var y = h - Math.min(t[i][1], 20) / 20 * h
                if (i === 0)
                    ctx.moveTo(x, y)
                else
                    ctx.lineTo(x, y)
            }
            ctx.stroke()

            // Flag strips.
            for (i = 0; i + 1 < t.length; i++) {
                var x0 = (t[i][0] - t0) / span * w
                var x1 = (t[i + 1][0] - t0) / span * w
                var flags = t[i][2]
                if (flags & 1) { // tetris ready
                    ctx.fillStyle = "#ffd700"
                    ctx.fillRect(x0, h, x1 - x0 + 1, 4)
                }
                if (flags & 8) { // in drought
                    ctx.fillStyle = "#f83800"
                    ctx.fillRect(x0, h + 4, x1 - x0 + 1, 4)
                }
                if (flags & 6) { // double well / clean slope
                    ctx.fillStyle = "#3cbcfc"
                    ctx.fillRect(x0, h + 8, x1 - x0 + 1, 4)
                }
            }
        }
    }
}
