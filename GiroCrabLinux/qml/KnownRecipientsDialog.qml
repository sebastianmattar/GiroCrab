import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// KnownRecipientsSheet.swift ("Empfänger verwalten", #978): every IBAN the outgoing bookings
// went to, newest first, hidden ones on top; hiding changes no booking (docs/EMPFAENGER_UI.md).
Sheet {
    id: sheet
    sheetWidth: 560
    property var recipients: []
    /// Why the last read failed; the rows of the last good read stay.
    property string loadError: ""
    readonly property var hidden: recipients.filter(r => r.hidden)
    readonly property var suggested: recipients.filter(r => !r.hidden)

    function reload() {
        try { recipients = AppModel.knownRecipients(searchField.text); loadError = "" }
        catch (e) { loadError = e.message }
    }
    function toggle(r) { AppModel.setRecipientHidden(r.iban, !r.hidden); reload() }
    onAboutToShow: { searchField.text = ""; reload() }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0
        RowLayout {
            Layout.fillWidth: true
            Layout.bottomMargin: 12
            Text { text: qsTr("Empfänger"); font.pixelSize: Theme.body + 2; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true
                   Accessible.role: Accessible.Heading }
            Button { text: qsTr("Fertig"); highlighted: true; onClicked: sheet.close() }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        Text {
            Layout.fillWidth: true; Layout.topMargin: 12; wrapMode: Text.WordWrap
            text: qsTr("Aus Ihren bisherigen Zahlungen, je IBAN eine Zeile. Ausgeblendete IBANs schlägt „Neue Überweisung“ nicht mehr vor; die Buchungen bleiben.")
            font.pixelSize: Theme.callout; color: Theme.secondary
        }
        TextField {
            id: searchField
            Layout.fillWidth: true; Layout.topMargin: 10
            placeholderText: qsTr("Name oder IBAN suchen")
            onTextEdited: sheet.reload()
            Sensitive {}
        }
        ColumnLayout {
            visible: sheet.loadError !== ""
            Layout.fillWidth: true; Layout.topMargin: 10
            spacing: 4
            Text { text: qsTr("Empfänger konnten nicht geladen werden"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.warning }
            Text { text: sheet.loadError; font.pixelSize: Theme.caption; color: Theme.tertiary; wrapMode: Text.WordWrap; maximumLineCount: 2
                   elide: Text.ElideRight; Layout.fillWidth: true }
            Button { flat: true; text: qsTr("Erneut versuchen"); onClicked: sheet.reload() }
        }
        Text {
            visible: sheet.recipients.length === 0 && sheet.loadError === ""
            Layout.fillWidth: true; Layout.topMargin: 24; Layout.bottomMargin: 24
            horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap
            text: searchField.text.trim() === "" ? qsTr("Hier erscheinen die Empfänger Ihrer Zahlungen, sobald Buchungen mit IBAN abgerufen sind.")
                                                 : qsTr("Keine Treffer")
            font.pixelSize: Theme.body; color: Theme.secondary
        }
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            visible: sheet.recipients.length > 0
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(list.implicitHeight + 24, 460, sheet.maxBodyHeight - 160)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: list
                y: 12
                width: scroll.availableWidth - 4
                spacing: 18
                SettingsFormSection {
                    visible: sheet.hidden.length > 0
                    title: qsTr("Ausgeblendet (%1)").arg(sheet.hidden.length)
                    Repeater { model: sheet.hidden; RecipientRow {} }
                }
                SettingsFormSection {
                    visible: sheet.suggested.length > 0
                    title: qsTr("Vorgeschlagen (%1)").arg(sheet.suggested.length)
                    Repeater { model: sheet.suggested; RecipientRow {} }
                }
                Item { implicitHeight: 4 }
            }
        }
    }

    component RecipientRow: RowLayout {
        id: row
        required property var modelData
        Layout.fillWidth: true
        spacing: 10
        SFIcon { visible: row.modelData.hidden; symbol: "eye.slash"; size: 15; color: Theme.tertiary }
        ColumnLayout { spacing: 1; Layout.fillWidth: true
            Text { text: row.modelData.name; font.pixelSize: Theme.body; color: row.modelData.hidden ? Theme.secondary : Theme.text
                   elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
            Text { text: Fmt.iban(row.modelData.iban); font.pixelSize: Theme.caption; font.family: "monospace"; color: Theme.secondary
                   elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} } }
        ColumnLayout { spacing: 1
            Text { Layout.alignment: Qt.AlignRight; text: row.modelData.times_used === 1 ? qsTr("1 Buchung") : qsTr("%1 Buchungen").arg(row.modelData.times_used); font.pixelSize: Theme.caption; color: Theme.tertiary }
            Text { Layout.alignment: Qt.AlignRight; text: qsTr("zuletzt %1").arg(Fmt.medium(row.modelData.last_used)); font.pixelSize: Theme.caption; color: Theme.tertiary } }
        Button { flat: true; text: row.modelData.hidden ? qsTr("Einblenden") : qsTr("Ausblenden"); onClicked: sheet.toggle(row.modelData)
                 ToolTip.text: row.modelData.hidden ? qsTr("Wieder vorschlagen") : qsTr("Nicht mehr vorschlagen")
                 ToolTip.visible: hovered; ToolTip.delay: 600 }
        TapHandler { acceptedButtons: Qt.RightButton; onTapped: rowMenu.popup() }
        AppMenu { id: rowMenu
            AppMenuItem { text: row.modelData.hidden ? qsTr("Wieder vorschlagen") : qsTr("Nicht mehr vorschlagen"); onTriggered: sheet.toggle(row.modelData) } }
    }
}
