import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// PlanningUI.swift › BudgetProgressCard
Card {
    function tone(p) { return p > 100 ? Theme.negative : (p >= 80 ? Theme.warning : Theme.inflow) }
    ColumnLayout {
        width: parent.width
        spacing: 10
        RowLayout {
            Layout.fillWidth: true
            Text { text: qsTr("Monatsbudgets"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
            IconButton {
                text: qsTr("Budget hinzufügen"); symbol: "plus.circle"; symbolSize: 14
                display: AbstractButton.TextBesideIcon; flat: true
                onClicked: addMenu.popup()
                AppMenu {
                    id: addMenu
                    Repeater { model: addMenu.visible ? AppModel.categories.filter(c => c.budget_cents === null) : []; AppMenuItem { required property var modelData; text: modelData.path; onTriggered: AppModel.budgetCategoryID = modelData.id } }
                }
            }
        }
        Text {
            visible: AppModel.budgets.length === 0
            text: qsTr("Noch keine Budgets. Legen Sie über das Kontextmenü einer Kategorie ein Monatsbudget fest.")
            font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
        }
        Repeater {
            model: AppModel.budgets
            ColumnLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 4
                // Button { editing = IDBox(id: s.categoryId) }
                TapHandler { onTapped: AppModel.budgetCategoryID = modelData.category_id }
                TapHandler { acceptedButtons: Qt.RightButton; onTapped: budgetMenu.popup() }
                HoverHandler { id: rowHover }
                ToolTip.visible: rowHover.hovered; ToolTip.delay: 600; ToolTip.text: qsTr("Budget bearbeiten …")
                AppMenu { id: budgetMenu
                    AppMenuItem { text: qsTr("Budget bearbeiten …"); icon.name: SF.icon("pencil"); onTriggered: AppModel.budgetCategoryID = modelData.category_id }
                    AppMenuItem { text: qsTr("Budget entfernen"); icon.name: SF.icon("trash"); onTriggered: AppModel.setBudget(null, modelData.category_id) } }
                RowLayout {
                    Text { text: modelData.name; font.pixelSize: Theme.callout + 1; color: Theme.text; Layout.fillWidth: true; Sensitive { labels: true } }
                    Text { text: Fmt.money(modelData.spent_cents) + " / " + Fmt.money(modelData.budget_cents); font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: tone(modelData.percent); Sensitive { amounts: true } }
                    // shows the row opens the editor (it looked read-only)
                    SFIcon { symbol: "pencil"; size: Theme.caption; opacity: rowHover.hovered ? 1 : 0 }
                }
                Rectangle {
                    Layout.fillWidth: true; height: 6; radius: 3; color: Theme.border
                    Rectangle { height: 6; radius: 3; color: tone(modelData.percent); width: parent.width * Math.min(Math.max(modelData.percent, 0), 100) / 100 }
                }
            }
        }
    }
}
