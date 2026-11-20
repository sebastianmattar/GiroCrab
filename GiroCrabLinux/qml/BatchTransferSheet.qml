import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import QtCore

// BatchTransferSheet.swift: "Sammelüberweisung" (GitHub #240) – many payments from one
// account in one order (HKCCM) with one TAN. Table editor, CSV import (button or drop),
// then a read-only review. The bank's TAN request is shown by TwoFactorSheet; the state
// lives in Payments.qml (BatchDraft), the session runs through AppModel.runPaymentSession.
Sheet {
    id: sheet
    readonly property var draft: Payments.batchDraft
    property bool reviewing: false
    /// Table selection: { rowKey: true }.
    property var selection: ({})
    readonly property int selectedCount: Object.keys(selection).length
    property bool demoReviewShown: false

    readonly property var account: Payments.batchAccounts.find(a => a.id === draft.account_id) || null
    readonly property bool pinStored: {
        if (!account || account.connection_id === null) return false
        if (AppModel.isDemo) return true
        const st = AppModel.connectionStatus(account.connection_id)
        return st ? st.has_pin : false
    }
    readonly property bool canReview: account !== null && Payments.batchRows.count > 0 && Payments.batchInvalidCount === 0
    readonly property string bookingText: draft.singleBookings ? qsTr("Einzelbuchungen pro Zahlung") : qsTr("Eine Sammelbuchung")

    // column widths of the table (TableColumn … .width(ideal:))
    readonly property real handleWidth: 30
    readonly property real nameWidth: 170 * Theme.textScale
    readonly property real ibanWidth: 250 * Theme.textScale
    readonly property real amountWidth: 115 * Theme.textScale

    visible: Payments.showBatchSheet
    // A table of many rows: wider than the single form (minWidth 760, idealWidth 920).
    sheetWidth: 920
    padding: 0
    closePolicy: AppModel.transferBusy ? Popup.NoAutoClose : Popup.CloseOnEscape
    onAboutToShow: {
        reviewing = AppModel.demoSheet === "batch-transfer-confirm" && !demoReviewShown // screenshots
        demoReviewShown = true
        selection = ({})
    }
    onClosed: if (Payments.showBatchSheet) Payments.cancelBatchSheet()

    function removeSelected() {
        Payments.removeBatchRows(selection)
        selection = ({})
    }
    function select(key, extend) {
        const s = extend ? Object.assign({}, selection) : {}
        if (extend && s[key]) delete s[key]; else s[key] = true
        selection = s
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        // MARK: header
        RowLayout {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 14; Layout.bottomMargin: 14
            spacing: 12
            Text {
                text: sheet.reviewing ? qsTr("Sammelüberweisung prüfen") : qsTr("Neue Überweisung")
                font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text
                Accessible.role: Accessible.Heading
            }
            Item { Layout.fillWidth: true }
            Button {
                visible: !sheet.reviewing
                flat: true; icon.name: SF.icon("square.and.arrow.down"); text: qsTr("CSV importieren …")
                onClicked: csvDialog.open()
                ToolTip.text: qsTr("Spalten: Name, IBAN, Betrag, Verwendungszweck (Trennzeichen ; , oder Tab). Datei hierher ziehen.")
                ToolTip.visible: hovered; ToolTip.delay: 600
            }
        }
        // TransferKindPicker(batch: true)
        Segmented {
            id: kindPicker
            visible: !sheet.reviewing
            Layout.alignment: Qt.AlignHCenter
            Layout.bottomMargin: 10
            options: [{ id: "single", title: qsTr("Einzeln") }, { id: "batch", title: qsTr("Sammelüberweisung") }]
            current: "batch"
            onActivated: id => { if (id === "single") Payments.switchToSingleTransfer() }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // MARK: editor – the choices side by side (label above) instead of a tall form
        ColumnLayout {
            id: options
            visible: !sheet.reviewing
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 12; Layout.bottomMargin: 12
            spacing: 8
            // side by side when the sheet is wide enough, else two rows of two
            GridLayout {
                Layout.fillWidth: true
                columns: sheet.availableWidth > 1040 ? 4 : 2
                columnSpacing: 16; rowSpacing: 8
                Labeled {
                    title: qsTr("Von Konto")
                    Layout.fillWidth: true
                    AppComboBox {
                        Layout.fillWidth: true
                        model: Payments.batchAccounts.map(a => a.name + " · " + Fmt.iban(a.iban))
                        currentIndex: Payments.batchAccounts.findIndex(a => a.id === sheet.draft.account_id)
                        onActivated: i => Payments.setBatch({ account_id: Payments.batchAccounts[i].id })
                        Sensitive {}
                    }
                }
                Labeled {
                    title: qsTr("Buchung auf dem Kontoauszug")
                    Layout.preferredWidth: 200 * Theme.textScale
                    AppComboBox {
                        Layout.fillWidth: true
                        model: [qsTr("Eine Sammelbuchung"), qsTr("Einzelbuchungen pro Zahlung")]
                        currentIndex: sheet.draft.singleBookings ? 1 : 0
                        onActivated: i => Payments.setBatch({ singleBookings: i === 1 })
                    }
                }
                Labeled {
                    title: qsTr("Ausführung")
                    Layout.fillWidth: true
                    AppComboBox {
                        Layout.fillWidth: true
                        model: [qsTr("Normale Überweisung"), qsTr("Terminüberweisung")]
                        currentIndex: sheet.draft.scheduled ? 1 : 0
                        onActivated: i => Payments.setBatch({ scheduled: i === 1 })
                    }
                }
                // Always there (disabled unless scheduled): the layout does not jump.
                Labeled {
                    title: qsTr("Ausführungsdatum")
                    DateField {
                        enabled: sheet.draft.scheduled
                        date: sheet.draft.executionDate
                        minimum: Payments.tomorrow()
                        onPicked: d => Payments.setBatch({ executionDate: d })
                    }
                }
            }
            RowLayout {
                visible: sheet.draft.importNote !== ""
                spacing: 6
                SFIcon { symbol: "doc.text"; size: 13; color: Theme.secondary }
                Text { text: sheet.draft.importNote; font.pixelSize: Theme.callout; color: Theme.secondary; Sensitive {} }
            }
        }

        // MARK: review – one compact band: the total, then what and from where
        RowLayout {
            id: summary
            visible: sheet.reviewing
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 12; Layout.bottomMargin: 12
            spacing: 20
            ColumnLayout {
                spacing: 2
                Text { text: qsTr("Summe"); font.pixelSize: Theme.caption; color: Theme.secondary }
                Text { text: Fmt.money(Payments.batchTotalCents); font.pixelSize: Theme.title; font.weight: Font.DemiBold
                       font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
            }
            Rectangle { width: 1; Layout.preferredHeight: 40; color: Theme.border }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3
                RowLayout {
                    spacing: 0
                    Text { text: qsTr("%1 Zahlungen").arg(Payments.batchRows.count) + " · "; font.pixelSize: Theme.callout; color: Theme.text }
                    Text { text: sheet.account ? sheet.account.name + " · " + Fmt.iban(sheet.account.iban) : ""
                           font.pixelSize: Theme.callout; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                }
                Text {
                    Layout.fillWidth: true; elide: Text.ElideRight
                    text: sheet.bookingText + " · " + (sheet.draft.scheduled ? qsTr("Terminüberweisung") : qsTr("Heute (SEPA-Überweisung)"))
                    font.pixelSize: Theme.callout; color: Theme.secondary
                }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // MARK: table (editable in the editor, read-only in the review)
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: columns.implicitHeight + 8
            color: Theme.withAlpha(Theme.secondary, 0.06)
            RowLayout {
                id: columns
                anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
                anchors.leftMargin: 12; anchors.rightMargin: 20
                spacing: 8
                Item { Layout.preferredWidth: sheet.handleWidth; visible: !sheet.reviewing }
                ColumnTitle { text: qsTr("Name"); Layout.preferredWidth: sheet.nameWidth }
                ColumnTitle { text: qsTr("IBAN"); Layout.preferredWidth: sheet.ibanWidth }
                ColumnTitle { text: qsTr("Betrag"); Layout.preferredWidth: sheet.amountWidth; horizontalAlignment: Text.AlignRight }
                ColumnTitle { text: qsTr("Verwendungszweck"); Layout.fillWidth: true }
            }
        }
        ListView {
            id: table
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 28 + (kindPicker.visible ? kindPicker.implicitHeight + 10 : 0)
                + (sheet.reviewing ? summary.implicitHeight + 24 + reviewFooter.implicitHeight : options.implicitHeight + 24 + editorFooter.implicitHeight + 20)
                + columns.implicitHeight + 8 + messages.implicitHeight + buttonRow.implicitHeight + 34
            Layout.preferredHeight: Math.max(160, Math.min(680 * Theme.textScale, sheet.maxBodyHeight) - chrome)
            clip: true
            model: Payments.batchRows
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.vertical: AppScrollBar {}
            delegate: Rectangle {
                id: rowItem
                required property int index
                required property var model
                readonly property bool selected: !sheet.reviewing && !!sheet.selection[model.rowKey]
                width: table.width
                implicitHeight: cells.implicitHeight + 6
                color: selected ? Theme.selection : (index % 2 ? Theme.withAlpha(Theme.secondary, 0.04) : "transparent")
                RowLayout {
                    id: cells
                    anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
                    anchors.leftMargin: 12; anchors.rightMargin: 20
                    spacing: 8
                    // row handle: click selects (Strg/Umschalt adds), like a table row
                    Text {
                        visible: !sheet.reviewing
                        Layout.preferredWidth: sheet.handleWidth
                        text: rowItem.index + 1
                        horizontalAlignment: Text.AlignRight
                        font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.tertiary
                        MouseArea {
                            anchors.fill: parent; anchors.margins: -4
                            onClicked: mouse => sheet.select(rowItem.model.rowKey, (mouse.modifiers & (Qt.ControlModifier | Qt.ShiftModifier)) !== 0)
                        }
                    }
                    // editor cells
                    Cell {
                        visible: !sheet.reviewing
                        Layout.preferredWidth: sheet.nameWidth
                        text: rowItem.model.name; placeholderText: qsTr("Name")
                        onTextEdited: Payments.updateBatchRow(rowItem.index, { name: text })
                        onActiveFocusChanged: if (activeFocus && !rowItem.selected) sheet.select(rowItem.model.rowKey, false)
                    }
                    RowLayout {
                        visible: !sheet.reviewing
                        Layout.preferredWidth: sheet.ibanWidth
                        Layout.fillWidth: false
                        spacing: 4
                        Cell {
                            Layout.fillWidth: true
                            text: rowItem.model.iban; placeholderText: qsTr("DE00 …")
                            font.family: "monospace"
                            onTextEdited: Payments.updateBatchRow(rowItem.index, { iban: text })
                            onActiveFocusChanged: if (activeFocus && !rowItem.selected) sheet.select(rowItem.model.rowKey, false)
                        }
                        Mark {
                            visible: rowItem.model.iban !== "" && !rowItem.model.ibanValid
                            symbol: "xmark.circle.fill"; tip: qsTr("IBAN ungültig (Prüfsumme/Länge)")
                        }
                    }
                    RowLayout {
                        visible: !sheet.reviewing
                        Layout.preferredWidth: sheet.amountWidth
                        Layout.fillWidth: false
                        spacing: 4
                        Mark { visible: rowItem.model.cents <= 0; symbol: "exclamationmark.triangle.fill"; tip: qsTr("Betrag > 0 eingeben") }
                        Cell {
                            Layout.fillWidth: true
                            text: rowItem.model.amountText; placeholderText: "0,00"; amounts: true
                            horizontalAlignment: TextInput.AlignRight
                            font.features: { "tnum": 1 }
                            onTextEdited: Payments.updateBatchRow(rowItem.index, { amountText: text })
                            onActiveFocusChanged: if (activeFocus && !rowItem.selected) sheet.select(rowItem.model.rowKey, false)
                        }
                    }
                    Cell {
                        visible: !sheet.reviewing
                        Layout.fillWidth: true
                        text: rowItem.model.purpose; placeholderText: qsTr("Verwendungszweck")
                        maximumLength: 140
                        onTextEdited: Payments.updateBatchRow(rowItem.index, { purpose: text })
                        onActiveFocusChanged: if (activeFocus && !rowItem.selected) sheet.select(rowItem.model.rowKey, false)
                    }
                    // review cells
                    Value { visible: sheet.reviewing; Layout.preferredWidth: sheet.nameWidth; text: rowItem.model.name }
                    Value { visible: sheet.reviewing; Layout.preferredWidth: sheet.ibanWidth; text: rowItem.model.iban; font.family: "monospace" }
                    Value { visible: sheet.reviewing; Layout.preferredWidth: sheet.amountWidth; text: Fmt.money(rowItem.model.cents); amounts: true
                            horizontalAlignment: Text.AlignRight; font.features: { "tnum": 1 } }
                    Value { visible: sheet.reviewing; Layout.fillWidth: true; text: rowItem.model.purpose }
                }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // editor footer: add / remove, faulty rows, count and sum
        RowLayout {
            id: editorFooter
            visible: !sheet.reviewing
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 10; Layout.bottomMargin: 10
            spacing: 8
            IconButton {
                symbol: "plus"
                onClicked: { Payments.addBatchRow(); Qt.callLater(() => table.positionViewAtEnd()) }
                ToolTip.text: qsTr("Zeile hinzufügen"); ToolTip.visible: hovered
                Accessible.name: qsTr("Zeile hinzufügen")
            }
            IconButton {
                symbol: "minus"
                enabled: sheet.selectedCount > 0
                onClicked: sheet.removeSelected()
                ToolTip.text: qsTr("Ausgewählte Zeilen entfernen (Strg+Z macht es rückgängig)"); ToolTip.visible: hovered
                Accessible.name: qsTr("Ausgewählte Zeilen entfernen")
            }
            Item { Layout.fillWidth: true }
            RowLayout {
                visible: Payments.batchInvalidCount > 0
                spacing: 4
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 13; color: Theme.negative }
                Text { text: qsTr("%1 fehlerhaft").arg(Payments.batchInvalidCount); font.pixelSize: Theme.callout; color: Theme.negative }
            }
            Text { text: qsTr("%1 Zahlungen · %2").arg(Payments.batchRows.count).arg(qsTr("Summe")); font.pixelSize: Theme.callout; color: Theme.secondary }
            Text { text: Fmt.money(Payments.batchTotalCents); font.pixelSize: Theme.title3; font.weight: Font.DemiBold
                   font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
        }

        // review footer: PIN, then the warning
        ColumnLayout {
            id: reviewFooter
            visible: sheet.reviewing
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 10; Layout.bottomMargin: 10
            spacing: 10
            TextField {
                visible: !sheet.pinStored
                Layout.preferredWidth: 360 * Theme.textScale
                echoMode: TextInput.Password
                placeholderText: qsTr("Banking-PIN") + " (" + qsTr("wird nicht gespeichert") + ")"
                text: sheet.draft.pin
                onTextEdited: Payments.setBatch({ pin: text })
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: 6
                SFIcon { symbol: "exclamationmark.shield"; size: 14; color: Theme.secondary; Layout.alignment: Qt.AlignTop }
                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                       text: qsTr("Bitte Empfänger und IBAN genau prüfen. Eine freigegebene Überweisung lässt sich meist nicht mehr zurückholen.") }
            }
        }

        RowLayout {
            id: messages
            visible: AppModel.transferError !== ""
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 8; Layout.bottomMargin: 8
            spacing: 6
            SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.negative; Layout.alignment: Qt.AlignTop }
            TextEdit { Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap
                       text: AppModel.transferError; color: Theme.negative; font.pixelSize: Theme.callout }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // MARK: buttons
        RowLayout {
            id: buttonRow
            Layout.fillWidth: true
            Layout.margins: 14
            spacing: 8
            Button { text: qsTr("Abbrechen"); onClicked: Payments.cancelBatchSheet() }
            Item { Layout.fillWidth: true }
            Button { visible: sheet.reviewing; text: qsTr("Zurück"); onClicked: { sheet.reviewing = false; AppModel.transferError = "" } }
            Button {
                visible: sheet.reviewing
                text: qsTr("Jetzt mit TAN freigeben"); highlighted: true
                enabled: !AppModel.transferBusy
                onClicked: Payments.startBatch()
            }
            Button {
                id: nextButton
                visible: !sheet.reviewing
                text: qsTr("Weiter"); highlighted: true
                enabled: sheet.canReview
                onClicked: { AppModel.transferError = ""; sheet.reviewing = true }
            }
        }
    }

    // .keyboardShortcut(.defaultAction)
    Shortcut {
        sequences: ["Return", "Enter"]
        enabled: sheet.visible && !csvDialog.visible
        onActivated: {
            if (!sheet.reviewing) { if (nextButton.enabled) nextButton.clicked() }
            else if (!AppModel.transferBusy) Payments.startBatch()
        }
    }
    // .onDeleteCommand: outside a text field (inside one, Entf/Rücktaste edit the text)
    Shortcut { sequences: [StandardKey.Delete, "Backspace"]; enabled: sheet.visible && !sheet.reviewing && sheet.selectedCount > 0; onActivated: sheet.removeSelected() }
    // Bearbeiten › Widerrufen („Zeilen entfernen“); inside a text field it undoes the typing
    Shortcut { sequences: [StandardKey.Undo]; enabled: sheet.visible && !sheet.reviewing && Payments.batchUndo.length > 0; onActivated: Payments.undoBatchRemoval() }

    // .fileImporter(allowedContentTypes: [.commaSeparatedText, .plainText, .text])
    FileDialog {
        id: csvDialog
        title: qsTr("CSV importieren …")
        currentFolder: StandardPaths.writableLocation(StandardPaths.DownloadLocation)
        nameFilters: [qsTr("CSV- und Textdateien (*.csv *.txt *.tsv)"), qsTr("Alle Dateien (*)")]
        onAccepted: Payments.importBatchCSV(selectedFile)
    }
    // .onDrop(of: [.fileURL])
    DropArea {
        parent: sheet.contentItem
        anchors.fill: parent
        enabled: !sheet.reviewing
        onDropped: drop => { if (drop.hasUrls) Payments.importBatchCSV(drop.urls[0]) }
        Rectangle {
            anchors.fill: parent; anchors.margins: 4
            visible: parent.containsDrag
            radius: 12; color: Theme.withAlpha(Theme.accent, 0.06)
            border.color: Theme.accent; border.width: 2
        }
    }

    component Labeled: ColumnLayout {
        property string title
        default property alias content: holder.data
        spacing: 4
        Text { text: parent.title; font.pixelSize: Theme.caption; color: Theme.secondary }
        ColumnLayout { id: holder; Layout.fillWidth: true; spacing: 0 }
    }
    component ColumnTitle: Text {
        font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary
        elide: Text.ElideRight
    }
    component Cell: TextField {
        /// the amount column: `.sensitive(.amounts)`
        property bool amounts: false
        font.pixelSize: Theme.body
        selectByMouse: true
        // a long purpose shows its start, not its end, while not being edited
        Component.onCompleted: cursorPosition = 0
        onEditingFinished: cursorPosition = 0
        Sensitive { amounts: parent.amounts }
    }
    component Value: Text {
        property bool amounts: false
        font.pixelSize: Theme.body; color: Theme.text
        elide: Text.ElideRight
        Sensitive { amounts: parent.amounts }
    }
    component Mark: SFIcon {
        property string tip
        size: 14
        color: Theme.negative
        HoverHandler { id: hover }
        ToolTip.visible: hover.hovered
        ToolTip.text: tip
        Accessible.name: tip
    }
}
