import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts

// MoneyMoneyDatabaseImportView.swift: MoneyMoney database import as an assistant
//  1. Source – a copy of MoneyMoney's data folder (database plus statement PDFs)
//     or a single `MoneyMoney.sqlite` (backup), and the database password;
//  2. Accounts – which accounts, account type, target;
//  3. Übernahme – what else, and one connection type per bank login;
//  4. Enable Banking – set up the user's application, only when needed;
//  then the import runs and the result shows the next step.
// MoneyMoney only runs on macOS: on Linux the user copies its folder (or the
// database file) from the Mac and picks it here. Everything is only read.
ColumnLayout {
    id: v
    /// The MoneyMoneyImportSheet: close(), source, importRunning.
    property var host
    spacing: 16

    property string step: "source"         // source | accounts | banks | enableBanking | importing | done
    property string filePath: ""
    property bool fileIsFolder: false
    /// Folder with the statement PDFs when the database is a lone file (backup).
    property string statementsPath: ""
    property bool importStatements: true
    property string password: ""
    property var preview: null              // MoneyMoneyDbPreview
    /// mm_id -> { selected, kind, target }
    property var choices: ({})
    /// Connection per MoneyMoney bank login: option key, "" = don't connect.
    property var bankChoice: ({})
    property bool importCategories: true
    property bool importNotes: true
    property bool importGroups: true
    property bool importTemplates: true
    property bool createConnections: true
    /// Enable Banking credentials entered in the assistant.
    property var ebValues: ({})
    property bool loading: false
    property var progress: null             // MoneyMoneyDbImportProgress
    property bool cancelling: false
    property string error: ""
    property var result: null               // MoneyMoneyDbImportResult
    property bool checksExpanded: false
    readonly property bool isDemo: AppModel.demoDbPreview !== null

    onStepChanged: if (host) host.importRunning = step === "importing"

    readonly property var kindChoices: [
        { id: "Checking", text: qsTr("Girokonto") }, { id: "Savings", text: qsTr("Tagesgeld / Sparen") },
        { id: "CreditCard", text: qsTr("Kreditkarte") }, { id: "PaymentService", text: qsTr("Zahlungsdienst") },
        { id: "Brokerage", text: qsTr("Depot") }, { id: "Cash", text: qsTr("Bargeld") }, { id: "Loan", text: qsTr("Darlehen") }
    ]

    function reset() {
        step = "source"; filePath = ""; fileIsFolder = false; statementsPath = ""; importStatements = true
        password = ""; preview = null; choices = ({}); bankChoice = ({}); importCategories = true; importNotes = true
        importGroups = true; importTemplates = true; createConnections = true; ebValues = ({}); loading = false
        progress = null; cancelling = false; error = ""; result = null; checksExpanded = false
        tableSelection = ({}); tableAnchor = -1
        if (AppModel.demoDbPreview) {
            filePath = "/home/max/Finanzdaten"
            fileIsFolder = true
            password = "demo-passwort"
            apply(AppModel.demoDbPreview)
            step = "accounts"
        }
    }
    /// The password is dropped from the view state when the sheet closes.
    function forget() { password = "" }

    function andList(names) {
        if (names.length <= 1) return names.join("")
        return names.slice(0, -1).join(", ") + " " + qsTr("und") + " " + names[names.length - 1]
    }

    // MARK: - Header texts

    readonly property bool allChecksOk: result !== null && result.account_checks.every(c => c.ok)
    readonly property string stepTitle: {
        switch (step) {
        case "source": return qsTr("Vorhandene Daten importieren")
        case "accounts": return qsTr("Konten")
        case "banks": return qsTr("Übernahme")
        case "enableBanking": return qsTr("Enable Banking einrichten")
        case "importing": return qsTr("Import läuft")
        default: return qsTr("Import abgeschlossen")
        }
    }
    readonly property string stepSubtitle: {
        switch (step) {
        case "source": return qsTr("Wählen Sie, woher die Daten kommen. Die Originaldaten bleiben unverändert.")
        case "accounts":
            if (!preview) return ""
            return [qsTr("%1 Konten").arg(preview.accounts.length), qsTr("%1 Buchungen").arg(Fmt.count(preview.total_bookings)), Fmt.yearSpan(preview)]
                .filter(x => !!x).join(" · ")
        case "banks": return qsTr("Was GiroCrab außerdem übernimmt und wie es künftig Umsätze abruft.")
        case "enableBanking": return qsTr("Für %1 braucht GiroCrab eine kostenlose Enable-Banking-Anwendung.").arg(ebBankNames)
        case "importing": return qsTr("Bitte warten Sie, bis der Import fertig ist.")
        default: {
            if (!result) return ""
            const line = qsTr("%1 Buchungen aus %2 Konten übernommen.").arg(Fmt.count(result.bookings_new)).arg(result.accounts_created + result.accounts_merged)
            const checks = result.account_checks
            const problems = checks.filter(c => !c.ok).length
            if (!checks.length) return line
            if (problems > 0) return line + " " + qsTr("%1 von %2 Konten weichen von den importierten Daten ab.").arg(problems).arg(checks.length)
            return line + " " + (checks.length === 1 ? qsTr("Das Konto stimmt mit den importierten Daten überein.")
                                                       : qsTr("Alle %1 Konten stimmen mit den importierten Daten überein.").arg(checks.length))
        }
        }
    }

    // MARK: - Steps

    readonly property var visibleSteps: needsEnableBankingSetup ? ["source", "accounts", "banks", "enableBanking"] : ["source", "accounts", "banks"]
    readonly property bool isLastStep: visibleSteps[visibleSteps.length - 1] === step
    readonly property bool canContinue: step === "source" ? (filePath !== "" && !loading)
        : step === "accounts" ? Object.keys(choices).some(k => choices[k].selected) : true

    function next() {
        error = ""
        if (step === "source") { loadPreview(false); return }
        if (step === "enableBanking") {
            for (const k in ebValues) if ((ebValues[k] || "").trim() !== "") AppModel.saveSetting(k, ebValues[k])
            runImport()
            return
        }
        if (isLastStep) runImport()
        else go(visibleSteps[visibleSteps.indexOf(step) + 1])
    }
    function back() {
        error = ""
        const i = visibleSteps.indexOf(step)
        if (i > 0) go(visibleSteps[i - 1])
    }
    function go(s) {
        if (s === "enableBanking") AppModel.psd2LinkHints = plannedPsd2Links
        step = s
    }

    // MARK: - Choices

    function choice(a) { return choices[a.mm_id] || { selected: true, kind: a.suggested_kind, target: a.existing_account_id } }
    function setChoice(a, patch) {
        const c = Object.assign({}, choices)
        c[a.mm_id] = Object.assign({}, choice(a), patch)
        choices = c
    }
    readonly property int selectedCount: preview ? preview.accounts.filter(a => choice(a).selected).length : 0
    /// Table(selection:) of the account step: mm_id -> true (rows highlighted, not imported yet).
    property var tableSelection: ({})
    property int tableAnchor: -1
    /// Click selects, Ctrl+click adds / removes, Shift+click extends from the last click.
    function selectRow(index, modifiers) {
        const accounts = preview ? preview.accounts : []
        const id = accounts[index].mm_id
        let sel = {}
        if (modifiers & Qt.ShiftModifier && tableAnchor >= 0) {
            if (modifiers & Qt.ControlModifier) sel = Object.assign({}, tableSelection)
            for (let i = Math.min(tableAnchor, index); i <= Math.max(tableAnchor, index); i++) sel[accounts[i].mm_id] = true
        } else if (modifiers & Qt.ControlModifier) {
            sel = Object.assign({}, tableSelection)
            if (sel[id]) delete sel[id]; else sel[id] = true
            tableAnchor = index
        } else {
            sel[id] = true
            tableAnchor = index
        }
        tableSelection = sel
    }
    /// Space switches the selected accounts on or off, like a checkbox.
    function toggleSelectedRows() {
        if (!preview) return false
        const rows = preview.accounts.filter(a => tableSelection[a.mm_id])
        if (!rows.length) return false
        const on = !rows.every(a => choice(a).selected)
        const c = Object.assign({}, choices)
        for (const a of rows) c[a.mm_id] = Object.assign({}, choice(a), { selected: on })
        choices = c
        return true
    }

    function apply(p) {
        preview = p
        const c = {}
        for (const a of p.accounts) c[a.mm_id] = choices[a.mm_id] || { selected: true, kind: a.suggested_kind, target: a.existing_account_id }
        choices = c
    }

    function accountSubtitle(a) {
        const parts = []
        if (a.bank_name !== "") parts.push(a.bank_name)
        if (a.iban !== "") parts.push(Fmt.iban(a.iban))
        else if (a.account_number !== "") parts.push(qsTr("Konto %1").arg(a.account_number))
        if (a.currency !== "EUR") parts.push(a.currency)
        return parts.join(" · ")
    }
    function bookingSummary(a) {
        const parts = []
        if (a.first_date && a.last_date) parts.push(Fmt.medium(a.first_date) + " – " + Fmt.medium(a.last_date))
        if (a.pending_count > 0) parts.push(qsTr("%1 vorgemerkt").arg(a.pending_count))
        if (a.holding_count > 0) parts.push(qsTr("%1 Depotpositionen").arg(a.holding_count))
        if (a.statement_count > 0) parts.push(qsTr("%1 Dokumente").arg(a.statement_count))
        if (a.group_name !== "") parts.push(qsTr("Gruppe „%1“").arg(a.group_name))
        return parts.join(" · ")
    }

    // MARK: - Bank connections

    /// The bank's connection options (the core's catalogue; the live Enable
    /// Banking list of the Mac app is not loaded here).
    function connectionOptions(b) { return b.connection_options }
    function chosenKey(b) {
        const k = bankChoice[b.mm_id]
        if (k !== undefined) return k
        const o = connectionOptions(b)
        return o.length ? o[0].key : ""
    }
    function chosenOption(b) { const k = chosenKey(b); return connectionOptions(b).find(o => o.key === k) || null }
    function setBankChoice(b, key) { const c = Object.assign({}, bankChoice); c[b.mm_id] = key; bankChoice = c }
    function optionLabel(o) {
        return o.via === "FinTs" ? qsTr("Online-Banking (FinTS)") : o.via === "OwnApi" ? qsTr("Schnittstelle der Bank") : qsTr("Enable Banking")
    }
    function connectorName(kind) { const d = kind ? AppModel.descriptor(kind) : null; return d ? d.display_name : qsTr("Bank") }
    /// Login name, and the bank code only when it is a real one.
    function bankDetail(b) {
        const code = b.bank_code.replace(/[^0-9]/g, "")
        const blz = code.length === 8 && code === b.bank_code.replace(/\s/g, "") ? qsTr("BLZ %1").arg(code) : null
        return [b.login_name === "" ? null : b.login_name, blz].filter(x => !!x).join(" · ")
    }
    readonly property bool chosenEnableBanking: preview ? preview.banks.some(b => { const o = chosenOption(b); return o !== null && o.via === "EnableBanking" }) : false
    readonly property bool enableBankingReady: {
        const a = AppModel.settingEntry("enablebanking.application_id"), k = AppModel.settingEntry("enablebanking.private_key_pem")
        return !!(a && a.has_value && k && k.has_value)
    }
    readonly property bool needsEnableBankingSetup: createConnections && chosenEnableBanking && !enableBankingReady
    readonly property string ebBankNames: preview ? andList(preview.banks.filter(b => { const o = chosenOption(b); return o !== null && o.via === "EnableBanking" }).map(b => b.name)) : ""
    /// The Enable Banking banks of this import with their selected accounts.
    readonly property var plannedPsd2Links: {
        if (!preview) return []
        return preview.banks.filter(b => { const o = chosenOption(b); return o !== null && o.via === "EnableBanking" }).map(b => ({
            connection_id: -b.mm_id, bank_name: b.name, created: true,
            accounts: preview.accounts.filter(a => a.mm_bank_id === b.mm_id && choice(a).selected)
                .map(a => ({ name: a.name, iban: a.iban, account_number: a.account_number }))
        }))
    }

    // MARK: - Actions

    function pathOf(url) { return decodeURIComponent(url.toString().replace(/^file:\/\//, "")) }
    function picked(path, folder) {
        filePath = path; fileIsFolder = folder
        statementsPath = ""; preview = null; choices = ({}); bankChoice = ({}); error = ""
    }

    FileDialog {
        id: filePicker
        title: qsTr("Datenbank auswählen (.sqlite)")
        nameFilters: [qsTr("Datenbank (*.sqlite)"), qsTr("Alle Dateien (*)")]
        onAccepted: v.picked(v.pathOf(selectedFile), false)
    }
    FolderDialog {
        id: folderPicker
        title: qsTr("Kopierten Ordner mit Datenbank und Kontoauszügen auswählen")
        onAccepted: v.picked(v.pathOf(selectedFolder), true)
    }
    FolderDialog {
        id: statementsPicker
        title: qsTr("Ordner mit den gesicherten Kontoauszügen (PDF) auswählen – Unterordner werden mit durchsucht")
        onAccepted: { v.statementsPath = v.pathOf(selectedFolder); v.loadPreview(true) }
    }

    /// `describe(error)`: for Authentication the core's own sentence (wrong password …),
    /// taken from the debug text with its escaped quotes undone; else the message.
    function errorText(err) {
        const e = AppModel.parseCoreError(err)
        const m = /message: "((?:[^"\\]|\\.)*)"/.exec(e.debug || "")
        return e.kind === "Authentication" && m ? m[1].replace(/\\(.)/g, (_, c) => c === "n" ? "\n" : c) : e.message
    }

    /// Reads the database; `stay` = keep the current step (statements folder changed).
    function loadPreview(stay) {
        if (filePath === "" || isDemo) return
        loading = true; error = ""
        AppModel.coreAsync("preview_moneymoney_database", {
            path: filePath, password: password, statements_dir: statementsPath === "" ? null : statementsPath
        }, p => {
            loading = false
            apply(p)
            if (!stay) go("accounts")
        }, err => {
            loading = false
            error = errorText(err)
        })
    }

    property Timer progressTimer: Timer {
        interval: 100; repeat: true
        running: v.step === "importing" && !v.isDemo
        onTriggered: { const p = AppModel.core("moneymoney_import_progress"); if (p) v.progress = p }
    }

    function runImport() {
        if (!preview) return
        error = ""
        if (isDemo) {
            // demo: nothing to read – show what the summary looks like
            result = AppModel.demoMoneyMoneyDbResult(preview, importStatements, plannedPsd2Links)
            password = ""
            step = "done"
            return
        }
        const options = {
            accounts: preview.accounts.map(a => { const c = choice(a); return { mm_id: a.mm_id, "import": c.selected, kind: c.kind, target_account_id: c.target } }),
            import_categories: importCategories,
            import_notes: importNotes,
            create_connections: createConnections,
            import_groups: importGroups,
            import_transfer_templates: importTemplates && preview.transfer_template_count > 0,
            import_statements: importStatements && preview.statements_found > 0,
            statements_dir: statementsPath === "" ? null : statementsPath,
            bank_choices: preview.banks.map(b => ({ mm_bank_id: b.mm_id, connection: chosenOption(b) }))
        }
        progress = { fraction: 0, account_index: 0, account_count: 0, account_name: "", bookings_done: 0, bookings_total: 0, documents_done: 0, documents_total: 0 }
        cancelling = false
        const from = step
        step = "importing"
        AppModel.coreAsync("import_moneymoney_database", { path: filePath, password: password, options: options }, r => {
            cancelling = false
            password = ""
            result = r
            AppModel.finishMoneyMoneyDbImport(r)
            step = "done"
        }, err => {
            // after a cancel or an error the assistant goes back, ready for another try
            const wasCancelled = cancelling
            cancelling = false
            step = from
            if (wasCancelled) { error = qsTr("Import abgebrochen – es wurde nichts übernommen."); return }
            error = errorText(err)
        })
    }
    function cancelImport() {
        cancelling = true
        AppModel.core("cancel_moneymoney_import")
    }

    /// Closes the import and starts the bank consent of that connection.
    function startConsent(connectionID) {
        const conn = AppModel.connections.find(c => c.id === connectionID)
        if (!conn) return
        host.close()
        AppModel.requestSync(conn)
    }

    function progressStatus(p) {
        if (!p || p.account_index === 0) return qsTr("Datenbank wird gelesen …")
        if (p.account_index > p.account_count) return qsTr("Bankzugänge und Gruppen werden angelegt …")
        return qsTr("Konto %1 von %2 · %3").arg(p.account_index).arg(p.account_count).arg(p.account_name)
    }
    function progressCounts(p) {
        let s = qsTr("%1 von %2 Buchungen").arg(Fmt.count(p.bookings_done)).arg(Fmt.count(p.bookings_total))
        if (p.documents_total > 0) s += " · " + qsTr("%1 von %2 Dokumenten").arg(Fmt.count(p.documents_done)).arg(Fmt.count(p.documents_total))
        return s
    }
    function checkNote(c) {
        const parts = []
        const missing = c.mm_bookings - (c.found_bookings + c.deleted_bookings)
        if (missing > 0) parts.push(qsTr("%1 Buchungen fehlen").arg(missing))
        if (c.deleted_bookings > 0) parts.push(qsTr("%1 in GiroCrab gelöscht").arg(c.deleted_bookings))
        if (c.other_bookings > 0) parts.push(qsTr("%1 weitere von der Bank").arg(c.other_bookings))
        if (c.balance_from_bank) parts.push(qsTr("Saldo von der Bank"))
        if (!c.mm_balance_shown) parts.push(qsTr("Saldo aus Umsätzen berechnet"))
        return parts.join(" · ")
    }

    // MARK: - Layout

    ImportAssistantHeader {
        title: v.stepTitle
        subtitle: v.stepSubtitle
        symbol: v.step === "done" ? (v.allChecksOk ? "checkmark.circle.fill" : "exclamationmark.triangle.fill") : "square.and.arrow.down.on.square"
        tint: v.step === "done" ? (v.allChecksOk ? Theme.inflow : Theme.warning) : Theme.accent
    }

    StackLayout {
        Layout.fillWidth: true
        Layout.preferredHeight: 470
        currentIndex: ["source", "accounts", "banks", "enableBanking", "importing", "done"].indexOf(v.step)

        // MARK: 1. Source
        ColumnLayout {
            spacing: 18
            ImportSourceSection {
                source: v.host ? v.host.source : "database"
                onChosen: s => v.host.source = s
            }
            FormSection {
                title: qsTr("Datenbank")
                footers: [qsTr("Kopieren Sie den Ordner mit Datenbank und Kontoauszügen vom Mac auf diesen Rechner und wählen Sie ihn hier, damit auch die Kontoauszüge übernommen werden. Eine einzelne Datenbankdatei (.sqlite), etwa aus einem Backup, geht auch."),
                          qsTr("Kein Datenbank-Passwort festgelegt? Legen Sie vorher eines in dem Programm fest, das die Datenbank erstellt hat."),
                          qsTr("Das Passwort der Datenbank wurde nicht in GiroCrab festgelegt. Tragen Sie es hier im Feld oben ein.")]
                ImportFileRow {
                    label: qsTr("Importdaten")
                    path: v.filePath
                    name: v.filePath.split("/").pop()
                    isFolder: v.fileIsFolder
                    loading: v.loading
                    onChoose: filePicker.open()
                    Button { text: qsTr("Ordner …"); onClicked: folderPicker.open() }
                }
                FormRow {
                    label: qsTr("Datenbank-Passwort")
                    TextField {
                        Layout.preferredWidth: 280
                        echoMode: TextInput.Password
                        placeholderText: qsTr("Erforderlich")
                        text: v.password
                        onTextEdited: v.password = text
                    }
                }
            }
            Item { Layout.fillHeight: true }
        }

        // MARK: 2. Accounts
        ColumnLayout {
            spacing: 10
            RowLayout {
                Layout.fillWidth: true
                Text { text: v.preview ? qsTr("%1 von %2 Konten ausgewählt").arg(v.selectedCount).arg(v.preview.accounts.length) : ""
                       font.pixelSize: Theme.callout; color: Theme.secondary }
                Item { Layout.fillWidth: true }
                Button {
                    visible: v.preview !== null
                    text: v.preview && v.selectedCount === v.preview.accounts.length ? qsTr("Alle abwählen") : qsTr("Alle auswählen")
                    onClicked: {
                        const all = v.selectedCount !== v.preview.accounts.length
                        const c = {}
                        for (const a of v.preview.accounts) c[a.mm_id] = Object.assign({}, v.choice(a), { selected: all })
                        v.choices = c
                    }
                }
            }
            Rectangle {
                Layout.fillWidth: true; Layout.fillHeight: true
                radius: 8; color: Theme.card; border.color: Theme.border
                clip: true
                ListView {
                    id: accountTable
                    anchors.fill: parent; anchors.margins: 1
                    clip: true
                    focus: v.step === "accounts"
                    Keys.onSpacePressed: e => { e.accepted = v.toggleSelectedRows() }
                    ScrollBar.vertical: AppScrollBar {}
                    readonly property bool showTarget: AppModel.allAccounts.length > 0
                    model: v.preview ? v.preview.accounts : []
                    headerPositioning: ListView.OverlayHeader
                    header: Rectangle {
                        width: accountTable.width; height: 26; z: 2
                        color: Theme.card
                        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
                        RowLayout {
                            anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 14; spacing: 8
                            Item { Layout.preferredWidth: 22 }
                            Text { text: qsTr("Konto"); Layout.fillWidth: true; font.pixelSize: Theme.caption; color: Theme.secondary }
                            Text { text: qsTr("Buchungen"); Layout.preferredWidth: 80; horizontalAlignment: Text.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary }
                            Text { text: qsTr("Saldo"); Layout.preferredWidth: 110; horizontalAlignment: Text.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary }
                            Text { text: qsTr("Kontoart"); Layout.preferredWidth: 150; font.pixelSize: Theme.caption; color: Theme.secondary }
                            Text { visible: accountTable.showTarget; text: qsTr("Ziel"); Layout.preferredWidth: 170; font.pixelSize: Theme.caption; color: Theme.secondary }
                        }
                    }
                    delegate: Rectangle {
                        id: rowItem
                        required property var modelData
                        required property int index
                        readonly property var c: v.choice(modelData)
                        width: accountTable.width; height: 44
                        readonly property bool rowSelected: !!v.tableSelection[modelData.mm_id]
                        color: rowSelected ? Theme.selection : index % 2 ? Theme.withAlpha(Theme.secondary, 0.05) : "transparent"
                        TapHandler {
                            acceptedModifiers: Qt.KeyboardModifierMask
                            onTapped: (_, button) => { v.selectRow(rowItem.index, point.modifiers); accountTable.forceActiveFocus() }
                        }
                        RowLayout {
                            anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 14; spacing: 8
                            CheckBox {
                                Layout.preferredWidth: 22
                                checked: rowItem.c.selected
                                onToggled: v.setChoice(rowItem.modelData, { selected: checked })
                                Accessible.name: qsTr("„%1“ importieren").arg(rowItem.modelData.name)
                            }
                            ColumnLayout {
                                spacing: 1; Layout.fillWidth: true
                                opacity: rowItem.c.selected ? 1 : 0.5
                                Text { text: rowItem.modelData.name; elide: Text.ElideRight; Layout.fillWidth: true; color: Theme.text; font.pixelSize: Theme.body }
                                Text { text: v.accountSubtitle(rowItem.modelData); elide: Text.ElideRight; Layout.fillWidth: true; color: Theme.secondary; font.pixelSize: Theme.caption }
                            }
                            Text {
                                Layout.preferredWidth: 80; horizontalAlignment: Text.AlignRight
                                text: Fmt.count(rowItem.modelData.booking_count); color: Theme.text; font.pixelSize: Theme.body; font.features: { "tnum": 1 }
                                HoverHandler { id: bookingsHover }
                                ToolTip.visible: bookingsHover.hovered && ToolTip.text !== ""; ToolTip.text: v.bookingSummary(rowItem.modelData); ToolTip.delay: 600
                            }
                            Text {
                                Layout.preferredWidth: 110; horizontalAlignment: Text.AlignRight
                                text: Fmt.money(rowItem.modelData.computed_balance_cents, rowItem.modelData.currency); color: Theme.text; font.pixelSize: Theme.body; font.features: { "tnum": 1 }
                                Sensitive { amounts: true }
                                HoverHandler { id: balanceHover }
                                ToolTip.visible: balanceHover.hovered; ToolTip.text: rowItem.modelData.balance_source; ToolTip.delay: 600
                            }
                            AppComboBox {
                                Layout.preferredWidth: 150
                                textRole: "text"
                                model: v.kindChoices
                                currentIndex: Math.max(0, v.kindChoices.findIndex(k => k.id === rowItem.c.kind))
                                onActivated: i => v.setChoice(rowItem.modelData, { kind: v.kindChoices[i].id })
                            }
                            AppComboBox {
                                id: targetBox
                                visible: accountTable.showTarget
                                Layout.preferredWidth: 170
                                textRole: "text"
                                readonly property var a: rowItem.modelData
                                model: {
                                    const list = [{ id: null, text: qsTr("Neues Konto") }]
                                    for (const acc of AppModel.allAccounts)
                                        list.push({ id: acc.id, text: acc.id === a.existing_account_id ? qsTr("%1 (gleiche IBAN)").arg(acc.name) : acc.name })
                                    if (a.existing_account_id !== null && !AppModel.allAccounts.some(x => x.id === a.existing_account_id))
                                        list.push({ id: a.existing_account_id, text: a.existing_account_name || qsTr("Vorhandenes Konto") })
                                    return list
                                }
                                currentIndex: Math.max(0, model.findIndex(o => o.id === rowItem.c.target))
                                onActivated: i => v.setChoice(rowItem.modelData, { target: model[i].id })
                            }
                        }
                    }
                }
            }
        }

        // MARK: 3. Data and bank connections
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            contentWidth: availableWidth
            ColumnLayout {
                width: parent.width
                spacing: 18
                // "Daten": what is taken over besides the bookings
                FormSection {
                    id: dataSection
                    visible: v.preview !== null
                    title: qsTr("Daten")
                    footers: [v.preview && v.preview.replaces_starter_categories && v.importCategories
                              ? qsTr("Die Kategorien ersetzen die GiroCrab-Startkategorien – Ihr Tresor enthält noch keine Buchungen.") : ""]
                    readonly property int groupCount: v.preview ? new Set(v.preview.accounts.map(a => a.group_name).filter(g => g !== "")).size : 0
                    FormRow { label: qsTr("Kategorien und Regeln (%1)").arg(v.preview ? v.preview.category_paths.length : 0)
                              Switch { checked: v.importCategories; onToggled: v.importCategories = checked } }
                    FormRow { label: qsTr("Notizen (%1)").arg(v.preview ? v.preview.notes_count : 0)
                              Switch { checked: v.importNotes; onToggled: v.importNotes = checked } }
                    FormRow { visible: dataSection.groupCount > 0
                              label: qsTr("Kontogruppen (%1)").arg(dataSection.groupCount)
                              Switch { checked: v.importGroups; onToggled: v.importGroups = checked } }
                    FormRow { visible: v.preview !== null && v.preview.transfer_template_count > 0
                              label: qsTr("Überweisungsvorlagen (%1)").arg(v.preview ? v.preview.transfer_template_count : 0)
                              Switch { checked: v.importTemplates; onToggled: v.importTemplates = checked } }
                    // Statement PDFs: the database only lists them, the files are in its
                    // `Statements` folder – or, for a backup, wherever the user saved them.
                    FormRow {
                        visible: v.preview !== null && v.preview.statement_count > 0
                        enabled: v.preview !== null && v.preview.statements_found > 0
                        label: v.preview ? qsTr("Kontoauszüge und Dokumente (%1, %2)").arg(v.preview.statements_found).arg(Fmt.byteCount(v.preview.statements_bytes)) : ""
                        detail: !v.preview || v.preview.statements_found >= v.preview.statement_count ? ""
                            : v.preview.statements_dir === null
                              ? qsTr("%1 PDFs liegen nicht bei dieser Datenbank. Wählen Sie den Ordner, falls Sie sie gesichert haben.").arg(v.preview.statement_count)
                              : qsTr("%1 von %2 PDFs gefunden, die übrigen werden ausgelassen.").arg(v.preview.statements_found).arg(v.preview.statement_count)
                        Switch { checked: v.importStatements; onToggled: v.importStatements = checked }
                    }
                    FormRow {
                        visible: v.preview !== null && v.preview.statement_count > 0 && v.preview.statements_found < v.preview.statement_count
                        label: qsTr("Ordner mit Kontoauszügen")
                        Button { text: v.statementsPath === "" ? qsTr("Auswählen …") : qsTr("Ändern …"); enabled: !v.loading; onClicked: statementsPicker.open() }
                    }
                }
                FormSection {
                    title: qsTr("Bankzugänge")
                    footers: [qsTr("Die Konten erscheinen unter ihrem Bankzugang. Der erste Abruf übernimmt sie, ohne Duplikate anzulegen.")]
                    FormRow { label: qsTr("Bankzugänge anlegen"); Switch { checked: v.createConnections; onToggled: v.createConnections = checked } }
                }
                FormSection {
                    visible: v.preview !== null
                    enabled: v.createConnections
                    footers: [v.createConnections && v.chosenEnableBanking && !v.enableBankingReady ? qsTr("Enable Banking richten Sie im nächsten Schritt ein.") : ""]
                    Repeater {
                        model: v.preview ? v.preview.banks : []
                        FormRow {
                            id: bankRow
                            required property var modelData
                            readonly property var options: v.connectionOptions(modelData)
                            label: modelData.name !== "" ? modelData.name : v.connectorName(modelData.connector)
                            detail: v.bankDetail(modelData)
                            AppComboBox {
                                visible: bankRow.options.length > 1
                                Layout.preferredWidth: 240
                                textRole: "text"
                                model: bankRow.options.map(o => ({ key: o.key, text: v.optionLabel(o) })).concat([{ key: "", text: qsTr("Nicht verbinden") }])
                                currentIndex: Math.max(0, model.findIndex(o => o.key === v.chosenKey(bankRow.modelData)))
                                onActivated: i => v.setBankChoice(bankRow.modelData, model[i].key)
                            }
                            Text {
                                visible: bankRow.options.length === 1
                                text: bankRow.options.length ? v.optionLabel(bankRow.options[0]) + (bankRow.modelData.existing_connection_id !== null ? " · " + qsTr("vorhanden") : "") : ""
                                color: Theme.secondary; font.pixelSize: Theme.body
                            }
                            Text { visible: bankRow.options.length === 0; text: qsTr("Nicht verfügbar"); color: Theme.tertiary; font.pixelSize: Theme.body }
                        }
                    }
                }
            }
        }

        // MARK: 4. Enable Banking
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            contentWidth: availableWidth
            EnableBankingSetupSection {
                width: parent.width - 6   // .padding(.trailing, 6)
                values: v.ebValues
                onEdited: (key, value) => { const x = Object.assign({}, v.ebValues); x[key] = value; v.ebValues = x }
            }
        }

        // MARK: Import running
        Item {
            ColumnLayout {
                anchors.centerIn: parent
                spacing: 14
                SFIcon { symbol: "square.and.arrow.down.on.square"; size: 44; color: Theme.accent; Layout.alignment: Qt.AlignHCenter }
                ProgressBar { Layout.preferredWidth: 380; Layout.alignment: Qt.AlignHCenter; from: 0; to: 1; value: v.progress ? v.progress.fraction : 0 }
                Text { Layout.alignment: Qt.AlignHCenter; Layout.maximumWidth: 600; elide: Text.ElideMiddle
                       text: v.progressStatus(v.progress); font.pixelSize: Theme.callout + 1; color: Theme.text }
                Text { Layout.alignment: Qt.AlignHCenter
                       visible: v.progress !== null && (v.progress.bookings_total > 0 || v.progress.documents_total > 0)
                       text: v.progress ? v.progressCounts(v.progress) : ""; font.pixelSize: Theme.caption; color: Theme.secondary; font.features: { "tnum": 1 } }
                Text { Layout.alignment: Qt.AlignHCenter
                       text: v.cancelling ? qsTr("Der Import wird abgebrochen …") : qsTr("„Import abbrechen“ verwirft alles – es wird dann nichts übernommen.")
                       font.pixelSize: Theme.caption; color: Theme.secondary }
            }
        }

        // MARK: Result
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            contentWidth: availableWidth
            ColumnLayout {
                id: doneCol
                width: parent.width
                spacing: 18
                readonly property var r: v.result
                readonly property var problems: r ? r.account_checks.filter(c => !c.ok) : []
                readonly property var orderedChecks: r ? problems.concat(r.account_checks.filter(c => c.ok)) : []
                readonly property int documentsSkipped: r ? r.documents_missing + r.documents_mismatched + r.documents_too_large : 0

                // Enable Banking connections the import set up: the bank consent is next.
                FormSection {
                    visible: doneCol.r !== null && doneCol.r.psd2_connections.length > 0
                    title: qsTr("Bankfreigaben")
                    footers: [v.enableBankingReady
                              ? qsTr("Konten, die noch nicht mit Ihrer Enable-Banking-Anwendung verknüpft sind, verknüpfen Sie auf enablebanking.com.")
                              : qsTr("Die Enable-Banking-Anwendung wird bei der ersten Freigabe eingerichtet. Verknüpfen Sie dabei alle Konten dieser Banken auf einmal.")]
                    Repeater {
                        model: doneCol.r ? doneCol.r.psd2_connections : []
                        FormRow {
                            required property var modelData
                            label: modelData.bank_name
                            detail: modelData.accounts.map(a => [a.name, a.iban === "" ? (a.account_number === "" ? null : qsTr("Konto %1").arg(a.account_number)) : Fmt.iban(a.iban)]
                                                         .filter(x => !!x).join(" · ")).join("\n")
                            Button { text: qsTr("Freigeben …"); onClicked: v.startConsent(modelData.connection_id) }
                        }
                    }
                    Text {
                        text: "<a href=\"https://enablebanking.com/cp/applications\">" + qsTr("Anwendungen auf enablebanking.com öffnen") + "</a>"
                        font.pixelSize: Theme.caption; linkColor: Theme.accent
                        onLinkActivated: link => Qt.openUrlExternally(link)
                    }
                }
                // what still needs the user comes first
                CheckSection { view: v; visible: doneCol.problems.length > 0; checks: doneCol.orderedChecks; problems: doneCol.problems.length }

                FormSection {
                    visible: doneCol.r !== null
                    title: qsTr("Übernommen")
                    CountRow { label: qsTr("Konten"); n: doneCol.r ? doneCol.r.accounts_created + doneCol.r.accounts_merged : 0
                               detail: doneCol.r && doneCol.r.accounts_merged > 0 ? qsTr("%1 bestehende ergänzt").arg(doneCol.r.accounts_merged) : "" }
                    CountRow { label: qsTr("Buchungen"); n: doneCol.r ? doneCol.r.bookings_new : 0
                               detail: doneCol.r && doneCol.r.bookings_duplicates > 0 ? qsTr("%1 waren bereits vorhanden").arg(Fmt.count(doneCol.r.bookings_duplicates)) : "" }
                    CountRow { label: qsTr("Kategorien"); n: doneCol.r ? doneCol.r.categories_created : 0 }
                    CountRow { label: qsTr("Notizen"); n: doneCol.r ? doneCol.r.notes_imported : 0 }
                    CountRow { label: qsTr("Regeln"); n: doneCol.r ? doneCol.r.rules_created : 0
                               detail: doneCol.r && doneCol.r.rules_disabled > 0 ? qsTr("%1 davon ausgeschaltet").arg(doneCol.r.rules_disabled) : "" }
                    CountRow { label: qsTr("Kontogruppen"); n: doneCol.r ? doneCol.r.groups_created : 0 }
                    CountRow { label: qsTr("Kontoauszüge und Dokumente"); n: doneCol.r ? doneCol.r.documents_imported : 0 }
                    CountRow { label: qsTr("Depotpositionen"); n: doneCol.r ? doneCol.r.holdings_imported : 0 }
                    CountRow { label: qsTr("Überweisungsvorlagen"); n: doneCol.r ? doneCol.r.transfer_templates_created : 0
                               detail: doneCol.r && doneCol.r.transfer_templates_existing > 0 ? qsTr("%1 bereits vorhanden").arg(doneCol.r.transfer_templates_existing) : "" }
                    CountRow { label: qsTr("Bankzugänge"); n: doneCol.r ? doneCol.r.connections_created : 0
                               detail: doneCol.r && doneCol.r.connections_existing > 0 ? qsTr("%1 bereits vorhanden").arg(doneCol.r.connections_existing) : "" }
                }

                // What the import left out, each with its reason.
                FormSection {
                    visible: doneCol.r !== null && (doneCol.r.banks_skipped_names.length > 0 || doneCol.documentsSkipped > 0 || doneCol.r.pending_skipped > 0 || doneCol.r.transfer_templates_skipped > 0)
                    title: qsTr("Nicht übernommen")
                    footers: [doneCol.r && doneCol.r.banks_skipped_names.length > 0
                              ? qsTr("Für diese Banken hat GiroCrab noch keinen Zugang. Ihre Konten und Buchungen sind übernommen, werden aber nicht automatisch aktualisiert.") : ""]
                    Repeater {
                        model: doneCol.r ? doneCol.r.banks_skipped_names : []
                        RowLayout {
                            required property string modelData
                            Layout.fillWidth: true
                            SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.warning }
                            Text { text: modelData; color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true }
                            Text { text: qsTr("Kein Abruf"); color: Theme.secondary; font.pixelSize: Theme.body }
                        }
                    }
                    CountRow { label: qsTr("Dokumente ausgelassen"); detail: qsTr("Nicht gefunden, beschädigt oder größer als 25 MB"); n: doneCol.documentsSkipped }
                    CountRow { label: qsTr("Vorgemerkte Buchungen ausgelassen"); detail: qsTr("Die Bank liefert sie gebucht erneut"); n: doneCol.r ? doneCol.r.pending_skipped : 0 }
                    CountRow { label: qsTr("Überweisungsvorlagen ausgelassen"); detail: qsTr("Ohne Empfänger, ohne gültige IBAN oder nicht in Euro"); n: doneCol.r ? doneCol.r.transfer_templates_skipped : 0 }
                }
                CheckSection { view: v; visible: doneCol.problems.length === 0 && doneCol.r !== null && doneCol.r.account_checks.length > 0
                               checks: doneCol.orderedChecks; problems: 0 }
            }
        }
    }

    RowLayout {
        visible: v.error !== ""
        Layout.fillWidth: true
        spacing: 6
        SFIcon { symbol: "exclamationmark.triangle"; size: 14; color: Theme.warning; Layout.alignment: Qt.AlignTop }
        TextEdit { text: v.error; readOnly: true; selectByMouse: true; color: Theme.warning; font.pixelSize: Theme.callout; wrapMode: Text.WordWrap; Layout.fillWidth: true }
    }

    // Footer per step
    RowLayout {
        visible: v.step === "importing"
        Layout.fillWidth: true
        Item { Layout.fillWidth: true }
        // closing would leave the import running unseen: cancel it instead
        Button { text: v.cancelling ? qsTr("Wird abgebrochen …") : qsTr("Import abbrechen"); enabled: !v.cancelling; onClicked: v.cancelImport() }
        // .keyboardShortcut(.cancelAction): the sheet ignores Esc while importing, the button takes it
        Shortcut { sequences: ["Esc"]; enabled: v.visible && v.step === "importing" && !v.cancelling; onActivated: v.cancelImport() }
    }
    RowLayout {
        visible: v.step === "done"
        Layout.fillWidth: true
        Item { Layout.fillWidth: true }
        Button { text: qsTr("Fertig"); highlighted: true; onClicked: v.host.close() }
        // .keyboardShortcut(.defaultAction); Esc closes through the sheet (importRunning is false here)
        Shortcut { sequences: ["Return", "Enter"]; enabled: v.visible && v.step === "done"; onActivated: v.host.close() }
    }
    AssistantFooter {
        visible: v.step !== "importing" && v.step !== "done"
        showBack: v.step !== "source"
        nextTitle: v.isLastStep ? qsTr("Importieren") : qsTr("Fortfahren")
        nextDisabled: !v.canContinue
        busy: v.loading
        onCancel: { v.password = ""; v.host.close() }
        onBack: v.back()
        onNext: v.next()
        Button { visible: v.step === "enableBanking"; text: qsTr("Später einrichten"); onClicked: v.runImport() }
    }

    /// countRow: zero rows are left out.
    component CountRow: FormRow {
        property int n: 0
        visible: n > 0
        value: Fmt.count(n)
        valueColor: Theme.text
    }

    /// Reconciliation with MoneyMoney: deviations open, otherwise folded away.
    component CheckSection: FormSection {
        id: cs
        property var checks: []
        property int problems: 0
        property var view
        readonly property bool open: problems > 0 || cs.view.checksExpanded
        Layout.fillWidth: true
        title: qsTr("Abgleich mit den importierten Daten")
        footers: [problems > 0 ? qsTr("Bitte prüfen Sie die markierten Konten. Ein erneuter Import ergänzt fehlende Buchungen, ohne Duplikate anzulegen.") : ""]
        RowLayout {
            visible: cs.problems === 0
            Layout.fillWidth: true
            SFIcon { symbol: cs.view.checksExpanded ? "chevron.down" : "chevron.right"; size: 12 }
            Text { text: cs.checks.length === 1 ? qsTr("Das Konto stimmt überein") : qsTr("Alle %1 Konten stimmen überein").arg(cs.checks.length)
                   color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true }
            TapHandler { onTapped: cs.view.checksExpanded = !cs.view.checksExpanded }
        }
        RowLayout {
            visible: cs.open
            Layout.fillWidth: true
            spacing: 16
            Text { text: qsTr("Konto"); Layout.fillWidth: true; font.pixelSize: Theme.caption; color: Theme.secondary }
            Text { text: qsTr("Buchungen"); Layout.preferredWidth: 90; horizontalAlignment: Text.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary }
            Text { text: qsTr("Saldo laut Import"); Layout.preferredWidth: 120; horizontalAlignment: Text.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary }
            Text { text: qsTr("Saldo GiroCrab"); Layout.preferredWidth: 120; horizontalAlignment: Text.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary }
        }
        Repeater {
            model: cs.open ? cs.checks : []
            ColumnLayout {
                id: checkRow
                required property var modelData
                readonly property int found: modelData.found_bookings + modelData.deleted_bookings
                Layout.fillWidth: true
                spacing: 6
                Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 16
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 6
                        SFIcon { symbol: modelData.ok ? "checkmark.circle" : "exclamationmark.triangle.fill"; size: 14
                                 color: modelData.ok ? Theme.inflow : Theme.warning; Layout.alignment: Qt.AlignTop
                                 Accessible.name: modelData.ok ? qsTr("Stimmt") : qsTr("Weicht ab") }
                        ColumnLayout {
                            spacing: 1; Layout.fillWidth: true
                            Text { text: modelData.name; elide: Text.ElideRight; Layout.fillWidth: true; color: Theme.text; font.pixelSize: Theme.body }
                            Text { readonly property string note: cs.view.checkNote(modelData)
                                   visible: note !== ""; text: note; color: Theme.secondary; font.pixelSize: Theme.caption; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                        }
                    }
                    Text { text: checkRow.found + " / " + modelData.mm_bookings; Layout.preferredWidth: 90; horizontalAlignment: Text.AlignRight
                           color: checkRow.found === modelData.mm_bookings ? Theme.text : Theme.warning; font.pixelSize: Theme.body; font.features: { "tnum": 1 } }
                    Text { text: Fmt.money(modelData.mm_balance_cents, modelData.currency); Layout.preferredWidth: 120; horizontalAlignment: Text.AlignRight
                           color: Theme.text; font.pixelSize: Theme.body; font.features: { "tnum": 1 }; Sensitive { amounts: true } }
                    Text { text: Fmt.money(modelData.girocrab_balance_cents, modelData.currency); Layout.preferredWidth: 120; horizontalAlignment: Text.AlignRight
                           color: modelData.balance_from_bank || modelData.girocrab_balance_cents === modelData.mm_balance_cents ? Theme.text : Theme.warning
                           font.pixelSize: Theme.body; font.features: { "tnum": 1 }; Sensitive { amounts: true } }
                }
            }
        }
    }
}
