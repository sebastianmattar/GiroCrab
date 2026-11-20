import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// MoneyMoneyImportSheet.swift › ImportSourceSection: where the data comes
// from, as radio buttons with a short explanation each. Identical on all
// assistants, so switching keeps the layout. „Datenbank“ covers GiroCrab's own
// backup and the other app's database; „Art“ picks between them (#529).
FormSection {
    id: s
    property string source              // "backup" | "database" | "export"
    /// Source.isDatabase: listed under „Datenbank“.
    readonly property bool isDatabase: source !== "export"
    signal chosen(string source)
    title: qsTr("Quelle")
    Repeater {
        model: [
            { id: "database", title: qsTr("Datenbank"),
              detail: qsTr("Der ganze Datenbestand auf einmal: eine Sicherung aus GiroCrab oder die Datenbank einer anderen App.") },
            { id: "export", title: qsTr("Exportdatei eines Kontos"),
              detail: qsTr("CSV, camt.053 oder MT940 – um einzelne Konten zu ergänzen.") }
        ]
        RowLayout {
            required property var modelData
            readonly property bool on: modelData.id === "export" ? !s.isDatabase : s.isDatabase
            // the radio binding: true → .backup, false → .export
            function pick() { if (!on) s.chosen(modelData.id === "export" ? "export" : "backup") }
            Layout.fillWidth: true
            spacing: 4
            RadioButton {
                checked: parent.on
                onClicked: parent.pick()
                Layout.alignment: Qt.AlignTop
            }
            ColumnLayout {
                spacing: 2
                Layout.fillWidth: true
                Text { text: modelData.title; color: Theme.text; font.pixelSize: Theme.body
                       MouseArea { anchors.fill: parent; enabled: s.enabled; onClicked: parent.parent.parent.pick() } }
                Text { text: modelData.detail; color: Theme.secondary; font.pixelSize: Theme.caption; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            }
        }
    }
    // Picker("Art") – product names, not translated
    FormRow {
        visible: s.isDatabase
        label: qsTr("Art")
        AppComboBox {
            readonly property var options: [{ id: "backup", title: "GiroCrab" }, { id: "database", title: qsTr("Datenbank") }]
            model: options
            textRole: "title"
            currentIndex: Math.max(0, options.findIndex(o => o.id === s.source))
            onActivated: i => s.chosen(options[i].id)
        }
    }
    Text {
        visible: s.isDatabase
        Layout.fillWidth: true; wrapMode: Text.WordWrap
        font.pixelSize: Theme.caption; color: Theme.secondary
        text: s.source === "backup"
              ? qsTr("Eine Sicherung aus „Tresor sichern …“, geöffnet mit ihrem Passwort. Ersetzt den Tresor.")
              : qsTr("Alle Konten mit Kategorien, Regeln, Bankzugängen und Kontoauszügen – empfohlen beim Umstieg.")
    }
}
