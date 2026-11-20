import QtQuick
import QtQuick.Layouts

// BalanceHeaderView.swift › KPI tile.
Rectangle {
    id: kpi
    property string title
    property string subtitle
    property string value
    property color tint: Theme.text
    property bool disclosure: false
    signal clicked()
    radius: 9
    color: Theme.canvas
    border.color: Theme.border
    border.width: 1
    implicitHeight: col.implicitHeight + 12
    ColumnLayout {
        id: col
        anchors.fill: parent
        anchors.leftMargin: 10; anchors.rightMargin: 10; anchors.topMargin: 6; anchors.bottomMargin: 6
        spacing: 2
        RowLayout {
            spacing: 3
            Text { text: kpi.title; font.pixelSize: Theme.caption; font.weight: Font.Medium; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: !kpi.disclosure }
            SFIcon { visible: kpi.disclosure; symbol: "chevron.right"; size: 9; color: Theme.tertiary }
            Item { Layout.fillWidth: kpi.disclosure }
        }
        Text { text: kpi.value; font.pixelSize: Theme.title3; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: kpi.tint; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true } }
        Item { Layout.fillHeight: true; implicitHeight: 2 }
        Text { text: kpi.subtitle; font.pixelSize: Theme.caption2; color: Theme.tertiary; wrapMode: Text.WordWrap; maximumLineCount: 2; Layout.fillWidth: true }
    }
    MouseArea { anchors.fill: parent; enabled: kpi.disclosure; cursorShape: Qt.PointingHandCursor; onClicked: kpi.clicked() }
}
