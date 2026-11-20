import QtQuick

// CategorySumChart.swift: monthly sums of the selected category (incl. subcategories)
// above its bookings (GitHub #502). Pure expense categories are drawn upwards
// (magnitude), mixed ones signed. Hovering a month shows its name and sum.
Item {
    id: chart
    /// [CategoryPeriodSum] from `category_history_by_unit` (key_length 7: `yyyy-MM`)
    property var sums: []
    property int hovered: -1

    /// CategorySumChart.bars: one entry per month with bookings; `slot` counts months
    /// from the first one, so months without bookings stay empty on the time axis.
    readonly property var bars: {
        const out = []
        let first = -1
        for (const s of sums) {
            const y = Number(s.period.slice(0, 4)), m = Number(s.period.slice(5, 7))
            if (!y || !m) continue
            const index = y * 12 + m - 1
            if (first < 0) first = index
            out.push({ year: y, month: m, slot: index - first, cents: s.cents })
        }
        return out
    }
    readonly property int slots: bars.length ? bars[bars.length - 1].slot + 1 : 0
    /// CategorySumChart.flipsSign: an expense-only series shows positive heights.
    readonly property bool flip: bars.length > 0 && bars.every(b => b.cents <= 0)
    function value(b) { return (flip ? -b.cents : b.cents) / 100 }

    /// Value axis with "nice" steps like Swift Charts' automatic marks (0 always included).
    readonly property var axis: {
        let lo = 0, hi = 0
        for (const b of bars) { lo = Math.min(lo, value(b)); hi = Math.max(hi, value(b)) }
        if (lo === hi) hi = lo + 1
        const raw = (hi - lo) / 3
        const mag = Math.pow(10, Math.floor(Math.log10(raw)))
        const step = [1, 2, 2.5, 5, 10].map(f => f * mag).find(s => s >= raw)
        const top = Math.ceil(hi / step) * step, bottom = Math.floor(lo / step) * step
        const ticks = []
        for (let v = bottom; v <= top + step / 2; v += step) ticks.push(Math.round(v * 100) / 100)
        return { lo: bottom, hi: top, ticks: ticks }
    }
    readonly property int axisHeight: Theme.scaled(16)   // holds a caption2 label
    /// Room for the trailing value labels.
    readonly property real labelWidth: {
        let w = 0
        for (const v of axis.ticks) w = Math.max(w, metrics.advanceWidth(tickText(v)))
        return Math.ceil(w) + 6
    }
    readonly property real plotWidth: Math.max(0, width - labelWidth)
    readonly property real plotHeight: Math.max(1, height - axisHeight - 4)
    function yAt(v) { return 2 + plotHeight - (v - axis.lo) / (axis.hi - axis.lo) * plotHeight }
    function slotX(slot) { return slots ? slot * plotWidth / slots : 0 }
    function tickText(v) { return Number(v).toLocaleCurrencyString(Fmt.locale, Fmt.currencySymbol("EUR")).replace(/[.,]00(?=\D*$)/, "") }
    FontMetrics { id: metrics; font.pixelSize: Theme.caption2 }

    onBarsChanged: { hovered = -1; canvas.requestPaint() }
    onWidthChanged: canvas.requestPaint()
    onHeightChanged: canvas.requestPaint()
    onHoveredChanged: canvas.requestPaint()
    Connections { target: Theme; function onDarkChanged() { canvas.requestPaint() } }

    Canvas {
        id: canvas
        anchors.fill: parent
        onPaint: {
            const ctx = getContext("2d")
            ctx.reset()
            if (!chart.bars.length || chart.plotWidth <= 0) return
            // AxisGridLine: dashed, tertiary at 45 %
            ctx.save()
            ctx.strokeStyle = Theme.withAlpha(Theme.tertiary, 0.45); ctx.lineWidth = 0.5
            ctx.setLineDash([2, 3])
            for (const v of chart.axis.ticks) {
                const y = Math.round(chart.yAt(v)) + 0.5
                ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(chart.plotWidth, y); ctx.stroke()
            }
            ctx.restore()
            const slotWidth = chart.plotWidth / chart.slots
            const barWidth = Math.max(1, Math.min(slotWidth * 0.62, 40))
            const zero = chart.yAt(0)
            chart.bars.forEach((b, i) => {
                const v = chart.value(b)
                // expense-only: accent; mixed: outflow below zero, inflow above
                const tint = b.cents < 0 && !chart.flip ? Theme.outflow : (b.cents < 0 ? Theme.accent : Theme.inflow)
                ctx.fillStyle = Theme.withAlpha(tint, chart.hovered < 0 || chart.hovered === i ? 0.85 : 0.35)
                const x = chart.slotX(b.slot) + (slotWidth - barWidth) / 2
                const y = chart.yAt(v)
                const top = Math.min(y, zero), h = Math.max(1, Math.abs(zero - y))
                const r = Math.min(1.5, barWidth / 2, h / 2)
                ctx.beginPath()
                ctx.moveTo(x + r, top); ctx.lineTo(x + barWidth - r, top)
                ctx.arcTo(x + barWidth, top, x + barWidth, top + r, r)
                ctx.lineTo(x + barWidth, top + h - r)
                ctx.arcTo(x + barWidth, top + h, x + barWidth - r, top + h, r)
                ctx.lineTo(x + r, top + h)
                ctx.arcTo(x, top + h, x, top + h - r, r)
                ctx.lineTo(x, top + r)
                ctx.arcTo(x, top, x + r, top, r)
                ctx.fill()
            })
        }
    }
    // chartYAxis: trailing value labels (currency, no decimals)
    Repeater {
        model: chart.bars.length ? chart.axis.ticks : []
        Text {
            required property var modelData
            text: chart.tickText(modelData)
            font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }; color: Theme.tertiary
            x: chart.plotWidth + 4
            y: Math.max(0, Math.min(chart.height - chart.axisHeight - height, chart.yAt(modelData) - height / 2))
            Sensitive { amounts: true }
        }
    }
    // chartXAxis: about six month labels ("MMM yy"), centred under their month
    Repeater {
        model: {
            if (!chart.slots) return []
            const every = Math.max(1, Math.ceil(chart.slots / 6))
            const out = []
            const first = chart.bars[0].year * 12 + chart.bars[0].month - 1
            for (let s = 0; s < chart.slots; s += every) {
                const i = first + s
                out.push({ slot: s, text: Fmt.monthLabel(Math.floor(i / 12) + "-" + String(i % 12 + 1).padStart(2, "0")) })
            }
            return out
        }
        Text {
            required property var modelData
            text: modelData.text
            font.pixelSize: Theme.caption2; color: Theme.tertiary
            y: chart.height - chart.axisHeight + 2
            x: Math.max(0, Math.min(chart.plotWidth - width, chart.slotX(modelData.slot + 0.5) - width / 2))
        }
    }
    // Annotation of the hovered month: month and year, the sum
    Rectangle {
        visible: chart.hovered >= 0
        readonly property var bar: chart.hovered >= 0 ? chart.bars[chart.hovered] : null
        color: Theme.card; border.color: Theme.border; radius: 6
        width: tip.implicitWidth + 14; height: tip.implicitHeight + 8
        x: bar ? Math.max(0, Math.min(chart.plotWidth - width, chart.slotX(bar.slot + 0.5) - width / 2)) : 0
        y: 0
        Column {
            id: tip
            anchors.centerIn: parent
            spacing: 1
            Text { anchors.horizontalCenter: parent.horizontalCenter; font.pixelSize: Theme.caption2; color: Theme.secondary
                   text: parent.parent.bar ? new Date(parent.parent.bar.year, parent.parent.bar.month - 1, 1).toLocaleDateString(Fmt.locale, "MMMM yyyy") : "" }
            Text { anchors.horizontalCenter: parent.horizontalCenter; font.pixelSize: Theme.caption; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                   color: parent.parent.bar && parent.parent.bar.cents < 0 ? Theme.outflow : Theme.text
                   text: parent.parent.bar ? Fmt.money(parent.parent.bar.cents) : ""
                   Sensitive { amounts: true } }
        }
    }
    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        onPositionChanged: m => {
            if (!chart.slots || m.x > chart.plotWidth) { chart.hovered = -1; return }
            const slot = Math.floor(m.x / chart.plotWidth * chart.slots)
            chart.hovered = chart.bars.findIndex(b => b.slot === slot)
        }
        onExited: chart.hovered = -1
    }
}
