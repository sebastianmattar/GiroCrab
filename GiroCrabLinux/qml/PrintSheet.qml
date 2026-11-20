import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore

// PrintSheet.swift (GitHub #53): options on the left, the bookings to print
// (with checkboxes) on the right. "Drucken …" hands the report to the system
// print dialog, which also prints into a PDF file. Paper and orientation are
// chosen there (QTextDocument lays the pages out after the dialog).
Sheet {
    id: sheet
    sheetWidth: 960
    padding: 0
    property var report: null
    /// id -> true
    property var selected: ({})

    /// PrintOptions, remembered across launches (UserDefaults "print.options").
    Settings {
        id: options
        category: "print"
        property bool show_category: true
        property bool show_note: true
        property bool show_totals: true
        property bool group_by_account: false
        property bool show_current_balance: true
        property bool show_daily_balance: false
        property bool first_line_only: false
        property string direction: "all"
        property bool newest_first: true
        property string logo: "header"
    }
    function optionValues() {
        return { show_category: options.show_category, show_note: options.show_note, show_totals: options.show_totals,
                 group_by_account: options.group_by_account, show_current_balance: options.show_current_balance,
                 show_daily_balance: options.show_daily_balance, first_line_only: options.first_line_only,
                 direction: options.direction, newest_first: options.newest_first, logo: options.logo }
    }

    readonly property var visibleRows: report ? PrintReport.rows(report.rows, { direction: options.direction, newest_first: options.newest_first }) : []
    readonly property var selectedRows: visibleRows.filter(r => !!selected[r.id])
    readonly property var selectionTotals: PrintReport.totals(selectedRows)

    onAboutToShow: {
        report = AppModel.printSource()
        const s = {}
        if (report) for (const r of report.rows) s[r.id] = true
        selected = s
    }

    function setSelected(rows, on) {
        const s = Object.assign({}, selected)
        for (const r of rows) { if (on) s[r.id] = true; else delete s[r.id] }
        selected = s
    }

    function printSelection() {
        if (!report || !selectedRows.length) return
        const r = Object.assign({}, report, { rows: selectedRows })
        const opts = optionValues()
        // close() unloads the sheet (lazy Loader in Main.qml): read everything first and
        // hand over AppModel's function itself, not a closure over this sheet's context.
        close()
        Qt.callLater(AppModel.printReport, r, opts)
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        // navigationTitle + toolbar (cancellation / confirmation)
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Item { Layout.fillWidth: true }
            Text { text: qsTr("Drucken"); font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text }
            Item { Layout.fillWidth: true }
            Button {
                text: qsTr("Drucken …"); highlighted: true; enabled: sheet.selectedRows.length > 0; onClicked: sheet.printSelection()
                Shortcut { sequences: ["Return", "Enter"]; enabled: sheet.visible && sheet.selectedRows.length > 0; onActivated: sheet.printSelection() }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        RowLayout {
            Layout.fillWidth: true
            Layout.preferredHeight: 560
            spacing: 0

            // MARK: Options
            ScrollView {
                ScrollBar.vertical: ScrollViewBar {}
                Layout.preferredWidth: 360; Layout.fillHeight: true
                contentWidth: availableWidth
                ColumnLayout {
                    x: 16; width: parent.width - 32
                    spacing: 18
                    Item { implicitHeight: 4 }
                    FormSection {
                        title: qsTr("Auswahl")
                        FormRow {
                            label: qsTr("Buchungen")
                            AppComboBox {
                                Layout.preferredWidth: 190
                                textRole: "title"; model: PrintReport.directions
                                currentIndex: Math.max(0, PrintReport.directions.findIndex(d => d.id === options.direction))
                                onActivated: i => options.direction = PrintReport.directions[i].id
                            }
                        }
                        FormRow {
                            label: qsTr("Reihenfolge")
                            AppComboBox {
                                Layout.preferredWidth: 190
                                model: [qsTr("Neueste zuerst"), qsTr("Älteste zuerst")]
                                currentIndex: options.newest_first ? 0 : 1
                                onActivated: i => options.newest_first = i === 0
                            }
                        }
                        FormRow { label: qsTr("Zeitraum"); value: AppModel.periodTitle }
                    }
                    FormSection {
                        title: qsTr("Inhalt")
                        footers: [options.show_daily_balance ? qsTr("Der Tagessaldo ist der Kontostand am Ende des Tages, unabhängig von der Auswahl.") : ""]
                        FormRow { label: qsTr("Kategorie"); Switch { checked: options.show_category; onToggled: options.show_category = checked } }
                        FormRow { label: qsTr("Notizen"); Switch { checked: options.show_note; onToggled: options.show_note = checked } }
                        FormRow { label: qsTr("Nur erste Zeile des Verwendungszwecks"); Switch { checked: options.first_line_only; onToggled: options.first_line_only = checked } }
                        FormRow { label: qsTr("Summe der Ein- und Ausgänge"); Switch { checked: options.show_totals; onToggled: options.show_totals = checked } }
                        FormRow { label: qsTr("Nach Konto gruppieren"); Switch { checked: options.group_by_account; onToggled: options.group_by_account = checked } }
                        FormRow { label: qsTr("Aktueller Kontostand"); Switch { checked: options.show_current_balance; onToggled: options.show_current_balance = checked } }
                        FormRow { label: qsTr("Tagessaldo"); Switch { checked: options.show_daily_balance; onToggled: options.show_daily_balance = checked } }
                        FormRow {
                            label: qsTr("GiroCrab-Logo")
                            AppComboBox {
                                Layout.preferredWidth: 170
                                textRole: "title"; model: PrintReport.logos
                                currentIndex: Math.max(0, PrintReport.logos.findIndex(d => d.id === options.logo))
                                onActivated: i => options.logo = PrintReport.logos[i].id
                            }
                        }
                    }
                    Item { implicitHeight: 4 }
                }
            }
            Rectangle { Layout.fillHeight: true; width: 1; color: Theme.border }

            // MARK: Bookings
            ColumnLayout {
                Layout.fillWidth: true; Layout.fillHeight: true
                spacing: 0
                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16; Layout.rightMargin: 16; Layout.topMargin: 10; Layout.bottomMargin: 10
                    spacing: 12
                    ColumnLayout {
                        spacing: 2
                        Text { text: qsTr("%1 von %2 ausgewählt").arg(sheet.selectedRows.length).arg(sheet.visibleRows.length)
                               font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
                        Text { text: qsTr("Einnahmen %1 · Ausgaben %2").arg(Fmt.money(sheet.selectionTotals.income)).arg(Fmt.money(sheet.selectionTotals.expense))
                               font.pixelSize: Theme.caption; color: Theme.secondary; font.features: { "tnum": 1 }; Sensitive { amounts: true } }
                    }
                    Item { Layout.fillWidth: true }
                    Button { text: qsTr("Alle auswählen"); enabled: sheet.selectedRows.length < sheet.visibleRows.length; onClicked: sheet.setSelected(sheet.visibleRows, true) }
                    Button { text: qsTr("Auswahl aufheben"); enabled: sheet.selectedRows.length > 0; onClicked: sheet.setSelected(sheet.visibleRows, false) }
                }
                Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
                // ContentUnavailableView
                ColumnLayout {
                    visible: sheet.visibleRows.length === 0
                    Layout.fillWidth: true; Layout.fillHeight: true
                    Item { Layout.fillHeight: true }
                    SFIcon { symbol: "doc.text"; size: 40; Layout.alignment: Qt.AlignHCenter }
                    Text { text: qsTr("Keine Buchungen"); font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text; Layout.alignment: Qt.AlignHCenter }
                    Text { text: qsTr("Im gewählten Zeitraum und Filter gibt es nichts zu drucken."); font.pixelSize: Theme.callout; color: Theme.secondary; Layout.alignment: Qt.AlignHCenter }
                    Item { Layout.fillHeight: true }
                }
                ListView {
                    id: list
                    visible: sheet.visibleRows.length > 0
                    Layout.fillWidth: true; Layout.fillHeight: true
                    clip: true
                    ScrollBar.vertical: AppScrollBar {}
                    model: sheet.visibleRows
                    delegate: Item {
                        id: rowItem
                        required property var modelData
                        width: list.width; height: 46
                        RowLayout {
                            anchors.fill: parent; anchors.leftMargin: 12; anchors.rightMargin: 16
                            spacing: 8
                            CheckBox {
                                checked: !!sheet.selected[rowItem.modelData.id]
                                onToggled: sheet.setSelected([rowItem.modelData], checked)
                            }
                            ColumnLayout {
                                spacing: 2; Layout.fillWidth: true
                                Text { text: rowItem.modelData.counterparty; elide: Text.ElideRight; Layout.fillWidth: true; color: Theme.text; font.pixelSize: Theme.body }
                                Text { text: Fmt.medium(rowItem.modelData.date) + " · " + rowItem.modelData.account_name; elide: Text.ElideRight; Layout.fillWidth: true
                                       color: Theme.secondary; font.pixelSize: Theme.caption }
                            }
                            Text {
                                text: Fmt.money(rowItem.modelData.amount_cents, rowItem.modelData.currency)
                                color: rowItem.modelData.amount_cents > 0 ? Theme.inflow : Theme.text
                                font.pixelSize: Theme.body; font.features: { "tnum": 1 }
                                Sensitive { amounts: true }
                            }
                        }
                        Rectangle { anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.right: parent.right; anchors.leftMargin: 44; height: 1; color: Theme.border; opacity: 0.6 }
                    }
                }
            }
        }
    }
}
