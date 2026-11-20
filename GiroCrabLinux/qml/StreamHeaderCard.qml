import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// BalanceHeaderView.swift › StreamHeaderCard: title, balance, chart, KPI tiles, filter row.
Card {
    id: card
    readonly property var account: AppModel.selectedAccount
    readonly property bool isDepot: account !== null && (account.kind === "Brokerage" || account.name.toLowerCase().indexOf("depot") >= 0)
    /// Sachwerte (#87): items instead of securities, a value chart, „Sachwert hinzufügen“.
    readonly property bool isTangible: account !== null && AppModel.isTangible(account.id)
    readonly property var selectedItem: account !== null ? AppModel.selectedAssetItem(account.id) : null
    /// The selected item's value instead of the account's.
    readonly property var balance: selectedItem
        ? { label: selectedItem.sold ? qsTr("Verkauft") : qsTr("Wert heute"), cents: selectedItem.holding.value_cents }
        : AppModel.headerBalance
    /// Sachwerte: "Sachwerte · Auto (Kombi)" while an item is selected, else "Sachwerte · Gesamt".
    readonly property string headerTitle: isTangible ? AppModel.title + " · " + (selectedItem ? selectedItem.setup.name : qsTr("Gesamt")) : AppModel.title
    /// The item's history follows the selected period like the account chart.
    readonly property var chartPoints: {
        if (!selectedItem) return AppModel.history
        const r = AppModel.periodRange()
        return AppModel.selectedAssetHistory.filter(p => (!r[0] || p.date >= r[0]) && (!r[1] || p.date <= r[1]))
    }
    /// AccountCopyMenu.bankDetails: name, IBAN and bank as a block for an e-mail.
    function bankDetails(bic) {
        const a = account
        const bank = bic ? (a.bank_name ? "BIC: " + bic + " (" + a.bank_name + ")" : "BIC: " + bic) : (a.bank_name ? "Bank: " + a.bank_name : "")
        return [a.owner ? "Name: " + a.owner : "", "IBAN: " + Fmt.iban(a.iban), bank].filter(s => s).join("\n")
    }
    readonly property int openFixed: AppModel.recurringMonth ? AppModel.recurringMonth.items.filter(i => !i.booked && i.amount_cents < 0).length : 0
    /// `@AppStorage(ViewLayout.compactHeaderKey)`: title, balance and the filter row only (#358).
    readonly property bool compact: AppModel.compactHeader
    readonly property string compactHeaderTitle: compact ? qsTr("Kopfbereich einblenden") : qsTr("Kopfbereich minimieren")

    ColumnLayout {
        width: parent.width
        spacing: 9
        RowLayout {
            Layout.fillWidth: true
            ColumnLayout {
                spacing: 3
                Layout.fillWidth: true
                Layout.maximumWidth: implicitWidth   // the chevron sits right after the title
                Layout.alignment: Qt.AlignTop
                // minimised: .headline (body size, semibold) instead of .title2
                Text { text: card.headerTitle; font.pixelSize: card.compact ? Theme.body + 2 : Theme.title2 + 3; font.weight: Font.DemiBold; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true
                       Sensitive { labels: card.account === null } }   // an account name, or a group / category
                Text {
                    visible: !card.compact
                    text: card.account ? [card.account.bank_name, Fmt.iban(card.account.iban)].filter(s => s).join(" · ") : AppModel.lastSyncText
                    font.pixelSize: Theme.caption; color: Theme.secondary
                    Sensitive { when: card.account !== null }   // IBAN
                }
                // BalanceHeaderView.swift: the account note (AccountEditSheet, #605)
                Text {
                    readonly property string note: card.account ? (card.account.note || "") : ""
                    visible: note !== "" && !card.compact
                    text: note; font.pixelSize: Theme.caption; color: Theme.secondary
                    wrapMode: Text.WordWrap; maximumLineCount: 2; elide: Text.ElideRight; Layout.fillWidth: true
                    HoverHandler { id: noteHover }
                    ToolTip.visible: noteHover.hovered && truncated; ToolTip.delay: 600; ToolTip.text: AppModel.masking(note)
                    Sensitive {}
                }
                // AccountCopyMenu on the title
                TapHandler {
                    acceptedButtons: Qt.RightButton
                    enabled: card.account !== null
                    onTapped: { accountCopyMenu.bic = AppModel.run(() => AppModel.core("bic_for_iban", { iban: card.account.iban })) || ""; accountCopyMenu.popup() }
                }
                AppMenu {
                    id: accountCopyMenu
                    property string bic: ""
                    AppMenuSeparator { contentItem: Text { text: qsTr("In Zwischenablage kopieren"); font.pixelSize: Theme.caption; color: Theme.secondary; leftPadding: 8 } }
                    AppMenuItem { text: qsTr("Bankverbindung"); visible: card.account !== null && card.account.iban !== ""; height: visible ? implicitHeight : 0
                               onTriggered: AppModel.copyToClipboard(card.bankDetails(accountCopyMenu.bic)) }
                    AppMenuItem { text: qsTr("IBAN"); visible: card.account !== null && card.account.iban !== ""; height: visible ? implicitHeight : 0
                               onTriggered: AppModel.copyToClipboard(card.account.iban) }
                    AppMenuItem { text: qsTr("BIC"); visible: accountCopyMenu.bic !== ""; height: visible ? implicitHeight : 0
                               onTriggered: AppModel.copyToClipboard(accountCopyMenu.bic) }
                    AppMenuItem { text: qsTr("Kontostand"); onTriggered: AppModel.copyToClipboard(Fmt.clipboard(card.account.balance_cents)) }
                }
            }
            // Minimise the header: more room for the booking list (GitHub #358).
            Item {
                Layout.alignment: Qt.AlignTop
                implicitWidth: compactIcon.width + 8; implicitHeight: compactIcon.height + 8
                SFIcon { id: compactIcon; anchors.centerIn: parent; symbol: "chevron.down"; size: 10; color: Theme.secondary
                         rotation: card.compact ? 0 : 180 }   // chevron.down / chevron.up
                MouseArea { id: compactArea; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor
                            onClicked: AppModel.compactHeader = !AppModel.compactHeader }
                ToolTip.visible: compactArea.containsMouse; ToolTip.delay: 600; ToolTip.text: card.compactHeaderTitle
                Accessible.role: Accessible.Button; Accessible.name: card.compactHeaderTitle
            }
            Item { Layout.fillWidth: true; Layout.preferredWidth: 0 }
            ColumnLayout {
                visible: card.balance !== null
                spacing: 2
                Layout.alignment: Qt.AlignTop
                // "<Label> kopieren" on the balance
                TapHandler {
                    acceptedButtons: Qt.RightButton
                    onTapped: balanceCopyMenu.popup()
                }
                AppMenu {
                    id: balanceCopyMenu
                    AppMenuItem { text: card.balance ? qsTr("%1 kopieren").arg(card.balance.label) : ""; icon.name: SF.icon("doc.on.doc")
                               onTriggered: AppModel.copyToClipboard(Fmt.clipboard(card.balance.cents)) }
                }
                Text {
                    Layout.alignment: Qt.AlignRight
                    text: card.balance ? Fmt.money(card.balance.cents) : ""
                    font.pixelSize: card.compact ? Theme.title3 + 3 : Theme.largeTitle + 4; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                    color: card.account && card.account.kind === "Loan" ? Theme.loan : Theme.text
                    Sensitive { amounts: true }
                }
                Text { visible: !card.compact; Layout.alignment: Qt.AlignRight; text: card.balance ? card.balance.label : ""; font.pixelSize: Theme.caption; color: Theme.secondary }
            }
        }

        LoanProgressCard {
            visible: !card.compact && card.account !== null && card.account.kind === "Loan"
            Layout.fillWidth: true
            account: card.account
            compact: true
        }
        HoldingsCard {
            visible: card.isDepot && !card.isTangible
            Layout.fillWidth: true
            account: card.isDepot ? card.account : null
        }

        BalanceChart {
            visible: !card.compact && (!card.isDepot || card.isTangible) && AppModel.showStatistics && AppModel.scope !== "Investments"
                     && !AppModel.sidebarSelection.startsWith("category:")
            Layout.fillWidth: true
            Layout.preferredHeight: 72
            points: card.chartPoints
            onSelectDay: day => AppModel.jumpToDay(day)
        }

        // A category: its monthly sums instead of the balance (CategorySumChart.swift, #502).
        CategorySumChart {
            visible: !card.compact && !card.isDepot && AppModel.showStatistics && AppModel.scope !== "Investments"
                     && AppModel.sidebarSelection.startsWith("category:")
            Layout.fillWidth: true
            Layout.preferredHeight: 72
            sums: AppModel.categoryHistory
        }

        RowLayout {
            visible: !card.isDepot && !card.compact
            Layout.fillWidth: true
            spacing: 10
            KPI { Layout.fillWidth: true; Layout.fillHeight: true; Layout.preferredWidth: 1
                  title: qsTr("Konsumausgaben"); subtitle: AppModel.periodSubtitle; value: AppModel.isLoadingSelection ? "–" : Fmt.money(-AppModel.consumptionThisMonth); tint: Theme.outflow }
            KPI { Layout.fillWidth: true; Layout.fillHeight: true; Layout.preferredWidth: 1
                  title: qsTr("Investiert"); subtitle: qsTr("Sparpläne & Perks"); value: AppModel.isLoadingSelection ? "–" : Fmt.money(AppModel.investedThisMonth); tint: Theme.invest }
            KPI { Layout.fillWidth: true; Layout.fillHeight: true; Layout.preferredWidth: 1
                  title: qsTr("Einnahmen"); subtitle: AppModel.periodSubtitle; value: AppModel.isLoadingSelection ? "–" : Fmt.money(AppModel.incomeThisMonth); tint: Theme.inflow }
            KPI { Layout.fillWidth: true; Layout.fillHeight: true; Layout.preferredWidth: 1
                  title: qsTr("Fixkosten offen"); subtitle: qsTr("Diesen Monat · %1 offen").arg(card.openFixed)
                  value: Fmt.money(-(AppModel.recurringMonth ? AppModel.recurringMonth.open_expense_cents : 0)); tint: Theme.outflow
                  disclosure: true
                  onClicked: AppModel.showRecurringSheet = true
                  ToolTip.visible: fixedHover.hovered; ToolTip.delay: 600; ToolTip.text: qsTr("Wiederkehrende Buchungen anzeigen")
                  HoverHandler { id: fixedHover } }
        }

        // filterRow
        RowLayout {
            visible: !card.isDepot
            Layout.fillWidth: true
            spacing: 12
            MenuLabel {
                symbol: "calendar"; text: AppModel.periodTitle
                onClicked: { periodMenu.rebuild(); periodMenu.popup() }
                // MoneyMoney's sections (#382), separated by dividers; rebuilt on every open
                // because the named months, quarters and years count back from today.
                Component { id: periodEntry
                    AppMenuItem { property string periodID; text: AppModel.periodTitleOf(periodID); checkable: true
                                  checked: AppModel.period === periodID; onTriggered: AppModel.choosePeriod(periodID) } }
                Component { id: periodDivider; AppMenuSeparator {} }
                AppMenu {
                    id: periodMenu
                    property var entries: []
                    function rebuild() {
                        for (const o of entries) removeItem(o)   // Menu.removeItem destroys it
                        const built = []
                        for (const section of AppModel.periodMenu()) {
                            for (const id of section) built.push(periodEntry.createObject(null, { periodID: id }))
                            built.push(periodDivider.createObject(null))
                        }
                        built.forEach((o, i) => insertItem(i, o))
                        entries = built
                    }
                    // The checkmark shows while the custom range is active (PlanningUI.swift › CustomPeriodForm).
                    AppMenuItem {
                        text: AppModel.period === "custom" ? qsTr("Benutzerdefiniert: %1 …").arg(AppModel.periodTitle) : qsTr("Benutzerdefiniert …")
                        checkable: true; checked: AppModel.period === "custom"
                        onTriggered: { checked = Qt.binding(() => AppModel.period === "custom"); AppModel.showCustomPeriod = true }
                    }
                }
            }
            MenuLabel {
                symbol: AppModel.statusFilter === "all" && AppModel.directionFilter === "All" ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill"
                text: AppModel.filterTitle
                onClicked: filterMenu.popup()
                AppMenu {
                    id: filterMenu
                    AppMenuSeparator { contentItem: Text { text: qsTr("Status"); font.pixelSize: Theme.caption; color: Theme.secondary; leftPadding: 8 } }
                    Repeater {
                        model: AppModel.statusFilters
                        AppMenuItem { required property var modelData; text: modelData.title; checkable: true
                                   checked: AppModel.statusFilter === modelData.id; onTriggered: AppModel.statusFilter = modelData.id }
                    }
                    AppMenuSeparator { contentItem: Text { text: qsTr("Buchungsart"); font.pixelSize: Theme.caption; color: Theme.secondary; leftPadding: 8 } }
                    Repeater {
                        model: AppModel.directions
                        AppMenuItem { required property var modelData; text: modelData.title; checkable: true
                                   checked: AppModel.directionFilter === modelData.id; onTriggered: AppModel.directionFilter = modelData.id }
                    }
                }
            }
            // TagFilterUI.swift › TagFilterButton (#228, part 3): the popover anchors on the button
            MenuLabel {
                id: tagFilterButton
                visible: TagFilter.available
                symbol: TagFilter.isActive ? "tag.fill" : "tag"; text: TagFilter.title; chevron: false
                tint: TagFilter.isActive ? Theme.accent : Theme.text
                onClicked: TagFilter.shown = !TagFilter.shown
                ToolTip.visible: hovered && !TagFilter.shown; ToolTip.delay: 600; ToolTip.text: qsTr("Nach Tags filtern")
                TagFilterPopover { wanted: TagFilter.shown && tagFilterButton.visible }
            }
            // Category views: only the bookings of the chosen accounts (GitHub #406).
            MenuLabel {
                visible: AppModel.showsCategoryAccountFilter && AppModel.allAccounts.length > 1
                symbol: AppModel.categoryAccountIDs.length ? "building.columns.fill" : "building.columns"
                text: AppModel.categoryAccountFilterTitle
                onClicked: accountMenu.popup()
                ToolTip.visible: hovered; ToolTip.delay: 600; ToolTip.text: qsTr("Nur Buchungen ausgewählter Konten anzeigen")
                AppMenu {
                    id: accountMenu
                    AppMenuItem { text: qsTr("Alle Konten"); checkable: true
                                  checked: AppModel.categoryAccountIDs.length === 0
                                  onTriggered: { checked = Qt.binding(() => AppModel.categoryAccountIDs.length === 0); AppModel.setCategoryAccountFilter([]) } }
                    AppMenuSeparator {}
                    Repeater {
                        model: AppModel.allAccounts
                        AppMenuItem {
                            required property var modelData
                            text: !modelData.bank_name || modelData.name.toLowerCase().indexOf(modelData.bank_name.toLowerCase()) >= 0
                                  ? modelData.name : modelData.name + " · " + modelData.bank_name
                            checkable: true
                            checked: AppModel.categoryAccountIDs.indexOf(modelData.id) >= 0
                            onTriggered: { checked = Qt.binding(() => AppModel.categoryAccountIDs.indexOf(modelData.id) >= 0); AppModel.toggleCategoryAccount(modelData.id) }
                        }
                    }
                }
            }
            MenuLabel { visible: card.account !== null; symbol: "doc.text"; text: qsTr("Dokumente"); chevron: false
                        onClicked: AppModel.openAccountDocuments(card.account) }
            Item { Layout.fillWidth: true }
            Text { text: qsTr("%1 Buchungen").arg(AppModel.bookingCountLabel); font.pixelSize: Theme.caption; color: Theme.secondary }
        }
        // depotRow
        RowLayout {
            visible: card.isDepot
            Layout.fillWidth: true
            IconButton { text: qsTr("Dokumente"); symbol: "doc.text"; symbolSize: 12; font.pixelSize: Theme.caption
                         onClicked: AppModel.openAccountDocuments(card.account) }
            Item { Layout.fillWidth: true }
            Text { visible: !card.isTangible; text: qsTr("Depot: Positionen statt Buchungen"); font.pixelSize: Theme.caption; color: Theme.secondary }
            BusyIndicator { visible: card.isTangible && AppModel.assetRefreshing; running: visible; implicitWidth: 18; implicitHeight: 18 }
            IconButton {
                visible: card.isTangible && !AppModel.assetRefreshing && card.account !== null
                         && AppModel.assetItems(card.account.id).some(i => i.setup.price_source !== "manual")
                text: qsTr("Goldpreis aktualisieren"); symbol: "arrow.clockwise"; symbolSize: 12; font.pixelSize: Theme.caption
                ToolTip.text: qsTr("Goldpreis jetzt abrufen")
                onClicked: AppModel.refreshAssetPrices(true)
            }
            IconButton {
                visible: card.isTangible
                text: qsTr("Sachwert hinzufügen"); symbol: "plus"; symbolSize: 12; font.pixelSize: Theme.caption
                onClicked: AppModel.openAssetEditor(card.account.id)
            }
        }
    }

    component MenuLabel: Item {
        id: ml
        property string symbol
        property string text
        property bool chevron: true
        property color tint: Theme.text
        readonly property bool hovered: mlArea.containsMouse
        signal clicked()
        implicitWidth: r.implicitWidth; implicitHeight: 22
        // keyboard: Tab reaches it, Space / Return opens it
        activeFocusOnTab: true
        Keys.onSpacePressed: ml.clicked()
        Keys.onReturnPressed: ml.clicked()
        Keys.onEnterPressed: ml.clicked()
        Accessible.role: Accessible.Button
        Accessible.name: ml.text
        Accessible.onPressAction: ml.clicked()
        Rectangle {
            visible: ml.activeFocus
            anchors.fill: parent; anchors.margins: -3
            radius: 5; color: "transparent"; border.width: 2; border.color: Theme.accent
        }
        RowLayout {
            id: r
            anchors.verticalCenter: parent.verticalCenter
            spacing: 5
            SFIcon { symbol: ml.symbol; size: 14; color: ml.tint }
            Text { text: ml.text; font.pixelSize: Theme.body; color: ml.tint }
            SFIcon { visible: ml.chevron; symbol: "chevron.down"; size: 9; color: ml.tint }
        }
        MouseArea { id: mlArea; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor; onClicked: ml.clicked() }
    }
}
