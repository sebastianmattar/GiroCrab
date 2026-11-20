import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// ExportSheet.swift – Umsätze exportieren (GitHub #53 / #372 / #401):
// output mode → searchable account list (by bank) → period → format.
// PDF = white-label Umsatzliste; CAMT.053 needs exactly one account (#372), no loan or depot (#483).
Sheet {
    id: sheet
    sheetWidth: isSelectionExport ? 480 : 560
    padding: 0

    property var options: BookingExport.load()
    property bool busy: false
    /// Export error, shown on the sheet itself ("" = none).
    property string failure: ""
    property bool showDetails: BookingExport.hasNonDefaultContent(options)
    /// Booking count and file name for the current choices, loaded on a worker thread
    /// (`null` while loading). Refreshed only when `previewKey` changes.
    property var previewCount: null
    property var previewName: null
    property int previewGeneration: 0
    property string accountSearch: ""
    /// id -> true
    property var selectedIDs: ({})
    /// Bank names that are collapsed (accounts hidden): name -> true
    property var collapsedBanks: ({})
    property date customFromDate: new Date()
    property date customToDate: new Date()

    function setOption(key, value) {
        const o = Object.assign({}, options)
        o[key] = value
        applyOptions(o)
    }
    /// The `.onChange` handlers of the Swift sheet, applied in one step.
    function applyOptions(o) {
        const old = options
        if (BookingExport.isCamt(o.format) && o.format !== old.format) {
            o.accountOutput = "combined"
            const first = firstCamtAccount
            if (!isSelectionExport && !camtAccountOKFor(o) && first) { const s = {}; s[first.id] = true; selectedIDs = s }
        }
        options = o
        persistOptions()
    }
    function setSelectedIDs(s) {
        selectedIDs = s
        if (isSelectionExport) return
        const o = Object.assign({}, options)
        o.selectedAccountIDs = selectedIDList
        options = o
        persistOptions()
    }

    readonly property var selectedIDList: Object.keys(selectedIDs).map(Number).sort((a, b) => a - b)
    readonly property int selectionCount: AppModel.exportBookingIDs !== null ? AppModel.exportBookingIDs.length
        : AppModel.isMultiSelection ? Object.keys(AppModel.selectedBookingIDs).length : (AppModel.selectedBookingID === null ? 0 : 1)
    readonly property bool canUseSelection: selectionCount > 0
    onCanUseSelectionChanged: if (!canUseSelection && !isSelectionExport) setOption("scope", "filtered")
    readonly property var exportIDs: {
        if (options.scope !== "selection") return null
        if (AppModel.exportBookingIDs !== null) return AppModel.exportBookingIDs
        const ids = Object.keys(AppModel.selectedBookingIDs).map(Number)
        return ids.length ? ids : (AppModel.selectedBookingID !== null ? [AppModel.selectedBookingID] : [])
    }

    /// Accounts available in the current sidebar scope (Übersicht → all).
    readonly property var poolAccounts: {
        const ids = AppModel.selectionAccountIDs()
        const pool = AppModel.allAccounts.filter(a => a.archived_unix === null || a.archived_unix === undefined)
        return ids.length ? pool.filter(a => ids.indexOf(a.id) >= 0) : pool
    }
    readonly property var filteredAccounts: {
        const q = Fmt.fold(accountSearch.trim())
        const base = poolAccounts.slice().sort((a, b) => a.bank_name !== b.bank_name
            ? a.bank_name.localeCompare(b.bank_name, undefined, { sensitivity: "base" }) : BookingExport.byName(a, b))
        if (q === "") return base
        return base.filter(a => [a.name, a.bank_name, a.iban, a.owner].some(s => Fmt.fold(s || "").indexOf(q) >= 0))
    }
    readonly property var accountsByBank: {
        const none = qsTr("Ohne Bank")
        const grouped = {}
        for (const a of filteredAccounts) {
            const bank = (a.bank_name || "").trim() === "" ? none : a.bank_name
            if (!grouped[bank]) grouped[bank] = []
            grouped[bank].push(a)
        }
        return Object.keys(grouped).sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }))
            .map(bank => ({ bank: bank, accounts: grouped[bank].sort(BookingExport.byName) }))
    }
    readonly property var selectedAccounts: poolAccounts.filter(a => !!selectedIDs[a.id]).sort(BookingExport.byName)

    /// Context-menu „Umsatz exportieren“ — lock to the marked bookings (no account/period chrome).
    readonly property bool isSelectionExport: AppModel.exportPreferSelection
    /// „Jedes Konto einzeln“ only when multiple accounts can actually produce separate files.
    readonly property bool showsOutputMode: !isSelectionExport && !BookingExport.isCamt(options.format)
        && options.scope !== "selection" && selectedAccounts.length > 1
    readonly property bool isBulk: showsOutputMode && options.accountOutput === "perAccount"
    /// A loan's balance is no sum of its own bookings, a depot has none: CAMT.053
    /// refuses both in the core, so the picker does too (#483).
    function camtUsable(account) { return account.kind !== "Loan" && account.kind !== "Brokerage" }
    /// camtAccountOK for options `o` (applyOptions checks the new format before it is set).
    function camtAccountOKFor(o) {
        if (!BookingExport.isCamt(o.format)) return true
        const ids = o.scope === "selection" && accountIDsForSelectedBookings.length ? accountIDsForSelectedBookings : selectedIDList
        const account = ids.length === 1 ? AppModel.account(ids[0]) : null
        return account !== null && camtUsable(account)
    }
    readonly property bool camtAccountOK: camtAccountOKFor(options)
    /// The single chosen account when CAMT.053 cannot cover it (loan or depot), else `null`.
    readonly property var camtUnusableAccount: {
        if (effectiveAccountIDs.length !== 1) return null
        const account = AppModel.account(effectiveAccountIDs[0])
        return account && !camtUsable(account) ? account : null
    }
    /// First account a CAMT.053 statement can cover.
    readonly property var firstCamtAccount: selectedAccounts.find(camtUsable) || poolAccounts.find(camtUsable) || null
    readonly property bool canExport: !busy && camtAccountOK && effectiveAccountIDs.length > 0
        && (isBulk || (previewCount || 0) > 0 || options.scope === "selection")

    readonly property var accountIDsForSelectedBookings: {
        const ids = exportIDs || []
        const out = {}
        for (const b of AppModel.bookings) if (ids.indexOf(b.id) >= 0) out[b.account_id] = true
        return Object.keys(out).map(Number).sort((a, b) => a - b)
    }
    /// Accounts used for the export query (selection → accounts of the marked bookings).
    readonly property var effectiveAccountIDs: options.scope === "selection" && accountIDsForSelectedBookings.length
        ? accountIDsForSelectedBookings : selectedIDList

    readonly property string suggestedName: isBulk ? qsTr("%1 × Umsaetze-….pdf").arg(selectedAccounts.length) : (previewName || "…")
    readonly property string previewKey: JSON.stringify({ o: options, a: effectiveAccountIDs, b: exportIDs })
    onPreviewKeyChanged: previewTimer.restart()

    readonly property string fallbackTitle: {
        if (options.scope === "selection")
            return selectionCount === 1 ? qsTr("Umsatz") : qsTr("%1 Umsaetze").arg(selectionCount)
        const n = effectiveAccountIDs.length
        const a = n === 1 ? AppModel.account(effectiveAccountIDs[0]) : null
        return a ? a.name : qsTr("%1 Konten").arg(n)
    }
    readonly property string saveTitle: {
        if (busy) return qsTr("Wird erstellt …")
        if (isBulk) return qsTr("Alle speichern (%1)").arg(selectedAccounts.length)
        const count = previewCount !== null ? " (" + previewCount + ")" : ""
        if (options.format === "pdf") return qsTr("PDF speichern%1").arg(count)
        if (options.format === "csv") return qsTr("CSV speichern%1").arg(count)
        return qsTr("CAMT speichern%1").arg(count)
    }
    readonly property string sheetTitle: isSelectionExport && selectionCount <= 1 ? qsTr("Umsatz exportieren") : qsTr("Umsätze exportieren")

    /// Write current options without re-triggering the bindings.
    /// Selection-export keeps period/account prefs for the next menu/toolbar open.
    function persistOptions() {
        const snapshot = Object.assign({}, options)
        if (isSelectionExport) {
            const previous = BookingExport.load()
            snapshot.scope = previous.scope === "selection" ? "filtered" : previous.scope
            snapshot.periodPreset = previous.periodPreset
            snapshot.customFrom = previous.customFrom
            snapshot.customTo = previous.customTo
            snapshot.selectedAccountIDs = previous.selectedAccountIDs
            snapshot.accountOutput = previous.accountOutput
            // Keep the saved format; the selection export may use it too.
            snapshot.format = previous.format
        } else {
            snapshot.selectedAccountIDs = selectedIDList
            if (snapshot.scope === "selection") snapshot.scope = "filtered"
        }
        BookingExport.save(snapshot)
    }

    function periodSubtitle(preset) {
        if (preset === "current") return AppModel.periodTitle
        if (preset === "custom") {
            if (!options.customFrom) return qsTr("Von–Bis wählen")
            return PrintReport.statementDate(options.customFrom) + " – " + PrintReport.statementDate(options.customTo)
        }
        if (preset === "sinceSalary")
            return AppModel.salaryCycle ? qsTr("Seit %1").arg(PrintReport.statementDate(AppModel.salaryCycle.last_date)) : qsTr("Kein Gehalt erkannt")
        return ""
    }
    function syncCustomDatesFromOptions() {
        if (!options.customFrom || !options.customTo) {
            const r = BookingExport.previousMonthRange()
            const o = Object.assign({}, options)
            o.customFrom = r[0] || Fmt.iso(new Date())
            o.customTo = r[1] || Fmt.iso(new Date())
            options = o
        }
        let from = Fmt.date(options.customFrom) || new Date(), to = Fmt.date(options.customTo) || new Date()
        if (from > to) { const t = from; from = to; to = t }
        customFromDate = from
        customToDate = to
    }
    function setCustomDate(isFrom, d) {
        if (isFrom) customFromDate = d; else customToDate = d
        setOption(isFrom ? "customFrom" : "customTo", Fmt.iso(d))
    }

    // MARK: - Lifecycle
    function bootstrap() {
        const o = Object.assign({}, options)
        if (isSelectionExport) {
            // Context menu: only the marked bookings — ignore saved period/account prefs.
            o.scope = "selection"
            const ids = accountIDsForSelectedBookings.length ? accountIDsForSelectedBookings : poolAccounts.map(a => a.id)
            const s = {}
            for (const id of ids) s[id] = true
            options = o
            selectedIDs = s
        } else {
            // Menu / toolbar / account: full period export (never reopen stuck on „Dieser Umsatz“).
            o.scope = "filtered"
            const pool = poolAccounts.map(a => a.id)
            let ids = o.selectedAccountIDs.filter(id => pool.indexOf(id) >= 0)
            if (!ids.length) {
                const scope = AppModel.selectionAccountIDs()
                ids = scope.length === 1 ? scope : pool
            }
            if (BookingExport.isCamt(o.format) && ids.length !== 1) {
                ids = firstCamtAccount ? [firstCamtAccount.id] : []
            } else if (BookingExport.isCamt(o.format) && AppModel.account(ids[0]) && !camtUsable(AppModel.account(ids[0]))) {
                ids = firstCamtAccount ? [firstCamtAccount.id] : []
            }
            const s = {}
            for (const id of ids) s[id] = true
            selectedIDs = s
            o.selectedAccountIDs = ids.slice().sort((a, b) => a - b)
            options = o
            if (accountsByBank.length > 4) {
                const c = {}
                for (const g of accountsByBank) c[g.bank] = true
                collapsedBanks = c
            }
            if (o.periodPreset === "custom") syncCustomDatesFromOptions()
        }
        persistOptions()
        previewTimer.restart()
    }

    onAboutToShow: bootstrap()
    onClosed: {
        persistOptions()
        AppModel.showExportSheet = false
    }

    /// Loads count + file name on a worker thread; a newer choice makes an older load stale.
    Timer {
        id: previewTimer
        interval: 60
        onTriggered: sheet.refreshPreview()
    }
    function refreshPreview() {
        const gen = ++previewGeneration
        const o = options, accountIDs = effectiveAccountIDs, bookingIDs = exportIDs
        if (!accountIDs.length && o.scope !== "selection") { previewCount = 0; previewName = null; return }
        previewCount = null
        const preset = o.scope === "selection" ? "current" : o.periodPreset
        BookingExport.loadExportBookings(accountIDs.length ? accountIDs : null, preset, bookingIDs, o, BookingExport.isCamt(o.format), (bookings, tagNote) => {
            if (gen !== previewGeneration) return
            const report = BookingExport.exportReport(bookings, accountIDs, preset, bookingIDs, o, tagNote)
            previewCount = report.rows.length
            const range = BookingExport.camtRange(accountIDs, preset, bookingIDs, o)
            previewName = BookingExport.defaultFilename(report.accounts, report.rows, o.format, fallbackTitle,
                                                        range ? range.from : null, range ? range.to : null)
        }, () => {
            if (gen !== previewGeneration) return
            previewCount = 0
            previewName = null
        })
    }

    /// Export error, shown on the sheet itself (the app's alert would stay hidden behind it).
    /// A CAMT.053 refusal of the core (InvalidInput) arrives as its plain message (dispatch.rs).
    function fail(message) {
        busy = false
        failure = message
    }
    /// Closes the sheet, then saves. close() unloads the sheet (lazy Loader in Main.qml): hand
    /// over BookingExport's function itself, not a closure over this sheet's context.
    function finish(present, a, b) {
        busy = false
        AppModel.exportBookingIDs = null
        AppModel.exportPreferSelection = false
        close()
        Qt.callLater(present, a, b)
    }
    function exportFiles(askWhere) {
        const ids = effectiveAccountIDs
        if (!ids.length && options.scope !== "selection") { fail(qsTr("Bitte mindestens ein Konto wählen.")); return }
        busy = true
        const o = Object.assign({}, options)
        if (isBulk) {
            // one file per account, one after the other (the PDF renderer takes one job at a time)
            const files = []
            const next = i => {
                if (i >= ids.length) {
                    if (!files.length) { fail(qsTr("Keine Umsätze für den Export.")); return }
                    finish(BookingExport.presentExportFiles, files)
                    return
                }
                BookingExport.buildFile([ids[i]], null, o, file => { if (file) files.push(file); next(i + 1) }, fail)
            }
            next(0)
            return
        }
        BookingExport.buildFile(ids.length ? ids : poolAccounts.map(a => a.id), exportIDs, o, file => {
            if (!file) { fail(qsTr("Keine Umsätze für den Export.")); return }
            finish(BookingExport.presentExportFile, file, askWhere)
        }, fail)
    }

    // MARK: - Rows

    /// choiceRow: title, optional subtitle, a checkmark on the chosen one.
    component ChoiceRow: Item {
        id: cr
        property string title
        property string subtitle
        property bool selected
        signal clicked()
        Layout.fillWidth: true
        implicitHeight: crCol.implicitHeight + 6
        RowLayout {
            anchors.fill: parent
            spacing: 10
            ColumnLayout {
                id: crCol
                spacing: 2
                Layout.fillWidth: true
                Text { text: cr.title; color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true; wrapMode: Text.WordWrap }
                Text { visible: cr.subtitle !== ""; text: cr.subtitle; color: Theme.secondary; font.pixelSize: Theme.caption
                       Layout.fillWidth: true; wrapMode: Text.WordWrap }
            }
            SFIcon { symbol: "checkmark"; size: 13; color: Theme.accent; opacity: cr.selected ? 1 : 0 }
        }
        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: cr.clicked() }
    }
    component ToggleRow: FormRow {
        id: tr
        property bool checked
        signal toggled(bool on)
        Switch { checked: tr.checked; onToggled: tr.toggled(checked) }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        // navigationTitle + toolbar (cancellation / confirmation)
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            spacing: 8
            Button { text: qsTr("Abbrechen"); enabled: !sheet.busy; onClicked: sheet.close() }
            Item { Layout.fillWidth: true }
            Text { text: sheet.sheetTitle; font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text }
            Item { Layout.fillWidth: true }
            Button {
                visible: !sheet.isBulk
                text: qsTr("Unter …")
                enabled: sheet.canExport && sheet.previewCount !== 0
                onClicked: sheet.exportFiles(true)
            }
            Button {
                text: sheet.saveTitle; highlighted: true
                enabled: sheet.canExport && (sheet.isBulk || sheet.previewCount !== 0)
                onClicked: sheet.exportFiles(false)
                Shortcut { sequences: ["Return", "Enter"]; enabled: sheet.visible && parent.enabled; onActivated: sheet.exportFiles(false) }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        ScrollView {

            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(form.implicitHeight + 32, sheet.maxBodyHeight - 60, sheet.isSelectionExport ? 520 : 720)
            contentWidth: availableWidth
            ColumnLayout {
                id: form
                x: 16; y: 16; width: scroll.availableWidth - 32
                spacing: 18

                // MARK: selectionSummarySection
                FormSection {
                    visible: sheet.isSelectionExport
                    footer: qsTr("Nur die markierten Buchungen. Konten und Zeitraum brauchst du hier nicht.")
                    FormRow {
                        label: sheet.selectionCount === 1 ? qsTr("Dieser Umsatz") : qsTr("Auswahl")
                        value: sheet.selectionCount === 1 ? qsTr("1 Buchung") : qsTr("%1 Buchungen").arg(sheet.selectionCount)
                    }
                }

                // MARK: outputModeSection
                FormSection {
                    visible: sheet.showsOutputMode
                    title: qsTr("Ausgabe")
                    Repeater {
                        model: BookingExport.outputs
                        ChoiceRow {
                            required property var modelData
                            title: modelData.title; subtitle: modelData.help
                            selected: sheet.options.accountOutput === modelData.id
                            onClicked: sheet.setOption("accountOutput", modelData.id)
                        }
                    }
                }

                // MARK: accountPickerSection
                FormSection {
                    visible: !sheet.isSelectionExport && sheet.options.scope !== "selection"
                    title: qsTr("Konten")
                    footer: BookingExport.isCamt(sheet.options.format) ? qsTr("CAMT.053 gilt immer für genau ein Konto (#372).")
                          : sheet.isBulk ? qsTr("Pro gewähltem Konto entsteht eine eigene Datei.") : ""
                    TextField {
                        visible: sheet.poolAccounts.length > 8
                        Layout.fillWidth: true
                        placeholderText: qsTr("Konto, Bank oder IBAN")
                        onTextChanged: sheet.accountSearch = text
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        Text { text: qsTr("%1 von %2 gewählt").arg(sheet.selectedIDList.length).arg(sheet.poolAccounts.length)
                               font.pixelSize: Theme.caption; color: Theme.secondary }
                        Item { Layout.fillWidth: true }
                        Button {
                            text: qsTr("Alle"); flat: true; enabled: sheet.selectedIDList.length !== sheet.poolAccounts.length
                            onClicked: { const s = {}; for (const a of sheet.poolAccounts) s[a.id] = true; sheet.setSelectedIDs(s) }
                        }
                        Button { text: qsTr("Keine"); flat: true; enabled: sheet.selectedIDList.length > 0; onClicked: sheet.setSelectedIDs({}) }
                    }
                    RowLayout {
                        readonly property bool allCollapsed: Object.keys(sheet.collapsedBanks).length === sheet.accountsByBank.length
                        Button {
                            text: parent.allCollapsed ? qsTr("Alle aufklappen") : qsTr("Alle zuklappen"); flat: true
                            font.pixelSize: Theme.caption
                            onClicked: {
                                const c = {}
                                if (!parent.allCollapsed) for (const g of sheet.accountsByBank) c[g.bank] = true
                                sheet.collapsedBanks = c
                            }
                        }
                        Item { Layout.fillWidth: true }
                    }
                    ScrollView {
                        ScrollBar.vertical: ScrollViewBar {}
                        id: accountScroll
                        Layout.fillWidth: true
                        Layout.preferredHeight: {
                            const n = sheet.filteredAccounts.length
                            if (n <= 6) return Math.max(n, 1) * 36 + sheet.accountsByBank.length * 28
                            return Math.min(320, Math.min(n, 12) * 36 + 80)
                        }
                        contentWidth: availableWidth
                        clip: true
                        Column {
                            width: accountScroll.availableWidth
                            Repeater {
                                model: sheet.accountsByBank
                                Column {
                                    id: bankGroup
                                    required property var modelData
                                    readonly property bool expanded: !sheet.collapsedBanks[modelData.bank]
                                    readonly property int selectedCount: modelData.accounts.filter(a => !!sheet.selectedIDs[a.id]).length
                                    width: parent.width
                                    // bankHeader
                                    RowLayout {
                                        width: parent.width; height: 28
                                        spacing: 6
                                        ToolButton {
                                            implicitWidth: 20; implicitHeight: 20
                                            icon.name: SF.icon(bankGroup.expanded ? "chevron.down" : "chevron.right")
                                            icon.width: 10; icon.height: 10
                                            ToolTip.visible: hovered
                                            ToolTip.text: bankGroup.expanded ? qsTr("Zuklappen") : qsTr("Aufklappen")
                                            onClicked: {
                                                const c = Object.assign({}, sheet.collapsedBanks)
                                                if (bankGroup.expanded) c[bankGroup.modelData.bank] = true; else delete c[bankGroup.modelData.bank]
                                                sheet.collapsedBanks = c
                                            }
                                        }
                                        CheckBox {
                                            tristate: true
                                            checkState: bankGroup.selectedCount === 0 ? Qt.Unchecked
                                                      : bankGroup.selectedCount === bankGroup.modelData.accounts.length ? Qt.Checked : Qt.PartiallyChecked
                                            nextCheckState: () => checkState
                                            onClicked: {
                                                const accounts = bankGroup.modelData.accounts
                                                if (BookingExport.isCamt(sheet.options.format)) {
                                                    // A group without a CAMT-capable account: leave the choice alone.
                                                    const one = accounts.find(sheet.camtUsable)
                                                    if (one) { const s = {}; s[one.id] = true; sheet.setSelectedIDs(s) }
                                                    return
                                                }
                                                const s = Object.assign({}, sheet.selectedIDs)
                                                const allOn = bankGroup.selectedCount === accounts.length
                                                for (const a of accounts) { if (allOn) delete s[a.id]; else s[a.id] = true }
                                                sheet.setSelectedIDs(s)
                                            }
                                        }
                                        Text { text: bankGroup.modelData.bank; font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold
                                               color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
                                        Text { text: bankGroup.selectedCount + "/" + bankGroup.modelData.accounts.length
                                               font.pixelSize: Theme.caption; color: Theme.secondary; font.features: { "tnum": 1 } }
                                    }
                                    Repeater {
                                        model: bankGroup.expanded ? bankGroup.modelData.accounts : []
                                        // accountCheckRow
                                        Item {
                                            id: accRow
                                            required property var modelData
                                            readonly property bool on: !!sheet.selectedIDs[modelData.id]
                                            readonly property bool usable: !BookingExport.isCamt(sheet.options.format) || sheet.camtUsable(modelData)
                                            readonly property string sub: !usable ? qsTr("Nicht für Darlehen und Depots")
                                                : [modelData.owner, modelData.iban ? Fmt.iban(modelData.iban) : ""].filter(x => !!x).join(" · ")
                                            width: bankGroup.width; height: 36
                                            RowLayout {
                                                anchors.fill: parent; anchors.leftMargin: 22; anchors.rightMargin: 4
                                                spacing: 8
                                                CheckBox { checked: accRow.on; enabled: accRow.usable; onClicked: accRow.toggle() }
                                                ColumnLayout {
                                                    spacing: 1; Layout.fillWidth: true
                                                    Text { text: accRow.modelData.name; color: accRow.usable ? Theme.text : Theme.secondary
                                                           font.pixelSize: Theme.body; elide: Text.ElideRight; Layout.fillWidth: true }
                                                    Text { visible: accRow.sub !== ""; text: accRow.sub; color: Theme.secondary; font.pixelSize: Theme.caption
                                                           elide: Text.ElideRight; Layout.fillWidth: true }
                                                }
                                            }
                                            function toggle() {
                                                if (!usable) return
                                                let s
                                                if (BookingExport.isCamt(sheet.options.format)) { s = {}; s[modelData.id] = true }
                                                else { s = Object.assign({}, sheet.selectedIDs); if (on) delete s[modelData.id]; else s[modelData.id] = true }
                                                sheet.setSelectedIDs(s)
                                            }
                                            MouseArea { anchors.fill: parent; anchors.leftMargin: 60; onClicked: accRow.toggle() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // MARK: periodSection
                FormSection {
                    visible: !sheet.isSelectionExport
                    title: qsTr("Zeitraum")
                    ChoiceRow {
                        visible: sheet.canUseSelection
                        title: sheet.selectionCount > 1 ? qsTr("Auswahl (%1)").arg(sheet.selectionCount) : qsTr("Dieser Umsatz")
                        subtitle: qsTr("Nur die markierten Buchungen")
                        selected: sheet.options.scope === "selection"
                        onClicked: sheet.setOption("scope", "selection")
                    }
                    Repeater {
                        model: BookingExport.menuCases
                        ChoiceRow {
                            required property string modelData
                            title: BookingExport.presetTitle(modelData)
                            subtitle: sheet.periodSubtitle(modelData)
                            selected: sheet.options.scope === "filtered" && sheet.options.periodPreset === modelData
                            onClicked: {
                                const o = Object.assign({}, sheet.options)
                                o.scope = "filtered"; o.periodPreset = modelData
                                sheet.applyOptions(o)
                                if (modelData === "custom") sheet.syncCustomDatesFromOptions()
                            }
                        }
                    }
                    FormRow {
                        visible: sheet.options.scope === "filtered" && sheet.options.periodPreset === "custom"
                        label: qsTr("Von")
                        DateField { date: sheet.customFromDate; maximum: sheet.customToDate; onPicked: d => sheet.setCustomDate(true, d) }
                    }
                    FormRow {
                        visible: sheet.options.scope === "filtered" && sheet.options.periodPreset === "custom"
                        label: qsTr("Bis")
                        DateField { date: sheet.customToDate; minimum: sheet.customFromDate; onPicked: d => sheet.setCustomDate(false, d) }
                    }
                }

                // MARK: formatSection
                FormSection {
                    title: qsTr("Format")
                    footer: BookingExport.isCamt(sheet.options.format) && !sheet.camtAccountOK
                        ? (sheet.camtUnusableAccount ? qsTr("Für Darlehen und Depots gibt es keinen CAMT.053-Auszug.")
                                                     : qsTr("CAMT.053 gilt immer für genau ein Konto (#372)."))
                        : sheet.options.scope === "selection" && BookingExport.isCamt(sheet.options.format)
                        ? qsTr("CAMT.053 für markierte Buchungen enthält deren Zeitraum mit den echten Salden des Kontos.") : ""
                    Repeater {
                        model: BookingExport.formats
                        ChoiceRow {
                            required property var modelData
                            title: modelData.title; subtitle: modelData.subtitle
                            selected: sheet.options.format === modelData.id
                            onClicked: sheet.setOption("format", modelData.id)
                        }
                    }
                }

                // MARK: detailsSection
                FormSection {
                    visible: sheet.options.format === "pdf" || sheet.options.format === "csv"
                    footer: sheet.options.format === "pdf" ? qsTr("Einstellungen bleiben gespeichert. Anfangssaldo und Endsaldo stehen immer im Kopf.") : ""
                    ToggleRow { label: qsTr("Kategorie"); checked: sheet.options.showCategory; onToggled: on => sheet.setOption("showCategory", on) }
                    ToggleRow { label: qsTr("Notizen"); checked: sheet.options.showNote; onToggled: on => sheet.setOption("showNote", on) }
                    // DisclosureGroup
                    Item {
                        Layout.fillWidth: true
                        implicitHeight: 24
                        RowLayout {
                            anchors.fill: parent
                            spacing: 6
                            SFIcon { symbol: sheet.showDetails ? "chevron.down" : "chevron.right"; size: 10 }
                            Text { text: qsTr("Weitere Optionen"); color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true }
                        }
                        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sheet.showDetails = !sheet.showDetails }
                    }
                    ToggleRow { visible: sheet.showDetails; Layout.leftMargin: 16; label: qsTr("Tagessaldo-Spalte")
                                checked: sheet.options.showDailyBalance; onToggled: on => sheet.setOption("showDailyBalance", on) }
                    ToggleRow { visible: sheet.showDetails; Layout.leftMargin: 16; label: qsTr("Nach Konto gruppieren")
                                checked: sheet.options.groupByAccount; onToggled: on => sheet.setOption("groupByAccount", on) }
                    ToggleRow { visible: sheet.showDetails; Layout.leftMargin: 16; label: qsTr("Nur erste Zeile Verwendungszweck")
                                checked: sheet.options.firstLineOnly; onToggled: on => sheet.setOption("firstLineOnly", on) }
                    FormRow {
                        visible: sheet.showDetails; Layout.leftMargin: 16
                        label: qsTr("Reihenfolge")
                        AppComboBox {
                            Layout.preferredWidth: 170
                            model: [qsTr("Älteste zuerst"), qsTr("Neueste zuerst")]
                            currentIndex: sheet.options.newestFirst ? 1 : 0
                            onActivated: i => sheet.setOption("newestFirst", i === 1)
                        }
                    }
                }

                // MARK: file name
                FormSection {
                    footer: qsTr("„Speichern“ legt direkt in Downloads ab. „Unter …“ fragt nach dem Ort.")
                    FormRow {
                        label: qsTr("Datei")
                        TextEdit {
                            Layout.maximumWidth: 330
                            text: sheet.suggestedName
                            readOnly: true; selectByMouse: true
                            wrapMode: TextEdit.WrapAnywhere
                            horizontalAlignment: Text.AlignRight
                            font.family: "monospace"; font.pixelSize: Theme.caption; color: Theme.secondary
                        }
                    }
                }
            }
        }
    }

    // .alert("Export fehlgeschlagen", isPresented: failure != nil)
    Sheet {
        id: failureDialog
        sheetWidth: 420
        visible: sheet.failure !== ""
        onClosed: sheet.failure = ""
        ColumnLayout {
            width: failureDialog.availableWidth
            spacing: 12
            Text { text: qsTr("Export fehlgeschlagen"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text
                   wrapMode: Text.WordWrap; Layout.fillWidth: true }
            TextEdit { text: sheet.failure; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; color: Theme.text
                       font.pixelSize: Theme.body; Layout.fillWidth: true }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("OK"); highlighted: true; onClicked: failureDialog.close() }
            }
        }
    }
}
