import QtQuick

// Tetris-rate trend vs total lines (gold), hand-drawn like the other GUIs.
Rectangle {
    /// [[lines, rate], ...]
    property var trend: []

    onTrendChanged: canvas.requestPaint()

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
        text: "TRT TREND"
        color: "#9aa3ad"
        font.family: pixelFont.name
        font.pixelSize: 12
    }

    Canvas {
        id: canvas
        anchors.fill: parent
        anchors.margins: 12
        anchors.topMargin: 32
        // Synchronous GUI-thread painting: the default cooperative mode
        // races the threaded scene-graph loop (access violations).
        renderStrategy: Canvas.Immediate
        renderTarget: Canvas.Image

        onPaint: {
            var ctx = getContext("2d")
            ctx.reset()
            var w = width
            var h = height

            // Grid: 25% steps.
            ctx.strokeStyle = "#2a333d"
            ctx.lineWidth = 1
            for (var g = 1; g < 4; g++) {
                var gy = h * g / 4
                ctx.beginPath()
                ctx.moveTo(0, gy)
                ctx.lineTo(w, gy)
                ctx.stroke()
            }

            var t = trend
            if (!t || t.length < 2)
                return
            var maxLines = Math.max(10, t[t.length - 1][0])
            ctx.strokeStyle = "#ffd700"
            ctx.lineWidth = 2
            ctx.beginPath()
            for (var i = 0; i < t.length; i++) {
                var x = t[i][0] / maxLines * w
                var y = h - Math.min(Math.max(t[i][1], 0), 1) * h
                if (i === 0)
                    ctx.moveTo(x, y)
                else
                    ctx.lineTo(x, y)
            }
            ctx.stroke()
        }
    }
}
