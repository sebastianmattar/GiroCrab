import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// CommandPalette.swift › CategorizerSheet: "C" – pick a category for the
// selected booking(s) (type to filter).
Sheet {
    id: sheet
    visible: AppModel.showCategorizer
    sheetWidth: 560
    padding: 20
    onAboutToShow: { query.text = ""; query.forceActiveFocus() }
    onClosed: AppModel.showCategorizer = false

    readonly property bool multi: AppModel.isMultiSelection
    readonly property var targets: multi ? AppModel.selectedBookings : (AppModel.selectedBooking ? [AppModel.selectedBooking] : [])

    /// Pickable categories (sub-categories and childless top-level ones) matching
    /// the query, grouped by their top-level category in sidebar order
    /// (`root === null`: childless top-level categories, first, without a header).
    readonly property var groups: {
        const all = AppModel.categories
        const byID = {}, parents = {}
        for (const c of all) { byID[c.id] = c; if (c.parent_id !== null) parents[c.parent_id] = true }
        const q = query.text.toLowerCase()
        const loose = [], order = [], byRoot = {}
        for (const c of all) {
            if (c.parent_id === null && parents[c.id]) continue
            if (q !== "" && c.path.toLowerCase().indexOf(q) < 0) continue
            if (c.parent_id === null) { loose.push(c); continue }
            let root = c
            for (let i = 0; i < 32 && root.parent_id !== null && byID[root.parent_id]; i++) root = byID[root.parent_id]
            if (!byRoot[root.id]) { order.push(root.id); byRoot[root.id] = [] }
            byRoot[root.id].push(c)
        }
        const out = loose.length ? [{ root: null, items: loose }] : []
        for (const id of order) out.push({ root: byID[id], items: byRoot[id] })
        return out
    }

    /// Chip label: the path below the top-level category ("Restaurant › Lieferdienst").
    function chipTitle(c) {
        if (c.depth <= 1) return c.name
        const names = [c.name]
        let next = c.parent_id
        while (next !== null && names.length < 32) {
            const p = AppModel.categoryByID(next)
            if (!p || p.parent_id === null) break
            names.unshift(p.name)
            next = p.parent_id
        }
        return names.join(" › ")
    }

    /// The category all selected bookings share (marked), else none.
    readonly property var currentCategory: {
        const ids = {}
        for (const b of targets) ids[b.category_id] = b.category_id
        const keys = Object.keys(ids)
        return keys.length === 1 ? ids[keys[0]] : undefined
    }

    function pick(id) {
        if (multi) AppModel.setCategoryFor(AppModel.selectedBookings, id)
        else if (AppModel.selectedBooking) AppModel.setCategory(AppModel.selectedBooking, id)
        close()
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 12
        ColumnLayout {
            visible: sheet.multi
            spacing: 2
            Text { text: qsTr("%1 Buchungen kategorisieren").arg(sheet.targets.length); font.pixelSize: Theme.body + 1; font.weight: Font.Bold; color: Theme.text }
            Text { text: Fmt.money(sheet.targets.reduce((s, b) => s + b.amount_cents, 0)); font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
        }
        RowLayout {
            visible: !sheet.multi && AppModel.selectedBooking !== null
            spacing: 10
            MerchantSignet { merchant: AppModel.selectedBooking ? AppModel.selectedBooking.merchant : null; categorySymbol: AppModel.selectedBooking ? AppModel.selectedBooking.category_symbol : "" }
            ColumnLayout {
                spacing: 0
                Text { text: AppModel.selectedBooking ? AppModel.selectedBooking.merchant.name : ""; font.pixelSize: Theme.body + 1; font.weight: Font.Bold; color: Theme.text; Sensitive {} }
                Text { text: AppModel.selectedBooking ? Fmt.money(AppModel.selectedBooking.amount_cents) : ""; font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
            }
        }
        TextField {
            id: query
            Layout.fillWidth: true
            placeholderText: qsTr("Kategorie suchen …")
            onAccepted: { const g = sheet.groups; if (g.length && g[0].items.length) sheet.pick(g[0].items[0].id) }
        }
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            Layout.preferredHeight: 300
            contentWidth: availableWidth
            clip: true
            Column {
                width: scroll.availableWidth
                spacing: 10
                Repeater {
                    model: sheet.groups
                    Column {
                        id: grp
                        required property var modelData
                        width: parent.width
                        spacing: 6
                        RowLayout {
                            visible: grp.modelData.root !== null
                            spacing: 5
                            SFIcon { symbol: grp.modelData.root ? grp.modelData.root.sf_symbol : ""; size: 12 }
                            Text { text: grp.modelData.root ? grp.modelData.root.name : ""; font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
                        }
                        // LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 6)])
                        Flow {
                            id: grid
                            width: parent.width - 2
                            spacing: 6
                            readonly property int columns: Math.max(1, Math.floor((width + 6) / (150 + 6)))
                            Repeater {
                                model: grp.modelData.items
                                CategoryChip {
                                    required property var modelData
                                    width: (grid.width - (grid.columns - 1) * 6) / grid.columns
                                    category: modelData
                                    label: sheet.chipTitle(modelData)
                                    selected: sheet.currentCategory === modelData.id
                                    onClicked: sheet.pick(modelData.id)
                                }
                            }
                        }
                    }
                }
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Button { text: qsTr("Kategorie entfernen"); onClicked: sheet.pick(null) }
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
        }
    }

    // BookingDetailView.swift › CategoryChip
    component CategoryChip: Rectangle {
        id: chip
        property var category
        property string label
        property bool selected
        signal clicked()
        implicitHeight: 30
        radius: 7
        color: selected ? Theme.selection : Theme.card
        border.color: selected ? Theme.accent : Theme.border
        border.width: selected ? 1.5 : 1
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 8; spacing: 5
            SFIcon { symbol: chip.category.sf_symbol; size: 13; color: Theme.text }
            Text { text: chip.label; font.pixelSize: Theme.caption + 1; font.weight: chip.selected ? Font.DemiBold : Font.Normal
                   color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
        }
        HoverHandler { id: chipHover }
        ToolTip.visible: chipHover.hovered
        ToolTip.delay: 600
        ToolTip.text: chip.category.path
        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: chip.clicked() }
    }
}
