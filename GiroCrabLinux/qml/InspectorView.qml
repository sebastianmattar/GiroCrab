import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs

// BookingDetailView.swift › InspectorView: the cards of the selected booking in the order of
// InspectorLayout (#448) – receipt, note, tags, salary, recurring, category grid, documents,
// merchant history, raw data – or the multi-selection card. „Anpassen“ in the bar above the
// scroll view switches to the edit mode (InspectorLayoutEditor.qml).
Rectangle {
    id: inspector
    color: Theme.canvas
    readonly property var b: AppModel.selectedBooking
    /// The booking the cards show: the last selected one, so cards that are about to go away
    /// never see `null` (their bindings run before the Loader around them deactivates).
    property var cardBooking: AppModel.selectedBooking
    onBChanged: if (b !== null) cardBooking = b
    /// The single-booking cards are shown, so their layout can be edited.
    /// BookingDetailView.swift: „Geplant“ and „Lastschriften“ show their own inspector instead of the booking cards.
    readonly property bool bookingPage: AppModel.sidebarSelection !== "planned" && AppModel.sidebarSelection !== "directDebits"
    readonly property bool showsBookingCards: bookingPage
                                              && !AppModel.isMultiSelection && b !== null
    /// `--demo-inspector-edit` (LaunchOptions.swift) opens the edit mode, for screenshots.
    property bool editingLayout: Qt.application.arguments.indexOf("--demo-inspector-edit") >= 0

    // InspectorSectionCard: one card per section, for the selected booking. In the edit mode
    // (`placeholderWhenEmpty`) a card that would not show for this booking stands in as a placeholder.
    property Component receiptCard: Component { ReceiptCard { booking: inspector.cardBooking } }
    property Component noteCard: Component { NoteCard { booking: inspector.cardBooking } }
    // TagsUI.swift › TagsCard (#228)
    property Component tagsCard: Component { Card { TagsSection { width: parent.width; bookings: inspector.cardBooking ? [inspector.cardBooking] : [] } } }
    property Component salaryCard: Component { SalaryCard { booking: inspector.cardBooking } }
    property Component salaryPlaceholder: Component { PlaceholderCard { section: "salary"; hint: qsTr("Nur bei Einnahmen sichtbar") } }
    property Component recurringCard: Component { RecurringCard { booking: inspector.cardBooking } }
    property Component merchantHistoryCard: Component { MerchantHistoryCard { booking: inspector.cardBooking } }
    property Component categoriesCard: Component { CategoryGridCard { booking: inspector.cardBooking } }
    property Component documentsCard: Component { DocumentsCard { booking: inspector.cardBooking } }
    property Component rawDataCard: Component { RawDataCard { booking: inspector.cardBooking } }
    property Component plannedInspector: Component { PlannedInspector {} }
    property Component mandateInspector: Component { MandateInspector {} }
    function cardComponent(section, placeholderWhenEmpty) {
        switch (section) {
        case "receipt": return receiptCard
        case "note": return noteCard
        case "tags": return tagsCard
        case "salary": return cardBooking && cardBooking.amount_cents > 0 ? salaryCard : placeholderWhenEmpty ? salaryPlaceholder : null
        case "recurring": return recurringCard
        case "merchantHistory": return merchantHistoryCard
        case "categories": return categoriesCard
        case "documents": return documentsCard
        case "rawData": return rawDataCard
        }
        return null
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0
        // The bar sits above the scroll view (as on the Mac, #448, #492).
        LayoutBar { visible: inspector.showsBookingCards; Layout.fillWidth: true }

    ScrollView {
        id: scroll
        ScrollBar.vertical: ScrollViewBar {}
        Layout.fillWidth: true
        Layout.fillHeight: true
        contentWidth: availableWidth
        ColumnLayout {
            width: parent.width
            spacing: 14
            Item { implicitHeight: 2 }

            LoanProgressCard {
                visible: AppModel.selectedAccount !== null && AppModel.selectedAccount.kind === "Loan"
                account: visible ? AppModel.selectedAccount : null
                Layout.fillWidth: true; Layout.leftMargin: 16; Layout.rightMargin: 16
            }

            // Keine Buchung ausgewählt
            Card {
                visible: inspector.bookingPage && !AppModel.isMultiSelection && inspector.b === null
                Layout.fillWidth: true; Layout.leftMargin: 16; Layout.rightMargin: 16
                ColumnLayout {
                    width: parent.width; spacing: 10
                    RowLayout { SFIcon { symbol: "doc.text.magnifyingglass"; size: 16; color: Theme.text }
                        Text { text: qsTr("Keine Buchung ausgewählt"); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text } }
                    Text { text: qsTr("Wählen Sie eine Buchung, um Beleg, Händler-Historie und Kategorie zu sehen.")
                           font.pixelSize: Theme.callout + 1; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                }
            }

            // MultiSelectionCard
            Card {
                id: multi
                visible: inspector.bookingPage && AppModel.isMultiSelection
                Layout.fillWidth: true; Layout.leftMargin: 16; Layout.rightMargin: 16
                readonly property real income: AppModel.selectedBookings.filter(x => x.amount_cents > 0).reduce((s, x) => s + x.amount_cents, 0)
                readonly property real expense: AppModel.selectedBookings.filter(x => x.amount_cents < 0).reduce((s, x) => s + x.amount_cents, 0)
                ColumnLayout {
                    id: multiColumn
                    width: parent.width; spacing: 12
                    RowLayout { SFIcon { symbol: "checkmark.circle"; size: 16; color: Theme.text }
                        Text { text: qsTr("%1 Buchungen ausgewählt").arg(AppModel.selectedBookings.length); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text } }
                    GridLayout {
                        columns: 2; Layout.fillWidth: true; rowSpacing: 4
                        Text { text: qsTr("Summe"); color: Theme.text; font.pixelSize: Theme.callout + 1; Layout.fillWidth: true }
                        Text { text: Fmt.money(multi.income + multi.expense); color: Theme.secondary; font.pixelSize: Theme.callout + 1; Sensitive { amounts: true } }
                        Text { visible: multi.income !== 0; text: qsTr("Einnahmen"); color: Theme.text; font.pixelSize: Theme.callout + 1 }
                        Text { visible: multi.income !== 0; text: Fmt.money(multi.income); color: Theme.secondary; font.pixelSize: Theme.callout + 1; Sensitive { amounts: true } }
                        Text { visible: multi.expense !== 0; text: qsTr("Ausgaben"); color: Theme.text; font.pixelSize: Theme.callout + 1 }
                        Text { visible: multi.expense !== 0; text: Fmt.money(multi.expense); color: Theme.secondary; font.pixelSize: Theme.callout + 1; Sensitive { amounts: true } }
                    }
                    // MultiSelectionTags (#228)
                    TagsSection { Layout.fillWidth: true; bookings: multi.visible ? AppModel.selectedBookings : [] }
                    Button { text: qsTr("Kategorisieren …"); icon.name: SF.icon("tag"); highlighted: true; Layout.fillWidth: true; onClicked: AppModel.showCategorizer = true }
                    Button { text: qsTr("Auswahl aufheben"); onClicked: AppModel.selectBooking(AppModel.selectedBookingID) }
                    Text { text: qsTr("Strg-Klick fügt eine Buchung hinzu oder entfernt sie, ⇧-Klick wählt einen Bereich, Strg+A alle.")
                           font.pixelSize: Theme.caption; color: Theme.tertiary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                }
            }

            Loader {
                active: inspector.bookingPage && !AppModel.isMultiSelection && inspector.b !== null
                visible: active
                Layout.fillWidth: true; Layout.leftMargin: 16; Layout.rightMargin: 16
                // edit mode: room on top for the ⊖ / ⊕ buttons on the cards' corners
                Layout.topMargin: inspector.editingLayout ? 8 : 0
                sourceComponent: inspector.editingLayout ? layoutEditor : sectionCards
            }
            // PlannedInspector / MandateInspector
            Loader {
                active: !inspector.bookingPage
                visible: active
                Layout.fillWidth: true; Layout.leftMargin: 16; Layout.rightMargin: 16
                sourceComponent: AppModel.sidebarSelection === "planned" ? plannedInspector : mandateInspector
            }
            Item { implicitHeight: 2 }
        }
    }
    }

    Component {
        id: sectionCards
        ColumnLayout {
            spacing: 14
            Repeater {
                model: InspectorLayout.shown
                Loader {
                    required property string modelData
                    Layout.fillWidth: true
                    visible: status === Loader.Ready
                    sourceComponent: inspector.cardComponent(modelData, false)
                }
            }
        }
    }
    Component {
        id: layoutEditor
        InspectorLayoutEditor {
            booking: inspector.b
            cardComponent: (section, placeholder) => inspector.cardComponent(section, placeholder)
            flickable: scroll.contentItem
        }
    }

    // InspectorLayoutBar: „Anpassen“ switches to the edit mode, which shows a hint,
    // „Zurücksetzen“ once something changed, and „Fertig“.
    component LayoutBar: Rectangle {
        color: Theme.canvas
        implicitHeight: barRow.implicitHeight + 12
        RowLayout {
            id: barRow
            anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
            anchors.leftMargin: 16; anchors.rightMargin: 16; anchors.topMargin: 8
            spacing: 12
            Text {
                visible: inspector.editingLayout
                text: qsTr("Ziehen zum Verschieben")
                font.pixelSize: Theme.caption; color: Theme.secondary
                wrapMode: Text.WordWrap; maximumLineCount: 2; elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Item { visible: !inspector.editingLayout; Layout.fillWidth: true }
            IconButton {
                visible: inspector.editingLayout && !InspectorLayout.isStandard
                text: qsTr("Zurücksetzen"); symbol: ""; font.pixelSize: Theme.caption; textColor: Theme.accent
                ToolTip.text: qsTr("Standard-Reihenfolge wiederherstellen und alle Bereiche einblenden")
                onClicked: InspectorLayout.reset()
            }
            IconButton {
                text: inspector.editingLayout ? qsTr("Fertig") : qsTr("Anpassen")
                symbol: inspector.editingLayout ? "" : "slider.horizontal.3"; symbolSize: 12
                font.pixelSize: Theme.caption; font.weight: inspector.editingLayout ? Font.DemiBold : Font.Normal
                textColor: Theme.accent; tint: Theme.accent
                ToolTip.text: inspector.editingLayout ? qsTr("Bearbeitung der Seitenleiste beenden")
                                                      : qsTr("Reihenfolge der Bereiche ändern und Bereiche ausblenden")
                onClicked: inspector.editingLayout = !inspector.editingLayout
            }
        }
    }

    // InspectorPlaceholderCard: stand-in for a card that has nothing to show for this booking.
    component PlaceholderCard: Card {
        id: ph
        property string section
        property string hint
        readonly property var info: InspectorLayout.section(section)
        ColumnLayout {
            width: parent.width; spacing: 4
            CardTitle { text: ph.info.title; symbol: ph.info.symbol }
            Text { text: ph.hint; font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
        }
    }

    component CardTitle: RowLayout {
        property string text
        property string symbol
        property color tint: Theme.text
        spacing: 6
        SFIcon { visible: parent.symbol !== ""; symbol: parent.symbol; size: 14; color: parent.tint }
        Text { text: parent.text; font.pixelSize: Theme.subheadline + 2; font.weight: Font.DemiBold; color: parent.tint }
    }

    // ReceiptCard
    component ReceiptCard: Card {
        id: rc
        property var booking
        readonly property var amazonRef: AmazonOrderRef.parse(booking)
        ColumnLayout {
            width: parent.width; spacing: 12
            RowLayout {
                Item { Layout.fillWidth: true }
                IconButton {
                    text: qsTr("Text bearbeiten …"); symbol: "pencil"; symbolSize: 12
                    display: AbstractButton.TextBesideIcon; font.pixelSize: Theme.caption
                    ToolTip.text: qsTr("Gegenpartei und Verwendungszweck korrigieren"); ToolTip.visible: hovered
                    onClicked: { nameField.text = rc.booking.counterparty; purposeField.text = rc.booking.purpose; editPopup.open() }
                    Popup {
                        id: editPopup
                        y: parent.height; x: parent.width - width
                        width: 340; padding: 14
                        ColumnLayout {
                            anchors.fill: parent; spacing: 8
                            Text { text: qsTr("Gegenpartei"); font.pixelSize: Theme.caption; color: Theme.secondary }
                            TextField { id: nameField; Layout.fillWidth: true }
                            Text { text: qsTr("Verwendungszweck"); font.pixelSize: Theme.caption; color: Theme.secondary }
                            TextArea { id: purposeField; Layout.fillWidth: true; Layout.preferredHeight: 90; wrapMode: TextEdit.Wrap }
                            RowLayout {
                                Item { Layout.fillWidth: true }
                                Button { text: qsTr("Abbrechen"); onClicked: editPopup.close() }
                                Button { text: qsTr("Speichern"); highlighted: true
                                         onClicked: { AppModel.updateText(rc.booking, nameField.text, purposeField.text); editPopup.close() } }
                            }
                        }
                    }
                }
            }
            RowLayout {
                spacing: 12
                MerchantSignet { merchant: rc.booking.merchant; categorySymbol: rc.booking.category_symbol; size: 48 }
                // Always as high as a two-line name: the amount and everything below keep
                // their place from booking to booking. A one-line name block is centred
                // next to the signet (free space above and below, not between the lines).
                ColumnLayout {
                    id: titleBlock
                    spacing: 2; Layout.fillWidth: true
                    Layout.minimumWidth: 0   // the name and account line elide (larger text size, #343)
                    Layout.preferredHeight: Math.max(implicitHeight, implicitHeight - merchantName.implicitHeight
                                                     + Math.ceil(nameMetrics.lineSpacing * 2))
                    Item { Layout.fillHeight: true }
                    // longer names end in "…" after two lines (full name as tooltip)
                    Text {
                        id: merchantName
                        text: rc.booking.merchant.name
                        wrapMode: Text.Wrap; maximumLineCount: 2; elide: Text.ElideRight
                        font.pixelSize: Theme.title3 + 2; font.weight: Font.DemiBold; color: Theme.text
                        Layout.fillWidth: true
                        FontMetrics { id: nameMetrics; font: merchantName.font }
                        HoverHandler { id: nameHover; enabled: merchantName.truncated }
                        ToolTip.visible: nameHover.hovered; ToolTip.text: AppModel.masking(text); ToolTip.delay: 500
                        Sensitive {}
                    }
                    Text { text: rc.booking.account_name + " · " + rc.booking.payment_method; font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                    // the counter account: an own account or pocket by name (#523), else its IBAN
                    TextEdit {
                        readonly property var counter: BookingDetails.counterAccount(rc.booking)
                        visible: counter !== null || rc.booking.counterparty_iban !== ""
                        text: counter ? qsTr("⇄ %1 (%2)").arg(counter.name).arg(Fmt.iban(rc.booking.counterparty_iban)) : Fmt.iban(rc.booking.counterparty_iban)
                        readOnly: true; selectByMouse: true; wrapMode: Text.Wrap; Layout.fillWidth: true
                        font.pixelSize: Theme.caption; font.family: counter ? Qt.application.font.family : "monospace"; color: Theme.secondary
                        Sensitive {}
                    }
                    MerchantDomainRow { merchant: rc.booking.merchant; Layout.fillWidth: true }
                    Item { Layout.fillHeight: true }
                }
            }
            Text {
                text: Fmt.signed(rc.booking.amount_cents)
                font.pixelSize: Theme.largeTitle + 6; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                color: rc.booking.amount_cents > 0 ? Theme.inflow : Theme.outflow
                Sensitive { amounts: true }
            }
            RowLayout {
                spacing: 6
                SFIcon { symbol: "calendar"; size: 12; color: Theme.tertiary }
                Text { text: Fmt.longDate(rc.booking.booking_date); font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true }
                FlowBadge { flow: rc.booking.flow }
            }
            // Amazon order: one item per line instead of the ";" purpose run;
            // an item with a stored ASIN opens its product page (#354)
            ColumnLayout {
                visible: rc.amazonRef !== null && rc.amazonRef.itemList.length > 0
                Layout.fillWidth: true; spacing: 2
                Repeater {
                    model: visible ? rc.amazonRef.itemList : []
                    TextEdit {
                        id: amazonItem
                        required property int index
                        required property string modelData
                        readonly property string url: AmazonOrderRef.productURL(rc.amazonRef, index)
                        text: modelData; readOnly: true; selectByMouse: url === ""
                        wrapMode: Text.Wrap; font.pixelSize: Theme.callout + 1; Layout.fillWidth: true
                        color: url !== "" ? Theme.accent : Theme.text
                        HoverHandler { id: itemHover; enabled: amazonItem.url !== ""; cursorShape: Qt.PointingHandCursor }
                        TapHandler { enabled: amazonItem.url !== ""; onTapped: Qt.openUrlExternally(amazonItem.url) }
                        ToolTip.visible: itemHover.hovered; ToolTip.delay: 500
                        ToolTip.text: qsTr("Produkt auf %1 öffnen").arg(rc.amazonRef.domain)
                        Sensitive {}
                    }
                }
            }
            TextEdit {
                visible: !(rc.amazonRef !== null && rc.amazonRef.itemList.length > 0) && rc.booking.clean_purpose !== ""
                text: rc.booking.clean_purpose; readOnly: true; selectByMouse: true
                wrapMode: Text.Wrap; font.pixelSize: Theme.callout + 1; color: Theme.text; Layout.fillWidth: true
                Sensitive {}
            }
            // refund: jump to the order it belongs to (same order code)
            RowLayout {
                id: refundJump
                readonly property var source: AmazonOrderRef.refundSource(rc.booking, AppModel.bookings)
                visible: source !== null
                Layout.fillWidth: true; spacing: 5
                SFIcon { symbol: "arrow.uturn.backward"; size: 11; color: Theme.accent; Layout.alignment: Qt.AlignTop; Layout.topMargin: 2 }
                Text {
                    text: refundJump.source ? qsTr("Erstattung zu: %1").arg(refundJump.source.items) : ""
                    font.pixelSize: Theme.caption; color: Theme.accent; wrapMode: Text.Wrap; Layout.fillWidth: true
                    Sensitive {}
                }
                HoverHandler { id: refundHover; cursorShape: Qt.PointingHandCursor }
                TapHandler { onTapped: AppModel.selectedBookingID = refundJump.source.id }
                ToolTip.visible: refundHover.hovered; ToolTip.delay: 600
                ToolTip.text: qsTr("Zur erstatteten Bestellung springen")
            }
            RowLayout {
                Text { text: qsTr("Geprüft"); font.pixelSize: Theme.caption; color: Theme.text }
                Switch { checked: rc.booking.checked; onToggled: AppModel.setChecked(checked, rc.booking); scale: 0.8 }
                Item { Layout.fillWidth: true }
            }
        }
    }

    component FlowBadge: Rectangle {
        property string flow
        readonly property color tint: flow === "Investment" || flow === "Perk" ? Theme.invest : flow === "Income" ? Theme.inflow
                                    : flow === "Loan" ? Theme.loan : flow === "Transfer" ? Theme.secondary : Theme.outflow
        implicitWidth: fl.implicitWidth + 14; implicitHeight: fl.implicitHeight + 4; radius: height / 2
        color: Theme.withAlpha(tint, 0.08)
        Text { id: fl; anchors.centerIn: parent; text: AppModel.flowLabels[parent.flow] || ""; font.pixelSize: Theme.caption2; font.weight: Font.DemiBold; color: parent.tint }
    }

    component MerchantDomainRow: RowLayout {
        property var merchant
        readonly property string sourceLabel: ({ dictionary: qsTr("Wörterbuch"), company: qsTr("Firmenname"), person: qsTr("Privatperson"),
                                                 override: qsTr("manuell") })[merchant.domain_source] || qsTr("keine Domain")
        id: domainRow
        spacing: 4
        // BookingDetailView.swift › MerchantDomainRow: edit button → field for a manual domain
        property bool editing: false
        onMerchantChanged: editing = false
        function save() { MerchantLogos.setMerchantDomain(merchant, domainField.text); editing = false }
        SFIcon { visible: !domainRow.editing; symbol: "globe"; size: 10; color: Theme.tertiary }
        Text { visible: !domainRow.editing
               text: merchant.domain || (merchant.is_person ? qsTr("Privatperson – kein Logo") : qsTr("Domain festlegen"))
               font.family: "monospace"; font.pixelSize: Theme.caption; color: Theme.tertiary
               elide: Text.ElideRight; Layout.minimumWidth: 24; Layout.preferredWidth: implicitWidth
               // a guessed domain spells out the (redacted) counterparty; dictionary brands show their logo anyway
               Sensitive { when: merchant.domain !== "" && merchant.domain_source !== "dictionary" } }
        Text { visible: !domainRow.editing && merchant.domain !== ""; text: "· " + parent.sourceLabel; font.pixelSize: Theme.caption2; color: Theme.tertiary }
        IconButton {
            visible: !domainRow.editing
            symbol: "pencil"; symbolSize: 10; implicitWidth: 20; implicitHeight: 18
            tint: Theme.tertiary
            ToolTip.text: qsTr("Eigene Domain für das Logo eintragen")
            Accessible.name: qsTr("Domain bearbeiten")
            onClicked: { domainField.text = merchant.domain; domainRow.editing = true; domainField.forceActiveFocus() }
        }
        TextField {
            id: domainField
            visible: domainRow.editing
            Layout.maximumWidth: 180; Layout.fillWidth: true
            placeholderText: qsTr("z. B. enbw.com")
            font.pixelSize: Theme.caption
            inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText | Qt.ImhUrlCharactersOnly
            onAccepted: domainRow.save()
            Keys.onEscapePressed: domainRow.editing = false
            Sensitive {}
        }
        Button { visible: domainRow.editing; text: qsTr("Logo laden"); font.pixelSize: Theme.caption; onClicked: domainRow.save() }
        IconButton {
            visible: domainRow.editing && merchant.domain_source === "override"
            symbol: "arrow.uturn.backward"; symbolSize: 12
            ToolTip.text: qsTr("Automatische Erkennung wiederherstellen")
            onClicked: { MerchantLogos.setMerchantDomain(merchant, null); domainRow.editing = false }
        }
        IconButton {
            visible: domainRow.editing
            symbol: "xmark"; symbolSize: 12
            ToolTip.text: qsTr("Abbrechen")
            onClicked: domainRow.editing = false
        }
        Item { Layout.fillWidth: true }
    }

    // NoteCard: saved when the editor loses focus. SwiftUI recreates the card per
    // booking (saving on disappear); here the draft is saved to the booking it was
    // typed for before the next booking's note is loaded.
    component NoteCard: Card {
        id: nc
        property var booking
        /// The booking the editor's text belongs to.
        property var shown: null
        property string draft: ""
        onBookingChanged: {
            if (booking && shown && booking.id === shown.id) {
                // same booking, fresh data (e.g. after a reload)
                shown = booking
                if (!note.activeFocus) note.text = booking.note
                return
            }
            const prev = shown, text = draft
            shown = booking
            note.text = booking ? booking.note : ""
            if (prev && text.trim() !== prev.note) AppModel.updateNote(prev, text)
            claimFocus()
        }
        function save() { if (shown && draft.trim() !== shown.note) AppModel.updateNote(shown, draft) }
        /// Takes the focus requested by a double-click / Return in the list for this booking.
        function claimFocus() {
            if (!shown || AppModel.pendingNoteFocus !== shown.id) return
            AppModel.pendingNoteFocus = null
            Qt.callLater(() => { note.forceActiveFocus(); note.cursorPosition = note.length })
        }
        Connections { target: AppModel; function onPendingNoteFocusChanged() { nc.claimFocus() } }
        Component.onCompleted: if (shown === null && booking) { shown = booking; note.text = booking.note; claimFocus() }
        Component.onDestruction: {
            const s = shown, text = draft
            if (s && text.trim() !== s.note) Qt.callLater(() => AppModel.updateNote(s, text))
        }
        ColumnLayout {
            width: parent.width; spacing: 6
            CardTitle { text: qsTr("Notiz"); symbol: "note.text" }
            TextArea {
                id: note
                Layout.fillWidth: true; Layout.minimumHeight: 54; Layout.maximumHeight: 120
                wrapMode: TextEdit.Wrap; font.pixelSize: Theme.callout + 1
                onTextChanged: nc.draft = text
                onActiveFocusChanged: if (!activeFocus) nc.save()
                // Esc: leave the note (saves via focus loss) and go back to the list
                Keys.onEscapePressed: AppModel.focusList()
                Sensitive {}
            }
        }
    }

    // SalaryCard
    component SalaryCard: Card {
        property var booking
        readonly property string choice: booking.salary_flag === true ? "yes" : booking.salary_flag === false ? "no" : "auto"
        ColumnLayout {
            width: parent.width; spacing: 8
            CardTitle { text: qsTr("Gehalt / Rente"); tint: booking.is_salary ? Theme.inflow : Theme.text }
            Segmented {
                Layout.fillWidth: true
                options: [{ id: "auto", title: qsTr("Automatisch") }, { id: "yes", title: qsTr("Ja") }, { id: "no", title: qsTr("Nein") }]
                current: choice
                onActivated: id => AppModel.setSalaryFlag(booking, id === "auto" ? null : id === "yes")
            }
            Text {
                Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                text: choice === "yes" ? qsTr("Als Gehalt markiert. Weitere Eingänge von diesem Konto zählen ebenfalls als Gehalt.")
                    : choice === "no" ? qsTr("Wird nie als Gehalt gezählt.")
                    : booking.is_salary ? qsTr("Als Gehalt erkannt (Purpose-Code, Geschäftsvorfallcode oder Verwendungszweck).")
                    : qsTr("Nicht als Gehalt erkannt. Markieren Sie den Eingang, damit der Zeitraum „Seit letztem Gehalt“ ihn nutzt.")
            }
        }
    }

    // RecurringUI.swift › RecurringCard
    component RecurringCard: Card {
        id: rec
        property var booking
        readonly property var series: AppModel.recurringSeries(booking)
        ColumnLayout {
            width: parent.width; spacing: 8
            RowLayout {
                CardTitle { text: qsTr("Wiederkehrend"); symbol: "repeat"; tint: rec.series === null ? Theme.text : Theme.accent; Layout.fillWidth: true }
                ToolButton {
                    text: (rec.series === null ? qsTr("Markieren") : qsTr("Ändern")) + "  ▾"
                    flat: true
                    onClicked: recMenu.popup()
                    AppMenu {
                        id: recMenu
                        Repeater {
                            model: ["Monthly", "Quarterly", "HalfYearly", "Yearly"]
                            AppMenuItem { required property string modelData; text: AppModel.intervalLabels[modelData]; checkable: true
                                       checked: rec.series !== null && rec.series.interval === modelData
                                       onTriggered: AppModel.markRecurring(rec.booking, modelData) }
                        }
                        AppMenuItem { text: qsTr("Individuell …"); checkable: true
                                   checked: rec.series !== null && rec.series.interval === "Custom"
                                   onTriggered: { checked = Qt.binding(() => rec.series !== null && rec.series.interval === "Custom"); AppModel.customRecurringBooking = rec.booking } }
                        AppMenuSeparator {}
                        AppMenuItem { text: qsTr("Automatisch erkennen"); enabled: rec.series !== null && rec.series.source === "Manual"; onTriggered: AppModel.resetRecurring(rec.series.series_key) }
                        AppMenuItem { text: qsTr("Nicht wiederkehrend"); enabled: rec.series !== null; onTriggered: AppModel.ignoreRecurring(rec.series) }
                    }
                }
            }
            Text {
                visible: rec.series !== null
                text: rec.series ? AppModel.intervalText(rec.series) + " · " + qsTr("nächste am %1").arg(Fmt.medium(rec.series.next_due)) : ""
                font.pixelSize: Theme.callout + 1; color: Theme.text
            }
            Text {
                Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                text: rec.series === null
                    ? qsTr("Nicht als wiederkehrend erkannt. Markieren Sie die Buchung, damit sie in den anstehenden Fixkosten erscheint.")
                    : rec.series.source === "Manual" ? qsTr("Manuell markiert · ca. %1").arg(Fmt.money(rec.series.amount_cents))
                    : qsTr("Automatisch erkannt aus %1 Buchungen · ca. %2").arg(rec.series.occurrences).arg(Fmt.money(rec.series.amount_cents))
                Sensitive { amounts: true; when: rec.series !== null }
            }
        }
    }

    // MerchantHistoryCard.statsRow
    component StatsRow: ColumnLayout {
        id: sr
        property var stats: null
        property string label: ""
        property string accountName: ""
        spacing: 4; Layout.fillWidth: true
        Text { visible: sr.label !== ""; text: sr.accountName ? sr.label + " · " + sr.accountName : sr.label
               font.pixelSize: Theme.caption; font.weight: Font.Medium; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
               Sensitive { when: sr.accountName !== "" } }
        RowLayout {
            spacing: 10; Layout.fillWidth: true
            Repeater {
                model: sr.stats ? [[qsTr("Buchungen"), sr.stats.count + "×", false], [qsTr("Summe"), Fmt.money(sr.stats.total_cents), true], [qsTr("Ø Betrag"), Fmt.money(sr.stats.average_cents), true]] : []
                ColumnLayout { required property var modelData; spacing: 2; Layout.fillWidth: true; Layout.preferredWidth: 1
                    Text { text: modelData[0]; font.pixelSize: Theme.caption2; color: Theme.secondary }
                    Text { text: modelData[1]; font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true
                           Sensitive { amounts: true; when: modelData[2] } } }
            }
        }
        Text { visible: sr.stats !== null && sr.stats.first_date !== ""
               text: sr.stats ? qsTr("Seit %1 · zuletzt %2").arg(Fmt.medium(sr.stats.first_date)).arg(Fmt.medium(sr.stats.last_date)) : ""
               font.pixelSize: Theme.caption2; color: Theme.tertiary }
    }

    // MerchantHistoryCard
    component MerchantHistoryCard: Card {
        id: mh
        property var booking
        readonly property var s: AppModel.merchantStats && AppModel.merchantStats.key === booking.merchant.key ? AppModel.merchantStats : null
        /// The merchant also occurs on other accounts (#299): the booking's own account's stats.
        readonly property var own: {
            const o = AppModel.merchantAccountStats
            return s && o && o.account_id === booking.account_id && o.stats.key === s.key && o.stats.count < s.count ? o.stats : null
        }
        ColumnLayout {
            width: parent.width; spacing: 10
            // The heading opens the list of this merchant's bookings (#495).
            RowLayout {
                id: historyHeading
                readonly property bool enabledLink: mh.booking.merchant.key !== ""
                spacing: 4
                opacity: enabledLink ? 1 : 0.5
                CardTitle { text: qsTr("Händler-Historie") }
                SFIcon { symbol: "chevron.right"; size: 10; color: Theme.secondary }
                Accessible.role: Accessible.Button
                Accessible.name: qsTr("Händler-Historie")
                Accessible.description: qsTr("Alle Buchungen dieses Händlers im gewählten Zeitraum anzeigen")
                HoverHandler { id: historyHover; enabled: historyHeading.enabledLink; cursorShape: Qt.PointingHandCursor }
                TapHandler { enabled: historyHeading.enabledLink; onTapped: MerchantFocus.focusMerchant(mh.booking) }
                ToolTip.visible: historyHover.hovered; ToolTip.delay: 600
                ToolTip.text: qsTr("Alle Buchungen dieses Händlers im gewählten Zeitraum anzeigen")
            }
            // total and the booking's own account side by side, else one unlabelled row
            StatsRow { visible: mh.s !== null; stats: mh.s; label: mh.own ? qsTr("Alle Konten") : "" }
            StatsRow { visible: mh.own !== null; stats: mh.own; label: qsTr("Dieses Konto"); accountName: mh.booking.account_name }
            Rectangle { visible: mh.booking.merchant.isin !== "" || mh.booking.merchant.asset_class !== ""; Layout.fillWidth: true; height: 1; color: Theme.border }
            RowLayout {
                visible: mh.booking.merchant.isin !== "" || mh.booking.merchant.asset_class !== ""
                spacing: 14
                Repeater {
                    model: [[qsTr("Anlage"), mh.booking.merchant.asset_class], [qsTr("ISIN"), mh.booking.merchant.isin], [qsTr("WKN"), mh.booking.merchant.wkn]].filter(x => x[1])
                    ColumnLayout { required property var modelData; spacing: 1
                        Text { text: modelData[0]; font.pixelSize: Theme.caption2; color: Theme.secondary }
                        TextEdit { text: modelData[1]; readOnly: true; selectByMouse: true; font.family: "monospace"; font.pixelSize: Theme.caption; color: Theme.text } }
                }
            }
        }
    }

    // CategoryGridCard: top-level chips, then one row of chips per opened level.
    component CategoryGridCard: Card {
        id: cg
        property var booking
        property var openPath: null
        // a reload hands in a new object for the same booking: keep the opened level
        property var openFor: null
        onBookingChanged: { const id = booking ? booking.id : null; if (id !== openFor) { openFor = id; openPath = null } }
        readonly property var currentPath: {
            if (booking.category_id === null) return []
            const path = []
            let next = booking.category_id
            while (next !== null && path.length < 32) {
                const c = AppModel.categories.find(c => c.id === next)
                if (!c) break
                path.unshift(next)
                next = c.parent_id
            }
            const node = AppModel.categoryNode(booking.category_id)
            if (path.length && path[path.length - 1] === booking.category_id && node && !node.children) path.pop()
            return path
        }
        readonly property var path: openPath || currentPath
        function pick(node, level) {
            if (!node.children) AppModel.setCategory(booking, node.category.id)
            else openPath = path.slice(0, level).concat([node.category.id])
        }
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout {
                CardTitle { text: qsTr("Kategorie"); Layout.fillWidth: true }
                Text { text: cg.booking.category_path || SidebarModel.uncategorizedLabel; font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.maximumWidth: 160
                       Sensitive { labels: true; when: cg.booking.category_path !== "" } }
            }
            ChipGrid { card: cg; Layout.fillWidth: true; nodes: AppModel.categoryTree; selectedId: cg.path.length ? cg.path[0] : -1; level: 0; minimum: 104 }
            Repeater {
                model: cg.path
                ColumnLayout {
                    required property var modelData
                    required property int index
                    readonly property var node: AppModel.categoryNode(modelData)
                    visible: node !== null && !!node.children
                    Layout.fillWidth: true
                    spacing: 10
                    Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
                    ChipGrid { card: cg; Layout.fillWidth: true; nodes: parent.node && parent.node.children ? parent.node.children : []; filled: true; minimum: 120; level: index + 1
                               selectedId: index + 1 < cg.path.length ? cg.path[index + 1] : (cg.booking.category_id ?? -1) }
                }
            }
            Button { Layout.fillWidth: true; text: qsTr("Umsatz aufteilen (Splitbuchung) …"); icon.name: SF.icon("arrow.triangle.branch"); onClicked: AppModel.splitBooking = cg.booking }
            Button { Layout.fillWidth: true; text: qsTr("Regel für diesen Händler erstellen …"); icon.name: SF.icon("wand.and.stars"); highlighted: true
                     onClicked: AppModel.openRuleEditor(cg.booking) }
            Text { text: qsTr("In der Liste: C kategorisieren · R Regel erstellen"); font.pixelSize: Theme.caption2; color: Theme.tertiary }
        }
    }

    component ChipGrid: Flow {
        property var card
        property var nodes: []
        property var selectedId: -1
        property bool filled: false
        property int level: 0
        property int minimum: 104
        readonly property int columns: Math.max(1, Math.floor((width + 6) / (minimum + 6)))
        spacing: 6
        Repeater {
            model: parent.nodes
            CategoryChip {
                required property var modelData
                width: (parent.width - (parent.columns - 1) * 6) / parent.columns
                category: modelData.category
                selected: modelData.category.id === parent.selectedId
                filled: parent.filled
                onClicked: parent.card.pick(modelData, parent.level)
            }
        }
    }

    component CategoryChip: Rectangle {
        property var category
        property bool selected
        property bool filled
        signal clicked()
        implicitHeight: 30
        radius: 7
        color: selected ? Theme.selection : (filled ? Theme.canvas : Theme.card)
        border.color: selected ? Theme.accent : Theme.border
        border.width: selected ? 1.5 : 1
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 8; spacing: 5
            SFIcon { symbol: parent.parent.category.sf_symbol; size: 13; color: Theme.text }
            Text { text: parent.parent.category.name; font.pixelSize: Theme.caption + 1; font.weight: parent.parent.selected ? Font.DemiBold : Font.Normal
                   color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { labels: true } }
        }
        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: parent.clicked() }
    }

    // DocumentsUI.swift › DocumentsCard: attached receipts, preview, drop zone, export.
    component DocumentsCard: Card {
        id: dc
        property var booking
        property var selectedDoc: null
        property var docToDelete: null
        onBookingChanged: selectedDoc = null
        readonly property var docs: AppModel.documents(booking)
        readonly property var shown: selectedDoc || (docs.length ? docs[0] : null)
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout {
                CardTitle { text: qsTr("Belege & Abrechnungen"); symbol: "paperclip"; Layout.fillWidth: true }
                Text { visible: dc.docs.length > 0; text: dc.docs.length; font.pixelSize: Theme.caption; color: Theme.secondary }
            }
            Repeater {
                model: dc.docs
                Rectangle {
                    required property var modelData
                    Layout.fillWidth: true; implicitHeight: 40; radius: 7
                    color: dc.selectedDoc && dc.selectedDoc.id === modelData.id ? Theme.selection : "transparent"
                    RowLayout {
                        anchors.fill: parent; anchors.margins: 6; spacing: 8
                        SFIcon { symbol: AppModel.docSymbol(modelData); size: 15; color: Theme.accent; Layout.preferredWidth: 18 }
                        ColumnLayout { spacing: 1; Layout.fillWidth: true
                            Text { text: modelData.title; font.pixelSize: Theme.callout + 1; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                            Text { text: Fmt.medium(modelData.doc_date) + " · " + AppModel.byteCount(modelData.size_bytes); font.pixelSize: Theme.caption2; color: Theme.secondary } }
                        IconButton { symbol: "eye"; symbolSize: 14; ToolTip.text: qsTr("Vorschau (Leertaste)"); onClicked: AppModel.quickLook(modelData) }
                        IconButton { symbol: "ellipsis.circle"; symbolSize: 14; ToolTip.text: qsTr("Aktionen"); onClicked: docMenu.popup() }
                    }
                    TapHandler { onTapped: dc.selectedDoc = dc.selectedDoc && dc.selectedDoc.id === modelData.id ? null : modelData }
                    AppMenu { id: docMenu
                        AppMenuItem { text: qsTr("Im Dateimanager anzeigen"); onTriggered: AppModel.showInFileManager(modelData) }
                        AppMenuItem { text: qsTr("Zuordnung aufheben"); enabled: modelData.booking_id !== null && modelData.booking_id !== undefined
                                      onTriggered: AppModel.linkDocument(modelData, null) }
                        AppMenuSeparator {}
                        AppMenuItem { text: qsTr("Beleg löschen …"); onTriggered: dc.docToDelete = modelData } }
                }
            }
            DocumentPreview {
                visible: dc.shown !== null
                doc: dc.shown
                Layout.fillWidth: true; Layout.preferredHeight: 300
                radius: 8; border.color: Theme.border; clip: true
            }
            Rectangle {
                Layout.fillWidth: true; implicitHeight: dz.implicitHeight + 24; radius: 9
                color: drop.containsDrag ? Theme.selection : Theme.canvas
                border.color: drop.containsDrag ? Theme.accent : Theme.border
                ColumnLayout { id: dz; anchors.centerIn: parent; spacing: 4
                    SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "arrow.down.doc"; size: 18; color: drop.containsDrag ? Theme.accent : Theme.secondary }
                    Text { text: qsTr("PDF-Rechnung oder Kassenbon hierher ziehen"); font.pixelSize: Theme.caption; color: drop.containsDrag ? Theme.accent : Theme.secondary } }
                DropArea { id: drop; anchors.fill: parent; onDropped: d => { if (d.hasUrls) AppModel.attach(d.urls, dc.booking) } }
            }
            Button {
                Layout.fillWidth: true
                text: qsTr("Alle Belege exportieren …"); icon.name: SF.icon("square.and.arrow.up.on.square")
                ToolTip.text: qsTr("Für die Steuererklärung: alle Belege nach Jahr sortiert in einen Ordner"); ToolTip.visible: hovered
                onClicked: exportFolder.open()
            }
        }
        FolderDialog { id: exportFolder; title: qsTr("Ordner für die exportierten Belege wählen"); acceptLabel: qsTr("Exportieren")
                       onAccepted: AppModel.exportAllDocuments(AppModel.localPath(selectedFolder)) }
        Sheet {
            visible: dc.docToDelete !== null
            onClosed: dc.docToDelete = null
            sheetWidth: 380
            ColumnLayout { width: parent.width; spacing: 12
                Text { text: qsTr("Beleg löschen?"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text }
                Text { text: dc.docToDelete ? qsTr("„%1“ wird unwiderruflich gelöscht.").arg(dc.docToDelete.title) : ""; wrapMode: Text.WordWrap; Layout.fillWidth: true; color: Theme.text }
                RowLayout { Item { Layout.fillWidth: true }
                    Button { text: qsTr("Abbrechen"); onClicked: dc.docToDelete = null }
                    Button { text: qsTr("Löschen"); onClicked: { AppModel.deleteDocument(dc.docToDelete); dc.selectedDoc = null; dc.docToDelete = null } } } }
        }
    }

    // RawDataCard
    component RawDataCard: Card {
        id: raw
        property var booking
        property bool expanded: false
        function sepa(key) {
            const i = booking.purpose.indexOf(key + "+")
            return i < 0 ? null : booking.purpose.slice(i + key.length + 1).split(" ")[0]
        }
        readonly property var rows: {
            const r = [[qsTr("Verwendungszweck (roh)"), booking.purpose || "–"]]
            if (booking.booking_text) r.push([qsTr("Buchungstext"), booking.booking_text])
            const ref = AmazonOrderRef.parse(booking)
            if (ref) {
                // the order number opens the order on the shop (third entry: link)
                r.push([qsTr("Bestellnummer"), ref.code, AmazonOrderRef.orderURL(ref), qsTr("Bestellung auf %1 öffnen").arg(ref.domain)])
                if (ref.isSubscription) r.push([qsTr("Bestelltyp"), qsTr("Abonnement (Spar-Abo) – wiederkehrende Abbuchung")])
            }
            const e = sepa("EREF"), m = sepa("MREF"), c = sepa("CRED")
            if (e) r.push([qsTr("EREF (Ende-zu-Ende-Referenz)"), e])
            if (m) r.push([qsTr("MREF (Mandatsreferenz)"), m])
            if (c) r.push([qsTr("Gläubiger-ID"), c])
            if (booking.purpose_code) r.push([qsTr("Purpose-Code"), booking.purpose_code])
            if (booking.gvc) r.push([qsTr("Geschäftsvorfallcode"), booking.gvc])
            if (booking.counterparty_iban) r.push([qsTr("IBAN Gegenkonto"), Fmt.iban(booking.counterparty_iban)])
            r.push([qsTr("Gegenpartei (roh)"), booking.counterparty])
            if (booking.value_date) r.push([qsTr("Wertstellung"), Fmt.medium(booking.value_date)])
            r.push([qsTr("Konto"), booking.account_name])
            return r
        }
        ColumnLayout {
            width: parent.width; spacing: 0
            RowLayout {
                Layout.fillWidth: true
                CardTitle { text: qsTr("SEPA & Bank-Rohdaten"); symbol: "doc.plaintext"; Layout.fillWidth: true }
                SFIcon { symbol: "chevron.right"; size: 10; rotation: raw.expanded ? 90 : 0 }
                TapHandler { onTapped: raw.expanded = !raw.expanded }
            }
            ColumnLayout {
                visible: raw.expanded
                Layout.fillWidth: true; Layout.topMargin: 8
                spacing: 8
                Repeater {
                    model: raw.expanded ? raw.rows : []
                    ColumnLayout { required property var modelData; spacing: 1; Layout.fillWidth: true
                        Text { text: modelData[0]; font.pixelSize: Theme.caption2; color: Theme.secondary }
                        TextEdit {
                            id: rawValue
                            readonly property string url: modelData[2] || ""
                            text: modelData[1]; readOnly: true; selectByMouse: url === ""; wrapMode: Text.WrapAnywhere
                            font.family: "monospace"; font.pixelSize: Theme.caption; color: url !== "" ? Theme.accent : Theme.text
                            Layout.fillWidth: url === ""
                            HoverHandler { id: rawHover; enabled: rawValue.url !== ""; cursorShape: Qt.PointingHandCursor }
                            TapHandler { enabled: rawValue.url !== ""; onTapped: Qt.openUrlExternally(rawValue.url) }
                            ToolTip.visible: rawHover.hovered; ToolTip.delay: 500; ToolTip.text: modelData[3] || ""
                            // raw data is personal; codes and the value date stay visible
                            Sensitive { when: [qsTr("Purpose-Code"), qsTr("Geschäftsvorfallcode"), qsTr("Wertstellung")].indexOf(modelData[0]) < 0 }
                        } }
                }
            }
        }
    }
}
