import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// RecurringUI.swift › RecurringSheet ("Fixkosten"): what is booked / still due this month,
// all series, hidden ones. Double-click (or context menu) shows the booking.
Sheet {
    id: sheet
    sheetWidth: 600
    visible: AppModel.showRecurringSheet
    onClosed: AppModel.showRecurringSheet = false
    readonly property var month: AppModel.recurringMonth

    function show(bookingID) { sheet.close(); AppModel.revealBooking(bookingID) }

    AppMenu {
        id: actions
        property var series: null
        property var bookingID: null
        property bool booked: false
        AppMenuItem { visible: actions.bookingID !== null; height: visible ? implicitHeight : 0
                   text: actions.booked ? qsTr("Buchung anzeigen") : qsTr("Letzte Buchung anzeigen"); onTriggered: sheet.show(actions.bookingID) }
        AppMenuSeparator { visible: actions.bookingID !== null; height: visible ? implicitHeight : 0 }
        // seriesActions › „Wiederkehrend“ (#581): the interval of the whole series; the series'
        // latest booking carries the manual mark (AppModel+Recurring.swift › setRecurringInterval)
        AppMenu {
            title: qsTr("Wiederkehrend")
            Repeater {
                model: ["Monthly", "Quarterly", "HalfYearly", "Yearly"]
                AppMenuItem {
                    id: intervalItem
                    required property string modelData
                    readonly property bool current: actions.series !== null && actions.series.interval === modelData
                    text: AppModel.intervalLabels[modelData]
                    checkable: true
                    checked: current
                    onTriggered: {
                        checked = Qt.binding(() => intervalItem.current)   // the click flipped it
                        if (actions.series !== null && !current)
                            AppModel.markRecurring({ id: actions.series.last_booking_id }, modelData)
                    }
                }
            }
        }
        AppMenuItem { visible: actions.series !== null && actions.series.source === "Manual"; height: visible ? implicitHeight : 0
                   text: qsTr("Automatisch erkennen"); onTriggered: AppModel.resetRecurring(actions.series.series_key) }
        AppMenuItem { text: qsTr("Nicht wiederkehrend"); onTriggered: AppModel.ignoreRecurring(actions.series) }
        function openFor(series, bookingID, booked) { actions.series = series; actions.bookingID = bookingID; actions.booked = booked; popup() }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0
        RowLayout {
            Layout.fillWidth: true
            Layout.bottomMargin: 12
            Text { text: qsTr("Wiederkehrende Buchungen"); font.pixelSize: Theme.body + 2; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
            Button { text: qsTr("Fertig"); highlighted: true; onClicked: sheet.close() }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(list.implicitHeight + 24, 560)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: list
                y: 12
                width: scroll.availableWidth - 4
                spacing: 18
                SettingsFormSection {
                    visible: sheet.month !== null
                    title: qsTr("Diesen Monat")
                    footer: sheet.month ? qsTr("Noch offen: %1 Ausgaben, %2 Einnahmen · bereits gebucht: %3")
                        .arg(AppModel.masking(Fmt.money(-sheet.month.open_expense_cents), AppModel.redactAmounts)).arg(AppModel.masking(Fmt.money(sheet.month.open_income_cents), AppModel.redactAmounts))
                        .arg(AppModel.masking(Fmt.money(-sheet.month.booked_expense_cents), AppModel.redactAmounts)) : ""
                    Text { visible: sheet.month !== null && sheet.month.items.length === 0; text: qsTr("Diesen Monat ist nichts fällig."); font.pixelSize: Theme.body; color: Theme.secondary }
                    Repeater {
                        model: sheet.month ? sheet.month.items : []
                        SeriesRow {
                            required property var modelData
                            readonly property var target: modelData.booking_id !== null ? modelData.booking_id : modelData.series.last_booking_id
                            symbol: modelData.booked ? "checkmark.circle.fill" : "circle"
                            symbolColor: modelData.booked ? Theme.inflow : Theme.tertiary
                            symbolHelp: modelData.booked ? qsTr("Gebucht") : qsTr("Offen")
                            name: modelData.series.name
                            detail: modelData.booked ? qsTr("Gebucht am %1").arg(Fmt.medium(modelData.due_date)) : qsTr("Fällig am %1").arg(Fmt.medium(modelData.due_date))
                            cents: modelData.amount_cents
                            onActivated: sheet.show(target)
                            onMenuRequested: actions.openFor(modelData.series, target, modelData.booked)
                        }
                    }
                }
                SettingsFormSection {
                    title: qsTr("Alle (%1)").arg(AppModel.recurring.length)
                    Text { visible: AppModel.recurring.length === 0; Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.body; color: Theme.secondary
                           text: qsTr("Noch keine wiederkehrenden Buchungen erkannt. Sie können eine Buchung in der Detailansicht als wiederkehrend markieren.") }
                    Repeater {
                        model: AppModel.recurring
                        SeriesRow {
                            id: sr
                            required property var modelData
                            symbol: modelData.source === "Manual" ? "hand.point.up.left" : "sparkles"
                            symbolHelp: modelData.source === "Manual" ? qsTr("Manuell markiert") : qsTr("Automatisch erkannt")
                            name: modelData.name
                            detail: AppModel.intervalText(modelData) + " · " + qsTr("nächste am %1").arg(Fmt.medium(modelData.next_due))
                            cents: modelData.amount_cents
                            showsMenuButton: true
                            onActivated: sheet.show(modelData.last_booking_id)
                            onMenuRequested: actions.openFor(modelData, modelData.last_booking_id, false)
                            onMenuButtonClicked: actions.openFor(modelData, null, false)
                        }
                    }
                }
                SettingsFormSection {
                    visible: AppModel.recurringHidden.length > 0
                    title: qsTr("Ausgeblendet (%1)").arg(AppModel.recurringHidden.length)
                    Repeater {
                        model: AppModel.recurringHidden
                        RowLayout {
                            id: hr
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: 10
                            SFIcon { symbol: "eye.slash"; size: 15; color: Theme.tertiary }
                            ColumnLayout { spacing: 1; Layout.fillWidth: true
                                Text { text: hr.modelData.name; font.pixelSize: Theme.body; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                                Text { text: qsTr("Zuletzt am %1").arg(Fmt.medium(hr.modelData.last_date)); font.pixelSize: Theme.caption; color: Theme.tertiary } }
                            Text { text: Fmt.money(hr.modelData.amount_cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
                            Button { flat: true; text: qsTr("Einblenden"); onClicked: AppModel.resetRecurring(hr.modelData.series_key) }
                            TapHandler { acceptedButtons: Qt.RightButton; onTapped: hiddenMenu.popup() }
                            AppMenu { id: hiddenMenu; AppMenuItem { text: qsTr("Wieder einblenden"); onTriggered: AppModel.resetRecurring(hr.modelData.series_key) } }
                        }
                    }
                }
                Item { implicitHeight: 4 }
            }
        }
    }

    component SeriesRow: RowLayout {
        id: row
        property string symbol
        property color symbolColor: Theme.tertiary
        property string symbolHelp
        property string name
        property string detail
        property real cents
        property bool showsMenuButton: false
        signal activated()
        signal menuRequested()
        signal menuButtonClicked()
        Layout.fillWidth: true
        spacing: 10
        SFIcon { symbol: row.symbol; size: 15; color: row.symbolColor
                 ToolTip.visible: symHover.hovered; ToolTip.text: row.symbolHelp; HoverHandler { id: symHover } }
        ColumnLayout { spacing: 1; Layout.fillWidth: true
            Text { text: row.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
            Text { text: row.detail; font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true } }
        Text { text: Fmt.money(row.cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: row.cents < 0 ? Theme.outflow : Theme.inflow; Sensitive { amounts: true } }
        IconButton { visible: row.showsMenuButton; symbol: "ellipsis.circle"; ToolTip.text: qsTr("Aktionen"); onClicked: row.menuButtonClicked() }
        TapHandler { onDoubleTapped: row.activated() }
        TapHandler { acceptedButtons: Qt.RightButton; onTapped: row.menuRequested() }
    }
}
