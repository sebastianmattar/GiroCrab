import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Shapes

// BookingListView.swift › BookingStreamView: header card on top (macOS),
// then a scrolling stream of statistics cards and day cards with booking rows.
Rectangle {
    id: stream
    color: Theme.canvas
    readonly property bool isOverview: AppModel.sidebarSelection === "all"
    readonly property bool isNewBookings: AppModel.sidebarSelection === "newBookings"
    /// Cashflow and budget cards: overview always, a single account only when toggled on; never in „Neue Umsätze“
    /// and never for a depot (BookingListView.swift, #611). Each card follows its own switch
    /// (ViewLayout.cards, #544): the budgets stay when „Auswertungen anzeigen“ is off.
    readonly property bool showsAnalytics: (AppModel.showStatistics || AppModel.showBudgets) && !isNewBookings && AppModel.allAccounts.length > 0
                                           && (isOverview || showAnalytics) && AppModel.scope !== "Investments" && !isDepot
    readonly property bool isDepot: AppModel.selectedAccount !== null
        && (AppModel.selectedAccount.kind === "Brokerage" || AppModel.selectedAccount.name.toLowerCase().indexOf("depot") >= 0)
    readonly property bool isTangible: AppModel.selectedAccount !== null && AppModel.isTangible(AppModel.selectedAccount.id)
    /// @AppStorage("showAccountAnalytics")
    readonly property bool showAnalytics: AppModel.uiDefaults.showAccountAnalytics
    /// No accounts in the personal vault: the welcome card replaces header and placeholder.
    readonly property bool emptyVault: !AppModel.demoActive && AppModel.allAccounts.length === 0

    focus: true
    Keys.onDownPressed: AppModel.moveSelection(1)
    Keys.onUpPressed: AppModel.moveSelection(-1)
    Keys.onPressed: e => {
        if (AppModel.screenshotMode || AppModel.selectedBooking === null) return
        const b = AppModel.selectedBooking
        // Strg+C copies the selected booking (Bearbeiten › Kopieren)
        if (e.matches(StandardKey.Copy)) { AppModel.copyToClipboard(AppModel.clipboardLine(b)); e.accepted = true; return }
        if (e.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)) return
        // C: categorise, R: rule for this merchant
        if (e.key === Qt.Key_C) { AppModel.showCategorizer = true; e.accepted = true }
        else if (e.key === Qt.Key_R) { AppModel.openRuleEditor(b); e.accepted = true }
        // Return: edit the note in the inspector
        else if (e.key === Qt.Key_Return || e.key === Qt.Key_Enter) { AppModel.editNote(b); e.accepted = true }
        // Space: Quick Look of the first document (only; "Geprüft" is Strg+Umschalt+U)
        else if (e.key === Qt.Key_Space) e.accepted = AppModel.quickLookFirstDocument(b)
    }
    Connections {
        target: AppModel
        // Esc in the note field hands the keyboard back to the list
        function onFocusList() { stream.forceActiveFocus() }
    }

    // .dropDestination(for: URL.self) { model.dropAnywhere(urls) }
    DropArea {
        anchors.fill: parent
        keys: ["text/uri-list"]   // files only: a dragged booking is no receipt (#366)
        onDropped: d => { if (d.hasUrls) AppModel.dropAnywhere(d.urls) }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        BankingPolicyBanner { Layout.fillWidth: true }
        AnnouncementBanner { Layout.fillWidth: true }
        MerchantFocusBar { visible: MerchantFocus.focus !== null; Layout.fillWidth: true }

        // header(isOverview:emptyVault:scope:)
        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: false // a nested layout fills by default (all children hidden: empty vault)
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 18; Layout.bottomMargin: 10
            spacing: 14
            DemoBanner { visible: AppModel.demoActive && !AppModel.screenshotMode; Layout.fillWidth: true }
            StreamHeaderCard { visible: !stream.emptyVault; Layout.fillWidth: true }
            RowLayout {
                visible: !stream.emptyVault && AppModel.showStatistics && !stream.isOverview && !stream.isDepot && AppModel.scope !== "Investments" && AppModel.allAccounts.length > 0
                Rectangle {
                    implicitWidth: an.implicitWidth + 20; implicitHeight: an.implicitHeight + 8; radius: height / 2
                    color: stream.showAnalytics ? Theme.withAlpha(Theme.accent, 0.18) : Theme.withAlpha(Theme.secondary, 0.12)
                    RowLayout { id: an; anchors.centerIn: parent; spacing: 4
                        SFIcon { symbol: "chart.bar.xaxis"; size: 11; color: Theme.text }
                        Text { text: qsTr("Analysen"); font.pixelSize: Theme.caption; font.weight: Font.Medium; color: Theme.text } }
                    MouseArea { id: analyticsArea; anchors.fill: parent; hoverEnabled: true; onClicked: AppModel.uiDefaults.showAccountAnalytics = !stream.showAnalytics }
                    ToolTip.visible: analyticsArea.containsMouse; ToolTip.delay: 600
                    ToolTip.text: qsTr("Cashflow und Budgets für dieses Konto ein-/ausblenden")
                }
                Item { Layout.fillWidth: true }
            }
        }

        ListView {
            id: list
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: AppModel.streamModel
            reuseItems: true
            cacheBuffer: 800
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.vertical: AppScrollBar { id: vbar }
            bottomMargin: 18
            // The statistics cards above the stream grow after the first load:
            // stay at the top unless the user has scrolled away.
            property bool pinnedTop: true
            onMovingChanged: { if (!moving) pinnedTop = atYBeginning; else scrollDate.show() }

            // Linux addition (not in the Mac app): while the list is scrolled by hand, a bubble
            // next to the scroll bar handle shows the day of the topmost visible booking.
            // mouse wheel / touchpad (not always reported as `moving`), flicking, dragging the handle;
            // programmatic jumps (selection, paging) happen without the pointer over the list
            HoverHandler { id: listHover }
            readonly property bool userScrolling: moving || vbar.pressed || listHover.hovered

            header: ColumnLayout {
                width: list.width
                onImplicitHeightChanged: if (list.pinnedTop) Qt.callLater(list.positionViewAtBeginning)
                spacing: 14
                Item { implicitHeight: 4 }
                EmptyVaultCard {
                    visible: stream.emptyVault
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                }
                // BookingListView.swift: MerchantLogoTipView (#33), not in "Sparpläne & Perks"
                MerchantLogoTip {
                    visible: MerchantLogos.tipEligible && !stream.emptyVault && AppModel.scope !== "Investments"
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                }
                // Built only in "Sparpläne & Perks" (it is hidden everywhere else).
                Loader {
                    active: AppModel.scope === "Investments" && AppModel.dashboard !== null
                    visible: active
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                    sourceComponent: Component { InvestmentDashboardView {} }
                }
                CashflowCard {
                    visible: stream.showsAnalytics && AppModel.showStatistics
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                }
                BudgetProgressCard {
                    visible: stream.showsAnalytics && AppModel.showBudgets
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                }
                // DepotPositionsView (Sachwerte, #87): held and sold items instead of bookings
                Loader {
                    active: stream.isTangible
                    visible: active
                    Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
                    sourceComponent: Component { AssetPositionsView { account: AppModel.selectedAccount } }
                }
                // ProgressView while a slow selection load runs (AppModel.isLoadingSelection)
                BusyIndicator {
                    visible: AppModel.isLoadingSelection
                    running: visible
                    Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 40
                    implicitWidth: 24; implicitHeight: 24; padding: 0
                }
                // ContentUnavailableView "Keine Buchungen im Depot", in the middle of the visible area
                ColumnLayout {
                    visible: AppModel.dayGroups.length === 0 && stream.isDepot && !stream.isTangible && AppModel.searchText === "" && !AppModel.isLoadingSelection
                    Layout.alignment: Qt.AlignHCenter // the column is only as wide as its texts
                    Layout.topMargin: Math.max(40, (list.height - implicitHeight) / 2 - 20)
                    spacing: 6
                    SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "chart.line.uptrend.xyaxis"; size: 32 }
                    Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Keine Buchungen im Depot"); font.pixelSize: Theme.title2; font.weight: Font.Bold; color: Theme.text }
                    Text { Layout.alignment: Qt.AlignHCenter; Layout.maximumWidth: Math.min(460, list.width - 40); horizontalAlignment: Text.AlignHCenter
                           wrapMode: Text.WordWrap; color: Theme.secondary; font.pixelSize: Theme.body
                           text: qsTr("Ein Depot zeigt seine Positionen oben. Käufe, Verkäufe und Sparpläne stehen als Buchungen im Verrechnungskonto.") }
                }
                ColumnLayout {
                    visible: AppModel.dayGroups.length === 0 && !(stream.isDepot && AppModel.searchText === "") && !stream.emptyVault && !stream.isNewBookings && !AppModel.isLoadingSelection
                    Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 40
                    spacing: 6
                    SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: AppModel.searchText ? "magnifyingglass" : "tray"; size: 32 }
                    Text { Layout.alignment: Qt.AlignHCenter; text: AppModel.searchText ? qsTr("Keine Treffer") : qsTr("Keine Buchungen"); font.pixelSize: Theme.title2; font.weight: Font.Bold; color: Theme.text }
                    Text { Layout.alignment: Qt.AlignHCenter; color: Theme.secondary; font.pixelSize: Theme.body
                           text: AppModel.searchText ? qsTr("Nichts passt zu „%1“.").arg(AppModel.redactedSearch(AppModel.searchText)) : qsTr("Konto abrufen oder anderen Zeitraum wählen.") }
                }
                // NewBookingsCards: ContentUnavailableView "Keine neuen Umsätze"
                ColumnLayout {
                    visible: stream.isNewBookings && AppModel.streamItems.length === 0 && AppModel.page !== null && !AppModel.isLoadingSelection
                    Layout.alignment: Qt.AlignHCenter; Layout.topMargin: 40
                    spacing: 6
                    SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "checkmark.circle"; size: 32 }
                    Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Keine neuen Umsätze"); font.pixelSize: Theme.title2; font.weight: Font.Bold; color: Theme.text }
                    Text { Layout.alignment: Qt.AlignHCenter; color: Theme.secondary; font.pixelSize: Theme.body
                           text: qsTr("Alle Buchungen sind geprüft.") }
                }
            }

            // LoadMoreBookingsFooter. Below the info it reserves the estimated height of the
            // bookings not loaded yet, so the total length (and the scroll bar handle) stays put
            // while pages arrive. Whenever that space comes into view, the bookings up to the
            // visible position are loaded (also when the handle is dragged to the bottom).
            property real estItemHeight: 48   // measured after each page, see measureItems()
            readonly property real placeholderHeight: {
                const n = AppModel.bookings.length
                if (!AppModel.hasMoreBookings || n === 0) return 0
                return AppModel.remainingBookingCount * (AppModel.streamModel.count / n) * estItemHeight
            }
            function measureItems() {
                const h = list.contentHeight - (list.headerItem ? list.headerItem.height : 0)
                          - (list.footerItem ? list.footerItem.height : 0)
                const n = AppModel.streamModel.count
                if (n > 50 && h > 0) list.estItemHeight = h / n
            }
            function loadVisible() {
                const f = list.footerItem
                if (!AppModel.hasMoreBookings || AppModel.isLoadingMoreBookings || !f || !f.visible) return
                // A page just published off the GUI thread is not laid out yet: the footer
                // still sits where the empty list ended. A full page is always taller than the view.
                if (f.y - list.originY < list.height) return
                const missing = list.contentY + list.height - f.y   // px of the footer in view (+ below)
                if (missing < -400) return
                const perBooking = (AppModel.streamModel.count / Math.max(1, AppModel.bookings.length)) * list.estItemHeight
                AppModel.loadMoreBookings(Math.min(3000, Math.max(AppModel.defaultListLimit,
                                                                  Math.ceil(missing / perBooking) + 100)))
            }
            onContentYChanged: { Qt.callLater(loadVisible); if (userScrolling) scrollDate.show() }
            onHeightChanged: Qt.callLater(loadVisible)
            Connections {
                target: AppModel
                function onIsLoadingMoreBookingsChanged() {
                    if (!AppModel.isLoadingMoreBookings) { list.measureItems(); Qt.callLater(list.loadVisible) }
                }
            }
            footer: Item {
                width: list.width
                visible: AppModel.hasMoreBookings
                height: visible ? info.implicitHeight + 28 + list.placeholderHeight : 0
                onVisibleChanged: if (visible) Qt.callLater(list.loadVisible)
                ColumnLayout {
                    id: info
                    width: parent.width
                    spacing: 8
                    Item { implicitHeight: 14 }
                    Text {
                        Layout.alignment: Qt.AlignHCenter; Layout.maximumWidth: list.width - 40
                        horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap
                        text: qsTr("%1 von %2 Buchungen geladen – ältere Buchungen sind noch nicht sichtbar.")
                              .arg(AppModel.bookings.length).arg(AppModel.bookings.length + AppModel.remainingBookingCount)
                        font.pixelSize: Theme.caption; color: Theme.secondary
                    }
                    BusyIndicator { Layout.alignment: Qt.AlignHCenter; implicitWidth: 18; implicitHeight: 18; opacity: AppModel.isLoadingMoreBookings ? 1 : 0; running: AppModel.isLoadingMoreBookings }
                    RowLayout {
                        Layout.alignment: Qt.AlignHCenter; spacing: 10
                        Button { text: qsTr("Weitere Buchungen laden"); enabled: !AppModel.isLoadingMoreBookings; onClicked: AppModel.loadMoreBookings() }
                        Button { text: qsTr("Alle Buchungen laden"); onClicked: AppModel.loadAllBookings() }
                    }
                }
                // in the reserved space: a spinner in the middle of the visible part
                BusyIndicator {
                    readonly property real infoHeight: info.implicitHeight + 28
                    readonly property real viewMid: list.contentY + list.height / 2 - parent.y
                    visible: AppModel.isLoadingMoreBookings && viewMid > infoHeight + 40
                    running: visible
                    width: 28; height: 28
                    x: (parent.width - width) / 2
                    y: Math.min(parent.height - height, Math.max(infoHeight, viewMid - height / 2))
                }
            }

            delegate: Item {
                id: item
                required property int index
                readonly property var modelData: AppModel.streamItems[index] ?? ({ type: "none" })
                width: list.width
                // The previous selection's rows stay instantiated (rebuilding them is what costs
                // time) but out of sight and reach while a slow selection load runs.
                opacity: AppModel.isLoadingSelection ? 0 : 1
                enabled: !AppModel.isLoadingSelection
                // „Kompakt“ (#542): no day headers, the date stands in each row
                height: modelData.type === "header" ? (BookingPrefs.density === "compact" ? 0 : 14 + dayHeader.height)
                      : modelData.type === "account" ? 14 + accountHeader.height : row.height

                DayHeader {
                    id: dayHeader
                    visible: item.modelData.type === "header" && BookingPrefs.density !== "compact"
                    day: item.modelData.type === "header" ? item.modelData.day : null
                    x: 20; y: 14; width: item.width - 40
                }
                NewBookingsCardHeader {
                    id: accountHeader
                    visible: item.modelData.type === "account"
                    section: item.modelData.type === "account" ? item.modelData.section : null
                    x: 20; y: 14; width: item.width - 40
                }
                DayCardRow {
                    id: row
                    visible: item.modelData.type === "row"
                    booking: item.modelData.type === "row" ? item.modelData.booking : null
                    isLast: !!item.modelData.isLast
                    x: 20; width: item.width - 40
                }
            }

            Rectangle {
                id: scrollDate
                /// Day of the topmost visible booking ("" over the cards above the list).
                property string label: ""
                function show() {
                    const i = list.indexAt(40, list.contentY + 1)
                    const item = i >= 0 ? AppModel.streamItems[i] : null
                    const day = !item ? "" : item.type === "header" ? item.day.date : item.type === "row" ? item.booking.booking_date : ""
                    const d = Fmt.date(day)
                    label = d ? d.toLocaleDateString(Fmt.locale, Fmt.pattern("d. MMMM yyyy")) : estimate()
                    hideTimer.restart()
                }
                /// In the reserved space of bookings not loaded yet: month and year estimated from
                /// the pace of the loaded ones ("ca. März 2021"), "" elsewhere.
                function estimate() {
                    const f = list.footerItem, b = AppModel.bookings
                    if (!f || !f.visible || b.length < 2 || list.placeholderHeight <= 0) return ""
                    const start = f.y + f.height - list.placeholderHeight
                    const y = list.contentY + 1
                    if (y < start) return ""
                    const perBooking = (AppModel.streamModel.count / b.length) * list.estItemHeight
                    const below = Math.min(AppModel.remainingBookingCount, (y - start) / perBooking)
                    const first = Fmt.date(b[0].booking_date), last = Fmt.date(b[b.length - 1].booking_date)
                    if (!first || !last) return ""
                    const est = new Date(last.getTime() + (last.getTime() - first.getTime()) * below / b.length)
                    return qsTr("ca. %1").arg(est.toLocaleDateString(Fmt.locale, "MMMM yyyy"))
                }
                visible: opacity > 0
                opacity: label !== "" && (list.moving || vbar.pressed || hideTimer.running) ? 1 : 0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                z: 10
                parent: list
                radius: height / 2
                color: Theme.withAlpha(Theme.text, 0.82)
                implicitWidth: dateText.implicitWidth + 24
                implicitHeight: dateText.implicitHeight + 10
                x: list.width - width - 18
                // next to the middle of the scroll bar handle
                y: Math.max(4, Math.min(list.height - height - 4,
                                        (vbar.position + vbar.size / 2) * list.height - height / 2))
                Text {
                    id: dateText
                    anchors.centerIn: parent
                    text: scrollDate.label
                    font.pixelSize: Theme.callout; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                    color: Theme.canvas
                }
                Timer { id: hideTimer; interval: 700 }
            }

            function revealSelection() {
                const i = AppModel.streamItems.findIndex(x => x.type === "row" && x.booking.id === AppModel.selectedBookingID)
                if (i >= 0) list.positionViewAtIndex(i, ListView.Contain)
            }
            Connections {
                target: AppModel
                function onSidebarSelectionChanged() { list.pinnedTop = true; list.positionViewAtBeginning() }
                function onPeriodChanged() { list.pinnedTop = true; list.positionViewAtBeginning() }
                function onScopeChanged() { list.pinnedTop = true; list.positionViewAtBeginning() }
                // SelectionScroller: keep the selected booking visible.
                function onSelectedBookingIDChanged() {
                    if (AppModel.scrollOnSelect) Qt.callLater(list.revealSelection)
                }
                // click in the balance chart: that day's card to the top, its first booking selected
                function onScrollToDay(day) {
                    const i = AppModel.streamItems.findIndex(x => x.type === "header" && x.day.date === day)
                    if (i < 0) return
                    list.pinnedTop = false
                    list.positionViewAtIndex(i, ListView.Beginning)
                    const first = AppModel.streamItems[i + 1]
                    if (first && first.type === "row") AppModel.selectBooking(first.booking.id, false, false)
                }
            }
        }
    }

    // The drag image of bookings dragged onto a sidebar category (#366): the merchant (or the
    // number of bookings) in a capsule, following the pointer above the whole window.
    Rectangle {
        id: bookingGhost
        parent: Overlay.overlay
        visible: BookingDrag.active
        x: BookingDrag.scenePos.x - 12; y: BookingDrag.scenePos.y - height / 2
        width: Math.min(260, ghostLabel.implicitWidth + 20); height: 24; radius: 12
        color: Theme.card; border.color: Theme.border; opacity: 0.92
        Text { id: ghostLabel; anchors.centerIn: parent; width: parent.width - 20; elide: Text.ElideRight
               text: BookingDrag.label; font.pixelSize: Theme.body; color: Theme.text; Sensitive {} }
        Drag.active: BookingDrag.active
        Drag.keys: [BookingDrag.key]
        Drag.hotSpot.x: 12; Drag.hotSpot.y: height / 2
        Drag.dragType: Drag.Internal
    }

    // MerchantFocusBar (#495): shown above a merchant-centred list, names the merchant and
    // leads back to the view the list was opened from.
    component MerchantFocusBar: Rectangle {
        implicitHeight: focusRow.implicitHeight + 16
        color: Theme.card
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
        RowLayout {
            id: focusRow
            anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
            anchors.leftMargin: 20; anchors.rightMargin: 20
            spacing: 8
            SFIcon { symbol: "storefront"; size: 14; color: Theme.secondary }
            Text { text: qsTr("Alle Buchungen von"); font.pixelSize: Theme.callout + 1; color: Theme.secondary }
            Text { text: MerchantFocus.focus ? MerchantFocus.focus.name : ""; font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold
                   color: Theme.text; elide: Text.ElideRight; Layout.maximumWidth: Math.ceil(implicitWidth); Layout.fillWidth: true; Sensitive {} }
            Text { text: "· " + AppModel.periodTitle; font.pixelSize: Theme.callout + 1; color: Theme.secondary; elide: Text.ElideRight
                   Layout.fillWidth: true; Layout.maximumWidth: Math.ceil(implicitWidth) }
            Item { Layout.fillWidth: true; Layout.minimumWidth: 8 }
            IconButton {
                text: qsTr("Zurück"); symbol: "xmark.circle.fill"; symbolSize: 14; font.pixelSize: Theme.callout + 1
                ToolTip.text: qsTr("Händler-Filter aufheben und zur vorherigen Ansicht zurückkehren")
                onClicked: MerchantFocus.leave()
            }
        }
    }

    // DayCardSegment: one card per day, split into header + rows.
    // NewBookingsView.swift › NewBookingsCardHeader: signet, name, IBAN · bank, balance and pending sum.
    component NewBookingsCardHeader: Rectangle {
        id: nh
        property var section: null
        readonly property var account: section ? section.account : null
        readonly property string detail: !account ? ""
            : [account.iban ? Fmt.iban(account.iban) : account.account_number, account.bank_name].filter(x => x).join(" · ")
        height: headRow.implicitHeight + 24
        color: Theme.card
        radius: 12
        border.color: Theme.border
        Rectangle { anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.right: parent.right; height: parent.height / 2; color: Theme.card
                    Rectangle { anchors.left: parent.left; width: 1; height: parent.height; color: Theme.border }
                    Rectangle { anchors.right: parent.right; width: 1; height: parent.height; color: Theme.border } }
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
        RowLayout {
            id: headRow
            anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
            anchors.leftMargin: 16; anchors.rightMargin: 16; anchors.topMargin: 12
            spacing: 10
            AccountSignet { visible: nh.account !== null; account: nh.account; size: 28; Layout.alignment: Qt.AlignTop
                            tintColor: nh.account ? AppModel.accountColor(nh.account.id) : Theme.tertiary }
            ColumnLayout {
                spacing: 2; Layout.fillWidth: true; Layout.alignment: Qt.AlignTop
                Text { text: nh.section ? nh.section.title : ""; font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text
                       elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                Text { visible: nh.detail !== ""; text: nh.detail; font.pixelSize: Theme.caption; color: Theme.secondary
                       elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
            }
            ColumnLayout {
                spacing: 2; Layout.alignment: Qt.AlignTop
                Text { visible: nh.account !== null; Layout.alignment: Qt.AlignRight
                       text: nh.account ? Fmt.money(nh.account.balance_cents, nh.account.currency) : ""
                       font.pixelSize: Theme.body; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                       color: nh.account && nh.account.balance_cents < 0 ? Theme.negative : Theme.text; Sensitive { amounts: true } }
                Text { visible: nh.section !== null && nh.section.pendingCents !== 0; Layout.alignment: Qt.AlignRight
                       text: nh.section ? qsTr("Vorgemerkt: %1").arg(Fmt.money(nh.section.pendingCents, nh.account ? nh.account.currency : "EUR")) : ""
                       font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
            }
        }
    }

    component DayHeader: Rectangle {
        property var day
        height: 40
        color: Theme.card
        radius: 12
        border.color: Theme.border
        Rectangle { anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.right: parent.right; height: parent.height / 2; color: Theme.card
                    Rectangle { anchors.left: parent.left; width: 1; height: parent.height; color: Theme.border }
                    Rectangle { anchors.right: parent.right; width: 1; height: parent.height; color: Theme.border } }
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border }
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: 16; anchors.rightMargin: 16
            Text { text: day ? Fmt.dayHeader(day.date) : ""; font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
            Text { text: day ? Fmt.signed(day.sum) : ""; font.pixelSize: Theme.subheadline + 1; font.features: { "tnum": 1 }; color: day && day.sum > 0 ? Theme.inflow : Theme.secondary; Sensitive { amounts: true } }
        }
    }

    // BookingListView.swift › DayCardRow + DayCardSegment. „Kompakt“ (#542) draws no day
    // cards: the rows stand on the canvas, each with a hairline below it.
    component DayCardRow: Item {
        id: dc
        property var booking
        property bool isLast
        readonly property bool selected: booking !== null && !!AppModel.selectedBookingIDs[booking.id]
        readonly property bool compact: BookingPrefs.density === "compact"
        readonly property bool card: !compact
        height: row.implicitHeight

        // background: rounded only at the bottom of the day's last row
        Rectangle {
            visible: dc.card
            anchors.fill: parent
            color: Theme.card
            radius: dc.isLast ? 12 : 0
            border.color: Theme.border
            Rectangle { visible: dc.isLast; anchors.top: parent.top; width: parent.width; height: 12; color: Theme.card
                        Rectangle { anchors.left: parent.left; width: 1; height: parent.height; color: Theme.border }
                        Rectangle { anchors.right: parent.right; width: 1; height: parent.height; color: Theme.border } }
            Rectangle { visible: !dc.isLast; anchors.top: parent.top; anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.right: parent.right
                        anchors.topMargin: 0; anchors.bottomMargin: 0; anchors.leftMargin: 1; anchors.rightMargin: 1; color: Theme.card }
        }
        // not booked yet ("vorgemerkt"): light diagonal hatching under the row, as on the Mac
        Canvas {
            id: pendingHatch
            visible: dc.booking !== null && !!dc.booking.is_pending
            readonly property bool rounded: dc.isLast && dc.card
            anchors.fill: parent; anchors.margins: dc.card ? 1 : 0
            anchors.bottomMargin: rounded ? 4 : 0
            readonly property color lineColor: Theme.withAlpha(Theme.secondary, 0.14)
            onLineColorChanged: requestPaint()
            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()
            onVisibleChanged: if (visible) requestPaint()
            onRoundedChanged: requestPaint()
            onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                // the day's last row is rounded at the bottom: keep the lines inside
                const r = rounded ? 10 : 0
                ctx.beginPath()
                ctx.moveTo(0, 0)
                ctx.lineTo(width, 0)
                ctx.lineTo(width, height - r)
                ctx.arcTo(width, height, width - r, height, r)
                ctx.lineTo(r, height)
                ctx.arcTo(0, height, 0, height - r, r)
                ctx.closePath()
                ctx.clip()
                ctx.strokeStyle = lineColor
                ctx.lineWidth = 1
                ctx.beginPath()
                for (let x = -height; x < width; x += 7) {
                    ctx.moveTo(x, height)
                    ctx.lineTo(x + height, 0)
                }
                ctx.stroke()
            }
        }
        Rectangle {
            anchors.fill: parent; anchors.margins: dc.card ? 1 : 0
            anchors.bottomMargin: dc.isLast && dc.card ? 4 : 0
            radius: dc.isLast && dc.card ? 10 : 0
            color: dc.selected ? Theme.selection : "transparent"
        }
        BookingRow { id: row; width: parent.width; booking: dc.booking }
        DropArea {
            id: rowDrop
            anchors.fill: parent
            keys: ["text/uri-list"]
            onDropped: d => { if (d.hasUrls) { AppModel.selectedBookingID = dc.booking.id; AppModel.attach(d.urls, dc.booking) } }
        }
        Rectangle { anchors.fill: parent; visible: rowDrop.containsDrag; color: "transparent"; border.color: Theme.accent; border.width: 2; radius: 6 }
        Rectangle {
            visible: !dc.isLast || dc.compact
            x: dc.compact ? 14 : 64; width: parent.width - x - (dc.compact ? 0 : 1); height: 1
            anchors.bottom: parent.bottom; color: Theme.withAlpha(Theme.border, 0.7)
        }
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            onClicked: m => {
                stream.forceActiveFocus()
                // the context menu acts on its own row and leaves the selection alone
                if (m.button === Qt.RightButton) { stream.openRowMenu(dc.booking, dc, m.x, m.y); return }
                // the Amazon drill-down chevron sits below this area
                const c = row.chevron
                if (c.visible && c.contains(c.mapFromItem(dc, m.x, m.y))) { stream.toggleAmazonExpanded(dc.booking.id); return }
                const toggle = m.modifiers & Qt.ControlModifier, extend = m.modifiers & Qt.ShiftModifier
                // „Kompakt“: a click on the open row folds it, a click on another row opens that one
                if (dc.compact && AppModel.selectedBookingID === dc.booking.id && !toggle && !extend)
                    stream.compactCollapsedID = stream.compactCollapsedID === dc.booking.id ? null : dc.booking.id
                else
                    stream.compactCollapsedID = null
                AppModel.selectBooking(dc.booking.id, toggle, extend, true)
            }
            onDoubleClicked: m => { if (m.button === Qt.LeftButton) AppModel.editNote(dc.booking) }
            // .draggable(BookingDrag.payload(…)): onto a sidebar category (#366)
            DragHandler {
                target: null
                dragThreshold: 8
                acceptedButtons: Qt.LeftButton
                // neither the list (flick) nor the row's click may take it over
                grabPermissions: PointerHandler.CanTakeOverFromAnything
                onActiveChanged: {
                    if (active) BookingDrag.start(dc.booking)
                    else { bookingGhost.Drag.drop(); BookingDrag.end() }
                }
                onCentroidChanged: if (active) BookingDrag.scenePos = centroid.scenePosition
            }
        }
    }
    // One context menu for the whole list (BookingListView.swift .contextMenu): built once,
    // shown for the row that was right-clicked. A menu per row was rebuilt with every list
    // reload and made switching accounts slow with many bookings.
    property var menuBooking: null
    function openRowMenu(booking, item, x, y) {
        menuBooking = booking
        rowMenu.popup(item, x, y)
    }
    AppMenu {
        id: rowMenu
        // CategoryAssignMenu (#366): the category tree, search sheet, removal
        BookingCategoryMenu { booking: stream.menuBooking }
        AppMenuItem { text: qsTr("Regel für diesen Händler erstellen …"); icon.name: SF.icon("wand.and.stars"); onTriggered: AppModel.openRuleEditor(stream.menuBooking) }
        AppMenuItem { text: qsTr("Notiz bearbeiten …"); icon.name: SF.icon("note.text"); onTriggered: AppModel.editNote(stream.menuBooking) }
        // TagsMenu (#228)
        TagsMenu { booking: stream.menuBooking }
        AppMenuItem { text: qsTr("Umsatz aufteilen (Splitbuchung) …"); icon.name: SF.icon("arrow.triangle.branch"); onTriggered: AppModel.splitBooking = stream.menuBooking }
        // LoanAssignMenu(hidesWhenUnavailable: true): left out for loan bookings and unlinked inflows
        Instantiator {
            model: AppModel.loanAssignable(stream.menuBooking) ? 1 : 0
            delegate: LoanAssignMenu { booking: stream.menuBooking }
            onObjectAdded: (index, menu) => {
                let at = 0
                for (let i = 0; i < rowMenu.count; ++i)
                    if (rowMenu.itemAt(i) && rowMenu.itemAt(i).text === qsTr("Umsatz aufteilen (Splitbuchung) …")) at = i + 1
                rowMenu.insertMenu(at, menu)
            }
            onObjectRemoved: (index, menu) => rowMenu.removeMenu(menu)
        }
        AppMenuSeparator {}
        AppMenuItem {
            readonly property string who: stream.menuBooking ? (stream.menuBooking.counterparty || stream.menuBooking.merchant_name) : ""
            text: qsTr("Nach „%1“ suchen").arg(who); icon.name: SF.icon("magnifyingglass"); enabled: who !== ""
            onTriggered: AppModel.searchText = who
        }
        AppMenuItem { text: qsTr("Neue Überweisung aus Buchung …"); icon.name: SF.icon("paperplane"); enabled: AppModel.canTransfer
                   onTriggered: AppModel.openTransferFrom(stream.menuBooking) }
        AppMenuItem { text: qsTr("Als Dauerauftrag anlegen …"); icon.name: SF.icon("repeat"); enabled: Payments.canMakeStandingOrder(stream.menuBooking)
                   onTriggered: Payments.openStandingOrder("fromBooking", stream.menuBooking) }
        AppMenuItem { text: qsTr("Umsatz kopieren"); icon.name: SF.icon("doc.on.doc")
                   onTriggered: AppModel.copyToClipboard(AppModel.clipboardLine(stream.menuBooking)) }
        // exportMenuButton(for:)
        AppMenuItem {
            readonly property int count: AppModel.isMultiSelection && stream.menuBooking !== null && !!AppModel.selectedBookingIDs[stream.menuBooking.id]
                ? Object.keys(AppModel.selectedBookingIDs).length : 1
            text: count > 1 ? qsTr("%1 Umsätze exportieren …").arg(count) : qsTr("Umsatz exportieren …")
            icon.name: SF.icon("square.and.arrow.up")
            onTriggered: {
                if (!AppModel.selectedBookingIDs[stream.menuBooking.id]) AppModel.selectedBookingID = stream.menuBooking.id
                AppModel.openBookingExport(true)
            }
        }
        AppMenuSeparator {}
        AppMenuItem { text: stream.menuBooking && stream.menuBooking.checked ? qsTr("Als neu markieren") : qsTr("Als geprüft markieren"); icon.name: SF.icon("checkmark.circle")
                   onTriggered: AppModel.setChecked(!stream.menuBooking.checked, stream.menuBooking) }
    }

    /// BookingRow.amazonExpanded (`@State` per row on the Mac): booking ids with an
    /// open Amazon drill-down. Kept here, ListView delegates are recycled.
    property var amazonExpanded: ({})
    /// DayCardRow.compactDetailsCollapsed: „Kompakt“ – the selected row the user folded again.
    property var compactCollapsedID: null
    function toggleAmazonExpanded(id) {
        const e = Object.assign({}, amazonExpanded)
        if (e[id]) delete e[id]; else e[id] = true
        amazonExpanded = e
    }

    // BookingListView.swift › BookingRow: exactly three elements, signet · name + subtitle · amount.
    // BookingDensity (#542): „Kompakt“ is one line with the date in front (the active booking opens
    // with all details), „Standard“ adds a short purpose line, „Großzügig“ opens every booking.
    component BookingRow: Item {
        id: br
        property var booking
        readonly property string density: BookingPrefs.density
        readonly property bool compact: density === "compact"
        /// density.showsDetails(isActive:)
        readonly property bool showsDetails: booking !== null && (density === "spacious"
            || (compact && AppModel.selectedBookingID === booking.id && stream.compactCollapsedID !== booking.id))
        /// effectiveDensity: an opened row is laid out like „Großzügig“
        readonly property string effectiveDensity: showsDetails ? "spacious" : density
        readonly property bool isSingleLine: compact && !showsDetails
        readonly property real signetSize: isSingleLine ? 26 : 36
        /// Indent of the lines below the name: signet + spacing (+ the date column in „Kompakt“).
        readonly property real detailInset: Theme.scaled(36) + 12 + (compact ? 80 * Theme.textScale + 4 : 0)
        readonly property real verticalPadding: (effectiveDensity === "compact" ? 4 : effectiveDensity === "spacious" ? 9 : 5) * Theme.textScale
        readonly property bool showsCategoryChip: booking !== null && BookingPrefs.showsCategoryChip(booking.account_id)
        /// The purpose: complete (with line breaks) when opened, else one cleaned line (ListPurpose, #497).
        readonly property string visiblePurpose: booking === null ? ""
            : showsDetails ? BookingDetails.fullPurpose(booking) : BookingDetails.listPurpose(booking)
        /// Amazon orders: items, order code and the drill-down instead of the payment method.
        readonly property var amazonRef: AmazonOrderRef.parse(booking)
        /// Refunds show the refunded order's items (joined via the order code).
        readonly property var refundSource: amazonRef !== null && amazonRef.isRefund ? AmazonOrderRef.refundSource(booking, AppModel.bookings) : null
        readonly property bool expanded: amazonRef !== null && booking !== null && !!stream.amazonExpanded[booking.id]
        readonly property var counterAccount: BookingDetails.counterAccount(booking)
        property alias chevron: chevron
        /// Width the name line gets in a single line: the row minus date, signet, marks and amount.
        readonly property real nameLineWidth: width - 28 - (compact ? 80 * Theme.textScale + 8 : 0) - Theme.scaled(signetSize) - 8
            - (checkIcon.visible ? checkIcon.width + 8 : 0) - (pendingIcon.visible ? pendingIcon.width + 8 : 0)
            - (amountLabel.visible ? amountLabel.implicitWidth : orderAmount.implicitWidth + 16) - 8
        implicitHeight: Math.max((effectiveDensity === "compact" ? 28 : 44) * Theme.textScale, column.implicitHeight) + 2 * verticalPadding

        ColumnLayout {
            id: column
            visible: br.booking !== null
            x: 14; y: br.verticalPadding; width: parent.width - 28
            spacing: 0
            RowLayout {
                id: headLine
                Layout.fillWidth: true
                spacing: br.compact ? 8 : 12
                Text {
                    visible: br.compact
                    text: br.booking ? Fmt.medium(br.booking.booking_date) : ""
                    font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 }; color: Theme.secondary
                    Layout.preferredWidth: 80 * Theme.textScale
                    Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop
                }
                MerchantSignet {
                    size: br.signetSize
                    Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop
                    merchant: br.booking ? br.booking.merchant : null
                    categorySymbol: br.booking ? br.booking.category_symbol : ""
                    showNewDot: br.booking ? !br.booking.checked : false
                    opacity: br.booking && br.booking.is_pending ? 0.7 : 1
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    // the rest of the line, whatever the texts inside would like (the single
                    // line's category and tag markers depend on this width: no feedback)
                    Layout.preferredWidth: 0
                    Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop
                    spacing: 2
                    RowLayout {
                        id: nameLine
                        spacing: 5
                        Layout.fillWidth: true
                        Text {
                            id: nameText
                            text: br.booking ? (br.booking.merchant.name || br.booking.booking_text) : ""
                            font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text
                            wrapMode: br.showsDetails ? Text.Wrap : Text.NoWrap
                            elide: br.showsDetails ? Text.ElideNone : Text.ElideRight
                            Layout.fillWidth: true; Layout.maximumWidth: Math.ceil(implicitWidth)
                            // the name first: in a single line the purpose yields (layoutPriority)
                            Layout.minimumWidth: br.isSingleLine ? Math.min(Math.ceil(implicitWidth), Math.max(0, br.nameLineWidth - 30)) : 0
                            Sensitive {}
                        }
                        SFIcon { id: docIcon; visible: br.booking && br.booking.document_count > 0; symbol: "doc.text.fill"; size: 11; color: Theme.withAlpha(Theme.accent, 0.8) }
                        SFIcon { id: repeatIcon; visible: br.booking !== null && AppModel.recurringSeries(br.booking) !== null; symbol: "repeat"; size: 11 }
                        // Keep the name and amount first. Show the category only when its
                        // complete label fits (ViewThatFits); the purpose yields first.
                        BookingCategoryLabel {
                            id: singleLineCategory
                            booking: br.booking
                            // from widths outside the line's own layout (no recursive rearrange)
                            readonly property real room: br.nameLineWidth - Math.ceil(nameText.implicitWidth)
                                - (docIcon.visible ? docIcon.width + 5 : 0) - (repeatIcon.visible ? repeatIcon.width + 5 : 0) - 5
                            visible: br.isSingleLine && br.showsCategoryChip && room >= implicitWidth
                        }
                        // like the category: only when the markers fit, the purpose yields first
                        BookingTagMarkers {
                            tags: br.booking ? (br.booking.tags || []) : []
                            readonly property real room: singleLineCategory.room - (singleLineCategory.visible ? singleLineCategory.implicitWidth + 5 : 0)
                            visible: br.isSingleLine && tags.length > 0 && room >= implicitWidth
                        }
                        Text {
                            visible: br.isSingleLine && br.visiblePurpose !== ""
                            text: br.visiblePurpose
                            font.pixelSize: Theme.callout + 1; color: Theme.secondary
                            elide: Text.ElideRight; Layout.fillWidth: true
                            Sensitive {}
                        }
                        Item { Layout.fillWidth: true; Layout.preferredWidth: 0 }
                    }
                    RowLayout {
                        visible: !br.showsDetails && !br.isSingleLine
                        spacing: 6
                        Layout.fillWidth: true
                        clip: true
                        AccountChip {
                            visible: AppModel.showsAccountMarkers && br.booking !== null
                            label: br.booking ? AppModel.accountShortLabel(br.booking.account_id) : ""
                            tint: br.booking ? AppModel.accountColor(br.booking.account_id) : Theme.tertiary
                            symbol: br.booking ? AppModel.accountSymbol(br.booking.account_id) : ""
                        }
                        // category with the sidebar's icon and colour (unless hidden for this account, #336)
                        BookingCategoryLabel { booking: br.booking; visible: br.showsCategoryChip }
                        BookingTagMarkers { tags: br.booking ? (br.booking.tags || []) : [] }
                        // a transfer to an own account or pocket names it (#523), else the payment method
                        Text {
                            visible: br.amazonRef === null && br.counterAccount !== null
                            text: br.counterAccount ? qsTr("· ⇄ %1").arg(br.counterAccount.name) : ""
                            font.pixelSize: Theme.callout + 1; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                            Sensitive {}
                        }
                        Text {
                            visible: br.amazonRef === null && br.counterAccount === null && br.booking && br.booking.payment_method !== ""
                            text: br.booking ? "· " + br.booking.payment_method : ""
                            font.pixelSize: Theme.callout + 1; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                        }
                        // Amazon order: what was ordered beats the "Bestellung" kind label.
                        // Refund rows start flush like every other row ("· item…");
                        // the ↩ marker goes at the end of the item text.
                        Text {
                            id: amazonItems
                            visible: text !== ""
                            text: br.refundSource ? "· " + br.refundSource.items + "  ↩"
                                : br.amazonRef !== null && br.amazonRef.items !== "" ? "· " + br.amazonRef.items : ""
                            font.pixelSize: Theme.callout + 1; color: Theme.secondary; elide: Text.ElideRight
                            Layout.fillWidth: true; Layout.maximumWidth: Math.ceil(implicitWidth)
                            HoverHandler { id: itemsHover }
                            ToolTip.visible: itemsHover.hovered; ToolTip.delay: 600
                            ToolTip.text: br.amazonRef === null ? "" : br.amazonRef.isRefund
                                ? qsTr("Erstattung für Bestellung #%1").arg(br.amazonRef.code) : br.amazonRef.itemList.join("\n")
                            Sensitive {}
                        }
                        // items beyond the two shown; the drill-down lists everything
                        Text {
                            visible: br.amazonRef !== null && br.amazonRef.moreCount > 0
                            text: visible ? qsTr("+%1 weitere").arg(br.amazonRef.moreCount) : ""
                            font.pixelSize: Theme.caption2; font.weight: Font.Medium; color: Theme.tertiary
                            HoverHandler { id: moreHover }
                            ToolTip.visible: moreHover.hovered; ToolTip.delay: 600
                            ToolTip.text: br.amazonRef !== null ? br.amazonRef.itemList.slice(2).join("\n") : ""
                        }
                        // "D01-…" charges are the recurring Spar-Abo, plain orders are one-off
                        RowLayout {
                            visible: br.amazonRef !== null && br.amazonRef.isSubscription
                            spacing: 3
                            SFIcon { symbol: "arrow.triangle.2.circlepath"; size: 10; color: Theme.tertiary }
                            Text { text: qsTr("Abo"); font.pixelSize: Theme.caption2; font.weight: Font.Medium; color: Theme.tertiary }
                            HoverHandler { id: aboHover }
                            ToolTip.visible: aboHover.hovered; ToolTip.delay: 600
                            ToolTip.text: qsTr("Abonnement (Spar-Abo) – wiederkehrende Abbuchung")
                        }
                        Text {
                            visible: br.amazonRef !== null
                            text: visible ? "· #" + br.amazonRef.code : ""
                            font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 }; color: Theme.tertiary
                            Sensitive {}
                        }
                        // drill-down when there is more than the head can show (clicks: DayCardRow)
                        SFIcon {
                            id: chevron
                            visible: br.amazonRef !== null && (br.amazonRef.itemList.length > 1 || br.refundSource !== null)
                            symbol: br.expanded ? "chevron.down" : "chevron.right"; size: 10; color: Theme.tertiary
                            HoverHandler { id: chevronHover }
                            ToolTip.visible: chevronHover.hovered; ToolTip.delay: 600
                            ToolTip.text: br.expanded ? qsTr("Artikel einklappen") : qsTr("Alle Artikel anzeigen")
                        }
                        Item { Layout.fillWidth: true }
                    }
                }
                // the row just checked in „Neue Umsätze“ or under "Neu" stays without a mark until it leaves
                SFIcon { id: checkIcon; visible: br.booking && br.booking.checked && AppModel.lingeringBookingID !== br.booking.id; symbol: "checkmark"; size: 11; color: Theme.tertiary
                         Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop; Layout.topMargin: br.isSingleLine ? 0 : 4 }
                SFIcon { id: pendingIcon; visible: br.booking && br.booking.is_pending; symbol: "clock"; size: 11; color: Theme.tertiary
                         Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop; Layout.topMargin: br.isSingleLine ? 0 : 4 }
                AmountLabel { id: amountLabel; visible: br.amazonRef === null; cents: br.booking ? br.booking.amount_cents : 0
                              Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop }
                // Order account: the direction is never colour alone — charges get a
                // minus, refunds a plus (#571); both are badged symmetrically.
                Rectangle {
                    visible: br.amazonRef !== null
                    Layout.alignment: br.isSingleLine ? Qt.AlignVCenter : Qt.AlignTop
                    implicitWidth: orderAmount.implicitWidth + 16; implicitHeight: orderAmount.implicitHeight + 6
                    radius: height / 2
                    color: br.amazonRef !== null && br.amazonRef.isRefund ? Theme.inflowBadge : Theme.outflowBadge
                    Text {
                        id: orderAmount
                        anchors.centerIn: parent
                        text: br.amazonRef === null ? "" : br.amazonRef.isRefund ? Fmt.signed(br.booking.amount_cents) : Fmt.money(-Math.abs(br.booking.amount_cents))
                        font.pixelSize: Theme.body; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                        color: br.amazonRef !== null && br.amazonRef.isRefund ? Theme.inflow : Theme.charge
                        Sensitive { amounts: true }
                    }
                    HoverHandler { id: orderAmountHover }
                    ToolTip.visible: orderAmountHover.hovered; ToolTip.delay: 600
                    ToolTip.text: br.amazonRef !== null && br.amazonRef.isRefund ? qsTr("Gutschrift (Erstattung)") : qsTr("Bestellung")
                }
            }
            // „Standard“: one line of purpose below the name; opened rows: all of it
            Text {
                visible: (br.showsDetails || br.density === "standard") && br.amazonRef === null && br.visiblePurpose !== ""
                text: br.visiblePurpose
                Layout.fillWidth: true; Layout.leftMargin: br.detailInset
                font.pixelSize: Theme.callout + 1; color: Theme.secondary
                wrapMode: br.showsDetails ? Text.Wrap : Text.NoWrap
                maximumLineCount: br.showsDetails ? 1000 : 1
                elide: br.showsDetails ? Text.ElideNone : Text.ElideRight
                Sensitive {}
            }
            Loader {
                active: br.showsDetails
                visible: active
                Layout.fillWidth: true; Layout.leftMargin: br.detailInset; Layout.topMargin: 4
                sourceComponent: BookingInlineDetails { booking: br.booking }
            }
            // drill-down: every item on its own line, indented under the signet
            ColumnLayout {
                id: drill
                visible: br.amazonRef !== null && ((br.expanded && !br.compact) || br.showsDetails)
                Layout.fillWidth: true; Layout.leftMargin: br.detailInset; Layout.bottomMargin: 4
                spacing: 2
                Text { visible: br.refundSource !== null; text: qsTr("Erstattung zu:"); font.pixelSize: Theme.caption2; color: Theme.tertiary }
                Repeater {
                    model: !drill.visible ? [] : br.refundSource !== null ? br.refundSource.list : br.amazonRef.itemList
                    Text { required property string modelData; text: modelData; font.pixelSize: Theme.callout + 1; color: Theme.secondary
                           wrapMode: Text.Wrap; Layout.fillWidth: true; Sensitive {} }
                }
            }
        }
    }
    component AccountChip: Rectangle {
        property string label
        property color tint
        property string symbol
        implicitWidth: ac.implicitWidth + 12; implicitHeight: ac.implicitHeight + 3
        radius: height / 2
        color: Theme.withAlpha(tint, 0.12)
        RowLayout { id: ac; anchors.centerIn: parent; spacing: 4
            SFIcon { symbol: parent.parent.symbol; size: 11; color: parent.parent.tint }
            Text { text: parent.parent.label; font.pixelSize: Theme.subheadline + 1; font.weight: Font.Medium; color: parent.parent.tint; Sensitive {} } }
    }

    component BookingCategoryLabel: RowLayout {
        property var booking
        readonly property bool none: !booking || booking.category_name === ""
        readonly property color tint: none ? Theme.warning : Theme.readable(booking.category_color_hex)
        spacing: 3
        SFIcon { symbol: none ? "questionmark.folder" : (booking.category_symbol || "tag.fill"); size: 11; color: parent.tint }
        Text { text: none ? SidebarModel.uncategorizedLabel : booking.category_name; font.pixelSize: Theme.callout + 1; font.weight: Font.Medium; color: parent.tint; elide: Text.ElideRight
               Sensitive { labels: true } }   // also a custom „Ohne Kategorie“ label (#597)
    }

    // Onboarding.swift › EmptyVaultCard: the personal vault has no accounts yet.
    component EmptyVaultCard: Card {
        ColumnLayout {
            width: parent.width; spacing: 14
            RowLayout {
                spacing: 12
                AnimatedCrabLogo { size: 56 }
                Text { text: qsTr("Willkommen bei GiroCrab"); font.pixelSize: Theme.title3 + 2; font.weight: Font.DemiBold; color: Theme.text }
            }
            Text { text: qsTr("Ihr Tresor ist eingerichtet, enthält aber noch keine Konten. So geht es weiter:")
                   font.pixelSize: Theme.callout + 1; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            Repeater {
                model: [qsTr("Bank wählen und Anmeldenamen eingeben"),
                        qsTr("Fehlende Angaben ergänzen (z. B. FinTS-Produkt-ID, API-Schlüssel) – das Formular öffnet sich automatisch"),
                        qsTr("PIN eingeben und in der Banking-App freigeben")]
                RowLayout {
                    required property string modelData
                    required property int index
                    Layout.fillWidth: true; spacing: 10
                    Rectangle {
                        Layout.alignment: Qt.AlignTop
                        implicitWidth: 20; implicitHeight: 20; radius: 10; color: Theme.selection
                        Text { anchors.centerIn: parent; text: index + 1; font.pixelSize: Theme.caption; font.weight: Font.Bold; color: Theme.accent }
                    }
                    Text { text: modelData; font.pixelSize: Theme.callout + 1; color: Theme.text; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                }
            }
            Button { text: qsTr("Bank suchen und hinzufügen …"); icon.name: SF.icon("magnifyingglass"); highlighted: true
                     Layout.fillWidth: true; implicitHeight: 34; onClicked: AppModel.showAddBank = true }
            Button { text: qsTr("Vorhandene Daten importieren …"); icon.name: SF.icon("square.and.arrow.down.on.square"); highlighted: true
                     Layout.fillWidth: true; implicitHeight: 34; onClicked: AppModel.openMoneyMoneyImport() }
            Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
            RowLayout {
                Layout.fillWidth: true; spacing: 8
                Text { text: qsTr("Erst einmal ansehen?"); font.pixelSize: Theme.caption; color: Theme.secondary }
                Button { flat: true; text: qsTr("Demo-Daten && Demo-Banken laden (Kennung: demo / PIN: 12345)"); font.pixelSize: Theme.caption
                         palette.buttonText: Theme.accent; onClicked: AppModel.showDemo() }
                Item { Layout.fillWidth: true }
            }
        }
    }

    // Onboarding.swift › DemoBanner
    component DemoBanner: Rectangle {
        implicitHeight: db.implicitHeight + 24
        radius: 12
        color: Theme.selection
        border.color: Theme.withAlpha(Theme.accent, 0.18)
        RowLayout {
            id: db
            anchors.fill: parent; anchors.margins: 12
            spacing: 12
            SFIcon { symbol: "theatermasks.fill"; size: 18; color: Theme.accent; Layout.alignment: Qt.AlignTop }
            ColumnLayout {
                spacing: 2
                Layout.fillWidth: true
                Text { text: qsTr("Demo-Daten"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text }
                Text {
                    // DemoBanner.message: only an own vault without data is called empty
                    text: AppModel.hasOwnVault && AppModel.vaultMayHoldRealData()
                        ? qsTr("Sie sehen gerade erfundene Demo-Daten. Ihr eigener Tresor bleibt unverändert.")
                        : AppModel.hasOwnVault
                            ? qsTr("Ihr eigener Tresor ist noch leer. Hier sehen Sie erfundene Buchungen.")
                            : qsTr("Erfundene Buchungen – keine echte Bank verbunden. Sobald echte Daten da sind, schützt Face ID bzw. Ihr Gerätecode den Tresor automatisch; hier können Sie den Sperrbildschirm testen.")
                    font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
                }
                Text {
                    // DemoBanner.resetNote (#952): the demo vault is rebuilt on every start
                    text: qsTr("Änderungen werden beim Beenden zurückgesetzt.")
                    font.pixelSize: Theme.caption; font.weight: Font.Medium; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
                }
            }
            Button { text: AppModel.hasOwnVault || AppModel.vaultMayHoldRealData() ? qsTr("Zurück zum eigenen Tresor") : qsTr("Eigenes Konto anlegen")
                     Layout.alignment: Qt.AlignTop; onClicked: AppModel.showOwnVault() }
            Button { visible: !AppModel.hasOwnVault && !AppModel.vaultMayHoldRealData(); text: qsTr("Sperrbildschirm testen")
                     Layout.alignment: Qt.AlignTop; onClicked: AppModel.previewLock() }
        }
    }
}
