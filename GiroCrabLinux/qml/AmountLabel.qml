import QtQuick

// BookingListView.swift › AmountLabel: outflows dark slate (or red / orange, Einstellungen ›
// Allgemein › Negative Beträge, #218), inflows an emerald (or blue) badge; the sign always
// carries the direction too (#571).
Rectangle {
    property real cents: 0
    readonly property bool inflow: cents > 0
    implicitWidth: label.implicitWidth + (inflow ? 16 : 0)
    implicitHeight: label.implicitHeight + (inflow ? 6 : 0)
    radius: height / 2
    color: inflow ? BookingPrefs.positiveBadge : "transparent"
    Text {
        id: label
        anchors.centerIn: parent
        text: parent.inflow ? Fmt.signed(parent.cents) : Fmt.money(parent.cents)
        font.pixelSize: Theme.body
        font.weight: parent.inflow ? Font.DemiBold : Font.Medium
        font.features: { "tnum": 1 }
        color: parent.inflow ? BookingPrefs.positiveColor : parent.cents < 0 ? BookingPrefs.negativeColor : Theme.outflow
        Sensitive { amounts: true }
    }
}
