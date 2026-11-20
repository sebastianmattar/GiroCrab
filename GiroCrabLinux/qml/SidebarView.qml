import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt.labs.qmlmodels

// SidebarView.swift: column 1 – collapsible account sections with subtotals, then categories.
Rectangle {
    id: sidebar
    color: Theme.sidebar

    /// The List of the Swift view, flattened into typed rows. The accounts header with
    /// the search field is the ListView header, so typing does not rebuild it.
    /// The pages above the account tree (#521): opened daily or as a starting point, so with
    /// many accounts they must not end up below the visible part. They sit in the floating
    /// header together with the „Konten“ heading (see `accountsHeader`).
    readonly property var topRows: [
        // „Neue Umsätze“ (#352)
        { type: "nav", key: "newBookings", symbol: "tray.full", title: qsTr("Neue Umsätze"), badge: AppModel.newCount },
        // „Vermögen“ (#348): the counterpart of „Alle Konten“
        { type: "nav", key: "wealth", symbol: "chart.pie", title: qsTr("Vermögen"), badge: 0 }
    ]
    readonly property var rows: {
        const m = AppModel
        const sm = SidebarModel
        const out = []
        const shown = a => m.isSidebarAccountListed(a)
        if (!sm.accountsCollapsed || !m.allAccounts.length) {
            // the root of the account tree: „Alle Konten“
            out.push({ type: "allAccounts" })
            if (m.showsAccountTree) {
                if (m.accountView === "byGroup") {
                    // a flattened tree: subgroups follow their group, indented by depth
                    for (const g of m.userGroups) {
                        if (!m.isUserGroupListed(g)) continue
                        const open = m.isUserGroupOpen(g)
                        out.push({ type: "userGroup", group: g, open: open })
                        if (!open) continue
                        if (!m.isSidebarAccountSearching && !g.accounts.length && !g.is_ungrouped && m.userGroupSubtree(g.uuid).length === 1)
                            out.push({ type: "hint", depth: g.depth, text: qsTr("Konten hierher ziehen"), group: g })
                        // icon on the group header's icon axis (behind the chevron column)
                        g.accounts.forEach((a, i) => { if (shown(a)) out.push({ type: "account", account: a, indent: (g.depth + 1) * sm.iconInset, group: g, row: i }) })
                    }
                } else {
                    const byBank = m.accountView === "byBank"
                    for (const g of (byBank ? m.bankGroups : m.groups)) {
                        const listed = g.accounts.filter(shown)
                        // hide sections without a hit while searching, and ones holding only
                        // archived accounts while those are hidden (#814)
                        if (!m.isSidebarSectionListed(g.accounts)) continue
                        const open = byBank ? m.isBankGroupOpen(g.id) : m.isKindGroupOpen(g.id)
                        out.push({ type: "kindGroup", group: g, open: open, bank: byBank })
                        if (open) for (const a of listed) out.push({ type: "account", account: a, indent: 0 })
                    }
                }
            }
        }
        // payments and documents are used less often: below the accounts, each under its heading
        out.push({ type: "caption", key: "payments", title: qsTr("Zahlungen"), open: !sm.paymentsCollapsed })
        if (!sm.paymentsCollapsed) {
            // „Geplant“ (#240): ordered at the bank, not booked yet
            out.push({ type: "nav", key: "planned", symbol: "calendar.badge.clock", title: qsTr("Geplant"), badge: sm.plannedCount })
            // „Lastschriften“ (#143): mandates and collections
            out.push({ type: "nav", key: "directDebits", symbol: "arrow.down.left.arrow.up.right.square", title: qsTr("Lastschriften"), badge: sm.mandateCount })
        }
        out.push({ type: "caption", key: "documents", title: qsTr("Dokumente"), open: !sm.documentsCollapsed })
        if (!sm.documentsCollapsed)
            out.push({ type: "nav", key: "documents", symbol: "doc.on.doc", title: qsTr("Belege & PostBox"), badge: 0 })
        const pending = m.connections.filter(c => !m.allAccounts.some(a => a.connection_id === c.id))
        if (pending.length) {
            out.push({ type: "section", title: qsTr("Neue Bankzugänge") })
            for (const c of pending) out.push({ type: "connection", connection: c })
        }
        // caption style like „Konten“, „Zahlungen“ and „Dokumente“ (#676); „+“ lives in the bottom bar
        out.push({ type: "caption", key: "categories", title: qsTr("Kategorien"), open: !m.categoriesCollapsed })
        if (!m.categoriesCollapsed) {
            out.push({ type: "nav", key: "uncategorized", symbol: "questionmark.folder", title: sm.uncategorizedLabel,
                       badge: m.uncategorizedCount, tint: Theme.warning, regular: true, labels: true })
            function walk(nodes, depth) {
                for (const n of nodes) {
                    const open = !!m.expandedCategories[n.category.id]
                    out.push({ type: "category", category: n.category, depth: depth, hasChildren: !!n.children, open: open })
                    if (n.children && open) walk(n.children, depth + 1)
                }
            }
            walk(m.categoryTree, 0)
        }
        return out
    }
    /// SidebarView.swift › selectedCategoryID: a selected category becomes the parent of „Neue Kategorie“.
    readonly property var selectedCategoryID: AppModel.sidebarSelection.startsWith("category:") ? Number(AppModel.sidebarSelection.slice(9)) : null
    function toggleCaption(key) {
        if (key === "payments") SidebarModel.paymentsCollapsed = !SidebarModel.paymentsCollapsed
        else if (key === "documents") SidebarModel.documentsCollapsed = !SidebarModel.documentsCollapsed
        else if (key === "categories") AppModel.categoriesCollapsed = !AppModel.categoriesCollapsed
    }

    /// The selection key of a row, "" for rows that cannot be selected.
    function rowKey(r) {
        switch (r.type) {
        case "allAccounts": return "all"
        case "nav": return r.key
        case "userGroup": return "userGroup:" + r.group.uuid
        case "kindGroup": return (r.bank ? "bank:" : "group:") + r.group.id
        case "account": return "account:" + r.account.id
        case "category": return "category:" + r.category.id
        default: return ""
        }
    }
    /// ↑/↓: the next selectable row, kept in view.
    function moveSelection(delta) {
        const sel = AppModel.sidebarSelection
        // the pages in the header first, then the list rows
        const all = topRows.concat(rows)
        const top = topRows.length
        let i = all.findIndex(r => rowKey(r) === sel)
        if (i < 0) i = delta > 0 ? -1 : all.length
        for (i += delta; i >= 0 && i < all.length; i += delta) {
            const key = rowKey(all[i])
            if (key === "") continue
            AppModel.selectFromSidebar(key)
            // a header page or the first row bring the header back into view
            if (i <= top) list.contentY = -list.topMargin
            else list.positionViewAtIndex(i - top, ListView.Contain)
            return
        }
    }
    /// →/←: expand / collapse the selected category; ← on a sub-category selects its parent.
    function foldSelection(expand) {
        const sel = AppModel.sidebarSelection
        const i = rows.findIndex(r => rowKey(r) === sel)
        if (i < 0 || rows[i].type !== "category") return false
        const r = rows[i]
        if (r.hasChildren && r.open !== expand) { AppModel.toggleExpandedCategory(r.category.id); return true }
        if (expand) return false
        for (let j = i - 1; j >= 0 && rows[j].type === "category"; j--) {
            if (rows[j].depth === r.depth - 1) {
                AppModel.sidebarSelection = rowKey(rows[j])
                list.positionViewAtIndex(j, ListView.Contain)
                return true
            }
        }
        return false
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // The accounts header (with the search field) scrolls with the list but is not part of
        // the ListView: a rebuilt model gives the view's current item the focus, which would take
        // it from the search field after every keystroke.
        Item {
            id: keyNav
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            // List(selection:): ↑/↓ walk the selectable rows, ←/→ fold a category with
            // sub-categories (its DisclosureGroup); ← on a sub-category goes to its parent.
            // The focus sits here, not on the ListView (see the comment above).
            Keys.onPressed: e => {
                // keys the search field (a child) does not take stay with the field
                if (!keyNav.activeFocus) return
                if (e.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier | Qt.ShiftModifier)) return
                if (e.key === Qt.Key_Down) { sidebar.moveSelection(1); e.accepted = true }
                else if (e.key === Qt.Key_Up) { sidebar.moveSelection(-1); e.accepted = true }
                else if (e.key === Qt.Key_Right || e.key === Qt.Key_Left) e.accepted = sidebar.foldSelection(e.key === Qt.Key_Right)
            }
            // Mouse drags move accounts and groups (like the Mac sidebar); the wheel scrolls –
            // over the rows (handler on the ListView, which as a non-interactive Flickable does
            // not pass the wheel on) and over the accounts header (this one). All devices: the
            // default filter (mouse, touchpad) let no wheel through under Wayland.
            function scrollBy(e) {
                const max = Math.max(0, list.contentHeight - list.height + list.topMargin + list.bottomMargin)
                list.contentY = Math.max(-list.topMargin, Math.min(max - list.topMargin, list.contentY - e.angleDelta.y / 120 * 48))
            }
            WheelHandler { acceptedDevices: PointerDevice.AllDevices; onWheel: e => keyNav.scrollBy(e) }
        ListView {
            id: list
            anchors.fill: parent
            clip: true
            topMargin: accountsHeader.height
            bottomMargin: 10
            model: sidebar.rows
            boundsBehavior: Flickable.StopAtBounds
            interactive: false
            WheelHandler { acceptedDevices: PointerDevice.AllDevices; onWheel: e => keyNav.scrollBy(e) }
            /// A click on a row gives the sidebar the keyboard (SidebarRow).
            function takeKeyFocus() { keyNav.forceActiveFocus(Qt.MouseFocusReason) }
            ScrollBar.vertical: AppScrollBar {}
            delegate: DelegateChooser {
                role: "type"
                DelegateChoice { roleValue: "nav"; delegate: NavRow { onMenuRequested: key => sidebar.openNavMenu(key) } }
                // AllAccountsRoot.swift › AllAccountsRow: folds the tree, total per currency;
                // no count badge (it took the room of the name) – „Neue Umsätze“ and the accounts show it
                DelegateChoice { roleValue: "allAccounts"; delegate: SidebarRow {
                    id: allRow
                    selectionKey: "all"
                    // the height of a two-line account row, text centred (#658)
                    rowHeight: 34
                    TapHandler { acceptedButtons: Qt.RightButton; onTapped: allMenu.popup() }
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: SidebarModel.rowLeading; anchors.rightMargin: SidebarModel.rowTrailing
                        spacing: SidebarModel.gap
                        SidebarChevron { visible: AppModel.allAccounts.length > 0; open: AppModel.showsAccountTree
                                         onToggled: AppModel.allAccountsCollapsed = !AppModel.allAccountsCollapsed
                                         accessibleName: open ? qsTr("Alle Konten einklappen") : qsTr("Alle Konten ausklappen") }
                        Item { visible: AppModel.allAccounts.length === 0; implicitWidth: SidebarModel.slot }   // keeps the icon axis
                        SFIcon { symbol: "square.grid.2x2"; size: 16; color: Theme.secondary; Layout.preferredWidth: 20 }
                        Text { text: qsTr("Alle Konten"); font.pixelSize: Theme.body; font.weight: Font.Medium; color: Theme.text
                               elide: Text.ElideRight; Layout.fillWidth: true; Layout.minimumWidth: 30 }
                        // amounts never truncate; the name gives way (#343)
                        Repeater {
                            model: AppModel.allAccountsTotals
                            Text { required property var modelData
                                   text: Fmt.money(modelData.cents, modelData.currency)
                                   font.pixelSize: Theme.caption; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                                   color: allRow.selected ? Theme.text : (modelData.cents < 0 ? Theme.negative : Theme.secondary)
                                   Sensitive { amounts: true } }
                        }
                    }
                } }
                // AccountGroupsUI.swift › UserGroupHeader
                DelegateChoice { roleValue: "userGroup"; delegate: SidebarRow {
                    id: groupRow
                    required property var modelData
                    readonly property var g: modelData.group
                    selectionKey: "userGroup:" + g.uuid
                    // taller hit area for the drop zones (.padding(.vertical, 3))
                    topGap: g.depth === 0 ? 10 : 4
                    rowHeight: 30
                    TapHandler { acceptedButtons: Qt.RightButton; onTapped: sidebar.openGroupMenu(groupRow.g, groupRow) }
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: SidebarModel.rowLeading + g.depth * SidebarModel.iconInset; anchors.rightMargin: SidebarModel.rowTrailing
                        spacing: SidebarModel.gap
                        SidebarChevron { open: modelData.open; onToggled: AppModel.toggleCollapsedUserGroup(g.uuid)
                                         accessibleName: open ? qsTr("Gruppe einklappen") : qsTr("Gruppe ausklappen") }
                        // the group's colour, like an account's icon
                        SFIcon { symbol: g.sf_symbol; size: 15; Layout.preferredWidth: 20
                                 color: selected || !g.color_hex ? Theme.secondary : Theme.readable(g.color_hex) }
                        Text { text: g.name; font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                               Sensitive { labels: true } }
                        SFIcon { visible: !g.in_total; symbol: "eye.slash"; size: 10
                                 Accessible.name: qsTr("Nicht in der Gesamtsumme")
                                 HoverHandler { id: eyeHover }
                                 ToolTip.visible: eyeHover.hovered; ToolTip.delay: 600; ToolTip.text: qsTr("Nicht in der Gesamtsumme") }
                        // same size as in SectionHeader (#602)
                        Text { text: Fmt.money(g.balance_cents); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                               opacity: g.in_total ? 1 : 0.6
                               color: selected ? Theme.text : (g.balance_cents < 0 ? Theme.negative : Theme.secondary)
                               Sensitive { amounts: true } }
                    }
                    // an account goes to the top of this group; a group above it
                    // (upper half) or into it as a subgroup (lower half)
                    DropSlot {
                        key: "g:" + g.uuid
                        freeLeading: SidebarModel.rowLeading + SidebarModel.slot + g.depth * SidebarModel.iconInset
                        payload: g.is_ungrouped ? null : { type: "group", uuid: g.uuid, label: g.name }
                        edgeFor: (half, p) => {
                            if (p.type !== "group") return "below"
                            const into = half === "below" && !g.is_ungrouped
                            return AppModel.canDropGroup(p.uuid, g, into) ? (into ? "into" : "above") : null
                        }
                        onPerform: (p, edge) => {
                            if (p.type === "account") AppModel.moveAccount(p.id, g.uuid, 0)
                            else AppModel.dropUserGroup(p.uuid, g, edge !== "into")
                        }
                    }
                } }
                // "Nach Kontoart" or "Nach Bank": the same SectionHeader
                DelegateChoice { roleValue: "kindGroup"; delegate: SidebarRow {
                    required property var modelData
                    readonly property var g: modelData.group
                    readonly property bool isLoanGroup: g.accounts.length > 0 && g.accounts.every(a => a.kind === "Loan")
                    selectionKey: modelData.bank ? "bank:" + g.id : "group:" + g.id
                    topGap: 10
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: SidebarModel.rowLeading; anchors.rightMargin: SidebarModel.rowTrailing
                        spacing: SidebarModel.gap
                        SidebarChevron { open: modelData.open
                                         onToggled: modelData.bank ? AppModel.toggleCollapsedBank(g.id) : AppModel.toggleCollapsedGroup(g.id)
                                         accessibleName: open ? qsTr("„%1“ einklappen").arg(g.name) : qsTr("„%1“ ausklappen").arg(g.name) }
                        SFIcon { symbol: g.sf_symbol; size: 15; Layout.preferredWidth: 20 }
                        Text { text: g.name; font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                               Sensitive { labels: true; when: !modelData.bank } }   // bank names stay visible
                        Text { text: Fmt.money(g.balance_cents); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                               color: selected ? Theme.text : (g.balance_cents < 0 && isLoanGroup ? Theme.loan : (g.balance_cents < 0 ? Theme.negative : Theme.secondary))
                               Sensitive { amounts: true } }
                    }
                } }
                DelegateChoice { roleValue: "hint"; delegate: Item {
                    required property var modelData
                    readonly property int inset: 8 + SidebarModel.rowLeading + 33 + modelData.depth * SidebarModel.iconInset
                    width: ListView.view.width; height: 20
                    Text { x: parent.inset; anchors.verticalCenter: parent.verticalCenter; text: modelData.text; font.pixelSize: Theme.caption; color: Theme.tertiary }
                    // an account goes here; a group into the group (header lights up)
                    DropSlot {
                        key: "e:" + modelData.group.uuid
                        intoKey: "g:" + modelData.group.uuid
                        inset: parent.inset
                        edgeFor: (half, p) => p.type === "group" ? (AppModel.canDropGroup(p.uuid, modelData.group, true) ? "into" : null) : "above"
                        onPerform: (p, edge) => {
                            if (p.type === "account") AppModel.moveAccount(p.id, modelData.group.uuid, 0)
                            else AppModel.dropUserGroup(p.uuid, modelData.group, false)
                        }
                    }
                } }
                DelegateChoice { roleValue: "account"; delegate: AccountRow {
                    id: accountRow
                    onContextMenuRequested: sidebar.openAccountMenu(accountRow.account, accountRow)
                } }
                DelegateChoice { roleValue: "section"; delegate: Item {
                    required property var modelData
                    width: ListView.view.width; height: 30
                    Text { x: 8 + SidebarModel.rowLeading; anchors.bottom: parent.bottom; anchors.bottomMargin: 4; text: modelData.title; font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
                } }
                DelegateChoice { roleValue: "connection"; delegate: Item {
                    id: connectionRow
                    required property var modelData
                    width: ListView.view.width; height: 40
                    TapHandler { acceptedButtons: Qt.RightButton; onTapped: sidebar.openConnectionMenu(connectionRow.modelData.connection) }
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: 8 + SidebarModel.rowLeading; anchors.rightMargin: 10; spacing: 9
                        SymbolTile { symbol: modelData.connection.sf_symbol; size: 24 }
                        ColumnLayout { spacing: 1; Layout.fillWidth: true
                            Text { text: modelData.connection.display_name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
                            Text { readonly property bool setup: AppModel.missingSettings(modelData.connection).length > 0
                                   text: setup ? qsTr("Einrichtung fehlt") : qsTr("Noch nicht abgerufen"); font.pixelSize: Theme.caption2; color: setup ? Theme.warning : Theme.secondary }
                        }
                        // .disabled(bankingGate != nil).help(gate.message); the wrapper sees the hover of the disabled button
                        Item {
                            readonly property var gate: { void AppModel.bankingPolicy; return AppModel.bankingGate(modelData.connection) }
                            implicitWidth: pendingSync.implicitWidth; implicitHeight: pendingSync.implicitHeight
                            HoverHandler { id: pendingHover }
                            Button { id: pendingSync; anchors.fill: parent
                                     text: AppModel.missingSettings(modelData.connection).length ? qsTr("Einrichten") : qsTr("Abrufen")
                                     enabled: parent.gate === null
                                     ToolTip.text: parent.gate ? parent.gate.message : ""
                                     ToolTip.visible: pendingHover.hovered && ToolTip.text !== ""
                                     onClicked: AppModel.requestSync(modelData.connection) }
                        }
                    }
                } }
                // SidebarCaption / CategoriesHeader: heading of a group of sidebar pages in the style of
                // the „Konten“ heading; not selectable, a tap anywhere folds it
                DelegateChoice { roleValue: "caption"; delegate: Item {
                    id: captionRow
                    required property var modelData
                    width: ListView.view.width; height: 32
                    TapHandler { onTapped: sidebar.toggleCaption(captionRow.modelData.key) }
                    RowLayout {
                        x: 8 + SidebarModel.rowLeading; y: 12; width: parent.width - x - 14; height: 20
                        spacing: SidebarModel.gap
                        SidebarChevron { open: captionRow.modelData.open; onToggled: sidebar.toggleCaption(captionRow.modelData.key)
                                         accessibleName: open ? qsTr("%1 einklappen").arg(captionRow.modelData.title) : qsTr("%1 ausklappen").arg(captionRow.modelData.title) }
                        Text { text: captionRow.modelData.title; font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary
                               elide: Text.ElideRight; Layout.fillWidth: true; Accessible.role: Accessible.Heading }
                    }
                } }
                // SidebarView.swift › categoryRows: one icon axis per level – a leaf reserves the chevron column
                DelegateChoice { roleValue: "category"; delegate: SidebarRow {
                    id: catRow
                    required property var modelData
                    readonly property var c: modelData.category
                    selectionKey: "category:" + c.id
                    TapHandler { acceptedButtons: Qt.RightButton; onTapped: sidebar.openCategoryMenu(catRow.c, catRow) }
                    // WP3 (#366): bookings dragged from the list onto a category (leaves only)
                    BookingDropTarget { categoryID: catRow.c.id; enabled: !catRow.modelData.hasChildren }
                    RowLayout {
                        anchors.fill: parent; anchors.leftMargin: SidebarModel.rowLeading + modelData.depth * SidebarModel.iconInset; anchors.rightMargin: SidebarModel.rowTrailing
                        spacing: SidebarModel.gap
                        SidebarChevron { visible: modelData.hasChildren; open: modelData.open; onToggled: AppModel.toggleExpandedCategory(c.id)
                                         accessibleName: open ? qsTr("„%1“ einklappen").arg(c.name) : qsTr("„%1“ ausklappen").arg(c.name) }
                        Item { visible: !modelData.hasChildren; implicitWidth: SidebarModel.slot }
                        // CategoryRow: HStack(spacing: 8)
                        RowLayout {
                            spacing: 8
                            Layout.fillWidth: true
                            SFIcon { symbol: c.sf_symbol; size: 15; color: Theme.withAlpha(Theme.readable(c.color_hex), 0.85); Layout.preferredWidth: 20 }
                            Text { text: c.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { labels: true } }
                            Text { visible: c.total_cents !== 0; text: Fmt.money(c.total_cents); font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
                        }
                    }
                } }
            }
        }
        // The pages above the accounts and the „Konten“ heading with the search field: floating
        // above the first row (content y -height), 10 px below the top when scrolled up.
        Column {
            id: accountsHeader
            width: list.width
            y: -height - list.contentY
            Item { width: 1; height: 10 }
            Repeater {
                model: sidebar.topRows
                NavRow { width: accountsHeader.width; focusTarget: keyNav; onMenuRequested: key => sidebar.openNavMenu(key) }
            }
            AccountsSectionHeader { width: accountsHeader.width }
        }
        }

        // safeAreaInset(edge: .bottom): lock state + "Bank hinzufügen"
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: 34
            color: Theme.withAlpha(Theme.card, 0.6)
            Rectangle { anchors.top: parent.top; width: parent.width; height: 1; color: Theme.border }
            RowLayout {
                anchors.fill: parent; anchors.leftMargin: 12; anchors.rightMargin: 8; spacing: 6
                // Marketing screenshots (`--screenshot`) look like a real vault: no demo banner.
                readonly property bool demo: AppModel.isDemo && !AppModel.screenshotMode
                SFIcon { symbol: parent.demo ? "theatermasks" : "lock.fill"; size: 12 }
                Text {
                    text: parent.demo ? qsTr("Demo-Daten – keine echte Bank verbunden")
                                      : qsTr("Verschlüsselt · %1").arg(AppModel.lastSyncText)
                    font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                }
                IconButton { visible: parent.demo; text: AppModel.hasOwnVault ? qsTr("Eigener Tresor") : qsTr("Tresor öffnen"); symbol: ""; font.pixelSize: Theme.caption
                             onClicked: AppModel.showOwnVault() }
                // the „+“ menu (#680): accounts and groups, then categories
                IconButton { id: addButton; symbol: "plus"; symbolSize: 12; implicitWidth: 24; implicitHeight: 20
                             ToolTip.text: qsTr("Hinzufügen …"); ToolTip.visible: hovered; enabled: AppModel.isUnlocked
                             Accessible.name: qsTr("Hinzufügen")
                             onClicked: addMenu.popup(addButton, 0, -addMenu.implicitHeight) }
            }
        }
    }
    AppMenu {
        id: addMenu
        AppMenuItem { text: qsTr("Neues Konto …"); icon.name: SF.icon("building.columns"); onTriggered: AppModel.showAddBank = true }
        AppMenuItem { text: qsTr("Neue Kontengruppe …"); icon.name: SF.icon("folder.badge.plus")
                      onTriggered: AppModel.groupDialog = { kind: "create", movingAccount: null, parent: null } }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Neue Kategorie …"); icon.name: SF.icon("tag"); onTriggered: AppModel.newCategoryRequest = { parent: sidebar.selectedCategoryID } }
        AppMenuItem { text: qsTr("Neue Kategoriengruppe …"); icon.name: SF.icon("tag.circle"); onTriggered: AppModel.newCategoryRequest = { parent: null } }
    }
    function openNavMenu(key) { if (key === "newBookings") allMenu.popup(); else uncategorizedMenu.popup() }
    /// „Neue Umsätze“, „Vermögen“, „Geplant“, „Lastschriften“, „Belege & PostBox“ and „Ohne Kategorie“:
    /// a page row with the empty chevron column, icon, title and count badge.
    component NavRow: SidebarRow {
        id: navRow
        required property var modelData
        /// right click on „Neue Umsätze“ or „Ohne Kategorie“
        signal menuRequested(string key)
        selectionKey: modelData.key
        TapHandler {
            enabled: navRow.modelData.key === "newBookings" || navRow.modelData.key === "uncategorized"
            acceptedButtons: Qt.RightButton
            onTapped: navRow.menuRequested(navRow.modelData.key)
        }
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: SidebarModel.rowLeading; anchors.rightMargin: SidebarModel.rowTrailing
            spacing: SidebarModel.gap
            Item { implicitWidth: SidebarModel.slot }
            SFIcon { symbol: navRow.modelData.symbol; size: 15; color: navRow.modelData.tint || Theme.secondary; Layout.preferredWidth: 20 }
            Text { text: navRow.modelData.title; font.pixelSize: Theme.body; font.weight: navRow.modelData.regular ? Font.Normal : Font.Medium; color: Theme.text
                   Layout.fillWidth: true; elide: Text.ElideRight
                   Sensitive { labels: true; when: !!navRow.modelData.labels } }
            Badge { count: navRow.modelData.badge }
        }
    }
    // „Ohne Kategorie“ (#597): its own label; empty restores the default
    AppMenu {
        id: uncategorizedMenu
        AppMenuItem { text: qsTr("Umbenennen …"); icon.name: SF.icon("pencil"); onTriggered: AppModel.groupDialog = { kind: "renameUncategorized" } }
        AppMenuItem { visible: SidebarModel.uncategorizedName !== ""; height: visible ? implicitHeight : 0
                      text: qsTr("Standardbezeichnung"); icon.name: SF.icon("arrow.uturn.backward"); onTriggered: SidebarModel.setUncategorizedName("") }
    }

    // The drag image: the row title in a capsule, following the pointer.
    Rectangle {
        id: ghost
        readonly property point pos: sidebar.mapFromItem(null, AppModel.dragScenePos.x, AppModel.dragScenePos.y)
        visible: AppModel.dragPayload !== null
        x: pos.x - 12; y: pos.y - height / 2
        width: ghostText.implicitWidth + 20; height: 24; radius: 12
        color: Theme.card; border.color: Theme.border; opacity: 0.92
        Text { id: ghostText; anchors.centerIn: parent; text: AppModel.dragLabel; font.pixelSize: Theme.body; color: Theme.text }
        Drag.active: AppModel.dragPayload !== null
        Drag.keys: ["girocrab-sidebar"]
        Drag.hotSpot.x: 12; Drag.hotSpot.y: height / 2
        Drag.dragType: Drag.Internal
        Connections {
            target: AppModel
            function onDragEnded() { ghost.Drag.drop(); AppModel.dragPayload = null; AppModel.dropLine = null }
        }
    }
    // categoryMenu(_:) – same order as the menus of accounts and groups: rename,
    // look, place – then what only categories have – then delete
    property var menuCategory: null
    property var moveTargets: []
    function openCategoryMenu(c, row) {
        menuCategory = c
        moveTargets = AppModel.categoryMoveTargets(c.id)
        iconPopover.y = Math.max(8, Math.min(sidebar.height - 520, row.mapToItem(sidebar, 0, 0).y))
        categoryMenu.popup()
    }
    AppMenu {
        id: categoryMenu
        AppMenuItem { text: qsTr("Umbenennen …"); icon.name: SF.icon("pencil"); onTriggered: AppModel.renameCategoryTarget = sidebar.menuCategory }
        AppMenuItem { text: qsTr("Symbol && Farbe …"); icon.name: SF.icon("paintpalette"); onTriggered: AppModel.iconColorCategoryID = sidebar.menuCategory.id }
        AppMenu {
            id: moveMenu
            title: qsTr("Verschieben nach")
            icon.name: SF.icon("arrow.turn.down.right")
            AppMenuItem {
                text: qsTr("Oberste Ebene"); icon.name: SF.icon("arrow.up.to.line")
                checkable: true; checked: sidebar.menuCategory !== null && sidebar.menuCategory.parent_id === null
                onTriggered: AppModel.moveCategory(sidebar.menuCategory.id, null)
            }
            AppMenuSeparator {}
            Instantiator {
                model: sidebar.moveTargets
                delegate: AppMenuItem {
                    required property var modelData
                    text: AppModel.menuText("\u2003".repeat(modelData.depth) + modelData.name)
                    icon.name: SF.icon(modelData.sf_symbol)
                    checkable: true
                    checked: sidebar.menuCategory !== null && sidebar.menuCategory.parent_id === modelData.id
                    onTriggered: AppModel.moveCategory(sidebar.menuCategory.id, modelData.id)
                }
                onObjectAdded: (index, object) => moveMenu.insertItem(index + 2, object)
                onObjectRemoved: (index, object) => moveMenu.removeItem(object)
            }
        }
        AppMenuItem { text: qsTr("Duplizieren"); icon.name: SF.icon("plus.square.on.square"); onTriggered: SidebarModel.duplicateCategory(sidebar.menuCategory.id) }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Neue Unterkategorie …"); icon.name: SF.icon("plus"); onTriggered: AppModel.newCategoryRequest = { parent: sidebar.menuCategory.id } }
        AppMenuItem { visible: AppModel.showBudgets; height: visible ? implicitHeight : 0
                   text: qsTr("Monatsbudget festlegen …"); icon.name: SF.icon("gauge.with.dots.needle.67percent"); onTriggered: AppModel.budgetCategoryID = sidebar.menuCategory.id }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Kategorie löschen …"); icon.name: SF.icon("trash"); onTriggered: AppModel.deleteCategoryTarget = sidebar.menuCategory }
    }
    // MARK: account & group menus (SidebarView.swift › accountMenu, AccountGroupsUI.swift › UserGroupMenu)
    property var menuAccount: null
    property var menuGroup: null
    property var menuConnection: null
    property string menuBIC: ""
    /// The "Symbol & Farbe" popover sits next to the row the menu was opened on.
    function placePopover(row) { iconPopover.y = Math.max(8, Math.min(sidebar.height - 520, row.mapToItem(sidebar, 0, 0).y)) }
    function openAccountMenu(a, row, x, y) {
        menuAccount = a
        menuBIC = AppModel.bicForIban(a.iban) || ""
        placePopover(row)
        if (x === undefined) accountMenu.popup(); else accountMenu.popup(row, x, y)
    }
    function openGroupMenu(g, row, x, y) {
        menuGroup = g
        placePopover(row)
        const menu = g.is_ungrouped ? ungroupedMenu : groupMenu
        if (x === undefined) menu.popup(); else menu.popup(row, x, y)
    }
    function openConnectionMenu(c) { menuConnection = c; connectionMenu.popup() }
    readonly property var menuConnectionOfAccount: menuAccount && menuAccount.archived_unix === null && menuAccount.connection_id !== null
        ? (AppModel.connections.find(c => c.id === menuAccount.connection_id) || null) : null

    // „Alle Konten“ and „Neue Umsätze“: every account, whatever the list shows
    AppMenu {
        id: allMenu
        AppMenuItem { text: qsTr("Alle Buchungen als gelesen markieren"); icon.name: SF.icon("checkmark.circle"); enabled: AppModel.newCount > 0
                   onTriggered: AppModel.markAllChecked(AppModel.allAccounts.map(a => a.id)) }
    }
    // pending bank logins ("Neue Bankzugänge")
    AppMenu {
        id: connectionMenu
        AppMenuItem { text: qsTr("Bankzugang entfernen …"); icon.name: SF.icon("trash")
                   onTriggered: AppModel.sidebarDialog = { kind: "removeConnection", id: sidebar.menuConnection.id } }
    }
    // same order as a group's menu: rename, look, place – then what only
    // accounts have – then archive / delete
    AppMenu {
        id: accountMenu
        readonly property var a: sidebar.menuAccount
        readonly property bool archived: a !== null && a.archived_unix !== null
        // „Konto bearbeiten …“ (#639): name, owner, kind and note in one sheet
        AppMenuItem { text: qsTr("Konto bearbeiten …"); icon.name: SF.icon("square.and.pencil"); onTriggered: accountEditSheet.edit(accountMenu.a.id) }
        AppMenuItem { text: qsTr("Umbenennen …"); icon.name: SF.icon("pencil"); onTriggered: AppModel.groupDialog = { kind: "renameAccount", account: accountMenu.a } }
        AppMenuItem { text: qsTr("Symbol && Farbe …"); icon.name: SF.icon("paintpalette"); onTriggered: AppModel.iconColorTarget = { type: "account", id: accountMenu.a.id } }
        // WP3: CategoryChipToggle (#336, #604) – the tick follows the stored value
        CategoryChipToggle { account: accountMenu.a }
        AppMenu {
            id: kindMenu
            title: qsTr("Kontoart ändern …")
            icon.name: SF.icon("list.bullet.rectangle")
            Instantiator {
                model: AppModel.accountKindChoices
                delegate: AppMenuItem {
                    required property var modelData
                    // the current kind as "✓ Girokonto", like the Mac menu
                    text: accountMenu.a !== null && accountMenu.a.kind === modelData.id ? "✓ " + modelData.text : modelData.text
                    onTriggered: AppModel.setAccountKind(accountMenu.a.id, modelData.id)
                }
                onObjectAdded: (index, object) => kindMenu.insertItem(index, object)
                onObjectRemoved: (index, object) => kindMenu.removeItem(object)
            }
        }
        AppMenuItem { text: qsTr("Kontoinhaber …"); icon.name: SF.icon("person.crop.circle")
                   onTriggered: AppModel.groupDialog = { kind: "editOwner", account: accountMenu.a } }
        // AccountGroupMenu: named like the group menu's entry, so both menus read the same
        AppMenu {
            id: accountMoveMenu
            title: qsTr("Verschieben nach")
            icon.name: SF.icon("arrow.turn.down.right")
            Instantiator {
                model: AppModel.namedUserGroups
                delegate: AppMenuItem {
                    required property var modelData
                    text: AppModel.menuText(AppModel.indentedName(modelData))
                    icon.name: SF.icon(modelData.sf_symbol)
                    checkable: true
                    checked: accountMenu.a !== null && accountMenu.a.user_group_uuid === modelData.uuid
                    onTriggered: AppModel.moveAccount(accountMenu.a.id, modelData.uuid, null)
                }
                onObjectAdded: (index, object) => accountMoveMenu.insertItem(index, object)
                onObjectRemoved: (index, object) => accountMoveMenu.removeItem(object)
            }
            AppMenuItem { text: qsTr("Ohne Gruppe"); icon.name: SF.icon("tray"); checkable: true
                       checked: accountMenu.a !== null && !accountMenu.a.user_group_uuid
                       onTriggered: AppModel.moveAccount(accountMenu.a.id, null, null) }
            AppMenuSeparator {}
            AppMenuItem { text: qsTr("Neue Gruppe …"); icon.name: SF.icon("folder.badge.plus")
                       onTriggered: AppModel.groupDialog = { kind: "create", movingAccount: accountMenu.a.id, parent: null } }
        }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Alle Buchungen als gelesen markieren"); icon.name: SF.icon("checkmark.circle"); onTriggered: AppModel.markAllChecked([accountMenu.a.id]) }
        // ManualLoanAccountMenu + OpeningBalanceMenuItem (ManualLoansUI.swift, OpeningBalanceUI.swift)
        AppMenuItem { readonly property bool shown: AppModel.isEditableManualLoan(accountMenu.a)
                   visible: shown; height: shown ? implicitHeight : 0
                   text: qsTr("Darlehen bearbeiten …"); icon.name: SF.icon("pencil")
                   onTriggered: AppModel.openLoanEditor("edit", accountMenu.a.id) }
        AppMenuItem { readonly property bool shown: AppModel.isEditableManualLoan(accountMenu.a)
                   visible: shown; height: shown ? implicitHeight : 0
                   text: qsTr("Stand neu setzen …"); icon.name: SF.icon("calendar.badge.clock")
                   onTriggered: AppModel.openLoanEditor("anchor", accountMenu.a.id) }
        AppMenuSeparator { visible: AppModel.isEditableManualLoan(accountMenu.a); height: visible ? implicitHeight : 0 }
        AppMenuItem { readonly property bool shown: AppModel.canSetOpeningBalance(accountMenu.a)
                   visible: shown; height: shown ? implicitHeight : 0
                   text: qsTr("Startsaldo …"); icon.name: SF.icon("clock.arrow.circlepath")
                   onTriggered: AppModel.openOpeningBalance(accountMenu.a) }
        AppMenuItem {
            readonly property var conn: sidebar.menuConnectionOfAccount
            visible: conn !== null; height: visible ? implicitHeight : 0
            text: conn ? qsTr("%1 abrufen").arg(AppModel.menuText(accountMenu.a.name)) : ""
            icon.name: SF.icon("arrow.triangle.2.circlepath")
            enabled: conn !== null && AppModel.bankingGate(conn) === null
            onTriggered: AppModel.requestSync(conn)
        }
        // AccountCopyMenu (BalanceHeaderView.swift)
        AppMenu {
            id: copyMenu
            title: qsTr("In Zwischenablage kopieren")
            icon.name: SF.icon("doc.on.clipboard")
            AppMenuItem { visible: accountMenu.a !== null && accountMenu.a.iban !== ""; height: visible ? implicitHeight : 0
                       text: qsTr("Bankverbindung"); onTriggered: AppModel.copyToClipboard(AppModel.accountBankDetails(accountMenu.a, sidebar.menuBIC)) }
            AppMenuItem { visible: accountMenu.a !== null && accountMenu.a.iban !== ""; height: visible ? implicitHeight : 0
                       text: qsTr("IBAN"); onTriggered: AppModel.copyToClipboard(accountMenu.a.iban) }
            AppMenuItem { visible: sidebar.menuBIC !== ""; height: visible ? implicitHeight : 0
                       text: qsTr("BIC"); onTriggered: AppModel.copyToClipboard(sidebar.menuBIC) }
            AppMenuItem { text: qsTr("Kontostand"); onTriggered: AppModel.copyToClipboard(Fmt.clipboard(accountMenu.a.balance_cents)) }
        }
        AppMenuItem { text: qsTr("Belege anzeigen"); icon.name: SF.icon("doc.text.magnifyingglass")
                   onTriggered: { AppModel.documentsAccountFilter = accountMenu.a.id; AppModel.sidebarSelection = "documents" } }
        AppMenuSeparator {}
        AppMenuItem { visible: accountMenu.archived; height: visible ? implicitHeight : 0
                   text: qsTr("Reaktivieren"); icon.name: SF.icon("arrow.uturn.backward"); onTriggered: AppModel.setAccountArchived(accountMenu.a, false) }
        // only archived accounts can be deleted (GiroCrab/GiroCrab#43)
        AppMenuItem { visible: accountMenu.archived; height: visible ? implicitHeight : 0
                   text: qsTr("Konto löschen …"); icon.name: SF.icon("trash"); onTriggered: AppModel.sidebarDialog = { kind: "deleteAccount", account: accountMenu.a } }
        AppMenuItem { visible: !accountMenu.archived; height: visible ? implicitHeight : 0
                   text: qsTr("Konto archivieren …"); icon.name: SF.icon("archivebox"); onTriggered: AppModel.sidebarDialog = { kind: "archiveAccount", account: accountMenu.a } }
    }
    // UserGroupMenu: same order as an account's menu: rename, look, place – then
    // what only groups have – then delete
    AppMenu {
        id: groupMenu
        readonly property var g: sidebar.menuGroup
        AppMenuItem { text: qsTr("Umbenennen …"); icon.name: SF.icon("pencil"); onTriggered: AppModel.groupDialog = { kind: "rename", group: groupMenu.g } }
        AppMenuItem { text: qsTr("Symbol && Farbe …"); icon.name: SF.icon("paintpalette"); onTriggered: AppModel.iconColorTarget = { type: "group", uuid: groupMenu.g.uuid } }
        AppMenuItem { text: qsTr("Alle Buchungen als gelesen markieren"); icon.name: SF.icon("checkmark.circle")
                   onTriggered: AppModel.markAllChecked(AppModel.userGroupAccountIDs(groupMenu.g.uuid)) }
        AppMenu {
            id: groupMoveMenu
            title: qsTr("Verschieben nach")
            icon.name: SF.icon("arrow.turn.down.right")
            AppMenuItem {
                text: qsTr("Oberste Ebene"); icon.name: SF.icon("arrow.up.to.line")
                checkable: true; checked: groupMenu.g !== null && !groupMenu.g.parent_uuid
                onTriggered: AppModel.setUserGroupParent(groupMenu.g, null)
            }
            AppMenuSeparator {}
            Instantiator {
                model: groupMenu.g ? AppModel.parentCandidates(groupMenu.g) : []
                delegate: AppMenuItem {
                    required property var modelData
                    text: AppModel.menuText(AppModel.indentedName(modelData))
                    icon.name: SF.icon(modelData.sf_symbol)
                    checkable: true
                    checked: groupMenu.g !== null && groupMenu.g.parent_uuid === modelData.uuid
                    onTriggered: AppModel.setUserGroupParent(groupMenu.g, modelData.uuid)
                }
                onObjectAdded: (index, object) => groupMoveMenu.insertItem(index + 2, object)
                onObjectRemoved: (index, object) => groupMoveMenu.removeItem(object)
            }
        }
        AppMenuSeparator {}
        GroupFetchItem { group: groupMenu.g }
        AppMenuItem { text: qsTr("In Gesamtsumme einbeziehen"); checkable: true
                   checked: groupMenu.g !== null && groupMenu.g.include_in_total
                   onTriggered: AppModel.updateUserGroup(groupMenu.g, { includeInTotal: !groupMenu.g.include_in_total }) }
        AppMenuItem { text: qsTr("Neue Untergruppe …"); icon.name: SF.icon("folder.badge.plus")
                   onTriggered: AppModel.groupDialog = { kind: "create", movingAccount: null, parent: groupMenu.g } }
        AppMenuSeparator {}
        AppMenuItem { text: qsTr("Gruppe löschen …"); icon.name: SF.icon("trash"); onTriggered: AppModel.groupDialog = { kind: "delete", group: groupMenu.g } }
    }
    // "Ohne Gruppe": only the fetch button
    AppMenu {
        id: ungroupedMenu
        GroupFetchItem { group: sidebar.menuGroup }
    }
    /// Fetches every login behind the group's accounts, like "Alle abrufen".
    component GroupFetchItem: AppMenuItem {
        property var group: null
        text: qsTr("Konten abrufen"); icon.name: SF.icon("arrow.triangle.2.circlepath")
        enabled: group !== null && AppModel.syncingConnectionID === null
                 && AppModel.groupSyncConnections(group).some(c => AppModel.bankingGate(c) === null)
        onTriggered: AppModel.requestSyncGroup(group)
    }
    AccountEditSheet { id: accountEditSheet }
    // the "Symbol & Farbe" popover, anchored next to the category row
    IconColorPopover {
        id: iconPopover
        x: sidebar.width - 12
        y: 80
    }

    component Badge: Rectangle {
        property int count: 0
        visible: count > 0
        implicitWidth: Math.max(20, t.implicitWidth + 10); implicitHeight: 16
        radius: 8
        color: Theme.withAlpha(Theme.secondary, 0.15)
        Text { id: t; anchors.centerIn: parent; text: parent.count; font.pixelSize: Theme.caption; color: Theme.secondary }
    }
}
