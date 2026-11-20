import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// TangibleAssetsUI.swift › AssetEditorSheet: create / edit a Sachwert. A missing gold price
// is saved right away (edit mode only). Driven by `AppModel.assetEditor` { accountId, itemId }.
Sheet {
    id: sheet
    readonly property var request: AppModel.assetEditor
    readonly property var existing: request && request.itemId !== null ? AppModel.assetItem(request.itemId) : null
    visible: request !== null
    onAboutToShow: load()
    onClosed: AppModel.assetEditor = null
    sheetWidth: 560
    padding: 0

    /// "Eigene Angaben" in the denomination picker.
    readonly property real customDenomination: -1
    readonly property var weights: AppModel.run(() => AppModel.core("asset_weight_presets")) || []
    readonly property var units: [{ id: "g", grams: 1 }, { id: "kg", grams: 1000 }, { id: "oz", grams: NumberText.troyOunceGrams }]

    property string name: ""
    /// "XAU" (gold) or "manual".
    property string source: "manual"
    /// Grams of a denomination, or `customDenomination`.
    property real denomination: NumberText.troyOunceGrams
    property string weightText: ""
    property string unit: "g"
    property string purityText: ""
    property string interpolation: "Step"
    property date purchaseDate: new Date()
    property string quantityText: "1"
    property string priceText: ""
    /// "total" or "piece": whether the price field holds the total or the price of one piece.
    property string priceMode: "total"
    /// The text the price field was prefilled with (gold value on the purchase date); while
    /// unchanged it follows date, weight and quantity and is saved as "no purchase price".
    property var autoPriceText: null
    property string note: ""
    /// Chosen icon; new items start with the diamond, "" (older items) = the default for the type.
    property string symbol: AssetSymbols.newItemDefault
    property string saveError: ""
    property bool touched: false
    property bool loaded: false
    property string missingPriceText: ""

    // MARK: validation (undefined = invalid, null = none given)
    readonly property bool automatic: source !== "manual"
    readonly property bool priceIsAuto: autoPriceText !== null && priceText === autoPriceText
    readonly property var quantity: { const q = NumberText.parseNumber(quantityText); return q !== null && q > 0 ? q : null }
    /// Total paid (the core stores the total).
    readonly property var purchasePrice: {
        const t = priceText.trim()
        if (t === "" || priceIsAuto) return null
        const c = Fmt.parseCents(t)
        if (c === null || c < 0) return undefined
        return priceMode === "total" ? c : Math.round(c * (quantity || 1))
    }
    /// The entered price converted to the other mode; null while nothing to show (no price, quantity 1).
    readonly property var otherPrice: {
        const c = Fmt.parseCents(priceText.trim())
        if (c === null || c < 0 || quantity === null || quantity === 1) return null
        return Math.round(priceMode === "total" ? c / quantity : c * quantity)
    }
    /// Manual items: the value on the purchase date is the purchase price per piece.
    readonly property var startValue: purchasePrice === null || purchasePrice === undefined || quantity === null ? null : Math.round(purchasePrice / quantity)
    /// { grams per piece, fineness ‰ } or null
    readonly property var weight: {
        if (denomination !== customDenomination) return { grams: denomination, purity: 999.9 }
        const w = NumberText.parseNumber(weightText)
        if (w === null || w <= 0) return null
        const p = purityText.trim() === "" ? 1000 : NumberText.parseNumber(purityText)
        if (p === null || p <= 0 || p > 1000) return null
        return { grams: w * units.find(u => u.id === unit).grams, purity: p }
    }
    readonly property string problem: {
        if (name.trim() === "") return qsTr("Bitte einen Namen angeben.")
        if (quantity === null) return qsTr("Die Anzahl muss größer als 0 sein.")
        if (purchasePrice === undefined) return qsTr("Der Kaufpreis ist keine gültige Zahl.")
        if (automatic) { if (weight === null) return qsTr("Bitte Gewicht und Feingehalt (bis 1000 ‰) angeben.") }
        else if (startValue === null) return qsTr("Bitte den Kaufpreis angeben.")
        return ""
    }
    readonly property var setup: problem !== "" ? null : {
        name: name.trim(),
        price_source: source,
        weight_grams: automatic && weight ? weight.grams : 0,
        purity_permille: automatic && weight ? weight.purity : 1000,
        interpolation: interpolation,
        purchase_date: Fmt.iso(purchaseDate),
        quantity: quantity,
        purchase_price_cents: purchasePrice,
        start_value_cents: automatic ? null : startValue,
        note: note,
        symbol: symbol
    }
    readonly property string statusText: saveError !== "" ? saveError : (touched ? problem : "")
    readonly property string shownSymbol: symbol === "" ? AssetSymbols.defaultSymbol(source) : symbol

    /// The units of the presets in their order (g, oz, kg) – for the picker's blocks.
    function presetUnit(label) { const parts = label.split(" "); return parts[parts.length - 1] }
    readonly property var denominationOptions: weights.map(w => ({ title: w.label, grams: w.grams }))
        .concat([{ title: qsTr("Eigene Angaben (Schmuck o. ä.)"), grams: customDenomination }])

    function load() {
        loaded = false
        touched = false; saveError = ""; missingPriceText = ""
        name = ""; source = "manual"; denomination = NumberText.troyOunceGrams; weightText = ""; unit = "g"; purityText = ""
        interpolation = "Step"; purchaseDate = new Date(); quantityText = "1"; priceText = ""; priceMode = "total"
        autoPriceText = null; note = ""; symbol = AssetSymbols.newItemDefault
        // read the request directly: aboutToShow comes before the bindings above are updated
        const req = AppModel.assetEditor
        const item = req && req.itemId !== null ? AppModel.assetItem(req.itemId) : null
        if (item) {
            const s = item.setup
            name = s.name
            source = s.price_source
            if (s.price_source !== "manual") {
                const w = weights.find(w => Math.abs(w.grams - s.weight_grams) < 0.0005)
                if (s.purity_permille === 999.9 && w) {
                    denomination = w.grams
                } else {
                    denomination = customDenomination
                    weightText = NumberText.format(s.weight_grams, 4)
                    unit = "g"
                    purityText = s.purity_permille === 1000 ? "" : NumberText.format(s.purity_permille, 3)
                }
            }
            interpolation = s.interpolation
            purchaseDate = Fmt.date(s.purchase_date) || new Date()
            quantityText = NumberText.format(s.quantity)
            // manual items saved without a price: their start value is the price per piece
            const paid = s.purchase_price_cents !== null ? s.purchase_price_cents
                : (s.price_source === "manual" && s.start_value_cents !== null ? Math.round(s.start_value_cents * s.quantity) : null)
            priceText = paid !== null ? Fmt.plain(paid) : ""
            note = s.note
            symbol = s.symbol
        }
        loaded = true
        Qt.callLater(() => nameField.forceActiveFocus())
    }

    /// Gold: an empty (or still prefilled) price field shows the gold value on the purchase
    /// date – of all pieces or one, as the field label says. `force` (the user picked another
    /// purchase date) replaces an entered price too: it belonged to the old date.
    function prefillPrice(force) {
        if (!loaded) return
        if (!(force || priceText.trim() === "" || priceIsAuto)) return
        let text = ""
        const p = automatic && weight ? AppModel.assetPrice(source, Fmt.iso(purchaseDate)) : null
        if (p !== null) {
            const piece = p * weight.grams * weight.purity / 1000 / NumberText.troyOunceGrams
            text = Fmt.plain(Math.round(priceMode === "total" ? piece * (quantity || 1) : piece))
        }
        priceText = text
        autoPriceText = text === "" ? null : text
    }
    readonly property string prefillKey: [source, Fmt.iso(purchaseDate), denomination, weightText, purityText, unit,
                                          quantityText, priceMode, loaded].join("\u001F")
    onPrefillKeyChanged: prefillPrice(false)

    /// Switching total ⇄ per piece keeps the amount meaning the same.
    /// A gold value still prefilled is left to `prefillPrice`.
    function setPriceMode(mode) {
        const c = Fmt.parseCents(priceText.trim())
        if (!priceIsAuto && quantity !== null && c !== null)
            priceText = Fmt.plain(Math.round(mode === "total" ? c * quantity : c / quantity))
        priceMode = mode
    }

    function apply(method, args) {
        try {
            AppModel.changeAssets(method, args)
            saveError = ""
        } catch (e) {
            saveError = AppModel.parseCoreError(e.message).message
        }
    }

    function save() {
        touched = true
        if (setup === null) return
        try {
            AppModel.changeAssets("save_asset_item", { account_id: request.accountId, item_id: request.itemId, setup: setup })
            if (setup.price_source !== "manual") AppModel.refreshAssetPrices(false)
            close()
        } catch (e) {
            saveError = AppModel.parseCoreError(e.message).message
        }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        Text {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 20
            text: sheet.existing === null ? qsTr("Neuer Sachwert") : qsTr("Sachwert bearbeiten")
            font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Accessible.role: Accessible.Heading
        }

        ScrollView {

            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 20 + footer.implicitHeight + 33
            Layout.preferredHeight: Math.min(Math.max(form.implicitHeight + 40, 620 - chrome), sheet.maxBodyHeight - chrome)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: form
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18

                FormSection {
                    footer: sheet.automatic
                        ? qsTr("Der Wert folgt dem täglichen Goldpreis (EUR je Feinunze, Frankfurter API). Abgefragt werden nur Metall und Zeitraum.")
                        : qsTr("Für Dinge ohne Kurs, z. B. Haus, Auto oder Kunst: Sie tragen den Wert zu Stichtagen selbst ein.")
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 10
                        AssetSymbolTile {
                            id: symbolButton
                            symbol: sheet.shownSymbol
                            MouseArea { id: symbolArea; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor
                                        onClicked: symbolPicker.open() }
                            ToolTip.visible: symbolArea.containsMouse; ToolTip.delay: 500; ToolTip.text: qsTr("Symbol wählen")
                            Accessible.role: Accessible.Button; Accessible.name: qsTr("Symbol: %1").arg(AssetSymbols.nameOf(sheet.shownSymbol))
                            AssetSymbolPicker { id: symbolPicker; y: parent.height + 4; current: sheet.shownSymbol; onPicked: s => sheet.symbol = s }
                        }
                        TextField {
                            id: nameField
                            Layout.fillWidth: true
                            placeholderText: sheet.automatic ? qsTr("z. B. Goldbarren 100 g") : qsTr("z. B. Auto")
                            text: sheet.name; onTextEdited: sheet.name = text
                            Accessible.name: qsTr("Name")
                        }
                    }
                    // more price sources (silver, …) are added here later
                    FormRow {
                        label: qsTr("Typ")
                        AppComboBox {
                            implicitWidth: 220
                            model: [qsTr("Manuell"), qsTr("Gold")]
                            currentIndex: sheet.source === "manual" ? 0 : 1
                            onActivated: i => sheet.source = i === 0 ? "manual" : "XAU"
                        }
                    }
                }

                // gold: the denomination first, it sets the price below
                FormSection {
                    visible: sheet.automatic
                    footer: sheet.denomination === sheet.customDenomination
                        ? qsTr("Gewicht und Feingehalt, z. B. 585 ‰ für 14 Karat.")
                        : qsTr("Anlagegold (Münzen und Barren) mit 999,9 ‰ Feingehalt.")
                    FormRow {
                        label: qsTr("Stückelung")
                        AppComboBox {
                            implicitWidth: 260
                            model: sheet.denominationOptions.map(o => o.title)
                            currentIndex: Math.max(0, sheet.denominationOptions.findIndex(o => Math.abs(o.grams - sheet.denomination) < 0.0005))
                            onActivated: i => sheet.denomination = sheet.denominationOptions[i].grams
                        }
                    }
                    FormRow {
                        visible: sheet.denomination === sheet.customDenomination
                        label: qsTr("Gewicht je Stück")
                        TextField { Layout.preferredWidth: 180; placeholderText: qsTr("z. B. 10"); text: sheet.weightText; onTextEdited: sheet.weightText = text }
                        AppComboBox {
                            implicitWidth: 80
                            model: sheet.units.map(u => u.id)
                            currentIndex: sheet.units.findIndex(u => u.id === sheet.unit)
                            onActivated: i => sheet.unit = sheet.units[i].id
                        }
                    }
                    FormRow {
                        visible: sheet.denomination === sheet.customDenomination
                        label: qsTr("Feingehalt in ‰")
                        TextField { Layout.preferredWidth: 268; placeholderText: qsTr("optional, z. B. 999,9 oder 585")
                                    text: sheet.purityText; onTextEdited: sheet.purityText = text }
                    }
                    FormRow {
                        visible: sheet.weight !== null
                        label: qsTr("Feingold je Stück")
                        value: { const w = sheet.weight; if (!w) return ""; const fine = w.grams * w.purity / 1000
                                 return NumberText.format(fine, 3) + " g · " + NumberText.format(fine / NumberText.troyOunceGrams, 4) + " oz" }
                    }
                }

                // purchase: date, quantity, then the price (total or per piece)
                FormSection {
                    footer: !sheet.automatic ? ""
                        : sheet.priceIsAuto ? qsTr("Vorbelegt mit dem Goldwert am Kaufdatum. Ohne eigene Angabe gilt dieser Wert.")
                        : qsTr("Ohne Kaufpreis gilt der Goldwert am Kaufdatum.")
                    FormRow {
                        label: qsTr("Kaufdatum")
                        DateField {
                            date: sheet.purchaseDate; maximum: new Date()
                            onPicked: d => {
                                const changed = Fmt.iso(d) !== Fmt.iso(sheet.purchaseDate)
                                sheet.purchaseDate = d
                                if (changed && sheet.automatic) sheet.prefillPrice(true)
                            }
                        }
                    }
                    FormRow {
                        label: qsTr("Anzahl")
                        TextField { Layout.preferredWidth: 260; placeholderText: "1"; text: sheet.quantityText; onTextEdited: sheet.quantityText = text }
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 10
                        AppComboBox {
                            flat: true
                            implicitWidth: 190
                            model: [qsTr("Kaufpreis gesamt"), qsTr("Kaufpreis je Stück")]
                            currentIndex: sheet.priceMode === "total" ? 0 : 1
                            onActivated: i => sheet.setPriceMode(i === 0 ? "total" : "piece")
                        }
                        Item { Layout.fillWidth: true }
                        TextField {
                            Layout.preferredWidth: 260
                            placeholderText: sheet.automatic ? qsTr("optional") : qsTr("z. B. %1").arg(Fmt.plain(2500000))
                            text: sheet.priceText; onTextEdited: sheet.priceText = text
                            Accessible.name: qsTr("Kaufpreis")
                            Sensitive { amounts: true; when: parent.text !== "" }
                        }
                    }
                    // the other way round, read-only: per piece for a total and vice versa
                    FormRow {
                        visible: sheet.otherPrice !== null
                        label: sheet.priceMode === "total" ? qsTr("Kaufpreis je Stück") : qsTr("Kaufpreis gesamt")
                        value: sheet.otherPrice !== null ? Fmt.money(sheet.otherPrice) : ""
                        amounts: true
                    }
                }

                FormSection {
                    visible: !sheet.automatic
                    footer: qsTr("Der Kaufpreis je Stück ist der erste Wert. Spätere Werte, z. B. aus einem Gutachten, tragen Sie über „Bewertungen bearbeiten …“ im Kontextmenü der Position ein.")
                    FormRow {
                        label: qsTr("Zwischen zwei Werten")
                        AppComboBox {
                            implicitWidth: 260
                            model: [qsTr("Wert bleibt bis zum nächsten"), qsTr("Linear verlaufen")]
                            currentIndex: sheet.interpolation === "Linear" ? 1 : 0
                            onActivated: i => sheet.interpolation = i === 1 ? "Linear" : "Step"
                        }
                    }
                }

                FormSection {
                    TextArea {
                        Layout.fillWidth: true
                        Layout.preferredHeight: Math.min(Math.max(contentHeight + topPadding + bottomPadding, 34), 84)
                        wrapMode: TextEdit.Wrap
                        placeholderText: qsTr("z. B. Händler, Zertifikat, Aufbewahrung")
                        text: sheet.note; onTextChanged: if (text !== sheet.note) sheet.note = text
                        Accessible.name: qsTr("Notiz")
                    }
                }

                // values and quantities have their own sheets („Bewertungen bearbeiten …“, „Anzahl bearbeiten …“)
                FormSection {
                    id: missingSection
                    readonly property var missing: sheet.existing && sheet.existing.setup.price_source !== "manual" ? sheet.existing.missing_price_date : null
                    visible: missing !== null && missing !== undefined
                    title: missing ? "⚠ " + qsTr("Kurs am %1 fehlt").arg(Fmt.medium(missing)) : ""
                    footer: qsTr("Für diesen Tag hat die Kurs-API keinen Goldpreis (die Daten reichen etwa bis 2000 zurück). Ihr Wert gilt bis zum ersten verfügbaren Kurs und wird nie überschrieben.")
                    RowLayout {
                        Layout.fillWidth: true
                        TextField { Layout.fillWidth: true; placeholderText: qsTr("z. B. %1").arg(Fmt.plain(35000))
                                    text: sheet.missingPriceText; onTextEdited: sheet.missingPriceText = text
                                    Accessible.name: qsTr("Goldpreis je Feinunze") }
                        Button {
                            text: qsTr("Speichern")
                            enabled: { const c = Fmt.parseCents(sheet.missingPriceText); return c !== null && c > 0 }
                            onClicked: {
                                const date = missingSection.missing
                                sheet.apply("set_asset_price", { symbol: "XAU", date: date, price_cents: Fmt.parseCents(sheet.missingPriceText) })
                                sheet.missingPriceText = ""
                            }
                        }
                    }
                }
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        RowLayout {
            id: footer
            Layout.fillWidth: true
            Layout.margins: 16
            spacing: 8
            RowLayout {
                visible: sheet.statusText !== ""
                Layout.fillWidth: true
                spacing: 6
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 13; color: Theme.warning; Layout.alignment: Qt.AlignTop }
                Text { text: sheet.statusText; font.pixelSize: Theme.callout; color: Theme.warning; wrapMode: Text.WordWrap
                       maximumLineCount: 2; elide: Text.ElideRight; Layout.fillWidth: true }
            }
            Item { visible: sheet.statusText === ""; Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button {
                text: sheet.existing === null ? qsTr("Anlegen") : qsTr("Sichern")
                highlighted: true
                onClicked: sheet.save()
                Shortcut { sequences: ["Return", "Enter"]; enabled: sheet.visible && !symbolPicker.visible; onActivated: sheet.save() }
            }
        }
    }
}
