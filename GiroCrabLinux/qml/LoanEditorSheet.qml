import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// ManualLoansUI.swift › LoanEditorSheet: create / edit a manual loan, or set a new
// Stand, with a live preview. Driven by `AppModel.loanEditor` { mode, accountId, draft }.
Sheet {
    id: sheet
    readonly property var request: AppModel.loanEditor
    readonly property string mode: request ? request.mode : "create"
    readonly property bool isAnchor: mode === "anchor"
    readonly property var existing: request && request.accountId !== null ? AppModel.manualLoan(request.accountId) : null
    visible: request !== null
    onAboutToShow: load()
    onClosed: AppModel.loanEditor = null
    sheetWidth: 560
    padding: 0

    property string name: ""
    property string originalText: ""
    property date anchorDate: new Date()
    property string balanceText: ""
    property string rateText: ""
    property string rule: ""
    property var preview: null
    /// Rule the shown preview was computed for (stale while typing).
    property string previewRule: ""
    property string previewError: ""
    property string saveError: ""
    property bool loaded: false
    /// Validation hints appear once something was typed or saving was tried.
    property bool touched: false
    /// Field contents right after `load`, to tell an edit from the prefill.
    property string initialKey: ""
    property int previewGeneration: 0

    readonly property string heading: mode === "create" ? qsTr("Neues Darlehen") : mode === "edit" ? qsTr("Darlehen bearbeiten") : qsTr("Stand neu setzen")

    // MARK: validation (undefined = invalid, null = left empty)

    readonly property var balanceCents: { const c = Fmt.parseCents(balanceText); return c === null ? null : Math.abs(c) }
    readonly property var originalCents: {
        const t = originalText.trim()
        if (t === "") return null
        const c = Fmt.parseCents(t)
        return c !== null && c > 0 ? Math.abs(c) : undefined
    }
    readonly property var rate: {
        const t = rateText.trim()
        if (t === "") return null
        const p = Fmt.percentPpm(t)
        return p !== null && p >= 0 && p <= 300000 ? p : undefined
    }
    readonly property string problem: {
        if (name.trim() === "") return qsTr("Bitte einen Namen angeben.")
        if (balanceCents === null) return qsTr("Bitte die Restschuld am Stichtag angeben.")
        if (originalCents === undefined) return qsTr("Der ursprüngliche Kreditbetrag ist keine gültige Zahl.")
        if (rate === undefined) return qsTr("Der Sollzins muss zwischen 0 und 30 % liegen.")
        return previewError
    }
    readonly property var setup: balanceCents === null || originalCents === undefined || rate === undefined ? null : {
        name: name.trim(),
        original_cents: originalCents,
        anchor_date: Fmt.iso(anchorDate),
        anchor_balance_cents: balanceCents,
        interest_rate_ppm: rate,
        match_expr: rule.trim()
    }
    readonly property string statusText: saveError !== "" ? saveError : (touched ? problem : "")

    readonly property string previewKey: [name, originalText, Fmt.iso(anchorDate), balanceText, rateText, rule].join("\u001F")
    onPreviewKeyChanged: {
        if (loaded && previewKey !== initialKey) touched = true
        previewTimer.restart()
    }
    Timer { id: previewTimer; interval: 200; onTriggered: sheet.refreshPreview() }

    function load() {
        loaded = false
        touched = false
        preview = null; previewRule = ""; previewError = ""; saveError = ""
        name = ""; originalText = ""; anchorDate = new Date(); balanceText = ""; rateText = ""; rule = ""
        // read the request directly: aboutToShow comes before the bindings above are updated
        const req = AppModel.loanEditor
        if (!req) return
        const anchor = req.mode === "anchor"
        const existing = req.accountId !== null ? AppModel.manualLoan(req.accountId) : null
        const s = existing ? existing.setup : req.draft
        if (s) {
            name = s.name
            originalText = s.original_cents !== null ? Fmt.plain(s.original_cents) : ""
            anchorDate = Fmt.date(s.anchor_date) || new Date()
            if (anchor && existing) {
                // a new Stand starts from today's computed balance
                anchorDate = new Date()
                balanceText = Fmt.plain(existing.status.remaining_cents)
            } else {
                balanceText = s.anchor_balance_cents > 0 || existing !== null ? Fmt.plain(s.anchor_balance_cents) : ""
            }
            rateText = s.interest_rate_ppm !== null ? Fmt.percentText(s.interest_rate_ppm) : ""
            rule = s.match_expr
        }
        initialKey = previewKey
        loaded = true
        previewTimer.restart()
        Qt.callLater(() => (anchor ? balanceField : nameField).forceActiveFocus())
    }

    function refreshPreview() {
        const gen = ++previewGeneration
        if (setup === null) { preview = null; previewError = ""; return }
        const s = Object.assign({}, setup)
        if (s.name === "") s.name = "-"
        const req = AppModel.loanEditor
        AppModel.coreAsync("preview_manual_loan", { account_id: req ? req.accountId : null, setup: s, limit: 5 }, p => {
            if (gen !== previewGeneration) return
            preview = p
            previewRule = s.match_expr
            previewError = ""
            saveError = ""
        }, err => {
            if (gen !== previewGeneration) return
            previewError = AppModel.parseCoreError(err).message
            saveError = ""
        })
    }

    function save() {
        touched = true
        if (setup === null || problem !== "") return
        try {
            const id = AppModel.saveLoan(setup, mode === "create" ? null : request.accountId)
            if (mode === "create") AppModel.sidebarSelection = "account:" + id
            close()
        } catch (e) {
            saveError = AppModel.parseCoreError(e.message).message
        }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        ColumnLayout {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 20
            spacing: 4
            Text { text: sheet.heading; font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Accessible.role: Accessible.Heading }
            Text {
                visible: sheet.mode === "create"
                text: qsTr("Für Kredite ohne eigenes Konto bei der Bank, z. B. einen Autokredit, den Sie vom Girokonto abbezahlen.")
                Layout.fillWidth: true; wrapMode: Text.WordWrap
                font.pixelSize: Theme.callout; color: Theme.secondary
            }
        }

        ScrollView {

            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 20 + footer.implicitHeight + 33
            Layout.preferredHeight: Math.min(Math.max(form.implicitHeight + 40, (sheet.isAnchor ? 520 : 660) - chrome), sheet.maxBodyHeight - chrome)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: form
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18

                FormSection {
                    visible: !sheet.isAnchor
                    FormRow {
                        label: qsTr("Name")
                        TextField { id: nameField; Layout.preferredWidth: 260; placeholderText: qsTr("z. B. Autokredit")
                                    text: sheet.name; onTextEdited: sheet.name = text }
                    }
                    FormRow {
                        label: qsTr("Ursprünglicher Kreditbetrag")
                        TextField { Layout.preferredWidth: 260; placeholderText: qsTr("optional")
                                    text: sheet.originalText; onTextEdited: sheet.originalText = text; Sensitive { amounts: true; when: parent.text !== "" } }
                    }
                }
                FormSection {
                    title: qsTr("Stand")
                    footers: [qsTr("Die Restschuld am Ende des Stichtags, z. B. aus dem Jahreskontoauszug. Raten ab dem Folgetag werden abgezogen. Fehlt etwas oder stimmt etwas nicht, setzen Sie einfach einen neuen Stand.")]
                    FormRow {
                        label: qsTr("Stichtag")
                        DateField { date: sheet.anchorDate; maximum: new Date(); onPicked: d => sheet.anchorDate = d }
                    }
                    FormRow {
                        label: qsTr("Restschuld am Stichtag")
                        TextField { id: balanceField; Layout.preferredWidth: 260; placeholderText: qsTr("z. B. %1").arg(Fmt.plain(840000))
                                    text: sheet.balanceText; onTextEdited: sheet.balanceText = text; Sensitive { amounts: true; when: parent.text !== "" } }
                    }
                }
                FormSection {
                    visible: !sheet.isAnchor
                    title: qsTr("Zinsen")
                    footers: [qsTr("Mit Sollzins zählt von jeder Rate nur der Tilgungsanteil (Zinsen taggenau). Ohne Sollzins senkt die ganze Rate die Restschuld.")]
                    FormRow {
                        label: qsTr("Sollzins p. a. in %")
                        TextField { Layout.preferredWidth: 260; placeholderText: qsTr("z. B. 4,99")
                                    text: sheet.rateText; onTextEdited: sheet.rateText = text }
                    }
                }
                FormSection {
                    visible: !sheet.isAnchor
                    title: qsTr("Raten automatisch zuordnen")
                    footers: [qsTr("Gleiche Schreibweise wie bei Regeln: Wörter, „Phrasen“, name:, purpose:, AND, OR, NOT. Einzelne Buchungen ordnen Sie im Kontextmenü zu oder heben die Zuordnung dort auf.")]
                    FormRow {
                        label: qsTr("Suchregel")
                        TextField { Layout.preferredWidth: 260; placeholderText: qsTr("z. B. Autobank")
                                    text: sheet.rule; onTextEdited: sheet.rule = text }
                    }
                    MatchList { Layout.fillWidth: true }
                }
                FormSection {
                    title: qsTr("Vorschau")
                    PreviewRows { Layout.fillWidth: true }
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
                text: sheet.mode === "create" ? qsTr("Anlegen") : qsTr("Speichern")
                highlighted: true
                enabled: sheet.problem === ""
                onClicked: sheet.save()
                Shortcut { sequences: ["Return", "Enter"]; enabled: sheet.visible && sheet.problem === ""; onActivated: sheet.save() }
            }
        }
    }

    // matchList: the bookings the rule finds (5 at most), or why there are none yet.
    component MatchList: ColumnLayout {
        id: ml
        readonly property string current: sheet.rule.trim()
        readonly property bool searching: current !== "" && (sheet.preview === null || sheet.previewRule !== current)
        visible: current !== ""
        spacing: 6
        RowLayout {
            visible: ml.searching
            spacing: 6
            BusyIndicator { running: parent.visible; implicitWidth: 16; implicitHeight: 16 }
            Text { text: qsTr("Suche passende Buchungen …"); font.pixelSize: Theme.body; color: Theme.secondary }
        }
        Text {
            visible: !ml.searching && sheet.preview !== null && sheet.preview.matches.length === 0
            text: qsTr("Keine passenden Buchungen."); font.pixelSize: Theme.body; color: Theme.secondary
        }
        Repeater {
            model: !ml.searching && sheet.preview ? sheet.preview.matches : []
            RowLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 8
                Text { text: Fmt.medium(modelData.booking_date); font.pixelSize: Theme.callout; font.features: { "tnum": 1 }; color: Theme.secondary }
                Text { text: modelData.merchant.name === "" ? modelData.counterparty : modelData.merchant.name
                       font.pixelSize: Theme.callout; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                Text { text: Fmt.money(modelData.amount_cents); font.pixelSize: Theme.callout; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
            }
        }
        Text {
            visible: !ml.searching && sheet.preview !== null && sheet.preview.match_count > sheet.preview.matches.length
            text: sheet.preview ? qsTr("und %1 weitere").arg(sheet.preview.match_count - sheet.preview.matches.length) : ""
            font.pixelSize: Theme.caption; color: Theme.secondary
        }
    }

    component PreviewRows: ColumnLayout {
        id: pr
        readonly property var s: sheet.preview ? sheet.preview.status : null
        spacing: 8
        FormRow { visible: pr.s !== null && sheet.isAnchor && sheet.existing !== null; label: qsTr("Restschuld bisher"); amounts: true
                  value: sheet.existing ? Fmt.money(-sheet.existing.status.remaining_cents) : "" }
        RowLayout {
            visible: pr.s !== null
            Layout.fillWidth: true
            Text { text: qsTr("Restschuld"); font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Layout.fillWidth: true }
            Text { text: pr.s ? Fmt.money(-pr.s.remaining_cents) : ""; font.pixelSize: Theme.body; font.weight: Font.Bold
                   font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
        }
        FormRow { visible: pr.s !== null; label: qsTr("Stand vom"); value: pr.s ? Fmt.medium(pr.s.as_of_date) : "" }
        FormRow { visible: pr.s !== null; label: qsTr("Raten seit Stichtag"); amounts: true
                  value: pr.s ? pr.s.payment_count + " · " + Fmt.money(pr.s.paid_cents) : "" }
        FormRow { visible: pr.s !== null && pr.s.interest_cents > 0; label: qsTr("davon Zinsen / Tilgung"); amounts: true
                  value: pr.s ? Fmt.money(pr.s.interest_cents) + " / " + Fmt.money(pr.s.principal_cents) : "" }
        FormRow { visible: pr.s !== null && pr.s.before_anchor_count > 0; label: qsTr("Ältere Buchungen (nicht gezählt)")
                  value: pr.s ? String(pr.s.before_anchor_count) : "" }
        FormRow { visible: pr.s !== null && pr.s.months_left !== null && pr.s.months_left > 0; label: qsTr("Voraussichtlich noch")
                  value: pr.s && pr.s.months_left ? qsTr("%1 Monatsraten").arg(pr.s.months_left) : "" }
        Text {
            visible: pr.s === null
            text: qsTr("Vorschau erscheint, sobald Restschuld und Stichtag ausgefüllt sind.")
            Layout.fillWidth: true; wrapMode: Text.WordWrap
            font.pixelSize: Theme.body; color: Theme.secondary
        }
    }
}
