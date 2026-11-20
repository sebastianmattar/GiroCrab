import QtQuick
import QtQuick.Layouts

// LabeledContent in a grouped form: title (and an optional detail line) on the
// left, the value or control on the right. `splitValue` (Settings window): label and
// value share the width 50/50, the value wraps instead of widening the row.
RowLayout {
    id: r
    property string label
    property string detail
    /// Plain value text (`LabeledContent(_, value:)`); controls go in as children.
    property string value
    property color valueColor: Theme.secondary
    /// The split value can be selected with the mouse (.textSelection(.enabled)).
    property bool selectable: false
    property bool splitValue: false
    /// The control on the right takes all the width the label leaves (a borderless text
    /// field in a grouped form, FormTextField).
    property bool fillValue: false
    /// `value` is money (`.sensitive(.amounts)`): covered in privacy mode and in the bug report.
    property bool amounts: false
    default property alias trailing: trail.data
    Layout.fillWidth: true
    spacing: splitValue ? 12 : 10
    ColumnLayout {
        spacing: 1
        Layout.fillWidth: !r.fillValue
        Layout.preferredWidth: r.splitValue ? 200 : -1
        Text { text: r.label; color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true; wrapMode: Text.WordWrap }
        Text { visible: r.detail !== ""; text: r.detail; color: Theme.secondary; font.pixelSize: Theme.caption; Layout.fillWidth: true; wrapMode: Text.WordWrap }
    }
    TextEdit {
        visible: r.splitValue && r.value !== ""
        Layout.fillWidth: true; Layout.preferredWidth: 200
        text: r.value
        readOnly: true; selectByMouse: r.selectable
        font.pixelSize: Theme.body; color: r.valueColor
        horizontalAlignment: Text.AlignRight
        wrapMode: Text.Wrap
        Sensitive { amounts: true; when: r.amounts }
    }
    RowLayout {
        id: trail
        spacing: r.splitValue ? 6 : 8
        Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
        Layout.fillWidth: r.fillValue
        Text { visible: !r.splitValue && r.value !== ""; text: r.value; color: r.valueColor; font.pixelSize: Theme.body; font.features: { "tnum": 1 }
               Sensitive { amounts: true; when: r.amounts } }
    }
}
