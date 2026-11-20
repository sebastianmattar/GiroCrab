import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// PlanningUI.swift › BudgetEditorSheet: monthly budget of one category
// (`AppModel.budgetCategoryID`).
Sheet {
    id: sheet
    readonly property var category: AppModel.categoryByID(AppModel.budgetCategoryID)
    readonly property var cents: { const c = AppModel.parseCents(field.text); return c !== null && c > 0 ? c : null }
    visible: AppModel.budgetCategoryID !== null
    sheetWidth: 380
    padding: 22
    onAboutToShow: {
        field.text = category && category.budget_cents !== null ? Fmt.plain(category.budget_cents) : ""
        field.forceActiveFocus()
    }
    onClosed: AppModel.budgetCategoryID = null
    function save() {
        if (cents === null) return
        const id = category.id, c = cents
        close()
        AppModel.setBudget(c, id)
    }
    ColumnLayout {
        width: sheet.availableWidth
        spacing: 14
        Text { text: qsTr("Monatsbudget"); font.pixelSize: Theme.title3 + 1; font.weight: Font.Bold; color: Theme.text }
        Text { text: sheet.category ? sheet.category.path : ""; font.pixelSize: Theme.body; color: Theme.secondary }
        RowLayout {
            TextField {
                id: field
                Layout.preferredWidth: 160
                placeholderText: Fmt.plain(0)
                horizontalAlignment: TextInput.AlignRight
                font.features: { "tnum": 1 }
                onAccepted: sheet.save()
                Sensitive { amounts: true; when: parent.text !== "" }
            }
            Text { text: qsTr("€ pro Monat"); font.pixelSize: Theme.body; color: Theme.secondary }
        }
        Text { text: qsTr("Ausgaben der Unterkategorien zählen mit."); font.pixelSize: Theme.caption; color: Theme.tertiary }
        RowLayout {
            Layout.fillWidth: true
            Button {
                visible: sheet.category !== null && sheet.category.budget_cents !== null
                text: qsTr("Budget entfernen")
                palette.buttonText: Theme.negative
                onClicked: { const id = sheet.category.id; sheet.close(); AppModel.setBudget(null, id) }
            }
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button { text: qsTr("Speichern"); highlighted: true; enabled: sheet.cents !== null; onClicked: sheet.save() }
        }
    }
}
