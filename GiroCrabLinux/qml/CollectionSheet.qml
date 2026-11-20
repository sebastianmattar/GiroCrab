import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts
import QtCore

// CollectionSheet.swift: preparing and submitting a collection (GitHub #143) – the lines of
// the active mandates (amount, purpose, „Letzter Einzug“), then the `pain.008` files to save
// or to hand to the bank via FinTS (one TAN per file). State: DirectDebits.qml.
Sheet {
    id: sheet
    property bool b2bConfirmed: false
    property string pin: ""
    /// The file being saved (`exporting`).
    property var exporting: null
    readonly property bool hasFiles: DirectDebits.collectionFiles.length > 0
    readonly property bool hasB2b: DirectDebits.collectionFiles.some(f => f.scheme === "B2b")
    readonly property var stats: DirectDebits.lineStats

    visible: DirectDebits.showCollectionSheet
    sheetWidth: 820
    padding: 0
    closePolicy: AppModel.transferBusy ? Popup.NoAutoClose : Popup.CloseOnEscape
    onAboutToShow: { b2bConfirmed = false; pin = "" }
    onClosed: { pin = ""; if (DirectDebits.showCollectionSheet) DirectDebits.showCollectionSheet = false }

    function today() { const d = new Date(); d.setHours(0, 0, 0, 0); return d }
    function build() { if (stats.count > 0 && stats.invalid === 0) DirectDebits.buildCollection(DirectDebits.collectionDue) }
    function submit() {
        if (hasB2b && !b2bConfirmed) return
        DirectDebits.submitCollection(DirectDebits.collectionDue, b2bConfirmed, pin)
        pin = ""
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        RowLayout {
            id: head
            Layout.fillWidth: true
            Layout.margins: 20
            spacing: 10
            Text { Layout.fillWidth: true; text: qsTr("Einzug vorbereiten"); font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text
                   Accessible.role: Accessible.Heading }
            Text { text: qsTr("Fälligkeit"); font.pixelSize: Theme.body; color: Theme.text }
            DateField {
                date: DirectDebits.collectionDue
                minimum: sheet.today()
                onPicked: d => { DirectDebits.collectionDue = d; DirectDebits.collectionFiles = [] }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        readonly property real bodyHeight: Math.max(240, Math.min(560 * Theme.textScale, sheet.maxBodyHeight - head.implicitHeight - 40 - buttons.implicitHeight - 28
                                                                   - (message.visible ? message.implicitHeight + 8 : 0)))
        // MARK: lines
        ListView {
            id: lines
            visible: !sheet.hasFiles
            Layout.fillWidth: true
            Layout.preferredHeight: parent.bodyHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            model: DirectDebits.collectionLines
            ScrollBar.vertical: AppScrollBar {}
            delegate: Item {
                id: line
                required property int index
                required property string name
                required property string reference
                required property string sequenceText
                required property string nextSequence
                required property bool included
                required property string amountText
                required property string purpose
                required property bool lastCollection
                readonly property bool invalid: { void DirectDebits.linesRevision; return included && DirectDebits.lineCents({ amountText: amountText }) === null }
                width: ListView.view.width
                height: row.implicitHeight + 12
                RowLayout {
                    id: row
                    anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
                    anchors.leftMargin: 20; anchors.rightMargin: 20
                    spacing: 10
                    CheckBox { checked: line.included; onToggled: DirectDebits.setLine(line.index, { included: checked }); Accessible.name: line.name }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Text { Layout.fillWidth: true; elide: Text.ElideRight; text: line.name; font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text; Sensitive {} }
                        Text { Layout.fillWidth: true; elide: Text.ElideRight; text: line.reference + " · " + line.sequenceText
                               font.pixelSize: Theme.caption; color: Theme.secondary; Sensitive {} }
                    }
                    CheckBox {
                        visible: line.nextSequence === "RCUR"
                        text: qsTr("Letzter Einzug")
                        checked: line.lastCollection
                        onToggled: DirectDebits.setLine(line.index, { lastCollection: checked })
                        ToolTip.visible: hovered; ToolTip.delay: 600
                        ToolTip.text: qsTr("Setzt den Folgetyp FNAL; danach wird das Mandat deaktiviert.")
                    }
                    TextField {
                        Layout.preferredWidth: 180 * Theme.textScale
                        placeholderText: qsTr("Zweck")
                        text: line.purpose
                        onTextEdited: DirectDebits.setLine(line.index, { purpose: text })
                        Sensitive {}
                    }
                    TextField {
                        Layout.preferredWidth: 90 * Theme.textScale
                        horizontalAlignment: TextInput.AlignRight
                        placeholderText: qsTr("Betrag")
                        font.features: { "tnum": 1 }
                        color: line.invalid ? Theme.negative : Theme.text
                        text: line.amountText
                        onTextEdited: DirectDebits.setLine(line.index, { amountText: text })
                        Sensitive { amounts: true }
                    }
                }
                Rectangle { anchors.bottom: parent.bottom; x: 20; width: parent.width - 40; height: 1; color: Theme.withAlpha(Theme.border, 0.7) }
            }
        }

        // MARK: files
        ScrollView {
            id: filesScroll
            visible: sheet.hasFiles
            Layout.fillWidth: true
            Layout.preferredHeight: parent.bodyHeight
            contentWidth: availableWidth
            clip: true
            ScrollBar.vertical: ScrollViewBar {}
            ColumnLayout {
                x: 20; y: 16; width: filesScroll.availableWidth - 40
                spacing: 18
                FormSection {
                    title: qsTr("Dateien")
                    Repeater {
                        model: DirectDebits.collectionFiles
                        RowLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: 10
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Text { text: modelData.file_name; font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text }
                                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                                       text: qsTr("%1 · %2 · %3 Lastschriften · %4").arg(DirectDebits.schemeTitle(modelData.scheme)).arg(modelData.sequence)
                                             .arg(modelData.count).arg(Fmt.money(modelData.total_cents))
                                       Sensitive { amounts: true } }
                            }
                            Button { text: qsTr("Sichern …"); icon.name: SF.icon("square.and.arrow.down")
                                     onClicked: { sheet.exporting = modelData; saveDialog.open() } }
                        }
                    }
                }
                FormSection {
                    visible: DirectDebits.canSubmit
                    title: qsTr("Bei der Bank einreichen")
                    CheckBox {
                        visible: sheet.hasB2b
                        Layout.fillWidth: true
                        text: qsTr("Die B2B-Mandate sind bei den Banken der Zahlungspflichtigen hinterlegt")
                        checked: sheet.b2bConfirmed
                        onToggled: sheet.b2bConfirmed = checked
                    }
                    FormRow {
                        visible: !DirectDebits.pinStored
                        label: qsTr("Banking-PIN")
                        TextField {
                            Layout.preferredWidth: 240
                            echoMode: TextInput.Password
                            placeholderText: qsTr("wird nicht gespeichert")
                            text: sheet.pin
                            onTextEdited: sheet.pin = text
                        }
                    }
                    Text {
                        Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                        text: qsTr("GiroCrab reicht jede Datei bei der Bank ein; du bestätigst jede mit einer TAN. Danach stehen die Einzüge unter „Geplant“ und die Mandate laufen als Folgeeinzug weiter. Die Bank prüft Mandate und Gläubiger-ID selbst.")
                    }
                }
                FormSection {
                    Text {
                        Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                        text: qsTr("Alternativ reichst du die Dateien selbst bei deiner Bank ein (Banking-Portal, Datei-Upload) und markierst die Mandate danach als eingezogen, damit der nächste Einzug als Folgeeinzug läuft.")
                    }
                }
                Item { implicitHeight: 4 }
            }
        }

        RowLayout {
            id: message
            visible: DirectDebits.error !== ""
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 8
            spacing: 6
            SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.negative; Layout.alignment: Qt.AlignTop }
            TextEdit { Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap
                       text: DirectDebits.error; color: Theme.negative; font.pixelSize: Theme.callout }
        }
        Rectangle { Layout.fillWidth: true; Layout.topMargin: 8; height: 1; color: Theme.border }

        RowLayout {
            id: buttons
            Layout.fillWidth: true
            Layout.margins: 14
            spacing: 8
            Text { visible: !sheet.hasFiles; text: qsTr("%1 Lastschriften · %2").arg(sheet.stats.count).arg(Fmt.money(sheet.stats.total))
                   font.pixelSize: Theme.callout; color: Theme.secondary; Sensitive { amounts: true } }
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Schließen"); onClicked: DirectDebits.showCollectionSheet = false }
            Button {
                visible: !sheet.hasFiles
                text: qsTr("Dateien erzeugen"); highlighted: true
                enabled: sheet.stats.count > 0 && sheet.stats.invalid === 0
                onClicked: sheet.build()
            }
            Button {
                visible: sheet.hasFiles
                text: qsTr("Als eingezogen markieren")
                onClicked: DirectDebits.markCollected(DirectDebits.collectionFiles, DirectDebits.collectionDue)
            }
            Button {
                visible: sheet.hasFiles && DirectDebits.canSubmit
                text: qsTr("Bei der Bank einreichen"); highlighted: true
                enabled: !(sheet.hasB2b && !sheet.b2bConfirmed) && !AppModel.transferBusy
                onClicked: sheet.submit()
            }
        }
    }

    // .keyboardShortcut(.defaultAction)
    Shortcut {
        sequences: ["Return", "Enter"]
        enabled: sheet.visible
        onActivated: { if (!sheet.hasFiles) sheet.build(); else if (DirectDebits.canSubmit) sheet.submit() }
    }

    // .fileExporter(document: XMLFileDocument(…), contentType: .xml, defaultFilename: exporting?.fileName)
    FileDialog {
        id: saveDialog
        title: qsTr("Sichern …")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "xml"
        nameFilters: [qsTr("XML-Datei (*.xml)")]
        currentFolder: StandardPaths.writableLocation(StandardPaths.DocumentsLocation)
        selectedFile: currentFolder + "/" + (sheet.exporting ? sheet.exporting.file_name : "lastschrift.xml")
        onAccepted: {
            const f = sheet.exporting
            sheet.exporting = null
            if (f) AppModel.coreAsync("write_text_file", { path: AppModel.localPath(selectedFile), text: f.xml }, null,
                                      e => { DirectDebits.error = AppModel.parseCoreError(e).message })
        }
        onRejected: sheet.exporting = null
    }
}
