import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// MandateInspector.swift: inspector of „Lastschriften“ – the selected mandate, its open
// returns (Rücklastschriften) and the bookings under it.
ColumnLayout {
    id: insp
    readonly property var m: DirectDebits.selected
    readonly property var returns: { void DirectDebits.openReturns; return DirectDebits.returnsOf(m) }
    spacing: 14

    // Kein Mandat ausgewählt
    Card {
        visible: insp.m === null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout { SFIcon { symbol: "doc.text"; size: 16; color: Theme.text }
                Text { text: qsTr("Kein Mandat ausgewählt"); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text } }
            Text { text: qsTr("Wähle ein Mandat, um Details zu sehen.")
                   font.pixelSize: Theme.callout + 1; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
        }
    }

    Card {
        visible: insp.m !== null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 6
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; text: insp.m ? insp.m.debtor_name : ""
                   font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text; Sensitive {} }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                   text: insp.m ? DirectDebits.schemeTitle(insp.m.scheme) + " · " + DirectDebits.statusText(insp.m) : "" }
        }
    }

    Card {
        visible: insp.m !== null
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 10
            Field { label: qsTr("Mandatsreferenz"); value: insp.m ? insp.m.reference : "" }
            Field { label: qsTr("Unterschrieben am"); value: insp.m ? Fmt.medium(insp.m.signed_date) : ""; personal: false }
            Field { label: qsTr("IBAN"); value: insp.m ? insp.m.debtor_iban : ""; mono: true }
            Field { visible: !!insp.m && insp.m.debtor_bic !== ""; label: qsTr("BIC"); value: insp.m ? insp.m.debtor_bic : ""; mono: true }
            Field { visible: !!insp.m && insp.m.default_amount_cents > 0; label: qsTr("Betrag"); value: insp.m ? Fmt.money(insp.m.default_amount_cents) : ""; mono: true; amount: true }
            Field { visible: !!insp.m && insp.m.default_purpose !== ""; label: qsTr("Verwendungszweck"); value: insp.m ? insp.m.default_purpose : "" }
            Field { visible: !!insp.m && !!insp.m.last_collected; label: qsTr("Zuletzt eingezogen"); value: insp.m && insp.m.last_collected ? Fmt.medium(insp.m.last_collected) : ""; personal: false }
            Field { visible: !!insp.m && insp.m.note !== ""; label: qsTr("Notiz"); value: insp.m ? insp.m.note : "" }
        }
    }

    // returnsCard
    Card {
        visible: insp.m !== null && insp.returns.length > 0
        Layout.fillWidth: true
        id: returnsCard
        readonly property var last: insp.returns.length ? insp.returns[0] : null
        ColumnLayout {
            width: parent.width; spacing: 8
            RowLayout {
                spacing: 6
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 15; color: Theme.negative }
                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; text: qsTr("Rücklastschrift – bitte prüfen")
                       font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.negative }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.text
                   readonly property var r: returnsCard.last
                   text: !r ? "" : Fmt.medium(r.date) + " · " + Fmt.money(Math.abs(r.amount_cents)) + (r.reason === "" ? "" : " · " + r.reason)
                   Sensitive { amounts: true } }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                   text: qsTr("Die Bank hat einen Einzug zurückgegeben. Prüfe die Angaben des Mandats, bevor du erneut einziehst.") }
            // side by side when they fit, else stacked (ViewThatFits)
            Flow {
                Layout.fillWidth: true
                spacing: 8
                Button { text: qsTr("Als geprüft markieren"); icon.name: SF.icon("checkmark.circle"); onClicked: DirectDebits.acknowledgeReturns(insp.m) }
                Button { visible: !!insp.m && insp.m.active; text: qsTr("Deaktivieren"); icon.name: SF.icon("pause.circle")
                         onClicked: DirectDebits.setActive(insp.m, false) }
            }
        }
    }

    // bookingsCard
    Card {
        visible: insp.m !== null && DirectDebits.bookings.length > 0
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 8
            Text { text: qsTr("Buchungen zu diesem Mandat"); font.pixelSize: Theme.caption; color: Theme.secondary }
            Repeater {
                model: DirectDebits.bookings.slice(0, 6)
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: 6
                    Text { text: Fmt.medium(modelData.date); font.pixelSize: Theme.callout; color: Theme.text }
                    Text { visible: modelData.is_return; Layout.fillWidth: true; elide: Text.ElideRight
                           text: modelData.reason === "" ? qsTr("Rücklastschrift") : modelData.reason
                           font.pixelSize: Theme.caption; color: Theme.negative }
                    Item { visible: !modelData.is_return; Layout.fillWidth: true }
                    Text { text: Fmt.money(modelData.amount_cents); font.pixelSize: Theme.callout; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                }
            }
        }
    }

    Flow {
        visible: insp.m !== null
        Layout.fillWidth: true
        spacing: 8
        Button { text: qsTr("Bearbeiten …"); icon.name: SF.icon("pencil"); onClicked: DirectDebits.editMandate(insp.m) }
        Button { text: insp.m && insp.m.active ? qsTr("Deaktivieren") : qsTr("Aktivieren")
                 icon.name: SF.icon(insp.m && insp.m.active ? "pause.circle" : "play.circle")
                 onClicked: DirectDebits.setActive(insp.m, !insp.m.active) }
    }

    /// field(_:_:mono:sensitive:)
    component Field: ColumnLayout {
        property string label
        property string value
        property bool mono: false
        property bool personal: true
        /// money: `.sensitive(.amounts)`
        property bool amount: false
        Layout.fillWidth: true
        spacing: 2
        Text { text: parent.label; font.pixelSize: Theme.caption; color: Theme.secondary }
        TextEdit {
            Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.Wrap
            text: parent.value; font.pixelSize: Theme.callout; color: Theme.text
            font.features: parent.mono ? { "tnum": 1 } : {}
            Sensitive { amounts: parent.parent.amount; when: parent.parent.personal || parent.parent.amount }
        }
    }
}
