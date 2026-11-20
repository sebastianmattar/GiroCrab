import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// PlanningUI.swift › CashflowCard: income/expense bars per month, quarter or year.
Card {
    id: card
    property string granularity: "Month"
    /// @AppStorage("chartColorPalette")
    readonly property string paletteName: AppModel.uiDefaults.chartColorPalette
    property var rows: []
    readonly property var hexes: ({
        girocrab: { income: ["#38BDF8", "#0284C7"], expense: ["#F97316", "#EA580C"] },
        profi: { income: ["#3B82F6", "#2563EB"], expense: ["#64748B", "#475569"] },
        emerald: { income: ["#10B981", "#059669"], expense: ["#F43F5E", "#E11D48"] } })[paletteName]
    /// Off the GUI thread: every published booking list asks again (a load still running
    /// would otherwise hold the GUI thread at the vault mutex).
    function load() {
        // hidden (Auswertungen off, budgets alone above the list, #544): nothing to load
        if (!AppModel.isUnlocked || !visible) return
        AppModel.loadOffMain("cashflow", () => [["cashflow_by_period", { granularity: card.granularity, count: card.granularity === "Year" ? 5 : 6 }]],
                             r => { card.rows = r[0] })
    }
    onGranularityChanged: load()
    onVisibleChanged: if (visible) load()
    Connections { target: AppModel; function onBookingsChanged() { card.load() } }
    Component.onCompleted: load()
    function label(key) {
        if (granularity !== "Month") return key
        const d = Fmt.date(key + "-01")
        return d ? d.toLocaleDateString(Qt.locale(), "MMM yy") : key
    }
    readonly property real maxValue: {
        let m = 0
        for (const r of rows) m = Math.max(m, Math.abs(r.income_cents), Math.abs(r.expense_cents))
        if (m === 0) return 100
        const step = Math.pow(10, Math.floor(Math.log10(m / 100))) * 100
        return Math.ceil(m / step / 2) * step * 2
    }

    ColumnLayout {
        width: parent.width
        spacing: 8
        RowLayout {
            Layout.fillWidth: true
            Text { text: qsTr("Cashflow"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
            IconButton { symbol: "paintpalette"; symbolSize: 14;
                ToolTip.text: qsTr("Farbschema"); ToolTip.visible: hovered
                onClicked: paletteMenu.popup()
                AppMenu {
                    id: paletteMenu
                    Repeater {
                        model: [{ id: "girocrab", title: qsTr("GiroCrab — Cyan & Orange") }, { id: "profi", title: qsTr("Profi-Banking — Blau & Schiefer") },
                                { id: "emerald", title: qsTr("Klassisch — Smaragd & Rose") }]
                        AppMenuItem { required property var modelData; text: modelData.title; checkable: true; checked: card.paletteName === modelData.id; onTriggered: AppModel.uiDefaults.chartColorPalette = modelData.id }
                    }
                }
            }
            Segmented {
                Layout.preferredWidth: 220
                options: [{ id: "Month", title: qsTr("Monat") }, { id: "Quarter", title: qsTr("Quartal") }, { id: "Year", title: qsTr("Jahr") }]
                current: card.granularity
                onActivated: id => card.granularity = id
            }
        }
        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: 160
            // y axis + dashed grid
            Repeater {
                model: 4
                Item {
                    required property int index
                    readonly property real v: card.maxValue * (3 - index) / 3
                    y: index * (140 / 3); width: parent.width; height: 1
                    Text { text: Number(v / 100).toLocaleString(Qt.locale(), "f", 0); font.pixelSize: Theme.caption; color: Theme.secondary; anchors.verticalCenter: parent.verticalCenter
                           Sensitive { amounts: true } }   // chart axis values
                    Row {
                        x: 44; width: parent.width - 44; spacing: 3; clip: true
                        Repeater { model: Math.max(0, Math.ceil(parent.width / 6)); Rectangle { width: 3; height: 0.5; color: Theme.border } }
                    }
                }
            }
            Row {
                x: 44; width: parent.width - 44; height: 140
                Repeater {
                    model: card.rows
                    Item {
                        required property var modelData
                        width: parent.width / Math.max(1, card.rows.length); height: 140
                        Row {
                            anchors.bottom: parent.bottom; anchors.horizontalCenter: parent.horizontalCenter
                            spacing: width / 8
                            Repeater {
                                model: [{ v: modelData.income_cents, c: card.hexes.income }, { v: Math.abs(modelData.expense_cents), c: card.hexes.expense }]
                                Rectangle {
                                    required property var modelData
                                    anchors.bottom: parent.bottom
                                    width: Math.min(46, parent.parent.width / 4)
                                    height: Math.max(0, modelData.v / card.maxValue * 140)
                                    radius: 5
                                    gradient: Gradient {
                                        GradientStop { position: 0; color: Theme.withAlpha(modelData.c[0], 0.95) }
                                        GradientStop { position: 1; color: Theme.withAlpha(modelData.c[1], 0.45) }
                                    }
                                }
                            }
                        }
                        Text { anchors.horizontalCenter: parent.horizontalCenter; y: 144; text: card.label(modelData.key); font.pixelSize: Theme.caption; color: Theme.secondary }
                    }
                }
            }
        }
        RowLayout {
            spacing: 14
            Repeater {
                model: [{ c: card.hexes.income[0], t: qsTr("Einnahmen") }, { c: card.hexes.expense[0], t: qsTr("Ausgaben") }]
                RowLayout { required property var modelData; spacing: 5
                    Rectangle { width: 8; height: 8; radius: 4; color: modelData.c }
                    Text { text: modelData.t; font.pixelSize: Theme.caption; color: Theme.secondary } }
            }
        }
    }
}
