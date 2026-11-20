import QtQuick

// PrivacyRedaction.swift `.sensitive()` (#301): put `Sensitive {}` inside an item that shows
// personal data, `Sensitive { amounts: true }` on money (amounts, balances, sums, shares, chart
// axis values and readouts; also text that mixes names and amounts, `[.personal, .amounts]`) and
// `Sensitive { labels: true }` on category, group, budget and security names (hidden only on
// request). While Hilfe › Problem melden captures the main window, in privacy mode (#719,
// PrivacyMode) or with `--redact[=labels]` / `--privacy[=all]` it covers its parent with an opaque box,
// above the parent's own children (a text field's text, a chart's marks). A Text is covered
// only where it draws glyphs, so a label that fills its row does not black out the row.
Rectangle {
    /// `.sensitive(.labels)`
    property bool labels: false
    /// `.sensitive(.amounts)`: hidden by privacy mode „Beträge“ and by every personal redaction.
    property bool amounts: false
    /// `.sensitive([])`: false when there is nothing personal (bank name, empty field).
    property bool when: true

    readonly property bool isText: parent !== null && parent.paintedWidth !== undefined && parent.horizontalAlignment !== undefined
    /// Text that is neither elided nor clipped may draw past its width: cover all of it.
    readonly property bool overflows: isText && !parent.clip && parent.elide === Text.ElideNone
    readonly property real textWidth: !isText ? 0
        : Math.max(6, overflows || parent.width <= 0 ? parent.paintedWidth : Math.min(parent.width, parent.paintedWidth))
    readonly property real textHeight: !isText ? 0
        : Math.max(6, overflows || parent.height <= 0 ? parent.paintedHeight : Math.min(parent.height, parent.paintedHeight))

    visible: when && parent !== null
             && (AppModel.effectiveRedaction & (labels ? AppModel.redactLabels : amounts ? AppModel.redactAmounts : AppModel.redactPersonal)) !== 0
    z: 100000
    radius: 3
    color: Theme.redactionFill
    x: !isText ? 0
       : parent.horizontalAlignment === Text.AlignRight ? parent.width - textWidth
       : parent.horizontalAlignment === Text.AlignHCenter ? (parent.width - textWidth) / 2 : 0
    y: !isText ? 0
       : parent.verticalAlignment === Text.AlignBottom ? parent.height - textHeight
       : parent.verticalAlignment === Text.AlignVCenter ? (parent.height - textHeight) / 2 : 0
    width: isText ? textWidth : (parent ? parent.width : 0)
    height: isText ? textHeight : (parent ? parent.height : 0)
}
