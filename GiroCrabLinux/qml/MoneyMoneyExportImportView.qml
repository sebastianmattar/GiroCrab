import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts

// MoneyMoneyImportSheet.swift › MoneyMoneyExportImportView: per-account
// CSV/XML/CAMT/MT940 export (one account at a time) as a two-step assistant:
// 1. the file, 2. target account and what to take over.
ColumnLayout {
    id: v
    /// The MoneyMoneyImportSheet: close(), source, importRunning.
    property var host
    spacing: 16

    property string step: "file"          // file | options
    property string filePath: ""
    property string fileName: ""
    /// null = "Neues Konto aus der Datei"
    property var target: null
    /// The core's account suggestion is applied once per file; afterwards the
    /// user's choice (including "Neues Konto") stays.
    property bool suggestionApplied: false
    property var preview: null            // MoneyMoneyImportPreview
    property string error: ""
    property bool loading: false
    /// Bumped per preview request: only the newest answer (result or error) is shown.
    property int previewGeneration: 0
    property bool importing: false
    property bool keepCategories: true
    property bool keepNotes: true
    readonly property bool isDemo: AppModel.demoImportPreview !== null

    onImportingChanged: if (host) host.importRunning = importing

    function reset() {
        step = "file"; filePath = ""; fileName = ""; target = null; suggestionApplied = false
        preview = null; error = ""; loading = false; importing = false; keepCategories = true; keepNotes = true
        if (AppModel.demoImportPreview) {
            preview = AppModel.demoImportPreview
            fileName = "Umsaetze.xml"
            filePath = "Umsaetze.xml"
            // `--demo-sheet moneymoney` shows the "Übernahme" step (preview + target account)
            step = "options"
        }
    }

    readonly property string subtitle: {
        if (step === "file") return qsTr("Wählen Sie, woher die Daten kommen. Die Originaldaten bleiben unverändert.")
        if (!preview) return fileName
        return [fileName, qsTr("%1 neue Buchungen").arg(Fmt.count(preview.new_bookings)), Fmt.yearSpan(preview)]
            .filter(x => !!x).join(" · ")
    }


    // MARK: - Actions

    function load(url) {
        const path = decodeURIComponent(url.toString().replace(/^file:\/\//, ""))
        filePath = path
        fileName = path.split("/").pop()
        suggestionApplied = false
        error = ""
        target = null
        refreshPreview()
    }

    function refreshPreview() {
        if (filePath === "" || isDemo) return
        loading = true; error = ""
        const generation = ++previewGeneration
        AppModel.coreAsync("preview_moneymoney_file", { path: filePath, target_account_id: target }, p => {
            if (generation !== previewGeneration) return   // superseded by a newer file or choice
            loading = false
            if (!suggestionApplied) {
                suggestionApplied = true
                // preselect the account the file belongs to
                const s = p.suggested_account_id
                if (target === null && s !== null && AppModel.allAccounts.some(a => a.id === s)) {
                    target = s
                    refreshPreview()
                    return
                }
            }
            preview = p
        }, err => {
            if (generation !== previewGeneration) return   // a stale error must not hide the newer preview
            loading = false
            preview = null
            const e = AppModel.parseCoreError(err)
            error = e.kind === "InvalidInput"
                ? qsTr("In dieser Datei wurden keine Umsätze erkannt (erwartet: CSV mit Datum/Betrag, XML <transactions>, camt.053 oder MT940).")
                : e.message
        })
    }

    function runImport() {
        if (filePath === "") return
        importing = true; error = ""
        AppModel.coreAsync("import_moneymoney_file", {
            path: filePath, target_account_id: target,
            preserve_categories: keepCategories, preserve_notes: keepNotes
        }, r => {
            importing = false
            AppModel.finishMoneyMoneyImport(r)
            host.close()
        }, err => {
            importing = false
            error = AppModel.parseCoreError(err).message
        })
    }

    FileDialog {
        id: picker
        title: qsTr("Exportdatei auswählen")
        nameFilters: [qsTr("Exportdatei (*.csv *.xml *.sta *.mt940 *.txt)"), qsTr("Alle Dateien (*)")]
        onAccepted: v.load(selectedFile)
    }

    ImportAssistantHeader {
        title: v.step === "file" ? qsTr("Vorhandene Daten importieren") : qsTr("Übernahme")
        subtitle: v.subtitle
    }

    StackLayout {
        Layout.fillWidth: true
        Layout.preferredHeight: 470
        currentIndex: v.step === "file" ? 0 : 1

        // MARK: - 1. File
        Item {
            ColumnLayout {
                anchors.left: parent.left; anchors.right: parent.right
                spacing: 18
                ImportSourceSection {
                    source: v.host ? v.host.source : "export"
                    enabled: !v.importing
                    onChosen: s => v.host.source = s
                }
                FormSection {
                    title: qsTr("Datei")
                    footers: [qsTr("Erwartet wird eine Exportdatei mit den Umsätzen eines Kontos (CSV, XML, camt.053 oder MT940). Kategorien werden übernommen, sofern die Datei sie enthält. Sie können die Datei auch hierher ziehen.")]
                    ImportFileRow {
                        label: qsTr("Exportdatei")
                        path: v.filePath
                        name: v.fileName
                        loading: v.loading
                        onChoose: picker.open()
                    }
                }
            }
            // the whole step takes a dropped file; outlined while one hovers
            DropArea {
                id: drop
                anchors.fill: parent
                onDropped: d => { if (d.hasUrls && d.urls.length) { v.load(d.urls[0]); d.accept() } }
            }
            Rectangle {
                anchors.fill: parent
                color: "transparent"; radius: 10
                border.color: Theme.accent; border.width: 2
                visible: drop.containsDrag
            }
        }

        // MARK: - 2. Options
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            contentWidth: availableWidth
            enabled: !v.importing
            ColumnLayout {
                width: parent.width
                spacing: 18
                FormSection {
                    footers: [v.target === null && v.preview
                              ? qsTr("Neues Archivkonto „%1“ – der Saldo ist die Summe der Umsätze.").arg(v.preview.suggested_account_name)
                              : (v.preview && v.target !== null && v.target === v.preview.suggested_account_id
                                 ? qsTr("Vorgewählt, weil die Datei zu diesem Konto passt. Bereits vorhandene Buchungen werden nicht doppelt übernommen.") : "")]
                    FormRow {
                        label: qsTr("Zielkonto")
                        AppComboBox {
                            id: targetBox
                            Layout.preferredWidth: 320
                            textRole: "text"
                            model: [{ id: null, text: qsTr("Neues Konto aus der Datei") }]
                                .concat(AppModel.allAccounts.map(a => ({ id: a.id, text: a.name + " · " + a.bank_name })))
                            currentIndex: Math.max(0, model.findIndex(o => o.id === v.target))
                            onActivated: i => { v.target = model[i].id; v.refreshPreview() }
                        }
                    }
                    FormRow { label: qsTr("Kategorien übernehmen"); Switch { checked: v.keepCategories; onToggled: { v.keepCategories = checked; v.refreshPreview() } } }
                    FormRow { label: qsTr("Notizen übernehmen"); Switch { checked: v.keepNotes; onToggled: v.keepNotes = checked } }
                }
                FormSection {
                    visible: v.preview !== null
                    title: qsTr("Vorschau")
                    footers: [v.preview && v.preview.duplicates_skipped_or_enriched > 0
                              ? qsTr("Vorhandene Buchungen werden nicht doppelt angelegt, sie erhalten nur Kategorie und Notiz.") : ""]
                    FormRow { label: qsTr("Neue Buchungen"); value: v.preview ? Fmt.count(v.preview.new_bookings) : "" }
                    FormRow { visible: v.preview && v.preview.duplicates_skipped_or_enriched > 0
                              label: qsTr("Bereits vorhanden"); value: v.preview ? Fmt.count(v.preview.duplicates_skipped_or_enriched) : "" }
                    FormRow { label: qsTr("Notizen"); value: v.preview ? Fmt.count(v.preview.notes_count) : "" }
                    FormRow {
                        visible: v.preview && v.preview.detected_categories.length > 0
                        label: qsTr("Kategorien")
                        Text {
                            Layout.maximumWidth: 460
                            horizontalAlignment: Text.AlignRight; wrapMode: Text.WordWrap; maximumLineCount: 2; elide: Text.ElideRight
                            color: Theme.secondary; font.pixelSize: Theme.body
                            text: v.preview ? v.preview.detected_categories.slice(0, 6).join(", ") + (v.preview.detected_categories.length > 6 ? " …" : "") : ""
                        }
                    }
                }
            }
        }
    }

    RowLayout {
        visible: v.error !== ""
        Layout.fillWidth: true
        spacing: 6
        SFIcon { symbol: "exclamationmark.triangle"; size: 14; color: Theme.warning; Layout.alignment: Qt.AlignTop }
        Text { text: v.error; color: Theme.warning; font.pixelSize: Theme.callout; wrapMode: Text.WordWrap; Layout.fillWidth: true }
    }

    AssistantFooter {
        cancelDisabled: v.importing
        showBack: v.step === "options"
        backDisabled: v.importing
        nextTitle: v.step === "file" ? qsTr("Fortfahren") : (v.importing ? qsTr("Importiere …") : qsTr("Importieren"))
        nextDisabled: v.step === "file" ? (v.preview === null || v.loading) : (v.importing || !v.preview || v.preview.total_rows === 0)
        busy: v.importing
        onCancel: v.host.close()
        onBack: { v.error = ""; v.step = "file" }
        onNext: { if (v.step === "file") { v.error = ""; v.step = "options" } else v.runImport() }
    }
}
