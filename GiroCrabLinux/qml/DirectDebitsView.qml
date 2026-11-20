import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts
import QtCore

// DirectDebitsView.swift: „Lastschriften“ (GitHub #143) – mandates of a creditor account, the
// creditor ID and the preparation of a collection (`pain.008` files). State: DirectDebits.qml;
// sheets: MandateEditorSheet.qml, CollectionSheet.qml; inspector: MandateInspector.qml.
Rectangle {
    id: view
    color: Theme.canvas
    property string creditorText: ""

    Component.onCompleted: { DirectDebits.active = true; DirectDebits.reload(); creditorText = DirectDebits.creditorID }
    Component.onDestruction: DirectDebits.active = false
    Connections {
        target: DirectDebits
        // `.task(id:)` { … creditorText = model.creditorID }
        function onReloaded() { view.creditorText = DirectDebits.creditorID }
        // onChange(of: selectedMandateID): the list takes the focus and scrolls to the row
        function onSelectedIDChanged() {
            if (DirectDebits.selectedID === null) return
            list.forceActiveFocus()
            const i = DirectDebits.mandates.findIndex(m => m.id === DirectDebits.selectedID)
            if (i >= 0) list.positionViewAtIndex(i, ListView.Contain)
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.topMargin: 16
        spacing: 12

        ColumnLayout {
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 4
            Text { text: qsTr("Lastschriften"); font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Mandate verwalten und Einzüge als SEPA-Lastschrift-Datei (pain.008) vorbereiten. Dateien lassen sich hierher ziehen.") }
        }

        // ContentUnavailableView „Kein Konto zum Einziehen“
        Unavailable {
            visible: DirectDebits.debitAccounts.length === 0
            symbol: "building.columns"
            title: qsTr("Kein Konto zum Einziehen")
            detail: qsTr("Lastschriften ziehst du von einem Girokonto mit IBAN ein.")
        }

        // MARK: controls
        RowLayout {
            visible: DirectDebits.debitAccounts.length > 0
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 12
            // Picker("Gläubigerkonto")
            AppComboBox {
                Layout.minimumWidth: 150; Layout.maximumWidth: 260; Layout.preferredWidth: 220
                model: DirectDebits.debitAccounts.map(a => a.name)
                currentIndex: DirectDebits.account ? DirectDebits.debitAccounts.findIndex(a => a.id === DirectDebits.account.id) : -1
                onActivated: i => DirectDebits.setAccount(DirectDebits.debitAccounts[i].id)
                Accessible.name: qsTr("Gläubigerkonto")
                Sensitive { labels: true }
            }
            // Spacer + buttons, right-aligned; Linux: they wrap into a second line when the
            // column is narrow (listed right to left, so they read in the Mac's order)
            Flow {
                Layout.fillWidth: true
                Layout.minimumWidth: 0; Layout.preferredWidth: 0
                layoutDirection: Qt.RightToLeft
                spacing: 8
                Item {
                    width: collectButton.implicitWidth; height: collectButton.implicitHeight
                    Button {
                        id: collectButton
                        anchors.fill: parent
                        text: qsTr("Einzug vorbereiten …"); icon.name: SF.icon("arrow.down.doc"); highlighted: true
                        enabled: DirectDebits.activeCount > 0 && DirectDebits.creditorID !== ""
                        onClicked: DirectDebits.openCollection()
                    }
                    // .help on a disabled button: the wrapper takes the hover
                    HoverHandler { id: collectHover }
                    ToolTip.visible: collectHover.hovered && DirectDebits.creditorID === ""
                    ToolTip.delay: 600
                    ToolTip.text: qsTr("Zuerst die Gläubiger-ID eintragen.")
                }
                Button { text: qsTr("Mandat hinzufügen"); icon.name: SF.icon("plus"); onClicked: DirectDebits.newMandate() }
                Button {
                    text: qsTr("Exportieren …"); icon.name: SF.icon("square.and.arrow.up")
                    enabled: DirectDebits.mandates.length > 0
                    onClicked: exportDialog.open()
                }
                Button {
                    text: qsTr("Importieren …"); icon.name: SF.icon("square.and.arrow.down")
                    onClicked: importDialog.open()
                    ToolTip.visible: hovered; ToolTip.delay: 600
                    ToolTip.text: qsTr("CSV-Mitgliederliste oder SEPA-Lastschriftdatei (pain.008)")
                }
            }
        }
        RowLayout {
            visible: DirectDebits.debitAccounts.length > 0
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 8
            Text { text: qsTr("Gläubiger-ID"); font.pixelSize: Theme.callout; color: Theme.secondary }
            TextField {
                id: creditorField
                Layout.preferredWidth: 280
                placeholderText: qsTr("z. B. DE98ZZZ09999999999")
                font.pixelSize: Theme.callout; font.features: { "tnum": 1 }
                text: view.creditorText
                onTextEdited: view.creditorText = text
                onAccepted: DirectDebits.saveCreditorID(view.creditorText)
                Sensitive {}
            }
            Button {
                text: qsTr("Speichern")
                enabled: view.creditorText.trim() !== DirectDebits.creditorID
                onClicked: DirectDebits.saveCreditorID(view.creditorText)
            }
            Button {
                visible: DirectDebits.creditorID === "" && DirectDebits.suggestedCreditorID !== "" && view.creditorText === ""
                text: qsTr("„%1“ übernehmen").arg(DirectDebits.suggestedCreditorID)
                onClicked: { view.creditorText = DirectDebits.suggestedCreditorID; DirectDebits.saveCreditorID(view.creditorText) }
                Sensitive {}
            }
            Item { Layout.fillWidth: true }
        }
        // error / note
        RowLayout {
            visible: DirectDebits.debitAccounts.length > 0 && (DirectDebits.error !== "" || DirectDebits.note !== "")
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 6
            SFIcon { Layout.alignment: Qt.AlignTop; size: 14
                     symbol: DirectDebits.error !== "" ? "exclamationmark.triangle.fill" : "checkmark.circle"
                     color: DirectDebits.error !== "" ? Theme.negative : Theme.secondary }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout
                   text: DirectDebits.error !== "" ? DirectDebits.error : DirectDebits.note
                   color: DirectDebits.error !== "" ? Theme.negative : Theme.secondary }
        }

        // ContentUnavailableView „Keine Mandate“ with actions
        Item {
            visible: DirectDebits.debitAccounts.length > 0 && DirectDebits.mandates.length === 0
            Layout.fillWidth: true; Layout.fillHeight: true
            ColumnLayout {
                anchors.centerIn: parent
                width: Math.min(parent.width - 40, 420)
                spacing: 8
                SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "doc.text"; size: 36; color: Theme.tertiary }
                Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Keine Mandate"); font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text }
                Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap; font.pixelSize: Theme.body; color: Theme.secondary
                       text: qsTr("Lege ein Mandat an oder importiere eine Mitgliederliste (CSV) oder eine SEPA-Lastschriftdatei.") }
                RowLayout {
                    Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 4
                    Button { text: qsTr("Mandat hinzufügen"); onClicked: DirectDebits.newMandate() }
                    Button { text: qsTr("Importieren …"); onClicked: importDialog.open() }
                }
            }
        }

        // MARK: list
        ListView {
            id: list
            visible: DirectDebits.debitAccounts.length > 0 && DirectDebits.mandates.length > 0
            Layout.fillWidth: true; Layout.fillHeight: true
            clip: true
            focus: true
            activeFocusOnTab: true
            boundsBehavior: Flickable.StopAtBounds
            model: DirectDebits.mandates
            bottomMargin: 18
            ScrollBar.vertical: AppScrollBar {}
            Keys.onDownPressed: DirectDebits.moveSelection(1)
            Keys.onUpPressed: DirectDebits.moveSelection(-1)
            Keys.onEscapePressed: e => { if (DirectDebits.selectedID === null) { e.accepted = false; return } DirectDebits.selectedID = null }
            Component.onCompleted: if (!AppModel.screenshotPath) forceActiveFocus()
            header: Item {
                width: ListView.view.width
                height: 40
                Item {
                    x: 20; width: parent.width - 40; height: 40
                    CardSegment { anchors.fill: parent; isTop: true }
                    Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: 16; anchors.rightMargin: 16
                        Text { Layout.fillWidth: true; text: qsTr("Mandate"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text }
                        Text { text: qsTr("%1 aktiv, %2 inaktiv").arg(DirectDebits.activeCount).arg(DirectDebits.mandates.length - DirectDebits.activeCount)
                               font.pixelSize: Theme.subheadline + 1; color: Theme.secondary }
                    }
                }
            }
            delegate: Item {
                id: slot
                required property var modelData
                required property int index
                width: ListView.view.width
                height: mrow.height
                MandateRow {
                    id: mrow
                    x: 20; width: parent.width - 40
                    mandate: slot.modelData
                    isLast: slot.index === DirectDebits.mandates.length - 1
                }
            }
        }
    }

    // .onDrop(of: [.fileURL])
    DropArea {
        id: drop
        anchors.fill: parent
        enabled: DirectDebits.account !== null
        onDropped: d => { if (d.hasUrls) { d.accept(); DirectDebits.importFile(d.urls[0]) } }
    }
    Rectangle { anchors.fill: parent; anchors.margins: 8; visible: drop.containsDrag; color: "transparent"; radius: 14; border.color: Theme.accent; border.width: 2 }

    // .fileImporter(allowedContentTypes: [.commaSeparatedText, .plainText, .text, .xml])
    FileDialog {
        id: importDialog
        title: qsTr("Importieren …")
        currentFolder: StandardPaths.writableLocation(StandardPaths.DownloadLocation)
        nameFilters: [qsTr("CSV-, Text- und XML-Dateien (*.csv *.txt *.tsv *.xml)"), qsTr("Alle Dateien (*)")]
        onAccepted: DirectDebits.importFile(selectedFile)
    }
    // .fileExporter(… defaultFilename: "mandate.csv")
    FileDialog {
        id: exportDialog
        title: qsTr("Exportieren …")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "csv"
        nameFilters: [qsTr("CSV-Datei (*.csv)")]
        currentFolder: StandardPaths.writableLocation(StandardPaths.DocumentsLocation)
        selectedFile: currentFolder + "/mandate.csv"
        onAccepted: DirectDebits.exportTo(selectedFile)
    }

    // .confirmationDialog("Mandat löschen?")
    property var deleting: null
    ConfirmDialog {
        id: confirmDelete
        heading: qsTr("Mandat löschen?")
        message: view.deleting ? qsTr("Das Mandat „%1“ wird entfernt. Bereits eingereichte Einzüge bleiben unverändert.").arg(view.deleting.reference) : ""
        actionTitle: qsTr("Löschen")
        onConfirmed: if (view.deleting) DirectDebits.deleteMandate(view.deleting)
    }

    // context menu of a row
    property var menuMandate: null
    AppMenu {
        id: rowMenu
        AppMenuItem { text: qsTr("Bearbeiten …"); icon.name: SF.icon("pencil"); onTriggered: DirectDebits.editMandate(view.menuMandate) }
        AppMenuItem { text: view.menuMandate && view.menuMandate.active ? qsTr("Deaktivieren") : qsTr("Aktivieren")
                      icon.name: SF.icon(view.menuMandate && view.menuMandate.active ? "pause.circle" : "play.circle")
                      onTriggered: DirectDebits.setActive(view.menuMandate, !view.menuMandate.active) }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Löschen …"); icon.name: SF.icon("trash"); onTriggered: { view.deleting = view.menuMandate; confirmDelete.open() } }
    }

    MandateEditorSheet {}
    CollectionSheet {}

    component Unavailable: Item {
        property string symbol
        property string title
        property string detail
        Layout.fillWidth: true; Layout.fillHeight: true
        ColumnLayout {
            anchors.centerIn: parent
            width: Math.min(parent.width - 40, 420)
            spacing: 8
            SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: parent.parent.symbol; size: 36; color: Theme.tertiary }
            Text { Layout.alignment: Qt.AlignHCenter; text: parent.parent.title; font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text }
            Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap; font.pixelSize: Theme.body; color: Theme.secondary
                   text: parent.parent.detail }
        }
    }

    /// MandateRow: one mandate as a slice of the card (same look as a booking row).
    component MandateRow: Item {
        id: mr
        property var mandate: null
        property bool isLast: false
        readonly property bool selected: mandate !== null && DirectDebits.selectedID === mandate.id
        readonly property bool hasReturns: { void DirectDebits.openReturns; return DirectDebits.returnsOf(mandate).length > 0 }
        readonly property color tint: mandate && mandate.active ? Theme.accent : Theme.tertiary
        height: Math.max(44, content.implicitHeight) + 10
        CardSegment { anchors.fill: parent; isBottom: mr.isLast }
        Rectangle {
            anchors.fill: parent; anchors.margins: 1
            anchors.bottomMargin: mr.isLast ? 4 : 0
            radius: mr.isLast ? 10 : 0
            color: mr.selected ? Theme.selection : "transparent"
        }
        RowLayout {
            id: content
            opacity: mr.mandate && mr.mandate.active ? 1 : 0.65
            anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
            anchors.leftMargin: 14; anchors.rightMargin: 14; anchors.topMargin: 5
            spacing: 12
            Rectangle {
                Layout.alignment: Qt.AlignTop
                width: 36; height: 36; radius: 18
                color: Theme.withAlpha(mr.tint, 0.12)
                SFIcon { anchors.centerIn: parent; symbol: "doc.text"; size: 16; color: mr.tint }
            }
            ColumnLayout {
                Layout.fillWidth: true; Layout.alignment: Qt.AlignTop
                spacing: 2
                Text { Layout.fillWidth: true; elide: Text.ElideRight; font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text
                       text: mr.mandate ? mr.mandate.debtor_name : ""; Sensitive {} }
                Text { Layout.fillWidth: true; elide: Text.ElideRight; font.pixelSize: Theme.callout; color: Theme.secondary
                       text: mr.mandate ? mr.mandate.reference + " · " + DirectDebits.schemeTitle(mr.mandate.scheme) : ""; Sensitive {} }
                RowLayout {
                    visible: mr.hasReturns
                    spacing: 4
                    SFIcon { symbol: "exclamationmark.triangle.fill"; size: 11; color: Theme.negative }
                    Text { text: qsTr("Rücklastschrift – bitte prüfen"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.negative }
                }
                Text { visible: !mr.hasReturns; text: mr.mandate ? DirectDebits.statusText(mr.mandate) : ""; font.pixelSize: Theme.caption; color: Theme.tertiary }
            }
            ColumnLayout {
                Layout.alignment: Qt.AlignTop
                spacing: 2
                Text { visible: !!mr.mandate && mr.mandate.default_amount_cents > 0; Layout.alignment: Qt.AlignRight
                       text: mr.mandate ? Fmt.money(mr.mandate.default_amount_cents) : ""
                       font.pixelSize: Theme.body; font.weight: Font.Medium; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                Text { Layout.alignment: Qt.AlignRight; font.pixelSize: Theme.caption; color: Theme.secondary
                       text: !mr.mandate ? "" : mr.mandate.last_collected ? qsTr("zuletzt %1").arg(Fmt.medium(mr.mandate.last_collected)) : qsTr("noch nie eingezogen") }
            }
        }
        Rectangle {
            visible: !mr.isLast
            x: 64; width: parent.width - x - 1; height: 1
            anchors.bottom: parent.bottom; color: Theme.withAlpha(Theme.border, 0.7)
        }
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            onClicked: m => {
                list.forceActiveFocus()
                if (m.button === Qt.RightButton) { view.menuMandate = mr.mandate; rowMenu.popup(mr, m.x, m.y); return }
                DirectDebits.selectedID = mr.mandate.id
            }
            // .simultaneousGesture(TapGesture(count: 2)): edit
            onDoubleClicked: m => { if (m.button === Qt.LeftButton) DirectDebits.editMandate(mr.mandate) }
        }
        Accessible.role: Accessible.Button
        Accessible.name: mandate ? mandate.debtor_name + ", " + mandate.reference : ""
    }
}
