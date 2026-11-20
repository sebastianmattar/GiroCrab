import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// AccountEditSheet.swift: "Konto bearbeiten …" (#639, #605) – name, owner, account type and
// note in one place.
Sheet {
    id: sheet
    property var accountID: null
    /// The account when the sheet opened: what a sync changes meanwhile is no edit here (#980).
    property var original: null
    readonly property var account: accountID === null ? null : (AppModel.accountsByID[accountID] || null)
    sheetWidth: 440
    onClosed: accountID = null

    function edit(id) {
        const a = AppModel.accountsByID[id]
        if (!a) return
        accountID = id
        original = a
        nameField.text = a.name
        ownerField.text = a.owner || ""
        kindBox.currentIndex = Math.max(0, AppModel.accountKindChoices.findIndex(k => k.id === a.kind))
        noteField.text = a.note || ""
        open()
        nameField.forceActiveFocus()
    }
    function save() {
        const a = original
        if (!a || !account) return
        const newName = nameField.text.trim()
        if (newName === "") return
        const owner = ownerField.text.trim()
        const kind = AppModel.accountKindChoices[kindBox.currentIndex].id
        const note = noteField.text.trim()
        close()
        if (newName !== a.name) AppModel.renameAccount(a.id, newName)
        if (owner !== (a.owner || "")) AppModel.setAccountOwner(a.id, owner)
        if (kind !== a.kind) AppModel.setAccountKind(a.id, kind)
        if (note !== (a.note || "")) AppModel.setAccountNote(a.id, note)
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 14
        Text { text: qsTr("Konto bearbeiten"); font.pixelSize: Theme.title3; font.weight: Font.Bold; color: Theme.text }
        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: 12; rowSpacing: 8
            Text { text: qsTr("Kontoname"); font.pixelSize: Theme.body; color: Theme.text; Layout.alignment: Qt.AlignRight }
            TextField { id: nameField; Layout.fillWidth: true; onAccepted: sheet.save(); Sensitive {} }
            Text { text: qsTr("Name des Kontoinhabers"); font.pixelSize: Theme.body; color: Theme.text; Layout.alignment: Qt.AlignRight }
            TextField { id: ownerField; Layout.fillWidth: true; onAccepted: sheet.save(); Sensitive {} }
            Text { text: qsTr("Kontoart"); font.pixelSize: Theme.body; color: Theme.text; Layout.alignment: Qt.AlignRight }
            AppComboBox { id: kindBox; Layout.fillWidth: true; model: AppModel.accountKindChoices; textRole: "text" }
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 4
            Text { text: qsTr("Konto-Notiz"); font.pixelSize: Theme.body; color: Theme.text }
            ScrollView {
                Layout.fillWidth: true
                Layout.preferredHeight: 80
                ScrollBar.vertical: ScrollViewBar {}
                TextArea {
                    id: noteField
                    wrapMode: TextEdit.Wrap
                    font.pixelSize: Theme.body
                    background: Rectangle { radius: 6; color: "transparent"; border.color: Theme.withAlpha(Theme.tertiary, 0.5) }
                    Sensitive {}
                }
            }
            Text { text: qsTr("Erscheint unter dem Kontonamen in der Kontoansicht."); font.pixelSize: Theme.caption; color: Theme.tertiary
                   wrapMode: Text.WordWrap; Layout.fillWidth: true }
        }
        RowLayout {
            Layout.fillWidth: true
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button { text: qsTr("Sichern"); highlighted: true; enabled: nameField.text.trim() !== ""; onClicked: sheet.save() }
        }
    }
}
