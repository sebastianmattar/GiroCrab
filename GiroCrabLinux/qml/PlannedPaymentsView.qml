import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// PlannedPaymentsView.swift: „Geplant“ (GitHub #240) – everything already ordered at the bank
// but not booked yet, across accounts: month cards in the look of the booking stream, account
// and period filter, sums, ↑/↓ / Esc, context menu. State: Planned.qml; standing order
// actions: Payments.qml; inspector: PlannedInspector.qml.
Rectangle {
    id: view
    color: Theme.canvas

    Component.onCompleted: { Planned.active = true; Planned.reload() }
    Component.onDestruction: Planned.active = false

    /// The list flattened for the ListView: a month header, then its rows.
    readonly property var entries: {
        const out = []
        for (const m of Planned.months) {
            out.push({ header: true, month: m })
            m.items.forEach((p, i) => out.push({ header: false, payment: p, isLast: i === m.items.length - 1 }))
        }
        return out
    }
    /// PlannedPaymentsView.onChange(of: selectedPlannedID): the list takes the focus and scrolls to the row.
    Connections {
        target: Planned
        function onSelectedIDChanged() {
            if (Planned.selectedID === null) return
            list.forceActiveFocus()
            const i = view.entries.findIndex(e => !e.header && e.payment.id === Planned.selectedID)
            if (i >= 0) list.positionViewAtIndex(i, ListView.Contain)
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.topMargin: 16
        spacing: 12

        ColumnLayout {
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 4
            Text { text: qsTr("Geplant"); font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Zahlungen, die bei der Bank bereits beauftragt, aber noch nicht gebucht sind.") }
        }

        // filters + „Dauerauftrag anlegen …“
        RowLayout {
            Layout.leftMargin: 20; Layout.rightMargin: 20
            spacing: 12
            // Picker("Konto"): „Alle Konten“ or one account
            AppComboBox {
                id: accountPicker
                Layout.minimumWidth: 150; Layout.maximumWidth: 240; Layout.preferredWidth: 220
                readonly property var choices: [{ id: null, title: qsTr("Alle Konten") }]
                    .concat(AppModel.allAccounts.map(a => ({ id: a.id, title: a.name })))
                model: choices.map(c => c.title)
                currentIndex: Math.max(0, choices.findIndex(c => c.id === Planned.accountFilter))
                onActivated: i => Planned.setAccountFilter(choices[i].id)
                Accessible.name: qsTr("Konto")
                // the selected account's name
                Sensitive { when: Planned.accountFilter !== null }
            }
            // Picker("Zeitraum")
            AppComboBox {
                Layout.preferredWidth: 140
                model: Planned.horizons.map(h => Planned.horizonTitle(h))
                currentIndex: Planned.horizons.indexOf(Planned.horizon)
                onActivated: i => Planned.setHorizon(Planned.horizons[i])
                Accessible.name: qsTr("Zeitraum")
            }
            Item { Layout.fillWidth: true }
            Button {
                visible: Payments.canEditStandingOrders
                text: qsTr("Dauerauftrag anlegen …")
                icon.name: SF.icon("plus")
                onClicked: Payments.openStandingOrder("new", Planned.accountFilter)
                ToolTip.visible: hovered
                ToolTip.delay: 600
                ToolTip.text: qsTr("Legt bei der Bank einen neuen Dauerauftrag an (mit TAN).")
            }
        }

        // totals
        Card {
            Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
            padding: 12
            RowLayout {
                width: parent.width
                spacing: 24
                TotalCell { label: qsTr("Abgehend"); cents: Planned.totals.outgoing_cents }
                TotalCell { label: qsTr("Eingehend"); cents: Planned.totals.incoming_cents }
                Item { Layout.fillWidth: true }
            }
        }

        // ContentUnavailableView
        Item {
            visible: Planned.payments.length === 0
            Layout.fillWidth: true; Layout.fillHeight: true
            ColumnLayout {
                anchors.centerIn: parent
                width: Math.min(parent.width - 40, 420)
                spacing: 8
                SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "calendar.badge.clock"; size: 36; color: Theme.tertiary }
                Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Keine geplanten Zahlungen"); font.pixelSize: Theme.title3; font.weight: Font.DemiBold; color: Theme.text }
                Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap; font.pixelSize: Theme.body; color: Theme.secondary
                       text: qsTr("Terminüberweisungen und Daueraufträge deiner Bank erscheinen hier bis zur Ausführung.") }
            }
        }

        ListView {
            id: list
            visible: Planned.payments.length > 0
            Layout.fillWidth: true; Layout.fillHeight: true
            clip: true
            focus: true
            activeFocusOnTab: true
            boundsBehavior: Flickable.StopAtBounds
            model: view.entries
            bottomMargin: 18
            ScrollBar.vertical: AppScrollBar {}
            // Keyboard like the booking list: ↑/↓ move the selection, Esc clears it.
            Keys.onDownPressed: Planned.moveSelection(1)
            Keys.onUpPressed: Planned.moveSelection(-1)
            Keys.onEscapePressed: e => { if (Planned.selectedID === null) { e.accepted = false; return } Planned.selectedID = null }
            Component.onCompleted: if (!AppModel.screenshotPath) forceActiveFocus()
            delegate: Item {
                id: entry
                required property var modelData
                width: ListView.view.width
                height: (modelData.header ? 14 : 0) + (modelData.header ? head.height : row.height)
                MonthHeader { id: head; visible: entry.modelData.header; y: 14; x: 20; width: parent.width - 40
                              month: entry.modelData.header ? entry.modelData.month : null }
                PlannedRow { id: row; visible: !entry.modelData.header; x: 20; width: parent.width - 40
                             payment: entry.modelData.header ? null : entry.modelData.payment; isLast: !!entry.modelData.isLast }
            }
        }
    }

    // PlannedActions as a context menu
    property var menuPayment: null
    readonly property var menuOrder: menuPayment ? Payments.standingOrderFor(menuPayment) : null
    AppMenu {
        id: rowMenu
        AppMenuItem { visible: !!view.menuOrder && Payments.canEditStandingOrders && view.menuOrder.can_change; height: visible ? implicitHeight : 0
                      text: qsTr("Dauerauftrag ändern …"); icon.name: SF.icon("pencil"); onTriggered: Payments.openStandingOrder("edit", view.menuOrder) }
        AppMenuItem { visible: !!view.menuOrder && Payments.canEditStandingOrders && view.menuOrder.can_delete; height: visible ? implicitHeight : 0
                      text: qsTr("Dauerauftrag löschen …"); icon.name: SF.icon("trash"); onTriggered: Payments.openStandingOrder("delete", view.menuOrder) }
        AppMenuSeparator { visible: !!view.menuOrder && Payments.canEditStandingOrders; height: visible ? implicitHeight : 0 }
        AppMenuItem { text: qsTr("Konto anzeigen"); icon.name: SF.icon("building.columns")
                      onTriggered: AppModel.sidebarSelection = "account:" + view.menuPayment.account_id }
        AppMenuItem { visible: !!view.menuPayment && view.menuPayment.counterparty_iban !== ""; height: visible ? implicitHeight : 0
                      text: qsTr("IBAN kopieren"); icon.name: SF.icon("doc.on.doc"); onTriggered: AppModel.copyToClipboard(view.menuPayment.counterparty_iban) }
    }

    component TotalCell: ColumnLayout {
        property string label
        property real cents: 0
        spacing: 2
        Text { text: parent.label; font.pixelSize: Theme.caption; color: Theme.secondary }
        Text { text: Fmt.money(parent.cents); font.pixelSize: Theme.title3; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
               color: Theme.text; Sensitive { amounts: true } }
    }

    /// monthHeader: top of a month card, like the day header of the booking stream.
    component MonthHeader: Item {
        property var month: null
        height: 40
        CardSegment { anchors.fill: parent; isTop: true }
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: 16; anchors.rightMargin: 16
            Text { Layout.fillWidth: true; font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text
                   text: { if (!parent.parent.month) return ""; const d = Fmt.date(parent.parent.month.id + "-01")
                           return d ? d.toLocaleDateString(Fmt.locale, "MMMM yyyy") : parent.parent.month.id } }
            Text { readonly property real sum: parent.parent.month ? parent.parent.month.sumCents : 0
                   text: Fmt.signed(sum); font.pixelSize: Theme.subheadline + 1; font.features: { "tnum": 1 }
                   color: sum > 0 ? Theme.inflow : Theme.secondary; Sensitive { amounts: true } }
        }
    }

    /// PlannedRow: one planned payment as a slice of a month card (same look as a booking row).
    component PlannedRow: Item {
        id: pr
        property var payment: null
        property bool isLast: false
        readonly property bool selected: payment !== null && Planned.selectedID === payment.id
        height: Math.max(44, content.implicitHeight) + 10
        CardSegment { anchors.fill: parent; isBottom: pr.isLast }
        Rectangle {
            anchors.fill: parent; anchors.margins: 1
            anchors.bottomMargin: pr.isLast ? 4 : 0
            radius: pr.isLast ? 10 : 0
            color: pr.selected ? Theme.selection : "transparent"
        }
        RowLayout {
            id: content
            anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
            anchors.leftMargin: 14; anchors.rightMargin: 14; anchors.topMargin: 5
            spacing: 12
            Rectangle {
                Layout.alignment: Qt.AlignTop
                width: 36; height: 36; radius: 18
                color: Theme.withAlpha(Theme.accent, 0.12)
                SFIcon { anchors.centerIn: parent; symbol: pr.payment ? Planned.kindSymbol(pr.payment) : ""; size: 16; color: Theme.accent }
            }
            ColumnLayout {
                Layout.fillWidth: true; Layout.alignment: Qt.AlignTop
                spacing: 2
                Text { Layout.fillWidth: true; elide: Text.ElideRight; font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text
                       text: pr.payment ? pr.payment.counterparty : ""; Sensitive {} }
                Text { Layout.fillWidth: true; elide: Text.ElideRight; font.pixelSize: Theme.callout; color: Theme.secondary
                       text: !pr.payment ? "" : pr.payment.purpose === "" ? pr.payment.account_name : pr.payment.purpose + " · " + pr.payment.account_name
                       Sensitive {} }
                Text { text: pr.payment ? Planned.kindText(pr.payment) : ""; font.pixelSize: Theme.caption; color: Theme.tertiary }
            }
            ColumnLayout {
                Layout.alignment: Qt.AlignTop
                spacing: 2
                AmountLabel { Layout.alignment: Qt.AlignRight; cents: pr.payment ? pr.payment.amount_cents : 0 }
                Text { Layout.alignment: Qt.AlignRight; text: pr.payment ? Fmt.medium(pr.payment.date) : ""
                       font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary }
            }
        }
        Rectangle {
            visible: !pr.isLast
            x: 64; width: parent.width - x - 1; height: 1
            anchors.bottom: parent.bottom; color: Theme.withAlpha(Theme.border, 0.7)
        }
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            onClicked: m => {
                list.forceActiveFocus()
                if (m.button === Qt.RightButton) {
                    view.menuPayment = pr.payment
                    rowMenu.popup(pr, m.x, m.y)
                    return
                }
                Planned.selectedID = pr.payment.id
            }
        }
        Accessible.role: Accessible.Button
        Accessible.name: payment ? AppModel.masking(payment.counterparty) + ", " + AppModel.masking(Fmt.money(payment.amount_cents), AppModel.redactAmounts) + ", " + Fmt.medium(payment.date) : ""
    }
}
