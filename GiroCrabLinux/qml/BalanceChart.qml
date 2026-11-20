import QtQuick
import QtQuick.Controls

// BalanceHeaderView.swift › BalanceChart: balance over the selected period as a line
// (every day, soft area fill) or as bars (BalanceBars.swift: average balance per day,
// month or year, GitHub #203), chosen in Einstellungen › Allgemein › Saldoverlauf or by
// right-clicking the chart. Dashed rules mark each turn of the year, a slim trailing
// axis shows zero, maximum and minimum; hovering shows a readout, a click jumps to the
// day (bars: the last day of the bar). Bars are drawn on a Canvas like CategorySumChart.qml.
Item {
    id: chart
    property var points: []
    signal selectDay(string day)
    /// Hovered day (line) or bar (bars), -1 = none.
    property int hovered: -1

    readonly property bool isBars: BalanceChartPrefs.style === "bars"
    /// BalanceBarUnit.resolved(dayCount:)
    readonly property string unit: BalanceChartPrefs.resolvedUnit(points.length)
    readonly property var bars: isBars ? BalanceChartPrefs.buckets(points, unit) : []
    /// Values on screen, in cents: the days (line) or the bar averages.
    readonly property int count: isBars ? bars.length : points.length
    function valueAt(i) { return isBars ? bars[i].average : points[i].balance_cents }

    /// Value domain and axis marks: the line has 15 % room above and below (at least 1 €);
    /// bars start at zero and get 8 % room (lineChart / barChart in BalanceHeaderView.swift).
    readonly property var stats: {
        let lo = Infinity, hi = -Infinity
        for (let i = 0; i < count; i++) { const v = valueAt(i); lo = Math.min(lo, v); hi = Math.max(hi, v) }
        if (!count) { lo = 0; hi = 100 }
        let dlo, dhi
        if (isBars) {
            const l = Math.min(0, lo), h = Math.max(0, hi)
            const pad = Math.max((h - l) * 0.08, 100)
            dlo = l - (l < 0 ? pad : 0); dhi = h + pad
        } else {
            const pad = Math.max((hi - lo) * 0.15, 100)
            dlo = lo - pad; dhi = hi + pad
        }
        // axisValues(min:max:domain:): zero first, then max and min, each only if it keeps
        // a fifth of the height away from the labels already placed.
        const gap = (dhi - dlo) * 0.2
        const ticks = []
        for (const v of [0, hi, lo])
            if (v >= dlo && v <= dhi && ticks.every(t => Math.abs(t - v) >= gap)) ticks.push(v)
        return { lo: dlo, hi: dhi, ticks: ticks }
    }
    /// compactEuros: "4.820 €", "-850 €", "0 €", "1,2 Mio. €" – three significant digits.
    function compactEuros(cents) {
        const e = cents / 100
        const sig = (x) => {
            if (x === 0) return { v: 0, decimals: 0 }
            const digits = Math.floor(Math.log10(Math.abs(x))) + 1
            const f = Math.pow(10, digits - 3)
            const v = Math.round(x / f) * f
            return { v: v, decimals: Math.max(0, 3 - digits) }
        }
        const trim = (s) => s.indexOf(Fmt.locale.decimalPoint) >= 0 ? s.replace(new RegExp("\\" + Fmt.locale.decimalPoint + "?0+$"), "") : s
        if (Math.abs(e) < 1e6) {
            const r = sig(e)
            return trim(Number(r.v).toLocaleString(Fmt.locale, "f", r.decimals)) + " €"
        }
        const r = sig(e / 1e6)
        return trim(Number(r.v).toLocaleString(Fmt.locale, "f", r.decimals)) + (Fmt.english ? "M" : " Mio.") + " €"
    }
    FontMetrics { id: metrics; font.pixelSize: Theme.caption2; font.features: { "tnum": 1 } }
    /// Fixed minimum width (50 pt × text size), so changing values never shift the plot.
    readonly property real labelWidth: {
        let w = 50 * Theme.textScale
        for (const v of stats.ticks) w = Math.max(w, metrics.advanceWidth(compactEuros(v)))
        return Math.ceil(w) + 6
    }
    readonly property real plotWidth: Math.max(0, width - labelWidth)
    readonly property int axisHeight: Theme.scaled(16)   // holds a caption2 label
    readonly property real plotHeight: Math.max(1, height - axisHeight - 4)
    function yAt(c) { return 2 + plotHeight - (c - stats.lo) / (stats.hi - stats.lo) * plotHeight }
    /// Line: x of day i. Bars: left edge of bar i (`slot` wide).
    function xAt(i) { return isBars ? i * slot : (points.length < 2 ? 0 : i / (points.length - 1) * plotWidth) }
    readonly property real slot: bars.length ? plotWidth / bars.length : 0
    /// Centre of the hovered element (readout and rule).
    function centerX(i) { return isBars ? xAt(i) + slot / 2 : xAt(i) }

    /// Points actually drawn: with more days than pixels the balance is averaged per
    /// ~2 px bucket (first and last value kept), so 17 years of daily balances stay
    /// smooth and cheap. Each entry: { x, y } in pixels.
    readonly property var drawn: {
        const n = points.length
        if (isBars || n < 2 || plotWidth <= 0) return []
        const buckets = Math.max(2, Math.floor(plotWidth / 2))
        const out = []
        if (n <= buckets) {
            for (let i = 0; i < n; i++) out.push({ x: xAt(i), y: yAt(points[i].balance_cents) })
            return out
        }
        out.push({ x: 0, y: yAt(points[0].balance_cents) })
        for (let b = 1; b < buckets - 1; b++) {
            const from = Math.floor(b * n / buckets), to = Math.floor((b + 1) * n / buckets)
            let sum = 0
            for (let i = from; i < to; i++) sum += points[i].balance_cents
            out.push({ x: xAt((from + to - 1) / 2), y: yAt(sum / Math.max(1, to - from)) })
        }
        out.push({ x: plotWidth, y: yAt(points[n - 1].balance_cents) })
        return out
    }
    /// Monotone cubic curve through `pts` (Fritsch–Carlson, like Swift Charts'
    /// .interpolationMethod(.monotone)): smooth, but never overshoots the values.
    function traceMonotone(ctx, pts) {
        const n = pts.length
        ctx.moveTo(pts[0].x, pts[0].y)
        if (n === 2) { ctx.lineTo(pts[1].x, pts[1].y); return }
        const d = [], m = []
        for (let i = 0; i < n - 1; i++) {
            const dx = pts[i + 1].x - pts[i].x
            d.push(dx === 0 ? 0 : (pts[i + 1].y - pts[i].y) / dx)
        }
        m.push(d[0])
        for (let i = 1; i < n - 1; i++) m.push(d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2)
        m.push(d[n - 2])
        for (let i = 0; i < n - 1; i++) {
            if (d[i] === 0) { m[i] = 0; m[i + 1] = 0; continue }
            const a = m[i] / d[i], b = m[i + 1] / d[i], h = a * a + b * b
            if (h > 9) { const t = 3 / Math.sqrt(h); m[i] = t * a * d[i]; m[i + 1] = t * b * d[i] }
        }
        for (let i = 0; i < n - 1; i++) {
            const dx = (pts[i + 1].x - pts[i].x) / 3
            ctx.bezierCurveTo(pts[i].x + dx, pts[i].y + m[i] * dx, pts[i + 1].x - dx, pts[i + 1].y - m[i + 1] * dx, pts[i + 1].x, pts[i + 1].y)
        }
    }
    /// The ISO date that starts element i (day or bar).
    function dateAt(i) { return isBars ? bars[i].start : points[i].date }

    // .onChange(of: points / style / barUnit) { clearHover() }
    onPointsChanged: hovered = -1
    onIsBarsChanged: hovered = -1
    onUnitChanged: hovered = -1
    onDrawnChanged: canvas.requestPaint()
    onBarsChanged: canvas.requestPaint()
    onStatsChanged: canvas.requestPaint()
    onHoveredChanged: canvas.requestPaint()
    onWidthChanged: canvas.requestPaint()
    onHeightChanged: canvas.requestPaint()
    Connections { target: Theme; function onDarkChanged() { canvas.requestPaint() } }

    Canvas {
        id: canvas
        anchors.fill: parent
        onPaint: {
            const ctx = getContext("2d")
            ctx.reset()
            if (chart.plotWidth <= 0 || (chart.isBars ? chart.bars.length < 1 : chart.drawn.length < 2)) return
            const base = chart.height - chart.axisHeight
            // yAxis: AxisGridLine per mark, zero solid, the others dashed
            for (const v of chart.stats.ticks) {
                ctx.save()
                const zero = v === 0
                ctx.strokeStyle = Theme.withAlpha(Theme.tertiary, zero ? 0.7 : 0.45); ctx.lineWidth = zero ? 0.75 : 0.5
                if (!zero) ctx.setLineDash([2, 3])
                const y = Math.round(chart.yAt(v)) + 0.5
                ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(chart.plotWidth, y); ctx.stroke()
                ctx.restore()
            }
            // RuleMark per new year (dashed), BalanceBuckets.yearStarts; yearly bars: every
            // gap already is a turn of the year
            if (!(chart.isBars && chart.unit === "year")) {
                ctx.save()
                ctx.strokeStyle = Theme.withAlpha(Theme.tertiary, chart.isBars ? 0.6 : 0.5); ctx.lineWidth = 1
                ctx.setLineDash([2, 3])
                for (let i = 1; i < chart.count; i++) {
                    if (chart.dateAt(i).slice(0, 4) !== chart.dateAt(i - 1).slice(0, 4)) {
                        const x = Math.round(chart.xAt(i)) + 0.5
                        ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, base); ctx.stroke()
                    }
                }
                ctx.restore()
            }
            if (chart.isBars) {
                // BarMark: accent, outflow below zero; the hovered bar stands out
                const w = Math.max(1, chart.slot * 0.7)
                const zero = chart.yAt(0)
                chart.bars.forEach((b, i) => {
                    const tint = b.average < 0 ? Theme.outflow : Theme.accent
                    ctx.fillStyle = Theme.withAlpha(tint, chart.hovered < 0 || chart.hovered === i ? 0.85 : 0.35)
                    const x = chart.xAt(i) + (chart.slot - w) / 2
                    const y = chart.yAt(b.average)
                    const top = Math.min(y, zero), h = Math.max(1, Math.abs(zero - y))
                    const r = Math.min(1.5, w / 2, h / 2)
                    ctx.beginPath()
                    ctx.moveTo(x + r, top); ctx.lineTo(x + w - r, top)
                    ctx.arcTo(x + w, top, x + w, top + r, r)
                    ctx.lineTo(x + w, top + h - r)
                    ctx.arcTo(x + w, top + h, x + w - r, top + h, r)
                    ctx.lineTo(x + r, top + h)
                    ctx.arcTo(x, top + h, x, top + h - r, r)
                    ctx.lineTo(x, top + r)
                    ctx.arcTo(x, top, x + r, top, r)
                    ctx.fill()
                })
                return
            }
            const line = chart.drawn
            ctx.beginPath()
            chart.traceMonotone(ctx, line)
            ctx.lineTo(chart.plotWidth, base); ctx.lineTo(0, base); ctx.closePath()
            const g = ctx.createLinearGradient(0, 0, 0, base)
            g.addColorStop(0, Theme.withAlpha(Theme.accent, 0.14)); g.addColorStop(1, Theme.withAlpha(Theme.accent, 0.01))
            ctx.fillStyle = g; ctx.fill()
            ctx.beginPath()
            chart.traceMonotone(ctx, line)
            ctx.strokeStyle = Theme.withAlpha(Theme.accent, 0.8); ctx.lineWidth = 1.5; ctx.stroke()
            if (chart.hovered >= 0 && chart.hovered < chart.points.length) {
                const x = chart.xAt(chart.hovered)
                ctx.strokeStyle = Theme.withAlpha(Theme.secondary, 0.45); ctx.lineWidth = 1
                ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, base); ctx.stroke()
                ctx.fillStyle = Theme.accent
                ctx.beginPath(); ctx.arc(x, chart.yAt(chart.points[chart.hovered].balance_cents), 3.5, 0, 2 * Math.PI); ctx.fill()
            }
        }
    }
    // yAxis: trailing labels (compact euros, caption2, tertiary)
    Repeater {
        model: chart.count ? chart.stats.ticks : []
        Text {
            required property var modelData
            text: chart.compactEuros(modelData)
            font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }; color: Theme.tertiary
            x: chart.plotWidth + 4
            y: Math.max(0, Math.min(chart.height - chart.axisHeight - height, chart.yAt(modelData) - height / 2))
            Sensitive { amounts: true }
        }
    }
    // x-axis like BalanceHeaderView.swift › xAxis: marks on calendar boundaries –
    // years (> 400 days), days (< 62 days) or months, thinned so labels never crowd.
    // Bars of a month or year carry their label in the middle (`centered`).
    function axisStride(units, labelWidth, steps) {
        const fit = Math.max(plotWidth / labelWidth, 1)
        for (const s of steps) if (units / s <= fit) return s
        return steps[steps.length - 1]
    }
    readonly property var axisMarks: {
        const n = count
        if (n < (isBars ? 1 : 2) || plotWidth <= 0) return []
        const first = Fmt.date(dateAt(0)), last = Fmt.date(isBars ? bars[n - 1].last : points[n - 1].date)
        const span = (last - first) / 86400000
        const centered = isBars && unit !== "day"
        // the label of a unit starting at element i, centred up to the next change of `len`
        const mark = (i, len, text, centre) => {
            if (!centre) return { x: centerX(i), text: text }
            let j = i + 1
            while (j < n && dateAt(j).slice(0, len) === dateAt(i).slice(0, len)) j++
            return { x: (xAt(i) + (j < n ? xAt(j) : plotWidth)) / 2, text: text }
        }
        const marks = []
        if (span > 400) {
            const every = axisStride(span / 365.25, 64 * Theme.textScale, [1, 2, 5, 10, 20])
            for (let i = 1; i < n; i++) {
                const y = Number(dateAt(i).slice(0, 4))
                if (y !== Number(dateAt(i - 1).slice(0, 4)) && y % every === 0)
                    marks.push(mark(i, 4, String(y), centered && every === 1))
            }
        } else if (span < 62) {
            const every = axisStride(span, 64 * Theme.textScale, [7, 14, 28])
            for (let i = 0; i < n; i += every) marks.push(mark(i, 10, Fmt.shortDay(Fmt.date(dateAt(i))), false))
        } else {
            const every = axisStride(span / 30.44, 44 * Theme.textScale, [1, 2, 3, 6])
            for (let i = 1; i < n; i++) {
                const m = Number(dateAt(i).slice(5, 7))
                if (m !== Number(dateAt(i - 1).slice(5, 7)) && (m - 1) % every === 0)
                    marks.push(mark(i, 7, Qt.locale().monthName(m - 1, Locale.ShortFormat), centered && every === 1))
            }
        }
        return marks
    }
    Repeater {
        model: chart.axisMarks
        Text {
            required property var modelData
            text: modelData.text
            font.pixelSize: Theme.caption; color: Theme.tertiary
            y: chart.height - chart.axisHeight + 2
            x: Math.max(0, Math.min(chart.plotWidth - width, modelData.x - width / 2))
        }
    }
    /// barTitle: "2026", "März 2026", "Mo., 3. März 2026"; the line names the day the same way.
    function title(i) {
        if (i < 0 || i >= count) return ""
        const d = Fmt.date(dateAt(i))
        if (isBars && unit === "year") return String(d.getFullYear())
        if (isBars && unit === "month") return d.toLocaleDateString(Fmt.locale, "MMMM yyyy")
        return d.toLocaleDateString(Fmt.locale, "ddd, " + Fmt.pattern("d. MMM yyyy"))
    }
    // readout(_:prefix:cents:): title above, amount below (outflow colour if negative),
    // left of the pointer in the right half, else right of it.
    Rectangle {
        id: readout
        visible: chart.hovered >= 0 && chart.hovered < chart.count
        readonly property int cents: visible ? chart.valueAt(chart.hovered) : 0
        color: Theme.card; border.color: Theme.border; radius: 6
        width: tip.implicitWidth + 14; height: tip.implicitHeight + 8
        x: {
            if (!visible) return 0
            const c = chart.centerX(chart.hovered)
            const want = chart.hovered * 2 > chart.count ? c - width - 4 : c + 4
            return Math.max(0, Math.min(chart.plotWidth - width, want))
        }
        y: 0
        Column {
            id: tip
            anchors.centerIn: parent
            spacing: 1
            Text { anchors.horizontalCenter: parent.horizontalCenter; font.pixelSize: Theme.caption2; color: Theme.secondary
                   text: readout.visible ? chart.title(chart.hovered) : "" }
            Text { anchors.horizontalCenter: parent.horizontalCenter; font.pixelSize: Theme.caption; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                   color: readout.cents < 0 ? Theme.outflow : Theme.text
                   text: readout.visible ? (chart.isBars ? "Ø " : "") + Fmt.money(readout.cents) : ""
                   Sensitive { amounts: true } }
        }
    }
    Accessible.role: Accessible.Chart
    Accessible.name: qsTr("Saldoverlauf")
    Accessible.description: hovered < 0 || hovered >= count || (AppModel.effectiveRedaction & AppModel.redactAmounts) !== 0 ? ""
        : isBars ? qsTr("%1: Ø %2, Ende %3").arg(title(hovered)).arg(Fmt.money(bars[hovered].average)).arg(Fmt.money(bars[hovered].closing))
                 : title(hovered) + ": " + Fmt.money(points[hovered].balance_cents)
    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        function index(x) {
            if (!chart.count) return -1
            if (chart.isBars) return Math.max(0, Math.min(chart.count - 1, Math.floor(x / Math.max(1, chart.slot))))
            return Math.max(0, Math.min(chart.count - 1, Math.round(x / Math.max(1, chart.plotWidth) * (chart.count - 1))))
        }
        onPositionChanged: m => chart.hovered = index(m.x)
        onExited: chart.hovered = -1
        onClicked: m => {
            if (m.button === Qt.RightButton) { styleMenu.popup(); return }
            const i = index(m.x)
            if (i >= 0) chart.selectDay(chart.isBars ? chart.bars[i].last : chart.points[i].date)
        }
    }
    // styleMenu: right click switches between line and bars right at the chart
    AppMenu {
        id: styleMenu
        Repeater {
            model: BalanceChartPrefs.styles
            AppMenuItem { required property var modelData; text: modelData.title; checkable: true
                          checked: BalanceChartPrefs.style === modelData.id; onTriggered: BalanceChartPrefs.setStyle(modelData.id) }
        }
        AppMenuSeparator { visible: chart.isBars; height: visible ? implicitHeight : 0 }
        Repeater {
            model: chart.isBars ? BalanceChartPrefs.units : []
            AppMenuItem { required property var modelData; text: modelData.title; checkable: true
                          checked: BalanceChartPrefs.barUnit === modelData.id; onTriggered: BalanceChartPrefs.setBarUnit(modelData.id) }
        }
    }
}
