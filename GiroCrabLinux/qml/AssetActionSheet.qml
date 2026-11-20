import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// TangibleAssetsUI.swift › AssetValuesSheet („Bewertungen bearbeiten“), AssetQuantitiesSheet
// („Anzahl bearbeiten“) and AssetSellSheet („Sachwert verkaufen“) in the shared AssetQuickSheet
// frame. Values and quantities are saved as they are made (only „Fertig“); the sale is one
// core call. Driven by `AppModel.assetAction` { itemId, kind: "values" | "quantities" | "sell" }.
Sheet {
    id: sheet
    readonly property var request: AppModel.assetAction
    readonly property string kind: request ? request.kind : ""
    /// Re-read after every change (the model reloads the items).
    readonly property var item: request ? AppModel.assetItem(request.itemId) : null
    readonly property bool manual: item !== null && item.setup.price_source === "manual"
    visible: request !== null
    onAboutToShow: load()
    onClosed: AppModel.assetAction = null
    sheetWidth: 520
    padding: 0
    property string error: ""

    readonly property string heading: kind === "values" ? qsTr("Bewertungen bearbeiten")
        : kind === "quantities" ? qsTr("Anzahl bearbeiten") : qsTr("Sachwert verkaufen")

    // MARK: AssetItem helpers
    /// Pieces held on `day` (ISO): the latest quantity point up to it, 0 before the purchase.
    function heldOn(day) {
        if (!item) return 0
        let best = null
        for (const q of item.quantities) if (q.date <= day && (best === null || q.date > best.date)) best = q
        return best ? best.quantity : 0
    }
    /// The purchase date (date pickers start there), never after today.
    readonly property date purchaseDay: { const d = item ? Fmt.date(item.setup.purchase_date) : null; const t = new Date(); return d && d < t ? d : t }
    function today() { return new Date() }

    // value entry (ValueEntryRows) / quantity entry (BestandRow) / sale
    property date entryDate: new Date()
    property string entryText: ""
    /// "piece" or "total"
    property string entryMode: "piece"
    property string priceText: ""
    readonly property string entryDay: Fmt.iso(entryDate)
    readonly property real entryHeld: { void item; return heldOn(entryDay) }
    readonly property bool canTotal: entryHeld > 0
    readonly property var entered: { const c = Fmt.parseCents(entryText.trim()); return c !== null && c >= 0 ? c : null }
    readonly property var perPiece: entered === null ? null : entryMode === "total" && canTotal ? Math.round(entered / entryHeld) : entered
    readonly property var otherValue: !canTotal || entryHeld === 1 || entered === null ? null
        : Math.round(entryMode === "total" ? entered / entryHeld : entered * entryHeld)
    // sale
    /// A sale is from the day after the purchase on (the core refuses the purchase date).
    readonly property date firstSaleDay: { const d = new Date(purchaseDay); d.setDate(d.getDate() + 1); const t = new Date(); return d < t ? d : t }
    readonly property var count: { const n = NumberText.parseNumber(entryText); return n !== null && n > 0 && n <= entryHeld + 1e-9 ? n : null }
    /// undefined = invalid, null = none given
    readonly property var salePrice: { const t = priceText.trim(); if (t === "") return null
                                       const c = Fmt.parseCents(t); return c !== null && c >= 0 ? c : undefined }

    function load() {
        error = ""; entryText = ""; priceText = ""; entryMode = "piece"
        const req = AppModel.assetAction
        const it = req ? AppModel.assetItem(req.itemId) : null
        if (req && req.kind === "sell" && it) {
            const first = new Date(Fmt.date(it.setup.purchase_date) || new Date()); first.setDate(first.getDate() + 1)
            entryDate = new Date()
            if (entryDate < first) entryDate = first
        } else {
            entryDate = new Date()
        }
        prefill()
    }
    onEntryDayChanged: prefill()
    /// Values: the value the chosen day has so far. Sale: all pieces of that day and
    /// (manual items) their value on it. Quantities: nothing.
    function prefill() {
        const req = AppModel.assetAction
        if (!req) return
        const it = AppModel.assetItem(req.itemId)
        if (!it) return
        const held = heldOn(entryDay)
        if (req.kind === "values") {
            const v = AppModel.assetUnitValue(it.id, entryDay)
            entryText = v === null ? "" : Fmt.plain(entryMode === "total" && held > 0 ? Math.round(v * held) : v)
        } else if (req.kind === "sell") {
            entryText = held > 0 ? NumberText.format(held) : ""
            if (it.setup.price_source === "manual") {
                const v = AppModel.assetUnitValue(it.id, entryDay)
                priceText = v === null ? "" : Fmt.plain(v)
            }
        }
    }
    /// Switching keeps the amount meaning the same.
    function setEntryMode(mode) {
        const c = Fmt.parseCents(entryText.trim())
        if (canTotal && c !== null) entryText = Fmt.plain(Math.round(mode === "total" ? c * entryHeld : c / entryHeld))
        entryMode = mode
    }
    function change(method, args) {
        try { AppModel.changeAssets(method, args); error = ""; return true }
        catch (e) { error = AppModel.parseCoreError(e.message).message; return false }
    }
    function sell() {
        if (count === null || salePrice === undefined) return
        // one core call, one transaction: price and quantity or nothing
        if (change("sell_asset", { item_id: item.id, date: entryDay, count: count, price_cents: manual ? salePrice : null })) close()
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 20
            text: sheet.heading
            font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Accessible.role: Accessible.Heading
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.margins: 20
            spacing: 18

            FormSection {
                FormRow { label: qsTr("Sachwert"); value: sheet.item ? sheet.item.setup.name : "" }
                FormRow {
                    visible: sheet.kind === "sell"
                    label: qsTr("Verkauft am")
                    DateField { date: sheet.entryDate; minimum: sheet.firstSaleDay; maximum: new Date(); onPicked: d => sheet.entryDate = d }
                }
                FormRow {
                    visible: sheet.kind === "sell"
                    label: qsTr("Anzahl")
                    TextField { Layout.preferredWidth: 200; placeholderText: NumberText.format(sheet.entryHeld)
                                text: sheet.entryText; onTextEdited: sheet.entryText = text }
                }
                FormRow {
                    visible: sheet.kind === "sell" && sheet.manual
                    label: qsTr("Verkaufspreis je Stück")
                    TextField { Layout.preferredWidth: 200; placeholderText: qsTr("optional")
                                text: sheet.priceText; onTextEdited: sheet.priceText = text }
                }
            }

            // „Bewertungen bearbeiten“: the dated values (per piece) and a row for a new one
            FormSection {
                visible: sheet.kind === "values"
                footer: sheet.item && sheet.item.setup.interpolation === "Linear"
                    ? qsTr("Zwischen zwei Werten verläuft der Wert linear. Wird sofort gespeichert.")
                    : qsTr("Der Wert gilt bis zum nächsten eingetragenen Wert. Wird sofort gespeichert.")
                Repeater {
                    model: sheet.kind === "values" && sheet.item ? sheet.item.values.slice().reverse() : []
                    RowLayout {
                        required property var modelData
                        readonly property real held: sheet.heldOn(modelData.date)
                        Layout.fillWidth: true
                        spacing: 8
                        Text { text: Fmt.medium(modelData.date); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Layout.fillWidth: true }
                        // per piece; with several pieces then what they were worth together
                        Text { text: Fmt.money(modelData.value_cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                        Text { visible: parent.held > 0 && parent.held !== 1; font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.secondary
                               text: qsTr("gesamt %1").arg(Fmt.money(Math.round(modelData.value_cents * parent.held))); Sensitive { amounts: true } }
                        RemoveButton { visible: modelData.date !== sheet.item.setup.purchase_date
                                       onClicked: sheet.change("delete_asset_value", { item_id: sheet.item.id, date: modelData.date }) }
                    }
                }
                // ValueEntryRows (the Mac's one row, in two lines here: the date field is wider)
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    DateField { date: sheet.entryDate; minimum: sheet.purchaseDay; maximum: new Date(); onPicked: d => sheet.entryDate = d }
                    Item { Layout.fillWidth: true }
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    AppComboBox {
                        visible: sheet.canTotal
                        flat: true
                        implicitWidth: 140
                        model: [qsTr("Wert gesamt"), qsTr("Wert je Stück")]
                        currentIndex: sheet.entryMode === "total" ? 0 : 1
                        onActivated: i => sheet.setEntryMode(i === 0 ? "total" : "piece")
                    }
                    Text { visible: !sheet.canTotal; text: qsTr("Wert je Stück"); font.pixelSize: Theme.body; color: Theme.text }
                    TextField { Layout.fillWidth: true; Layout.minimumWidth: 80; placeholderText: qsTr("Wert"); horizontalAlignment: TextInput.AlignRight
                                text: sheet.entryText; onTextEdited: sheet.entryText = text }
                    Button {
                        text: qsTr("Eintragen")
                        enabled: sheet.perPiece !== null
                        onClicked: if (sheet.change("set_asset_value", { item_id: sheet.item.id, date: sheet.entryDay, value_cents: sheet.perPiece })) sheet.entryText = ""
                    }
                }
                FormRow {
                    visible: sheet.otherValue !== null
                    label: sheet.entryMode === "total" ? qsTr("Wert je Stück") : qsTr("Wert gesamt")
                    value: sheet.otherValue !== null ? Fmt.money(sheet.otherValue) : ""
                    amounts: true
                }
            }

            // „Anzahl bearbeiten“: the dated quantities and a row for a new one
            FormSection {
                visible: sheet.kind === "quantities"
                footer: qsTr("Teilverkauf oder Zukauf: die neue Gesamtanzahl ab einem Datum. 0 = alles verkauft. Der Verlauf davor bleibt erhalten. Wird sofort gespeichert.")
                Repeater {
                    model: sheet.kind === "quantities" && sheet.item ? sheet.item.quantities.slice().reverse() : []
                    RowLayout {
                        required property var modelData
                        readonly property bool purchase: modelData.date === sheet.item.setup.purchase_date
                        Layout.fillWidth: true
                        spacing: 8
                        Text { text: Fmt.medium(modelData.date); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Layout.fillWidth: true }
                        Text { font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text
                               text: parent.purchase ? qsTr("Kauf: %1").arg(NumberText.format(modelData.quantity)) : qsTr("ab hier: %1").arg(NumberText.format(modelData.quantity)) }
                        RemoveButton { visible: !parent.purchase
                                       onClicked: sheet.change("delete_asset_quantity", { item_id: sheet.item.id, date: modelData.date }) }
                    }
                }
                // BestandRow: „Neuer Bestand ab …“ (in two lines, see above)
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    DateField { date: sheet.entryDate; minimum: sheet.purchaseDay; maximum: new Date(); onPicked: d => sheet.entryDate = d }
                    Item { Layout.fillWidth: true }
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    TextField { id: newQuantity; Layout.fillWidth: true; Layout.minimumWidth: 80; placeholderText: qsTr("Neue Anzahl") }
                    Button {
                        text: qsTr("Eintragen")
                        enabled: { const q = NumberText.parseNumber(newQuantity.text); return q !== null && q >= 0 }
                        onClicked: if (sheet.change("set_asset_quantity", { item_id: sheet.item.id, date: sheet.entryDay, quantity: NumberText.parseNumber(newQuantity.text) })) newQuantity.text = ""
                    }
                }
            }

            Text {
                visible: sheet.kind === "sell"
                Layout.fillWidth: true; Layout.leftMargin: 10
                wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                text: sheet.entryHeld > 0
                    ? qsTr("Bestand an diesem Tag: %1. Verkaufte Sachwerte stehen danach unter „Ehemalige Positionen“; ihr Wert bis zum Verkauf bleibt im Verlauf.").arg(NumberText.format(sheet.entryHeld))
                    : qsTr("An diesem Tag ist kein Bestand vorhanden.")
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 16
            spacing: 8
            RowLayout {
                visible: sheet.error !== ""
                Layout.fillWidth: true
                spacing: 6
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 13; color: Theme.warning; Layout.alignment: Qt.AlignTop }
                Text { text: sheet.error; font.pixelSize: Theme.callout; color: Theme.warning; wrapMode: Text.WordWrap
                       maximumLineCount: 2; elide: Text.ElideRight; Layout.fillWidth: true }
            }
            Item { visible: sheet.error === ""; Layout.fillWidth: true }
            Button { visible: sheet.kind === "sell"; text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button {
                visible: sheet.kind === "sell"
                text: qsTr("Verkaufen")
                highlighted: true
                enabled: sheet.count !== null && sheet.salePrice !== undefined
                onClicked: sheet.sell()
            }
            Button { visible: sheet.kind !== "sell"; text: qsTr("Fertig"); highlighted: true; onClicked: sheet.close() }
        }
    }

    component RemoveButton: IconButton {
        symbol: "minus.circle"
        ToolTip.text: qsTr("Entfernen"); ToolTip.visible: hovered
        Accessible.name: qsTr("Entfernen")
    }
}
