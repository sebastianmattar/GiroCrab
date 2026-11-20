import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// TagFilterUI.swift › TagFilterPopover (#228, part 3): tick tags (several = bookings carrying all
// of them) or „Ohne Tag“. Each row shows what ticking it alone would list: its bookings and their
// sum under the other filters (period, accounts, category, status, search). Opened by the tag
// button of the stream header's filter row (StreamHeaderCard.qml), state in TagFilter.qml.
Popup {
    id: pop
    /// [Int64?: TagTotal] keyed by String(tag_id) ("null" = untagged); null while loading.
    property var totals: null
    padding: 12
    // stays inside the window: above the button when there is no room below it (like a popover's arrow edge)
    margins: 8
    // (the height settles once the content is laid out at the popover's width)
    onImplicitHeightChanged: if (visible) place()
    onOpened: place()
    function place() {
        if (!parent) return
        const top = parent.mapToItem(null, 0, 0).y
        const room = parent.Window.height
        y = top + parent.height + 6 + implicitHeight <= room - 8 || top - implicitHeight - 6 < 8 ? parent.height + 6 : -implicitHeight - 6
    }
    width: 340 * Theme.textScale
    // Esc closes it
    focus: true
    // a press on the tag button itself toggles it there (TagFilter.shown)
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
    background: Rectangle { radius: 9; color: Theme.card; border.color: Theme.border }
    // .task { totals = await model.loadTagTotals() }
    onAboutToShow: { place(); totals = null; TagFilter.loadTotals(m => { if (pop.visible) pop.totals = m || {} }) }
    onClosed: TagFilter.shown = false
    /// TagFilter.shown (and the button on screen): opens and closes the popover.
    property bool wanted: false
    onWantedChanged: wanted ? open() : close()
    Component.onCompleted: if (wanted) open()

    contentItem: ColumnLayout {
        spacing: 8
        ColumnLayout {
            spacing: 2
            Layout.fillWidth: true
            Text { text: qsTr("Nach Tags filtern"); font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
            Text {
                Layout.fillWidth: true; wrapMode: Text.WordWrap
                text: qsTr("Mehrere Tags: nur Buchungen, die alle tragen. Anzahl und Summe gelten für den Zeitraum und die übrigen Filter der Liste.")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
        ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(300 * Theme.textScale, tagColumn.implicitHeight)
            contentWidth: availableWidth
            clip: true
            ScrollBar.vertical: ScrollViewBar {}
            ColumnLayout {
                id: tagColumn
                // room for the scroll bar, so it never covers the sums
                width: scroll.availableWidth - (implicitHeight > scroll.height ? 12 : 0)
                spacing: 2
                Repeater {
                    model: TagsModel.tags
                    FilterRow {
                        required property var modelData
                        Layout.fillWidth: true
                        checked: TagFilter.tagIDs.indexOf(modelData.id) >= 0
                        total: pop.totals ? pop.totals[String(modelData.id)] : undefined
                        label: modelData.name
                        sensitiveLabel: true
                        dotColor: modelData.color_hex === "" ? Theme.secondary : Theme.readable(modelData.color_hex)
                        onToggled: TagFilter.toggle(modelData.id)
                    }
                }
            }
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
        FilterRow {
            Layout.fillWidth: true
            checked: TagFilter.untagged
            total: pop.totals ? pop.totals["null"] : undefined
            label: qsTr("Ohne Tag")
            symbol: "tag.slash"
            onToggled: TagFilter.toggleUntagged()
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
        RowLayout {
            Layout.fillWidth: true
            Text {
                visible: TagFilter.isActive
                text: qsTr("Angezeigt: %1 Buchungen").arg(Fmt.count(AppModel.bookingCountLabel))
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Zurücksetzen"); enabled: TagFilter.isActive; onClicked: TagFilter.clear() }
        }
    }

    /// One tickable line: box, label, bookings and sum.
    component FilterRow: Item {
        id: row
        property bool checked: false
        property var total
        property string label
        property bool sensitiveLabel: false
        property color dotColor: "transparent"
        property string symbol: ""
        signal toggled()
        implicitHeight: Math.max(22, rowLayout.implicitHeight + 6)
        // keyboard: Tab / Shift+Tab move between the rows, Space ticks
        activeFocusOnTab: true
        Keys.onSpacePressed: row.toggled()
        Accessible.role: Accessible.CheckBox
        Accessible.name: label
        Accessible.checkable: true
        Accessible.checked: checked
        Accessible.onToggleAction: row.toggled()
        Accessible.onPressAction: row.toggled()
        Rectangle {
            visible: row.activeFocus
            anchors.fill: parent
            radius: 5; color: "transparent"; border.width: 2; border.color: Theme.accent
        }
        RowLayout {
            id: rowLayout
            anchors.fill: parent
            spacing: 8
            // checkmark.square.fill / square
            Rectangle {
                implicitWidth: 14; implicitHeight: 14; radius: 3
                color: row.checked ? Theme.accent : "transparent"
                border.width: row.checked ? 0 : 1.5; border.color: Theme.secondary
                SFIcon { anchors.centerIn: parent; visible: row.checked; symbol: "checkmark"; size: 10; color: "white" }
            }
            Item {
                implicitWidth: 16 * Theme.textScale; implicitHeight: 14
                Rectangle { visible: row.symbol === ""; anchors.centerIn: parent; width: 8; height: 8; radius: 4; color: row.dotColor }
                SFIcon { visible: row.symbol !== ""; anchors.centerIn: parent; symbol: row.symbol || "tag"; size: 12; color: Theme.secondary }
            }
            Text {
                Layout.fillWidth: true
                text: row.label; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight
                Sensitive { when: row.sensitiveLabel }
            }
            Item { implicitWidth: 12 }
            Spinner { visible: pop.totals === null; size: 12 }
            Text {
                visible: pop.totals !== null
                readonly property int count: row.total ? row.total.booking_count : 0
                text: Fmt.count(count)
                font.pixelSize: Theme.callout; font.features: { "tnum": 1 }
                color: count === 0 ? Theme.tertiary : Theme.secondary
            }
            Text {
                visible: pop.totals !== null
                Layout.minimumWidth: 90 * Theme.textScale
                horizontalAlignment: Text.AlignRight
                text: row.total ? Fmt.money(row.total.sum_cents) : "–"
                font.pixelSize: Theme.callout; font.features: { "tnum": 1 }
                color: row.total && row.total.booking_count > 0 ? Theme.text : Theme.tertiary
                Sensitive { amounts: true }
            }
        }
        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: row.toggled() }
    }
}
