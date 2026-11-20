import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// TransferSheetView.swift: "Neue Überweisung" – SEPA credit transfer via FinTS
// (HKCCS) or bunq. A grouped form (HIG): „Von Konto“ first, then recipient (or an own
// account for an „Umbuchung“), payment and „Ausführung“ as one choice. After the
// read-only review the model starts the session; the bank's TAN request is shown by
// TwoFactorSheet. Accounts GiroCrab cannot send from end with a GiroCode instead.
// „Einzeln | Sammelüberweisung“ under the title switches to BatchTransferSheet.
Sheet {
    id: sheet
    // .formStyle(.grouped) on macOS: bold section headers, hairlines between the rows,
    // borderless right-aligned fields (FormTextField), footers at the trailing edge.
    component GroupedSection: FormSection {
        separators: true
        headerFont.pixelSize: Theme.body; headerFont.weight: Font.Bold; headerColor: Theme.text
        footerAlignment: Text.AlignRight; footerFontSize: Theme.body; footerTrailingInset: 10
    }
    readonly property var draft: AppModel.transferDraft
    property bool confirming: false
    /// The user chose „Als GiroCode anzeigen“ although the account can send (#663).
    property bool giroCodeChosen: false
    /// Opt-in clipboard check (#663): { payment, summary } the banner offers, null = no banner.
    property var clipboardOffer: null
    property var suggestions: []
    property string importNote: ""
    /// Linux only: a dropped invoice is being read (large PDFs take seconds) – the sheet shows a spinner.
    property bool readingInvoice: false
    property var templates: []
    readonly property int maxPurpose: 140
    /// "Umbuchung": the recipient is one of the user's own accounts.
    property bool ownTransfer: false
    property var ownTargetID: null
    property bool demoReviewShown: false

    readonly property var account: AppModel.transferFormAccounts.find(a => a.id === draft.account_id) || null
    /// The account can send from GiroCrab (FinTS / bunq); otherwise the form ends with a GiroCode.
    readonly property bool canSend: account !== null && AppModel.canSendTransfer(account.id)
    /// The banking policy blocks this account's login (bankingGate(forAccount:)).
    readonly property bool gated: { void AppModel.bankingPolicy; return account !== null && AppModel.bankingGateForAccount(account.id) !== null }
    readonly property bool ibanValid: AppModel.validateIban(draft.iban)
    readonly property string bicText: draft.bic.trim()
    readonly property bool bicValid: bicText === "" || AppModel.looksLikeBIC(bicText)
    readonly property var cents: { const c = AppModel.parseCents(draft.amountText); return c !== null && c > 0 ? c : null }
    readonly property bool isBunq: account !== null && account.connector === "Bunq"
    readonly property bool offersInstant: account !== null && AppModel.offersInstantChoice(account.connector)
    readonly property bool sendsInstant: draft.instant && offersInstant
    /// TransferDraft.executionMode: "regular" | "scheduled" | "instant".
    readonly property string executionMode: draft.instant ? "instant" : (draft.scheduled ? "scheduled" : "regular")
    readonly property bool isMagicDemoAccount: {
        if (!account) return false
        const c = AppModel.connections.find(x => x.id === account.connection_id)
        return c ? AppModel.isMagicDemoLogin(c.login_name) : false
    }
    readonly property bool pinStored: {
        if (!account || account.connection_id === null) return false
        if (AppModel.isDemo) return true
        const st = AppModel.connectionStatus(account.connection_id)
        return st ? st.has_pin : false
    }
    /// Confirmation step: the GiroCode when the account cannot send or the user chose it, else the review.
    readonly property bool showsGiroCode: confirming && (!canSend || giroCodeChosen)
    readonly property bool showsReview: confirming && !showsGiroCode
    readonly property string giroCodePayload: cents === null ? "" : (GiroCode.payload(draft.name, draft.iban, bicText, cents, draft.purpose) || "")
    readonly property bool canReview: account !== null && draft.name.trim() !== "" && ibanValid && bicValid && cents !== null
                                      && draft.purpose.length <= maxPurpose
    readonly property string accountLabel: account ? account.name + " · " + Fmt.iban(account.iban) : "–"
    /// Own accounts that can receive a transfer: any kind, but with an IBAN, not archived, not the sender.
    readonly property var ownTargets: AppModel.allAccounts.filter(a => a.iban !== "" && a.archived_unix === null && a.id !== draft.account_id)

    visible: AppModel.showTransferSheet
    sheetWidth: 560
    padding: 0
    closePolicy: AppModel.transferBusy ? Popup.NoAutoClose : Popup.CloseOnEscape
    onAboutToShow: {
        // a new presentation starts like a new SwiftUI view: form, no Umbuchung
        confirming = AppModel.demoSheet === "transfer-confirm" && !demoReviewShown // screenshots
        demoReviewShown = true
        giroCodeChosen = false
        clipboardOffer = null
        ownTransfer = false
        ownTargetID = null
        importNote = ""
        suggestions = []
        templates = AppModel.transferTemplates()
        checkClipboard()
    }
    onClosed: AppModel.showTransferSheet = false

    function close_() { AppModel.showTransferSheet = false }
    function pasteFromClipboard() { report(PaymentParser.parseText(AppModel.clipboardText())) }
    function importInvoice(url) {
        importNote = qsTr("Lese Rechnung …")
        const path = decodeURIComponent(url.toString().replace(/^file:\/\//, ""))
        // read off the GUI thread (large PDFs); a closed sheet drops the late answer
        readingInvoice = true
        PaymentParser.parseFile(path, p => {
            readingInvoice = false
            if (sheet.visible && !sheet.confirming) sheet.report(p || {})
        })
    }
    function report(p) {
        if (PaymentParser.isEmpty(p)) { importNote = qsTr("Nichts Erkennbares gefunden"); return }
        AppModel.setDraft(PaymentParser.patch(p))
        importNote = qsTr("Felder ausgefüllt – bitte prüfen")
    }
    function refreshSuggestions() { suggestions = nameField.activeFocus ? AppModel.suggestRecipients(draft.name) : [] }
    /// "Nicht mehr vorschlagen" (#978): the chips are read again in place, whatever has the focus.
    function hideRecipient(iban) {
        AppModel.setRecipientHidden(iban, true)
        suggestions = AppModel.suggestRecipients(draft.name)
    }

    /// Fills name and IBAN from the chosen own account (owner, else account name).
    function applyOwnTarget(id) {
        const t = ownTargets.find(a => a.id === id)
        if (!t) return
        AppModel.setDraft({ name: t.owner === "" ? t.name : t.owner, iban: AppModel.groupedIBAN(t.iban), bic: "" })
        suggestions = []
    }
    function setOwnTransfer(on) {
        ownTransfer = on
        if (on) {
            if (!ownTargets.some(a => a.id === ownTargetID)) ownTargetID = ownTargets.length ? ownTargets[0].id : null
            applyOwnTarget(ownTargetID)
        } else {
            AppModel.setDraft({ name: "", iban: "", bic: "" })
        }
    }
    /// .onChange(of: transferDraft.accountID): the own target can no longer be the sender.
    function senderChanged() {
        if (!ownTransfer) return
        if (!ownTargets.some(a => a.id === ownTargetID)) ownTargetID = ownTargets.length ? ownTargets[0].id : null
        applyOwnTarget(ownTargetID)
    }
    function setExecutionMode(mode) { AppModel.setDraft({ instant: mode === "instant", scheduled: mode === "scheduled" }) }
    function goToReview() { AppModel.transferError = ""; confirming = true }
    function goBack() { confirming = false; giroCodeChosen = false }
    /// Nothing typed or prefilled yet (template, link, booking and Umbuchung count as filled).
    function draftIsEmpty() {
        return !ownTransfer && [draft.name, draft.iban, draft.amountText, draft.purpose].every(f => f.trim() === "")
    }
    /// "DE89 •••• •••• 3000": country, check digits and the last four characters.
    function maskedIBAN(iban) {
        const groups = AppModel.groupedIBAN(iban).split(" ").filter(g => g !== "")
        const all = groups.join("")
        return groups.length > 2 ? groups[0] + " •••• •••• " + all.slice(-4) : "•".repeat(all.length)
    }
    /// Masked IBAN, recipient and amount where found.
    function offerSummary(p) {
        const cents = p.amountText === undefined ? null : AppModel.parseCents(p.amountText)
        return [maskedIBAN(p.iban), p.name, cents === null ? undefined : Fmt.money(cents)].filter(x => x !== undefined && x !== "").join(" · ")
    }
    /// Opt-in (#663): offers the clipboard as a payment when the empty form opens, only if it
    /// changed since the last offer and holds a valid IBAN (the core sets `iban` only then).
    /// The text is parsed in memory and never stored or logged; only a hash of it is remembered.
    /// Linux has no paste prompt, so it is simply read. Entries that password managers mark as
    /// secret (x-kde-passwordManagerHint = secret, org.nspasteboard.ConcealedType / TransientType)
    /// are skipped, like on macOS: QML's TextEdit.paste() hands over the text only, so the mime
    /// types are checked natively first (cpp/prepare.cpp) and a concealed entry is never read.
    function checkClipboard() {
        if (AppModel.demoSheet === "transfer-clipboard") { // invented payment, the real clipboard stays untouched
            const demo = { name: "Stadtwerke München", iban: "DE89370400440532013000", amountText: Fmt.plain(8500), purpose: "Abschlag Strom Kd-Nr 482910" }
            clipboardOffer = { payment: demo, summary: offerSummary(demo) }
            return
        }
        if (!AppModel.clipboardCheck || !draftIsEmpty()) return
        if (AppModel.clipboardConcealed()) return
        const text = AppModel.clipboardText()
        if (text === "" || text.length > 8 * 1024) return // longer text is no payment slip
        let h = 5381
        for (let i = 0; i < text.length; i++) h = ((h * 33) ^ text.charCodeAt(i)) | 0
        if (h === AppModel.clipboardHandledHash) return
        AppModel.clipboardHandledHash = h // offered (or skipped) once per content
        const p = PaymentParser.parseText(text)
        if (p.iban === undefined) return
        clipboardOffer = { payment: p, summary: offerSummary(p) }
    }
    /// "Übernehmen" fills only empty fields.
    function acceptClipboard() {
        const patch = PaymentParser.patch(clipboardOffer.payment)
        clipboardOffer = null
        for (const k of Object.keys(patch)) if (String(draft[k]).trim() !== "") delete patch[k]
        AppModel.setDraft(patch)
        importNote = qsTr("Felder ausgefüllt – bitte prüfen")
    }

    // ⇧Strg+V; Strg+V outside a text field (inside one, normal paste keeps working).
    Shortcut { sequences: ["Ctrl+Shift+V", StandardKey.Paste]; enabled: sheet.visible && !sheet.confirming; onActivated: sheet.pasteFromClipboard() }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        // MARK: header – title left, the two helpers (templates, paste) as small actions right
        RowLayout {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 14; Layout.bottomMargin: 14
            spacing: 12
            Text {
                text: sheet.confirming ? (sheet.showsGiroCode ? qsTr("GiroCode scannen") : qsTr("Überweisung prüfen")) : qsTr("Neue Überweisung")
                font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text
                Accessible.role: Accessible.Heading
            }
            Item { Layout.fillWidth: true }
            // TransferTemplatesMenu (PlanningUI.swift)
            Button {
                id: templatesButton
                visible: !sheet.confirming
                flat: true; icon.name: SF.icon("doc.on.doc"); text: qsTr("Vorlagen")
                onClicked: templatesMenu.popup(templatesButton, 0, templatesButton.height)
            }
            Button {
                visible: !sheet.confirming
                flat: true; icon.name: SF.icon("person.2"); text: qsTr("Empfänger")
                onClicked: recipientsDialog.open()
                ToolTip.text: qsTr("Empfänger verwalten: veraltete IBANs nicht mehr vorschlagen")
                ToolTip.visible: hovered; ToolTip.delay: 600
            }
            Button {
                visible: !sheet.confirming
                flat: true; icon.name: SF.icon("doc.on.clipboard"); text: qsTr("Einfügen")
                onClicked: sheet.pasteFromClipboard()
                ToolTip.text: qsTr("Erkennt IBAN, Betrag, Empfänger und Zweck (⇧Strg+V; Strg+V außerhalb von Textfeldern). Rechnung (PDF, ZUGFeRD, GiroCode) hierher ziehen.")
                ToolTip.visible: hovered; ToolTip.delay: 600
            }
        }
        // TransferKindPicker(batch: false)
        Segmented {
            id: kindPicker
            visible: !sheet.confirming && Payments.canBatchTransfer
            Layout.alignment: Qt.AlignHCenter
            Layout.bottomMargin: 10
            options: [{ id: "single", title: qsTr("Einzeln") }, { id: "batch", title: qsTr("Sammelüberweisung") }]
            current: "single"
            onActivated: id => { if (id === "batch") Payments.switchToBatchTransfer() }
        }
        // ClipboardPaymentBanner: "Übernehmen" fills only empty fields
        Rectangle {
            visible: !sheet.confirming && sheet.clipboardOffer !== null
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.bottomMargin: 10
            implicitHeight: offerRow.implicitHeight + 20
            radius: 10; color: Theme.withAlpha(Theme.accent, 0.12)
            RowLayout {
                id: offerRow
                x: 10; y: 10; width: parent.width - 20
                spacing: 10
                SFIcon { symbol: "doc.on.clipboard"; size: 14; color: Theme.accent }
                ColumnLayout {
                    Layout.fillWidth: true; spacing: 2
                    Text { text: qsTr("Zahlung in der Zwischenablage"); font.pixelSize: Theme.callout; font.weight: Font.DemiBold; color: Theme.text }
                    Text {
                        Layout.fillWidth: true
                        text: sheet.clipboardOffer ? sheet.clipboardOffer.summary : ""
                        elide: Text.ElideRight; font.pixelSize: Theme.caption; color: Theme.secondary
                        font.features: { "tnum": 1 }
                        Sensitive { amounts: true } // masked IBAN, recipient and amount
                    }
                }
                Button { text: qsTr("Ignorieren"); onClicked: sheet.clipboardOffer = null }
                Button { text: qsTr("Übernehmen"); highlighted: true; onClicked: sheet.acceptClipboard() }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        ScrollView {
            id: scroll
            ScrollBar.vertical: ScrollViewBar {}
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 28 + (kindPicker.visible ? kindPicker.implicitHeight + 10 : 0)
                                           + messages.implicitHeight + buttonRow.implicitHeight + 30
            readonly property real body: sheet.confirming ? (sheet.showsGiroCode ? giroCode.implicitHeight : review.implicitHeight) : form.implicitHeight
            Layout.preferredHeight: Math.min(body + 40, Math.min(700, sheet.maxBodyHeight) - chrome)
            contentWidth: availableWidth
            // several children (form / review): the ScrollView cannot guess the height itself
            contentHeight: body + 40
            clip: true

            // MARK: form
            ColumnLayout {
                id: form
                visible: !sheet.confirming
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 6
                    GroupedSection {
                        FormRow {
                            label: qsTr("Von Konto")
                            AppComboBox {
                                Layout.preferredWidth: 300
                                model: AppModel.transferFormAccounts.map(a => a.name + " · " + Fmt.iban(a.iban))
                                currentIndex: AppModel.transferFormAccounts.findIndex(a => a.id === sheet.draft.account_id)
                                onActivated: i => { AppModel.setDraft({ account_id: AppModel.transferFormAccounts[i].id }); sheet.senderChanged() }
                                Sensitive {}
                            }
                        }
                    }
                    // footer: why this account ends in a GiroCode instead of a direct transfer
                    Hint {
                        visible: sheet.account !== null && !sheet.canSend
                        symbol: sheet.gated ? "exclamationmark.shield" : "qrcode"
                        text: sheet.gated
                              // Security lock: no direct transfer, the GiroCode needs no bank contact.
                              ? qsTr("Die Sicherheitssperre verhindert direkte Überweisungen von diesem Konto. Am Ende erhalten Sie einen GiroCode zum Scannen in der App Ihrer Bank.")
                              : sheet.account && sheet.account.connector === "Comdirect"
                              // The comdirect REST API has no payment endpoint; FinTS has.
                              ? qsTr("Die comdirect-Schnittstelle (REST) kann keine Überweisungen ausführen. Verbinden Sie das Konto zusätzlich per FinTS, um direkt zu überweisen. Sonst erhalten Sie am Ende einen GiroCode zum Scannen in der comdirect-App.")
                              : qsTr("Von diesem Konto kann GiroCrab nicht direkt überweisen. Am Ende erhalten Sie einen GiroCode zum Scannen in der App Ihrer Bank.")
                    }
                }

                // Picker("Art der Zahlung").pickerStyle(.segmented).labelsHidden()
                GroupedSection {
                    visible: sheet.ownTargets.length > 0 || sheet.ownTransfer
                    Segmented {
                        Layout.fillWidth: true
                        options: [{ id: "transfer", title: qsTr("Überweisung") }, { id: "own", title: qsTr("Umbuchung") }]
                        current: sheet.ownTransfer ? "own" : "transfer"
                        onActivated: id => sheet.setOwnTransfer(id === "own")
                        Accessible.name: qsTr("Art der Zahlung")
                    }
                }

                GroupedSection {
                    visible: sheet.ownTransfer
                    title: qsTr("Eigenes Zielkonto")
                    footer: qsTr("Name und IBAN werden aus dem gewählten Konto übernommen.")
                    FormRow {
                        label: qsTr("Zielkonto")
                        AppComboBox {
                            Layout.preferredWidth: 300
                            model: sheet.ownTargets.map(a => a.name + " · " + Fmt.iban(a.iban))
                            currentIndex: sheet.ownTargets.findIndex(a => a.id === sheet.ownTargetID)
                            onActivated: i => { sheet.ownTargetID = sheet.ownTargets[i].id; sheet.applyOwnTarget(sheet.ownTargetID) }
                            Sensitive {}
                        }
                    }
                    FormRow {
                        label: qsTr("Name")
                        Text { text: sheet.draft.name; font.pixelSize: Theme.body; color: Theme.text; Sensitive {} }
                    }
                    FormRow {
                        label: qsTr("IBAN")
                        Text { text: Fmt.iban(sheet.draft.iban); font.pixelSize: Theme.body; font.family: "monospace"; color: Theme.text; Sensitive {} }
                    }
                }

                GroupedSection {
                    visible: !sheet.ownTransfer
                    title: qsTr("Empfänger")
                    footer: sheet.importNote || qsTr("Oder Rechnung hierher ziehen")
                    FormRow {
                        label: qsTr("Name")
                        fillValue: true
                        FormTextField {
                            id: nameField
                            placeholderText: qsTr("Name des Empfängers")
                            text: sheet.draft.name
                            onTextEdited: { AppModel.setDraft({ name: text }); sheet.refreshSuggestions() }
                            onActiveFocusChanged: sheet.refreshSuggestions()
                            Sensitive {}
                        }
                    }
                    Flickable {
                        visible: sheet.suggestions.length > 0
                        Layout.fillWidth: true
                        implicitHeight: chips.implicitHeight
                        contentWidth: chips.implicitWidth
                        clip: true
                        flickableDirection: Flickable.HorizontalFlick
                        Row {
                            id: chips
                            spacing: 6
                            // one chip per IBAN with its last use; ✕: "Nicht mehr vorschlagen" (#978). No
                            // context menu: a menu takes the focus from the name field, which clears the chips.
                            Repeater {
                                model: sheet.suggestions
                                delegate: Rectangle {
                                    id: chipBox
                                    required property var modelData
                                    width: chip.implicitWidth + closeButton.width + 20; height: chip.implicitHeight + 8
                                    radius: 7; color: Theme.selection
                                    Column {
                                        id: chip
                                        x: 8; y: 4
                                        Text { text: modelData.name; font.pixelSize: Theme.callout; color: Theme.text; elide: Text.ElideRight; width: Math.min(implicitWidth, 180); Sensitive {} }
                                        Text { text: Fmt.iban(modelData.iban); font.pixelSize: Theme.caption2; font.family: "monospace"; color: Theme.secondary; Sensitive {} }
                                        Text { text: qsTr("zuletzt %1").arg(Fmt.medium(modelData.last_used)); font.pixelSize: Theme.caption2; color: Theme.tertiary }
                                    }
                                    MouseArea {
                                        anchors.fill: parent; cursorShape: Qt.PointingHandCursor
                                        onClicked: { AppModel.setDraft({ name: modelData.name, iban: AppModel.groupedIBAN(modelData.iban) }); sheet.suggestions = [] }
                                    }
                                    IconButton {
                                        id: closeButton
                                        anchors.right: parent.right; anchors.rightMargin: 2; anchors.top: parent.top; anchors.topMargin: 2
                                        // a click must leave the focus in the name field (it keeps the chips)
                                        focusPolicy: Qt.NoFocus
                                        symbol: "xmark.circle.fill"; symbolSize: 13; implicitWidth: 22; implicitHeight: 22
                                        ToolTip.text: qsTr("Nicht mehr vorschlagen")
                                        Accessible.name: qsTr("Nicht mehr vorschlagen")
                                        onClicked: sheet.hideRecipient(chipBox.modelData.iban)
                                    }
                                }
                            }
                        }
                    }
                    FormRow {
                        label: qsTr("IBAN")
                        fillValue: true
                        FormTextField {
                            placeholderText: "DE00 0000 0000 0000 0000 00"
                            font.family: "monospace"
                            inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhPreferUppercase
                            text: sheet.draft.iban
                            onTextEdited: AppModel.setDraft({ iban: AppModel.groupedIBAN(text) })
                            Sensitive {}
                        }
                        ValidMark { visible: sheet.draft.iban !== ""; ok: sheet.ibanValid
                                    ToolTip.text: sheet.ibanValid ? qsTr("Prüfsumme korrekt") : qsTr("IBAN ungültig (Prüfsumme/Länge)") }
                    }
                    FormRow {
                        label: qsTr("BIC (optional)")
                        fillValue: true
                        FormTextField {
                            placeholderText: qsTr("z. B. GENODEF1KS2")
                            font.family: "monospace"
                            inputMethodHints: Qt.ImhNoPredictiveText
                            text: sheet.draft.bic
                            onTextEdited: AppModel.setDraft({ bic: text })
                            Sensitive {}
                        }
                        ValidMark { visible: sheet.bicText !== ""; ok: sheet.bicValid }
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 5
                    GroupedSection {
                        title: qsTr("Zahlung")
                        FormRow {
                            label: qsTr("Betrag")
                            fillValue: true
                            FormTextField {
                                placeholderText: Fmt.plain(0)
                                font.features: { "tnum": 1 }
                                text: sheet.draft.amountText
                                onTextEdited: AppModel.setDraft({ amountText: text })
                                Sensitive { amounts: true }
                            }
                            Text { text: "€"; color: Theme.secondary; font.pixelSize: Theme.body }
                        }
                        // TextField("Verwendungszweck", axis: .vertical).lineLimit(1...3): label left,
                        // the text right-aligned, growing to three lines
                        FormRow {
                            label: qsTr("Verwendungszweck")
                            fillValue: true
                            TextArea {
                                Layout.fillWidth: true
                                Layout.maximumHeight: 3 * contentHeight / Math.max(1, lineCount) + topPadding + bottomPadding
                                horizontalAlignment: TextEdit.AlignRight
                                leftPadding: 0; rightPadding: 0; topPadding: 4; bottomPadding: 4
                                font.pixelSize: Theme.body; color: Theme.text
                                placeholderText: qsTr("Verwendungszweck"); placeholderTextColor: Theme.tertiary
                                background: Item {}
                                wrapMode: TextEdit.Wrap
                                text: sheet.draft.purpose
                                onTextChanged: if (text !== sheet.draft.purpose) AppModel.setDraft({ purpose: text.slice(0, sheet.maxPurpose) })
                                Keys.onReturnPressed: e => e.accepted = true
                                Keys.onEnterPressed: e => e.accepted = true
                                Accessible.name: qsTr("Verwendungszweck")
                                Sensitive {}
                            }
                        }
                    }
                    // footer: amount hint left, purpose counter right
                    RowLayout {
                        Layout.fillWidth: true; Layout.leftMargin: 10; Layout.rightMargin: 10
                        Text { visible: sheet.draft.amountText !== "" && sheet.cents === null; text: qsTr("Betrag > 0 eingeben"); font.pixelSize: Theme.body; color: Theme.negative }
                        Item { Layout.fillWidth: true }
                        Text {
                            text: sheet.draft.purpose.length + "/" + sheet.maxPurpose
                            font.pixelSize: Theme.body; font.features: { "tnum": 1 }
                            color: sheet.draft.purpose.length >= sheet.maxPurpose ? Theme.negative : Theme.tertiary
                        }
                    }
                }

                ColumnLayout {
                    visible: sheet.canSend && sheet.offersInstant
                    Layout.fillWidth: true
                    spacing: 5
                    GroupedSection {
                        title: qsTr("Ausführung")
                        // Mutually exclusive choices: one radio group (HIG), not
                        // two checkboxes where one disables the other.
                        ButtonGroup { id: modeGroup }
                        Repeater {
                            model: [{ id: "regular", title: qsTr("Normale Überweisung") },
                                    { id: "scheduled", title: qsTr("Terminüberweisung") },
                                    { id: "instant", title: qsTr("Echtzeitüberweisung (SEPA Instant)") }]
                            // .pickerStyle(.radioGroup).labelsHidden() in a grouped form: the
                            // choice's title on the left, its radio button at the trailing edge
                            RowLayout {
                                required property var modelData
                                Layout.fillWidth: true
                                Text { Layout.fillWidth: true; text: modelData.title; font.pixelSize: Theme.body; color: Theme.text
                                       MouseArea { anchors.fill: parent; onClicked: sheet.setExecutionMode(modelData.id) } }
                                RadioButton {
                                    ButtonGroup.group: modeGroup
                                    padding: 0
                                    checked: sheet.executionMode === modelData.id
                                    onToggled: if (checked) sheet.setExecutionMode(modelData.id)
                                    Accessible.name: modelData.title
                                }
                            }
                        }
                        FormRow {
                            visible: sheet.executionMode === "scheduled"
                            label: qsTr("Ausführungsdatum")
                            // DatePicker(in: tomorrow...)
                            DateField {
                                date: Fmt.date(sheet.draft.executionDate) || new Date()
                                minimum: { const t = new Date(); t.setHours(0, 0, 0, 0); t.setDate(t.getDate() + 1); return t }
                                onPicked: d => AppModel.setDraft({ executionDate: Fmt.iso(d) })
                            }
                        }
                    }
                    // footer (instantNote)
                    ColumnLayout {
                        visible: sheet.executionMode === "instant"
                        Layout.fillWidth: true; Layout.leftMargin: 10
                        spacing: 6
                        Text {
                            Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                            text: qsTr("Kommt in Sekunden an und ist nicht rückholbar. Bietet die Bank sie nicht an, wird nichts gesendet; Sie können sie dann als normale Überweisung senden.")
                        }
                        Hint {
                            visible: sheet.cents !== null && sheet.cents >= 10000000
                            indent: false; symbol: "exclamationmark.triangle"; tint: Theme.warning
                            text: qsTr("Viele Banken begrenzen Echtzeitüberweisungen (oft auf 100.000 € oder ein eigenes Limit). Die Bank kann den Auftrag dann ablehnen.")
                        }
                    }
                }

                DemoHintBanner {
                    visible: sheet.canSend && (AppModel.isDemo || sheet.isMagicDemoAccount)
                    Layout.fillWidth: true
                    credentials: false
                    buttonTitle: qsTr("PIN 12345 einfügen")
                    onFill: AppModel.setDraft({ pin: "12345" })
                }
                GroupedSection {
                    visible: sheet.canSend && !sheet.pinStored
                    footer: sheet.isBunq ? qsTr("Der API-Schlüssel wird nicht gespeichert.") : qsTr("Die PIN wird nicht gespeichert.")
                    FormRow {
                        label: sheet.isBunq ? qsTr("bunq-API-Schlüssel") : qsTr("Banking-PIN")
                        TextField {
                            Layout.preferredWidth: 260
                            echoMode: TextInput.Password
                            horizontalAlignment: TextInput.AlignRight
                            placeholderText: qsTr("wird nicht gespeichert")
                            text: sheet.draft.pin
                            onTextEdited: AppModel.setDraft({ pin: text })
                        }
                    }
                }
            }

            // MARK: confirmation (send) – read-only review in the same grouped style as the form:
            // the amount first, then recipient, payment and execution, each as label + value.
            ColumnLayout {
                id: review
                visible: sheet.showsReview
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18
                GroupedSection {
                    BigAmount { Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 6; Layout.bottomMargin: 6 }
                }
                GroupedSection {
                    title: qsTr("Empfänger")
                    ReviewRow { key: qsTr("Name"); value: sheet.draft.name }
                    ReviewRow { key: qsTr("IBAN"); value: sheet.draft.iban; mono: true }
                    ReviewRow { visible: sheet.bicText !== ""; key: qsTr("BIC"); value: sheet.bicText.toUpperCase(); mono: true }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 6
                    GroupedSection {
                        title: qsTr("Zahlung")
                        ReviewRow { key: qsTr("Verwendungszweck"); value: sheet.draft.purpose === "" ? "–" : sheet.draft.purpose }
                        ReviewRow { key: qsTr("Von Konto"); value: sheet.accountLabel }
                        ReviewRow {
                            key: qsTr("Ausführung"); personal: false
                            value: {
                                if (sheet.sendsInstant) return qsTr("Sofort (Echtzeitüberweisung)")
                                const iso = AppModel.executionDateISO(sheet.draft)
                                return iso ? qsTr("Am %1 (Terminüberweisung)").arg(iso) : qsTr("Heute (SEPA-Überweisung)")
                            }
                        }
                    }
                    Hint {
                        indent: false; symbol: "exclamationmark.shield"
                        text: qsTr("Bitte Empfänger und IBAN genau prüfen. Eine freigegebene Überweisung lässt sich meist nicht mehr zurückholen.")
                    }
                }
            }

            // MARK: confirmation (GiroCode) – read-only account (e.g. C24)
            ColumnLayout {
                id: giroCode
                visible: sheet.showsGiroCode
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 14
                Card {
                    Layout.fillWidth: true
                    RowLayout {
                        width: parent.width
                        spacing: 16
                        GiroCodeImage {
                            Layout.preferredWidth: 200; Layout.preferredHeight: 200; Layout.alignment: Qt.AlignTop
                            payload: sheet.showsGiroCode ? sheet.giroCodePayload : ""
                            Sensitive { amounts: true } // the code carries name, IBAN, amount and purpose
                        }
                        ColumnLayout {
                            Layout.fillWidth: true; Layout.alignment: Qt.AlignTop
                            spacing: 8
                            BigAmount {}
                            SummaryRow { key: qsTr("An"); value: sheet.draft.name }
                            SummaryRow { key: qsTr("IBAN"); value: sheet.draft.iban }
                            SummaryRow { visible: sheet.bicText !== ""; key: qsTr("BIC"); value: sheet.bicText.toUpperCase() }
                            SummaryRow { key: qsTr("Zweck"); value: sheet.draft.purpose === "" ? "–" : sheet.draft.purpose }
                            SummaryRow { key: qsTr("Von"); value: sheet.accountLabel }
                        }
                    }
                }
                Hint {
                    indent: false; symbol: "camera.viewfinder"; size: Theme.callout
                    text: qsTr("In der App Ihrer Bank „Fotoüberweisung“ bzw. „QR-Code scannen“ wählen, den Code scannen und dort mit TAN freigeben. GiroCrab sendet nichts.")
                }
                Hint {
                    visible: sheet.canSend
                    indent: false; symbol: "info.circle"; size: Theme.callout
                    text: qsTr("Ausführungsdatum und Echtzeitüberweisung wählen Sie in der App Ihrer Bank; ein GiroCode enthält sie nicht.")
                }
            }
        }

        ColumnLayout {
            id: messages
            visible: AppModel.transferError !== "" || AppModel.transferInstantDeclined !== null
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.bottomMargin: 12; Layout.topMargin: 4
            spacing: 8
            RowLayout {
                visible: AppModel.transferError !== ""
                Layout.fillWidth: true; spacing: 6
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.negative; Layout.alignment: Qt.AlignTop }
                TextEdit { Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap
                           text: AppModel.transferError; color: Theme.negative; font.pixelSize: Theme.callout }
            }
            // Instant was not possible and nothing was sent: offer a regular
            // transfer, which needs its own approval.
            Rectangle {
                visible: AppModel.transferInstantDeclined !== null
                Layout.fillWidth: true
                implicitHeight: declined.implicitHeight + 24
                radius: 8
                color: Theme.withAlpha(Theme.warning, 0.1)
                ColumnLayout {
                    id: declined
                    x: 12; y: 12; width: parent.width - 24
                    spacing: 8
                    RowLayout { spacing: 6
                        SFIcon { symbol: "bolt.slash.fill"; size: 14; color: Theme.text }
                        Text { text: qsTr("Echtzeitüberweisung nicht ausgeführt"); font.pixelSize: Theme.callout; font.weight: Font.Bold; color: Theme.text } }
                    Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.text
                           text: qsTr("Es wurde kein Geld gesendet. Grund der Bank: %1").arg(AppModel.transferInstantDeclined || "") }
                    Button { text: qsTr("Als normale Überweisung senden"); enabled: !AppModel.transferBusy; onClicked: AppModel.sendAsRegularTransfer() }
                    Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                           text: qsTr("Kommt in der Regel am nächsten Bankarbeitstag an. Sie geben sie erneut frei.") }
                }
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // MARK: buttons
        RowLayout {
            id: buttonRow
            Layout.fillWidth: true
            Layout.margins: 14
            spacing: 8
            Button { visible: !sheet.showsGiroCode; text: qsTr("Abbrechen"); onClicked: sheet.close_() }
            Button { visible: sheet.showsGiroCode; text: qsTr("Zurück"); onClicked: sheet.goBack() }
            Item { Layout.fillWidth: true }
            Button { visible: sheet.showsReview; text: qsTr("Zurück"); onClicked: sheet.goBack() }
            Button {
                visible: sheet.showsGiroCode
                text: qsTr("Fertig"); highlighted: true
                onClicked: sheet.close_()
            }
            Button {
                visible: sheet.showsReview
                text: sheet.isBunq ? qsTr("In der bunq-App freigeben") : qsTr("Jetzt mit TAN freigeben")
                highlighted: true
                enabled: !AppModel.transferBusy
                onClicked: AppModel.startTransfer()
            }
            // nothing is sent, so no PIN is needed
            Button {
                visible: !sheet.confirming && sheet.canSend
                text: qsTr("Als GiroCode anzeigen")
                enabled: sheet.canReview && sheet.giroCodePayload !== ""
                onClicked: { sheet.giroCodeChosen = true; sheet.goToReview() }
            }
            Button {
                id: nextButton
                visible: !sheet.confirming
                text: sheet.canSend ? qsTr("Weiter") : qsTr("GiroCode erstellen")
                highlighted: true
                enabled: sheet.canReview && !(sheet.canSend && !sheet.pinStored && sheet.draft.pin === "")
                         && !(!sheet.canSend && sheet.giroCodePayload === "")
                onClicked: sheet.goToReview()
            }
        }
    }

    // .keyboardShortcut(.defaultAction)
    Shortcut {
        sequences: ["Return", "Enter"]
        enabled: sheet.visible && !templateName.visible && !deleteConfirm.visible
        onActivated: {
            if (!sheet.confirming) { if (nextButton.enabled) nextButton.clicked() }
            else if (sheet.showsGiroCode) sheet.close_()
            else if (!AppModel.transferBusy) AppModel.startTransfer()
        }
    }

    // TransferSheetView.swift › .onDrop: Rechnung (PDF, ZUGFeRD-XML, Bild oder PDF mit GiroCode) hierher ziehen.
    DropArea {
        parent: sheet.contentItem
        anchors.fill: parent
        enabled: !sheet.confirming && !sheet.readingInvoice
        onDropped: drop => { if (drop.hasUrls) sheet.importInvoice(drop.urls[0]) }
        Rectangle {
            anchors.fill: parent; anchors.margins: 4
            visible: parent.containsDrag
            radius: 12; color: Theme.withAlpha(Theme.accent, 0.06)
            border.color: Theme.accent; border.width: 2
        }
    }

    // Linux only: while a dropped invoice is read, a spinner over the form says so (the Mac
    // only changes the footer text, which can be scrolled out of view here).
    Rectangle {
        parent: sheet.contentItem
        anchors.fill: parent
        visible: sheet.readingInvoice
        z: 10
        radius: 12; color: Theme.withAlpha(Theme.card, 0.8)
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons; hoverEnabled: true; onWheel: w => w.accepted = true }
        Column {
            anchors.centerIn: parent
            spacing: 10
            BusyIndicator { anchors.horizontalCenter: parent.horizontalCenter; running: parent.parent.visible
                            implicitWidth: 40; implicitHeight: 40 }
            Text { anchors.horizontalCenter: parent.horizontalCenter; text: qsTr("Lese Rechnung …")
                   font.pixelSize: Theme.body; color: Theme.text }
        }
    }

    AppMenu {
        id: templatesMenu
        // `if !templates.isEmpty { Menu("Vorlage löschen") }`: the submenu's entry
        // is hidden while there are no templates.
        onAboutToShow: {
            for (let i = 0; i < count; ++i) {
                const item = itemAt(i)
                if (item && item.subMenu === deleteMenu) {
                    item.visible = sheet.templates.length > 0
                    item.height = item.visible ? item.implicitHeight : 0
                }
            }
        }
        Instantiator {
            model: sheet.templates
            delegate: AppMenuItem {
                required property var modelData
                text: modelData.name + " · " + modelData.recipient_name
                onTriggered: { sheet.ownTransfer = false; AppModel.applyTransferTemplate(modelData) }
            }
            onObjectAdded: (i, o) => templatesMenu.insertItem(i, o)
            onObjectRemoved: (i, o) => templatesMenu.removeItem(o)
        }
        AppMenuSeparator { visible: sheet.templates.length > 0; height: visible ? implicitHeight : 0 }
        AppMenu {
            id: deleteMenu
            title: qsTr("Vorlage löschen")
            enabled: sheet.templates.length > 0
            Instantiator {
                model: sheet.templates
                delegate: AppMenuItem {
                    required property var modelData
                    text: modelData.name + " …"
                    onTriggered: { deleteConfirm.template = modelData; deleteConfirm.open() }
                }
                onObjectAdded: (i, o) => deleteMenu.insertItem(i, o)
                onObjectRemoved: (i, o) => deleteMenu.removeItem(o)
            }
        }
        AppMenuItem {
            text: qsTr("Als Vorlage speichern …"); icon.name: SF.icon("square.and.arrow.down")
            enabled: sheet.draft.name.trim() !== "" && sheet.draft.iban !== ""
            onTriggered: { templateNameField.text = sheet.draft.name; templateName.open() }
        }
    }

    // .capturableSheet { KnownRecipientsSheet() }: hidden IBANs drop out of the chips at once
    KnownRecipientsDialog { id: recipientsDialog; onClosed: sheet.refreshSuggestions() }

    // .alert("Vorlage speichern") { TextField("Name der Vorlage") }
    Sheet {
        id: templateName
        sheetWidth: 340
        ColumnLayout {
            width: templateName.availableWidth
            spacing: 12
            Text { text: qsTr("Vorlage speichern"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text }
            TextField { id: templateNameField; Layout.fillWidth: true; placeholderText: qsTr("Name der Vorlage"); onAccepted: saveButton.clicked() }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: templateName.close() }
                Button {
                    id: saveButton
                    text: qsTr("Speichern"); highlighted: true
                    onClicked: { AppModel.saveTransferTemplate(templateNameField.text); sheet.templates = AppModel.transferTemplates(); templateName.close() }
                }
            }
        }
    }

    // .confirmationDialog("Vorlage „…“ löschen?")
    Sheet {
        id: deleteConfirm
        property var template: null
        sheetWidth: 340
        ColumnLayout {
            width: deleteConfirm.availableWidth
            spacing: 12
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap
                   text: qsTr("Vorlage „%1“ löschen?").arg(deleteConfirm.template ? deleteConfirm.template.name : "")
                   font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: deleteConfirm.close() }
                Button {
                    text: qsTr("Löschen")
                    palette.buttonText: Theme.negative
                    onClicked: { AppModel.deleteTransferTemplate(deleteConfirm.template.id); sheet.templates = AppModel.transferTemplates(); deleteConfirm.close() }
                }
            }
        }
    }

    component Hint: RowLayout {
        property alias text: hintText.text
        property alias symbol: hintIcon.symbol
        property bool indent: true
        property int size: Theme.caption
        property color tint: Theme.secondary
        Layout.fillWidth: true
        Layout.leftMargin: indent ? 10 : 0
        spacing: 6
        SFIcon { id: hintIcon; size: 13; Layout.alignment: Qt.AlignTop; color: parent.tint }
        Text { id: hintText; Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: parent.size; color: parent.tint }
    }
    /// reviewRow: LabeledContent with the value right-aligned, selectable, never truncated.
    component ReviewRow: RowLayout {
        property string key
        property string value
        property bool mono: false
        property bool personal: true
        Layout.fillWidth: true
        spacing: 12
        Text { text: parent.key; color: Theme.text; font.pixelSize: Theme.body; Layout.alignment: Qt.AlignTop }
        TextEdit {
            Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignRight
            text: parent.value; color: Theme.text; font.pixelSize: Theme.body
            font.family: parent.mono ? "monospace" : Qt.application.font.family
            Sensitive { when: parent.parent.personal }
        }
    }
    /// summaryRow of the GiroCode review: all of them are personal.
    component SummaryRow: RowLayout {
        property string key
        property string value
        Layout.fillWidth: true
        spacing: 8
        Text { Layout.preferredWidth: 90 * Theme.textScale; Layout.alignment: Qt.AlignTop; text: parent.key; color: Theme.secondary; font.pixelSize: Theme.body }
        TextEdit { Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; text: parent.value; color: Theme.text; font.pixelSize: Theme.body
                   Sensitive {} }
    }
    component BigAmount: Text {
        text: Fmt.money(sheet.cents === null ? 0 : sheet.cents).replace("−", "")
        font.pixelSize: Theme.largeTitle; font.weight: Font.DemiBold; color: Theme.text
        font.features: { "tnum": 1 }
        Sensitive { amounts: true }
    }
    component ValidMark: SFIcon {
        property bool ok
        symbol: ok ? "checkmark.circle.fill" : "xmark.circle.fill"
        size: 16
        color: ok ? Theme.inflow : Theme.negative
        HoverHandler { id: hover }
        ToolTip.visible: hover.hovered && ToolTip.text !== ""
    }
}
