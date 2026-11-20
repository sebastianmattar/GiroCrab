import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Onboarding.swift › BankSetupSheet: settings a connector still needs before syncing.
Sheet {
    id: sheet
    readonly property var request: AppModel.setupRequest
    property var values: ({})
    visible: request !== null
    sheetWidth: 480
    onAboutToShow: values = ({})
    onRejected: AppModel.cancelSetupRequest()
    ColumnLayout {
        width: sheet.availableWidth
        spacing: 16
        RowLayout {
            spacing: 10
            SymbolTile { symbol: sheet.request ? sheet.request.connection.sf_symbol : ""; size: 34; tint: Theme.accent }
            ColumnLayout { spacing: 2; Layout.fillWidth: true
                Text { text: sheet.request ? qsTr("%1 einrichten").arg(sheet.request.connection.display_name) : ""; font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text }
                Text { text: qsTr("Diese Angaben fehlen noch, bevor Buchungen abgerufen werden können."); font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true } }
            SyncBatchProgressLabel {}
        }
        // Shown together in EnableBankingSetupSection (guide, key file, check).
        EnableBankingSetupSection {
            readonly property var ebKeys: ["enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url"]
            visible: sheet.request !== null && sheet.request.keys.some(k => ebKeys.indexOf(k) >= 0)
            Layout.fillWidth: true
            values: sheet.values
            onEdited: (key, value) => { const x = Object.assign({}, sheet.values); x[key] = value; sheet.values = x }
        }
        Repeater {
            model: sheet.request ? sheet.request.keys.filter(k => k.indexOf("enablebanking.") !== 0 || ["enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url"].indexOf(k) < 0) : []
            SettingField { required property string modelData; key: modelData; Layout.fillWidth: true
                           onEdited: v => { const x = Object.assign({}, sheet.values); x[key] = v; sheet.values = x } }
        }
        SyncPromptButtons {
            Layout.fillWidth: true
            defaultTitle: qsTr("Speichern & abrufen")
            // The FinTS product ID is an optional override and never blocks the button (#842).
            defaultEnabled: sheet.request !== null && sheet.request.keys.every(k => k === "fints.product_id" || (sheet.values[k] || "").trim() !== "")
            onCancel: AppModel.cancelSetupRequest()
            onAccept: AppModel.completeSetup(sheet.request, sheet.values)
        }
    }
}
