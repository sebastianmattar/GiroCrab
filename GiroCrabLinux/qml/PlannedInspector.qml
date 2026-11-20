import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// PlannedPaymentsView.swift › PlannedInspector: details of the selected planned payment (HIG:
// an inspector shows the selection, never a leftover from another list) and PlannedActions.
ColumnLayout {
    id: insp
    readonly property var p: Planned.selected
    readonly property var order: p ? Payments.standingOrderFor(p) : null
    readonly property bool canEdit: order !== null && Payments.canEditStandingOrders
    spacing: 14

    // Keine Zahlung ausgewählt
    Card {
        visible: insp.p === null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout { SFIcon { symbol: "calendar.badge.clock"; size: 16; color: Theme.text }
                Text { text: qsTr("Keine Zahlung ausgewählt"); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text } }
            Text { text: qsTr("Wählen Sie eine geplante Zahlung, um Details zu sehen.")
                   font.pixelSize: Theme.callout + 1; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
        }
    }

    Card {
        visible: insp.p !== null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 6
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; text: insp.p ? insp.p.counterparty : ""
                   font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text; Sensitive {} }
            Text { text: insp.p ? Fmt.money(insp.p.amount_cents) : ""; font.pixelSize: Theme.title; font.weight: Font.DemiBold
                   font.features: { "tnum": 1 }; color: insp.p && insp.p.amount_cents < 0 ? Theme.negative : Theme.invest; Sensitive { amounts: true } }
            Text { text: insp.p ? Planned.kindText(insp.p) : ""; font.pixelSize: Theme.callout; color: Theme.secondary }
        }
    }

    Card {
        visible: insp.p !== null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 10
            Field { label: qsTr("Ausführung"); value: insp.p ? Fmt.medium(insp.p.date) : ""; personal: false }
            Field { label: qsTr("Konto"); value: insp.p ? insp.p.account_name : "" }
            Field { visible: !!insp.p && insp.p.counterparty_iban !== ""; label: qsTr("IBAN"); value: insp.p ? Fmt.iban(insp.p.counterparty_iban) : ""; mono: true }
            Field { visible: !!insp.p && insp.p.purpose !== ""; label: qsTr("Verwendungszweck"); value: insp.p ? insp.p.purpose : "" }
        }
    }

    // HStack { PlannedActions(payment: p) }.buttonStyle(.bordered) – wraps in the narrow column
    Flow {
        visible: insp.p !== null
        Layout.fillWidth: true
        spacing: 8
        Button { visible: insp.canEdit && insp.order.can_change; text: qsTr("Dauerauftrag ändern …"); icon.name: SF.icon("pencil")
                 onClicked: Payments.openStandingOrder("edit", insp.order) }
        Button { visible: insp.canEdit && insp.order.can_delete; text: qsTr("Dauerauftrag löschen …"); icon.name: SF.icon("trash")
                 palette.buttonText: Theme.negative; onClicked: Payments.openStandingOrder("delete", insp.order) }
        Button { text: qsTr("Konto anzeigen"); icon.name: SF.icon("building.columns")
                 onClicked: AppModel.sidebarSelection = "account:" + insp.p.account_id }
        Button { visible: !!insp.p && insp.p.counterparty_iban !== ""; text: qsTr("IBAN kopieren"); icon.name: SF.icon("doc.on.doc")
                 onClicked: AppModel.copyToClipboard(insp.p.counterparty_iban) }
    }

    /// field(_:_:mono:sensitive:)
    component Field: ColumnLayout {
        property string label
        property string value
        property bool mono: false
        property bool personal: true
        Layout.fillWidth: true
        spacing: 2
        Text { text: parent.label; font.pixelSize: Theme.caption; color: Theme.secondary }
        TextEdit {
            Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.Wrap
            text: parent.value; font.pixelSize: Theme.callout; color: Theme.text
            font.features: parent.mono ? { "tnum": 1 } : {}
            Sensitive { when: parent.parent.personal }
        }
    }
}
