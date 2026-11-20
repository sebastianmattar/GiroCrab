import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore

// DepotPositionsView.swift (Sachwerte, #87): the items of a Sachwerte account below its
// header, as cards like the day cards of the booking list – held items, and sold ones in
// their own card. A click selects an item (its own value chart in the header), a double
// click opens the editor; a click on the empty space shows the whole account again.
Item {
    id: view
    property var account: null
    readonly property var items: account ? AppModel.assetItems(account.id) : []
    readonly property var held: items.filter(i => !i.sold).sort((a, b) => b.holding.value_cents - a.holding.value_cents)
    readonly property var sold: items.filter(i => i.sold).sort((a, b) => a.setup.name.localeCompare(b.setup.name))
    /// @AppStorage("showSoldAssets")
    Settings { id: prefs; property bool showSoldAssets: false }
    property var deleting: null
    /// DepotPositionsView.swift (#351): the classification of each item (`classified_assets`),
    /// reloaded with the account and after an own assignment.
    property var classified: []
    property int classifiedGeneration: 0
    function reloadClassified() {
        const gen = ++classifiedGeneration
        if (!account) { classified = []; return }
        AppModel.coreAsync("classified_assets", { account_id: account.id }, r => { if (gen === view.classifiedGeneration) view.classified = r }, () => {})
    }
    onAccountChanged: reloadClassified()
    onItemsChanged: reloadClassified()
    Connections { target: AssetClassification; function onVersionChanged() { view.reloadClassified() } }
    function classificationOf(item) {
        const c = classified.find(a => a.asset_item_id === item.id)
        return c ? c.classification : null
    }
    implicitHeight: Math.max(420, column.implicitHeight)

    // a click on the empty space around the cards ends the item selection (a background
    // layer, so it never sees clicks on the rows)
    MouseArea { anchors.fill: parent; z: -1; onClicked: AppModel.deselectAssetItem() }

    ColumnLayout {
        id: column
        width: parent.width
        spacing: 14

        // ContentUnavailableView „Noch keine Sachwerte“
        ColumnLayout {
            visible: view.items.length === 0
            Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 40
            spacing: 6
            SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "shippingbox"; size: 32 }
            Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Noch keine Sachwerte"); font.pixelSize: Theme.title2; font.weight: Font.Bold; color: Theme.text }
            Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Fügen Sie z. B. Goldmünzen, Barren oder ein Auto hinzu."); font.pixelSize: Theme.body; color: Theme.secondary }
            Button { Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 6; text: qsTr("Sachwert hinzufügen …")
                     onClicked: AppModel.openAssetEditor(view.account.id) }
        }

        PositionCard {
            visible: view.held.length > 0
            rows: view.held
            // No total here: the header above already shows it.
            header: Text { text: qsTr("Positionen"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text }
            onHeaderTapped: AppModel.deselectAssetItem()
        }
        PositionCard {
            visible: view.sold.length > 0
            rows: prefs.showSoldAssets ? view.sold : []
            header: RowLayout {
                spacing: 6
                SFIcon { symbol: "chevron.right"; size: 10; rotation: prefs.showSoldAssets ? 90 : 0
                         Behavior on rotation { NumberAnimation { duration: 200 } } }
                Text { text: qsTr("Ehemalige Positionen (%1)").arg(view.sold.length); font.pixelSize: Theme.subheadline + 1
                       font.weight: Font.DemiBold; color: Theme.secondary }
                Accessible.role: Accessible.Button
                Accessible.description: prefs.showSoldAssets ? qsTr("Ausgeklappt") : qsTr("Eingeklappt")
            }
            onHeaderTapped: prefs.showSoldAssets = !prefs.showSoldAssets
        }
    }

    ConfirmDialog {
        /// the item asked about (`deleting` is reset when the dialog closes)
        property var target: null
        visible: view.deleting !== null
        onAboutToShow: target = view.deleting
        heading: view.deleting ? qsTr("„%1“ löschen?").arg(view.deleting.setup.name) : ""
        message: qsTr("Der Sachwert und sein Verlauf werden entfernt.")
        actionTitle: qsTr("Löschen")
        onClosed: view.deleting = null
        onConfirmed: {
            const item = target
            if (item) try { AppModel.changeAssets("delete_asset_item", { item_id: item.id }) } catch (e) { AppModel.errorMessage = AppModel.parseCoreError(e.message).message }
        }
    }

    function detail(item) {
        if (item.sold && item.quantities.length)
            return qsTr("verkauft am %1").arg(Fmt.medium(item.quantities[item.quantities.length - 1].date))
        if (item.missing_price_date) return qsTr("Kurs für den %1 fehlt").arg(Fmt.medium(item.missing_price_date))
        const qty = NumberText.format(item.holding.quantity) + " × " + Fmt.money(item.holding.current_price_cents)
        if (!item.value_date) return qty + " · " + qsTr("noch kein Kurs")
        const source = item.setup.price_source === "manual" ? qsTr("Wert vom %1").arg(Fmt.medium(item.value_date)) : qsTr("Kurs vom %1").arg(Fmt.medium(item.value_date))
        return qty + " · " + source
    }

    /// PositionCard: a header and rows, styled like the booking list's day cards.
    component PositionCard: Rectangle {
        id: card
        property var rows: []
        property alias header: headerSlot.data
        signal headerTapped()
        Layout.fillWidth: true
        implicitHeight: cardColumn.implicitHeight
        radius: 12
        color: Theme.card
        border.color: Theme.border
        clip: true
        ColumnLayout {
            id: cardColumn
            width: parent.width
            spacing: 0
            Item {
                Layout.fillWidth: true
                implicitHeight: headerSlot.childrenRect.height + 20
                Item { id: headerSlot; x: 16; y: 10; width: parent.width - 32; height: childrenRect.height }
                MouseArea { anchors.fill: parent; onClicked: card.headerTapped() }
            }
            Rectangle { visible: card.rows.length > 0; Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
            Repeater {
                model: card.rows
                ColumnLayout {
                    required property var modelData
                    required property int index
                    Layout.fillWidth: true
                    spacing: 0
                    AssetRow { item: modelData; Layout.fillWidth: true }
                    Rectangle { visible: index < card.rows.length - 1; Layout.fillWidth: true; Layout.leftMargin: 64; implicitHeight: 1
                                color: Theme.withAlpha(Theme.border, 0.7) }
                }
            }
        }
    }

    /// PositionRow of a Sachwert, laid out like a booking row.
    component AssetRow: Rectangle {
        id: row
        property var item
        readonly property var classification: view.classificationOf(item)
        readonly property bool selected: AppModel.selectedAssetItemID === item.id
        implicitHeight: Math.max(44, rowLayout.implicitHeight) + 10
        color: selected ? Theme.selection : "transparent"
        RowLayout {
            id: rowLayout
            anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
            anchors.leftMargin: 14; anchors.rightMargin: 14
            spacing: 12
            opacity: row.item.sold ? 0.6 : 1
            AssetSymbolTile { symbol: AssetSymbols.symbolOf(row.item) }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Text { text: row.item.setup.name; font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 3
                    SFIcon { visible: !!row.item.missing_price_date; symbol: "exclamationmark.triangle.fill"; size: 11; color: Theme.warning }
                    // PositionRow: the classification in front of the detail
                    Text { text: (row.classification ? AssetClassification.compactTitle(row.classification) + " · " : "") + view.detail(row.item); font.pixelSize: Theme.callout; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true } }
                }
            }
            Text {
                visible: !(row.item.sold && row.item.holding.value_cents === 0)
                text: Fmt.money(row.item.holding.value_cents)
                font.pixelSize: Theme.body; font.weight: Font.Medium; font.features: { "tnum": 1 }; color: Theme.text
                Sensitive { amounts: true }
            }
        }
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            onClicked: m => {
                if (m.button === Qt.RightButton) { menu.popup(); return }
                AppModel.selectAssetItem(row.item.id)
            }
            onDoubleClicked: m => {
                if (m.button !== Qt.LeftButton) return
                // the second click toggled the selection off again: keep the item selected
                if (AppModel.selectedAssetItemID !== row.item.id) AppModel.selectAssetItem(row.item.id)
                AppModel.openAssetEditor(view.account.id, row.item.id)
            }
        }
        Accessible.role: Accessible.Button
        Accessible.name: row.item.setup.name
        AppMenu {
            id: menu
            AppMenuItem { text: qsTr("Klassifizierung bearbeiten"); visible: row.classification !== null; height: visible ? implicitHeight : 0
                          onTriggered: AssetClassification.edit(row.item.setup.name, row.classification) }
            AppMenuItem { text: qsTr("Sachwert bearbeiten …"); onTriggered: AppModel.openAssetEditor(view.account.id, row.item.id) }
            // gold follows the price API; only manual items carry own values
            AppMenuItem { text: qsTr("Bewertungen bearbeiten …"); visible: row.item.setup.price_source === "manual"; height: visible ? implicitHeight : 0
                          onTriggered: AppModel.openAssetAction("values", row.item.id) }
            AppMenuItem { text: qsTr("Anzahl bearbeiten …"); onTriggered: AppModel.openAssetAction("quantities", row.item.id) }
            AppMenuItem { text: qsTr("Sachwert verkaufen …"); visible: !row.item.sold; height: visible ? implicitHeight : 0
                          onTriggered: AppModel.openAssetAction("sell", row.item.id) }
            AppMenuSeparator {}
            AppMenuItem { text: qsTr("Sachwert löschen …"); onTriggered: view.deleting = row.item }
        }
    }
}
