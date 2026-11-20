import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// BalanceHeaderView.swift › HoldingsCard: positions of a depot account. DepotPositionsView.swift
// (#351): each position carries its classification („Aktien · ETF“ in the detail line) and
// „Klassifizierung bearbeiten“ in its context menu.
ColumnLayout {
    id: hc
    property var account
    /// [ClassifiedAsset]: { holding, classification, asset_item_id }
    property var classified: []
    readonly property var holdings: classified.map(c => c.holding)
    spacing: 6
    property int generation: 0
    /// .task(id: account, balance, balance date, assetClassificationVersion), off the GUI thread
    function reload() {
        const gen = ++generation
        if (!account) { classified = []; return }
        AppModel.coreAsync("classified_assets", { account_id: account.id }, r => { if (gen === hc.generation) hc.classified = r }, () => {})
    }
    onAccountChanged: reload()
    Connections { target: AppModel; function onGroupsChanged() { hc.reload() } }
    Connections { target: AssetClassification; function onVersionChanged() { hc.reload() } }

    /// HoldingsCard.calculatedGain: cost = quantity × purchase price.
    function gain(h) {
        if (!(h.quantity > 0) || h.purchase_price_cents <= 0 || h.value_cents < 0) return null
        const cost = Math.round(h.quantity * h.purchase_price_cents)
        if (!(cost >= 1)) return null
        const g = h.value_cents - cost
        return { cents: g, percent: g / cost * 100 }
    }
    function qty(q) { return Number(q).toLocaleString(Qt.locale(), "f", 4).replace(/[.,]?0+$/, "") }
    function line(h) {
        const id = h.isin || h.wkn
        return [h.name, id, qty(h.quantity) + " × " + Fmt.money(h.current_price_cents), Fmt.money(h.value_cents)].filter(x => x).join(" · ")
    }

    Text { visible: hc.holdings.length > 0; text: qsTr("Positionen"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
    Repeater {
        model: hc.classified
        RowLayout {
            id: row
            required property var modelData
            readonly property var h: modelData.holding
            readonly property var c: modelData.classification
            readonly property var g: hc.gain(h)
            Layout.fillWidth: true
            ColumnLayout { spacing: 1; Layout.fillWidth: true
                Text { text: row.h.name || row.h.isin || row.h.wkn; font.pixelSize: Theme.callout + 1; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true
                       Sensitive { labels: true } }
                // PositionRow: the classification travels with its position, in front of the detail
                Text { text: [AssetClassification.compactTitle(row.c), hc.qty(row.h.quantity) + " × " + Fmt.money(row.h.current_price_cents) + " · " + (row.h.isin || row.h.wkn)].join(" · ")
                       font.pixelSize: Theme.caption2; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true }
                       ToolTip.visible: hover.hovered; ToolTip.delay: 600; ToolTip.text: AssetClassification.sourceDescription(row.c)
                       HoverHandler { id: hover } } }
            ColumnLayout { spacing: 1
                Text { Layout.alignment: Qt.AlignRight; text: Fmt.money(row.h.value_cents); font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                Text { visible: row.g !== null; Layout.alignment: Qt.AlignRight; font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }
                       readonly property var g: row.g
                       text: g ? qsTr("GuV (berechnet): %1 (%2 %)").arg(Fmt.money(g.cents)).arg(Number(g.percent).toLocaleString(Qt.locale(), "f", 2)) : ""
                       color: g && g.cents < 0 ? "red" : "green"; Sensitive { amounts: true } } }
            TapHandler { acceptedButtons: Qt.RightButton; onTapped: menu.popup() }
            AppMenu { id: menu
                AppMenuItem { text: qsTr("Klassifizierung bearbeiten"); visible: !!row.c.security_key; height: visible ? implicitHeight : 0
                              onTriggered: AssetClassification.edit(row.h.name, row.c) }
                AppMenuItem { text: qsTr("Position kopieren"); onTriggered: AppModel.copyToClipboard(hc.line(row.h)) }
                AppMenuItem { text: qsTr("Alle Positionen kopieren"); onTriggered: AppModel.copyToClipboard(hc.holdings.map(hc.line).join("\n")) } }
        }
    }
}
