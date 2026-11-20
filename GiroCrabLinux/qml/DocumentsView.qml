import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import QtCore

// DocumentsUI.swift › DocumentsView: "Belege & PostBox" – list left, preview right.
// The scope (Alle Dokumente / Belege / PostBox, #526) sits in the toolbar like on the Mac
// (Main.qml › DocumentsScope). Linux keeps its search field above the list.
Rectangle {
    id: view
    color: Theme.canvas
    property string search: ""
    property bool unlinkedOnly: false
    /// List selection by id; the shown document is looked up in `docs`, so one that is
    /// filtered out or deleted leaves the preview (Swift: `docs.first { $0.id == selected }`).
    property var selectedID: null
    readonly property var selected: selectedID === null ? null : (docs.find(d => d.id === selectedID) || null)
    property var docToDelete: null
    /// model.allDocuments(search:unlinkedOnly:accountId:scope:)
    readonly property var docs: {
        void DocumentsScope.current
        return AppModel.sidebarSelection !== "documents" ? []
            : AppModel.allDocuments(search, unlinkedOnly, AppModel.documentsAccountFilter).filter(d => DocumentsScope.includes(d))
    }
    /// Nothing filtered: the general empty state instead of „Keine passenden Dokumente“.
    readonly property bool unfiltered: DocumentsScope.current === "all" && AppModel.documentsAccountFilter === null && !unlinkedOnly && search === ""

    function showAll() {
        DocumentsScope.current = "all"
        AppModel.documentsAccountFilter = null
        unlinkedOnly = false
        searchField.text = ""
    }
    // The account filter is only set on purpose ("Belege anzeigen" on an account); opening
    // the section later starts at "Alle Konten" again (#670, `.onDisappear`).
    Connections {
        target: AppModel
        function onSidebarSelectionChanged() { if (AppModel.sidebarSelection !== "documents") AppModel.documentsAccountFilter = null }
    }

    // One context menu for all documents (built once, not per row).
    property var menuDoc: null
    AppMenu { id: docMenu
        AppMenuItem { text: qsTr("Vorschau"); onTriggered: AppModel.quickLook(view.menuDoc) }
        // „Im Finder anzeigen“ (#622)
        AppMenuItem { text: qsTr("Im Dateimanager anzeigen"); onTriggered: AppModel.showInFileManager(view.menuDoc) }
        AppMenuItem { text: qsTr("Zuordnung aufheben")
                      enabled: !!view.menuDoc && view.menuDoc.booking_id !== null && view.menuDoc.booking_id !== undefined
                      onTriggered: AppModel.linkDocument(view.menuDoc, null) }
        // move to another account or none (#838), one submenu per sidebar section
        AppMenu {
            id: accountMenu
            title: qsTr("Konto zuordnen")
            AppMenuItem { text: qsTr("Ohne Konto")
                          enabled: !!view.menuDoc && view.menuDoc.account_id !== null && view.menuDoc.account_id !== undefined
                          onTriggered: AppModel.setDocumentAccount(view.menuDoc, null) }
            Instantiator {
                model: AppModel.accountMenuSections
                delegate: AppMenu {
                    id: sectionMenu
                    required property var modelData
                    title: AppModel.menuText(modelData.title)
                    Instantiator {
                        model: sectionMenu.modelData.accounts
                        delegate: AppMenuItem {
                            required property var modelData
                            text: AppModel.menuText(modelData.name)
                            enabled: !!view.menuDoc && view.menuDoc.account_id !== modelData.id
                            onTriggered: AppModel.setDocumentAccount(view.menuDoc, modelData.id)
                        }
                        onObjectAdded: (index, object) => sectionMenu.insertItem(index, object)
                        onObjectRemoved: (index, object) => sectionMenu.removeItem(object)
                    }
                }
                onObjectAdded: (index, object) => accountMenu.addMenu(object)
                onObjectRemoved: (index, object) => accountMenu.removeMenu(object)
            }
        }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Beleg löschen …"); onTriggered: view.docToDelete = view.menuDoc } }

    RowLayout {
        anchors.fill: parent
        spacing: 0
        ColumnLayout {
            // The list keeps its width; the preview takes what is left.
            Layout.preferredWidth: Math.min(520, Math.max(400, view.width * 0.55)); Layout.minimumWidth: 430; Layout.fillHeight: true
            clip: true
            Layout.margins: 18
            spacing: 12
            ColumnLayout { spacing: 4; Layout.fillWidth: true
                Text { text: DocumentsScope.heading; font.pixelSize: Theme.title2 + 3; font.weight: Font.DemiBold; color: Theme.text }
                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                       text: DocumentsScope.description } }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 8
                // ViewThatFits: account picker and switch in one row, else stacked
                GridLayout {
                    Layout.fillWidth: true
                    columns: view.width < 980 ? 1 : 3
                    columnSpacing: 12; rowSpacing: 8
                    // same order and groups as the sidebar (#412)
                    AppComboBox {
                        readonly property var options: AppModel.accountMenuOptions(a => a.name, true)
                        Layout.minimumWidth: 150; Layout.maximumWidth: 270; Layout.fillWidth: true
                        model: options
                        textRole: "title"
                        currentIndex: Math.max(0, options.findIndex(o => o.id === AppModel.documentsAccountFilter && !o.header))
                        onActivated: i => { if (!options[i].header) AppModel.documentsAccountFilter = options[i].id }
                        ToolTip.text: qsTr("Dokumente eines Kontos anzeigen"); ToolTip.visible: hovered; ToolTip.delay: 600
                        Sensitive { when: AppModel.documentsAccountFilter !== null && AppModel.documentsAccountFilter !== AppModel.documentsWithoutAccount }   // the selected account's name
                    }
                    Item { visible: view.width >= 980; Layout.fillWidth: true }
                    Switch { text: qsTr("Nur ohne zugeordneten Umsatz"); checked: view.unlinkedOnly; onToggled: view.unlinkedOnly = checked }
                }
                Text { text: qsTr("%1 Dokumente").arg(view.docs.length); font.pixelSize: Theme.caption; color: Theme.secondary }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 12
                    Button {
                        text: qsTr("Beleg hinzufügen …"); icon.name: SF.icon("plus.circle")
                        onClicked: importDialog.open()
                        ToolTip.text: qsTr("PDFs oder Fotos auswählen und einem passenden Umsatz zuordnen"); ToolTip.visible: hovered; ToolTip.delay: 600
                    }
                    Item { Layout.fillWidth: true }
                    Button {
                        text: qsTr("Alle exportieren …"); icon.name: SF.icon("square.and.arrow.up"); onClicked: folderDialog.open()
                        ToolTip.text: qsTr("Die gesamte Dokumentensammlung exportieren, unabhängig von den Filtern"); ToolTip.visible: hovered; ToolTip.delay: 600
                    }
                }
            }
            TextField { id: searchField; Layout.fillWidth: true; placeholderText: qsTr("Belege durchsuchen"); onTextChanged: view.search = text
                        Sensitive { when: parent.text !== "" } }
            ListView {
                id: list
                Layout.fillWidth: true; Layout.fillHeight: true
                clip: true
                model: view.docs
                ScrollBar.vertical: AppScrollBar {}
                delegate: Rectangle {
                    id: docRow
                    required property var modelData
                    readonly property bool bank: DocumentsScope.isBankDocument(modelData)
                    width: ListView.view.width; height: rowCol.implicitHeight + 14
                    radius: 6
                    color: view.selectedID === modelData.id ? Theme.selection : "transparent"
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: 8; anchors.rightMargin: 8; spacing: 10
                        SFIcon { symbol: AppModel.docSymbol(modelData); size: 18; color: Theme.accent; Layout.preferredWidth: 22 }
                        ColumnLayout { id: rowCol; spacing: 2; Layout.fillWidth: true
                            Text { text: modelData.title; font.pixelSize: Theme.body + 1; font.weight: Font.Medium; color: Theme.text
                                   wrapMode: Text.Wrap; maximumLineCount: 2; elide: Text.ElideRight; Layout.fillWidth: true
                                   Sensitive { amounts: true } }   // DocumentsUI.swift: [.personal, .amounts]
                            Text { text: DocumentsScope.originTitle(modelData) + " · " + (modelData.account_name || qsTr("Ohne Konto"))
                                   font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                            Text { text: modelData.booking_label || qsTr("Ohne zugeordneten Umsatz"); font.pixelSize: Theme.caption
                                   color: !modelData.booking_label && !docRow.bank ? Theme.warning : Theme.secondary
                                   elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true } } }   // "merchant · amount"
                        Text { text: Fmt.medium(modelData.doc_date); font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary }
                    }
                    MouseArea {
                        anchors.fill: parent; acceptedButtons: Qt.LeftButton | Qt.RightButton
                        onClicked: m => { view.selectedID = modelData.id
                                          if (m.button === Qt.RightButton) { view.menuDoc = modelData; docMenu.popup() } }
                        onDoubleClicked: AppModel.quickLook(modelData)
                    }
                }
                ColumnLayout {
                    anchors.centerIn: parent; visible: view.docs.length === 0; spacing: 8; width: parent.width - 40
                    SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "doc.on.doc"; size: 30 }
                    Text { Layout.alignment: Qt.AlignHCenter; font.pixelSize: Theme.title3 + 1; font.weight: Font.Bold; color: Theme.text
                           text: view.unfiltered ? qsTr("Keine Belege") : qsTr("Keine passenden Dokumente") }
                    Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap; color: Theme.secondary
                           text: view.unfiltered ? qsTr("Belege kommen beim Abruf (Trade Republic, comdirect-PostBox, FinTS-Kontoauszug) oder per Drag & Drop.")
                                                 : qsTr("Für diese Auswahl wurden keine Dokumente gefunden.") }
                    Button { visible: !view.unfiltered; Layout.alignment: Qt.AlignHCenter; text: qsTr("Alle Dokumente anzeigen"); onClicked: view.showAll() }
                }
            }
        }
        Rectangle { Layout.fillHeight: true; implicitWidth: 1; color: Theme.border }
        Item {
            Layout.fillWidth: true; Layout.fillHeight: true; Layout.preferredWidth: 520
            DocumentPreview { anchors.fill: parent; visible: view.selected !== null; doc: view.selected }
            ColumnLayout {
                anchors.centerIn: parent; visible: view.selected === null; spacing: 8; width: Math.min(parent.width - 40, 360)
                SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "doc.richtext"; size: 30 }
                Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Dokument auswählen"); font.pixelSize: Theme.title3 + 1; font.weight: Font.Bold; color: Theme.text }
                Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap; color: Theme.secondary
                       text: qsTr("Ein Dokument aus der Liste auswählen, um die Vorschau zu öffnen.") }
            }
        }
    }
    // .dropDestination: drop anywhere → auto-match
    DropArea {
        id: drop
        anchors.fill: parent
        onDropped: d => { if (d.hasUrls) AppModel.dropAnywhere(d.urls) }
    }
    Rectangle { anchors.fill: parent; anchors.margins: 8; visible: drop.containsDrag; color: "transparent"; radius: 14; border.color: Theme.accent; border.width: 2 }

    // importButton › .fileImporter([.pdf, .png, .jpeg, .heic], allowsMultipleSelection: true)
    FileDialog {
        id: importDialog
        title: qsTr("Beleg hinzufügen …")
        fileMode: FileDialog.OpenFiles
        nameFilters: [qsTr("PDFs und Fotos (*.pdf *.png *.jpg *.jpeg *.heic)")]
        currentFolder: StandardPaths.writableLocation(StandardPaths.DownloadLocation)
        onAccepted: {
            if (!selectedFiles.length) return
            // An unmatched upload has no account. Make it visible even when the dialog
            // was opened from PostBox or a filtered account.
            DocumentsScope.current = "receipts"
            AppModel.documentsAccountFilter = null
            searchField.text = ""
            view.unlinkedOnly = false
            AppModel.dropAnywhere(selectedFiles)
        }
    }
    FolderDialog { id: folderDialog; title: qsTr("Ordner für die exportierten Belege wählen"); acceptLabel: qsTr("Exportieren")
                   onAccepted: AppModel.exportAllDocuments(AppModel.localPath(selectedFolder)) }
    Sheet {
        visible: view.docToDelete !== null
        onClosed: view.docToDelete = null
        sheetWidth: 380
        ColumnLayout { width: parent.width; spacing: 12
            Text { text: qsTr("Beleg löschen?"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text }
            Text { text: view.docToDelete ? qsTr("„%1“ wird unwiderruflich gelöscht.").arg(view.docToDelete.title) : ""; wrapMode: Text.WordWrap; Layout.fillWidth: true; color: Theme.text
                   Sensitive {} }
            RowLayout { Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: view.docToDelete = null }
                Button { text: qsTr("Löschen"); onClicked: { AppModel.deleteDocument(view.docToDelete); view.docToDelete = null } } } }
    }
}
