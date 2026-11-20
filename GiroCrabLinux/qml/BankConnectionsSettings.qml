import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// SettingsView.swift › BankConnectionsSettings: connected banks (edit, sync, remove),
// "Bank hinzufügen" and the FinTS registration used for all FinTS banks.
Item {
    id: pane
    property var confirmTarget: null

    // --demo-settings banks:edit|add|remove (screenshots)
    function presentDemoSheet() {
        const s = AppModel.demoSettingsSheet
        AppModel.demoSettingsSheet = ""
        if (s === "edit" && AppModel.connections.length) editSheet.edit(AppModel.connections[0])
        else if (s === "select" && AppModel.connections.length) editSheet.edit(AppModel.connections[AppModel.connections.length - 1])
        else if (s === "add") addBank.open()
        else if (s === "remove" && AppModel.connections.length) { confirmTarget = AppModel.connections[0]; confirmRemove.open() }
    }
    Connections {
        target: AppModel
        function onShowSettingsChanged() { if (AppModel.showSettings && AppModel.demoSettingsSheet !== "") Qt.callLater(pane.presentDemoSheet) }
    }
    Component.onCompleted: if (AppModel.showSettings && AppModel.demoSettingsSheet !== "") Qt.callLater(presentDemoSheet)

    ConnectionEditSheet { id: editSheet }
    AddBankSheet { id: addBank }
    ConfirmDialog {
        id: confirmRemove
        heading: pane.confirmTarget ? qsTr("„%1“ entfernen?").arg(pane.confirmTarget.display_name) : ""
        message: qsTr("Alle Konten dieses Bankzugangs werden samt Buchungen, Belegen und gespeicherten Zugangsdaten aus dem Tresor gelöscht. Das lässt sich nicht widerrufen.")
        actionTitle: qsTr("Entfernen")
        onConfirmed: AppModel.removeConnection(pane.confirmTarget.id)
    }

    ScrollView {

        ScrollBar.vertical: ScrollViewBar {}
        id: scroll
        anchors.fill: parent
        contentWidth: availableWidth
        ColumnLayout {
            width: scroll.availableWidth - 40
            x: 20
            spacing: 18
            Item { implicitHeight: 2 }
            SettingsFormSection {
                visible: AppModel.demoActive
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    SFIcon { symbol: "theatermasks"; size: 16; color: Theme.secondary; Layout.alignment: Qt.AlignTop }
                    Text {
                        Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout + 1; color: Theme.secondary
                        text: AppModel.hasOwnVault
                            ? qsTr("Sie sehen gerade Demo-Daten. Neue Bankzugänge werden in Ihrem eigenen Tresor angelegt – die App wechselt dabei automatisch dorthin.")
                            : qsTr("Demo-Modus: Bankzugänge werden hier nur simuliert. Für echte Konten zuerst den eigenen Tresor öffnen.")
                    }
                }
                Button { visible: !AppModel.hasOwnVault; text: qsTr("Eigenen Tresor öffnen"); onClicked: AppModel.showOwnVault() }
            }
            SettingsFormSection {
                title: qsTr("Verbundene Banken")
                Text { visible: AppModel.connections.length === 0; text: qsTr("Noch keine Bank hinzugefügt."); font.pixelSize: Theme.body; color: Theme.secondary }
                Repeater {
                    model: AppModel.connections
                    RowLayout {
                        id: row
                        required property var modelData
                        readonly property var d: AppModel.descriptor(modelData.connector)
                        Layout.fillWidth: true
                        spacing: 10
                        SymbolTile { symbol: row.modelData.sf_symbol; size: 30; tint: Theme.accent }
                        ColumnLayout {
                            spacing: 1
                            Layout.fillWidth: true
                            Text { text: row.modelData.display_name; font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
                            Text { text: (row.d ? row.d.protocol : "") + (row.modelData.login_name === "" ? "" : " · " + row.modelData.login_name)
                                   font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true }
                            TapHandler { onTapped: editSheet.edit(row.modelData) }
                        }
                        Text { visible: row.modelData.last_sync_unix !== null; text: row.modelData.last_sync_unix !== null ? Fmt.relative(row.modelData.last_sync_unix) : ""
                               font.pixelSize: Theme.caption; color: Theme.secondary }
                        IconButton { symbol: "slider.horizontal.3"; ToolTip.text: qsTr("Bearbeiten / Details"); onClicked: editSheet.edit(row.modelData) }
                        IconButton { symbol: "arrow.triangle.2.circlepath"; ToolTip.text: qsTr("Jetzt abrufen")
                                     enabled: AppModel.syncingConnectionID === null; onClicked: AppModel.requestSync(row.modelData) }
                        IconButton { symbol: "trash"; ToolTip.text: qsTr("Bankzugang entfernen …")
                                     onClicked: { pane.confirmTarget = row.modelData; confirmRemove.open() } }
                    }
                }
            }
            SettingsFormSection {
                title: qsTr("Bank hinzufügen")
                IconButton { symbol: "magnifyingglass"; text: qsTr("Bank suchen und hinzufügen …"); tint: Theme.accent; onClicked: addBank.open() }
                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                       text: qsTr("Suche nach Bankname, BLZ oder IBAN: FinTS-Banken, Banken mit eigener API (bunq, comdirect, Trade Republic, PayPal) und PSD2-Banken (N26, Revolut, C24 …).") }
            }
            // Which FinTS product registration is used for all FinTS banks. Server address and
            // the keys of the other connectors are edited per bank ("Bearbeiten / Details").
            SettingsFormSection {
                title: qsTr("FinTS-Registrierung")
                footer: AppModel.fintsBuiltinAvailable
                    ? qsTr("Das FinTS-Produkt von GiroCrab ist eingebaut. Eigene API-Anwendungen (zum Beispiel für comdirect oder PayPal) registrieren Sie selbst; die Werte liegen nur in Ihrem verschlüsselten Tresor.")
                    : qsTr("Nichts davon wird mit dieser Version ausgeliefert. Registrieren Sie Ihr eigenes FinTS-Produkt und Ihre eigenen API-Anwendungen; die Werte liegen nur in Ihrem verschlüsselten Tresor.")
                Text {
                    visible: AppModel.fintsBuiltinAvailable
                    Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                    text: qsTr("Offizielle FinTS-Produktregistrierung von GiroCrab aktiv.")
                }
                // Own product ID is only needed in self-built / developer binaries without embedded ID (#1092).
                OwnProductIDField { visible: !AppModel.fintsBuiltinAvailable || AppModel.showOwnProductID }
            }
            Item { implicitHeight: 8 }
        }

    }

    // SettingsView.swift › OwnProductIDField: applies on Return and when the field is left;
    // the value stays hidden, an empty field changes nothing.
    component OwnProductIDField: ColumnLayout {
        id: own
        readonly property bool hasValue: !!(AppModel.settingEntry("fints.product_id") && AppModel.settingEntry("fints.product_id").has_value)
        Layout.fillWidth: true
        spacing: 4
        function commit() {
            const v = field.text.trim()
            field.text = ""
            if (v === "") return
            AppModel.saveSetting("fints.product_id", v)
            AppModel.setFintsProductIDMode("own")
        }
        /// After a rejected registration the alert sends the user here (AppModel.enterOwnProductID).
        function focusIfAsked() {
            if (!AppModel.focusOwnProductID || !own.visible) return
            AppModel.focusOwnProductID = false
            Qt.callLater(() => field.forceActiveFocus())
        }
        Component.onCompleted: focusIfAsked()
        onVisibleChanged: focusIfAsked()
        Connections { target: AppModel; function onFocusOwnProductIDChanged() { own.focusIfAsked() } }
        SettingsFormRow {
            label: qsTr("Eigene Produktregistrierung")
            // SecureField in the grouped Form: no bezel, trailing text, at most 340 wide
            FormTextField {
                id: field
                Layout.fillWidth: false; Layout.preferredWidth: 340
                echoMode: TextInput.Password
                placeholderText: own.hasValue ? qsTr("Gespeichert – zum Ersetzen neu eingeben") : qsTr("Registrierungsnummer eintragen")
                onAccepted: own.commit()
                onActiveFocusChanged: if (!activeFocus) own.commit()
            }
        }
        Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
               text: qsTr("Nur nötig, wenn Sie Ihre eigene Registrierung statt der von GiroCrab verwenden. Sie bleibt gespeichert, auch wenn Sie die von GiroCrab nutzen.") }
        Text {
            text: "<a href=\"https://www.fints.org/de/hersteller/produktregistrierung\">" + qsTr("Produktregistrierung bei der Deutschen Kreditwirtschaft beantragen") + "</a>"
            font.pixelSize: Theme.caption; color: Theme.accent; linkColor: Theme.accent
            onLinkActivated: link => Qt.openUrlExternally(link)
        }
    }
}
