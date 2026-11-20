import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// LoanProgressCard.swift: repayment progress plus the editable "Ursprünglicher Kreditbetrag"
// (HKSAL only reports the remaining balance, so live loans need it entered once).
// Manual loans (ManualLoansUI.swift) keep amount, Stand and rate in the loan editor.
Rectangle {
    id: card
    property var account
    property bool compact: false
    property bool editing: false
    readonly property var manual: account ? AppModel.manualLoan(account.id) : null
    readonly property var initial: account && account.initial_balance_cents !== null && Math.abs(account.initial_balance_cents) > 0 ? Math.abs(account.initial_balance_cents) : null
    readonly property var repaid: initial ? Math.max(0, Math.min(1, 1 - Math.abs(account.balance_cents) / initial)) : null
    function commit() {
        const t = amount.text.trim()
        if (t === "") AppModel.setAccountInitialBalance(account.id, null)
        else { const c = Fmt.parseCents(t); if (c !== null) AppModel.setAccountInitialBalance(account.id, c) }
        editing = false
    }
    color: compact ? "transparent" : Theme.card
    radius: 12
    border.color: compact ? "transparent" : Theme.border
    implicitHeight: col.implicitHeight + (compact ? 0 : 32)
    ColumnLayout {
        id: col
        anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
        anchors.margins: card.compact ? 0 : 16
        spacing: 8
        RowLayout {
            Layout.fillWidth: true
            SFIcon { visible: !card.compact; symbol: "house"; size: 15; color: Theme.text }
            Text { visible: !card.compact; text: qsTr("Tilgung"); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text }
            Text { visible: card.repaid !== null; text: qsTr("%1 getilgt").arg(Fmt.percent(card.repaid || 0)); font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; color: Theme.loan; Sensitive { amounts: true } }
            Text { visible: card.repaid === null; text: qsTr("Ursprünglicher Kreditbetrag fehlt"); font.pixelSize: Theme.caption; color: Theme.secondary }
            Item { Layout.fillWidth: true }
            // manual loans: amount, Stand and rate live in the loan editor
            IconButton {
                visible: card.manual !== null && card.compact
                text: qsTr("Stand neu setzen …")
                font.pixelSize: Theme.caption
                onClicked: AppModel.openLoanEditor("anchor", card.account.id)
            }
            TextField {
                id: amount; visible: card.manual === null && card.editing; Layout.preferredWidth: 130
                placeholderText: qsTr("z. B. %1").arg(Fmt.plain(28000000)); onAccepted: card.commit()
                Sensitive { amounts: true }
            }
            Button { visible: card.manual === null && card.editing; text: qsTr("Abbrechen"); onClicked: card.editing = false }
            Button { visible: card.manual === null && card.editing; text: qsTr("Speichern"); highlighted: true; onClicked: card.commit() }
            // .keyboardShortcut(.cancelAction) on "Abbrechen": Esc ends editing wherever the focus is
            Shortcut { sequences: ["Esc"]; enabled: card.editing && card.visible && card.manual === null; onActivated: card.editing = false }
            IconButton {
                visible: card.manual === null && !card.editing
                text: card.initial === null ? qsTr("Betrag eingeben") : qsTr("Ursprünglich: %1").arg(Fmt.money(card.initial))
                font.pixelSize: Theme.caption
                // narrow inspector: the title shrinks (…) instead of running past the card
                Layout.fillWidth: true; Layout.maximumWidth: Math.ceil(implicitWidth)
                implicitWidth: origText.implicitWidth + 12
                contentItem: Text { id: origText; text: parent.text; elide: Text.ElideRight; horizontalAlignment: Text.AlignRight
                                    verticalAlignment: Text.AlignVCenter; font.pixelSize: Theme.caption; color: Theme.text
                                    Sensitive { amounts: true; when: card.initial !== null } }   // the original loan amount
                ToolTip.text: qsTr("Ursprünglicher Kreditbetrag bearbeiten")
                onClicked: { amount.text = card.initial ? Fmt.plain(card.initial) : ""; card.editing = true; amount.forceActiveFocus() }
            }
        }
        Rectangle {
            visible: card.repaid !== null
            Layout.fillWidth: true; height: 4; radius: 2; color: Theme.withAlpha(Theme.loan, 0.2)
            Rectangle { height: 4; radius: 2; width: parent.width * (card.repaid || 0); color: Theme.loan }
            Sensitive { amounts: true }   // LoanProgressCard.swift: the bar tells the share too
        }
        ManualLoanDetails {
            visible: card.manual !== null && !card.compact
            loan: card.manual
            Layout.fillWidth: true
        }
    }
}
