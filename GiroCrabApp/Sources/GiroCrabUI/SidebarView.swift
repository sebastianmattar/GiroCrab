import SwiftUI
import GiroCrabCore

/// Column 1: collapsible account sections with subtotals, then categories.
struct SidebarView: View {
    @Environment(AppModel.self) private var model
    /// The one sidebar dialog on screen (group dialogs live in `model.groupDialog`).
    @State private var dialog: SidebarDialog?
    @State private var budgetCategory: IDBox?
    /// The account whose settings sheet is open.
    @State private var editedAccount: IDBox?
    @AppStorage(ViewLayout.budgetsKey) private var showBudgets = true
    /// Where a drop would land: a thin line above or below one row.
    @State private var dropLine: DropLine?
    /// Collapsed state of the section headers (Kategorien lives in the model).
    @AppStorage("sidebar.accountsCollapsed") private var accountsCollapsed = false
    @AppStorage("sidebar.paymentsCollapsed") private var paymentsCollapsed = false
    @AppStorage("sidebar.documentsCollapsed") private var documentsCollapsed = false

    /// One inset for every sidebar row, so chevrons and icons share their columns.
    static let captionInsets = rowInsets
    private static let rowInsets = EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 6)

    private func toggler(_ flag: Binding<Bool>) -> () -> Void {
        { withAnimation(Motion.animation(.easeInOut(duration: 0.15))) { flag.wrappedValue.toggle() } }
    }

    var body: some View {
        @Bindable var model = model
        SidebarSelectionList {
            // Pages across all accounts that are opened daily or as a starting point stay above the
            // account tree: with many accounts they must not end up below the visible part.
            // „Neue Umsätze“ (#352)
            HStack(spacing: DisclosureChevron.gap) {
                SidebarLeadingSlot()
                Image(systemName: "tray.full").foregroundStyle(Theme.secondary).frame(width: 20)
                Text("Neue Umsätze").fontWeight(.medium).appDefaultFont()
                Spacer()
            }
            .badge(model.newCount)
            .tag(SidebarItem.newBookings)
            .listRowInsets(Self.rowInsets)
            .contextMenu {
                Button("Alle Buchungen als gelesen markieren", systemImage: "checkmark.circle") { model.markAllChecked(accountIDs: model.allAccounts.map(\.id)) }
                    .disabled(model.newCount == 0)
            }
            // „Vermögen“ (#348): the counterpart of „Alle Konten“
            HStack(spacing: DisclosureChevron.gap) {
                SidebarLeadingSlot()
                Image(systemName: "chart.pie").foregroundStyle(Theme.secondary).frame(width: 20)
                Text("Vermögen").fontWeight(.medium).appDefaultFont()
                Spacer()
            }
            .tag(SidebarItem.wealth)
            .listRowInsets(Self.rowInsets)
            if !model.allAccounts.isEmpty {
                AccountsSectionHeader(open: !accountsCollapsed, toggle: toggler($accountsCollapsed))
                    .listRowInsets(Self.rowInsets)
            }
            if !accountsCollapsed || model.allAccounts.isEmpty {
            // the root of the account tree
            // no system badge here: it took the room of the name („All…“); „Neue Umsätze“ and the accounts show the count
            AllAccountsRow()
                .modifier(AccountRowHeight())
                .listRowInsets(Self.rowInsets)
                .tag(SidebarItem.allAccounts)
                .contextMenu {
                    Button("Alle Buchungen als gelesen markieren", systemImage: "checkmark.circle") { model.markAllChecked(accountIDs: model.allAccounts.map(\.id)) }
                        .disabled(model.newCount == 0)
                }
            if model.showsAccountTree {
                Group {
                    switch model.accountView {
                    case .byGroup: userGroupRows
                    case .byBank: bankGroupRows
                    case .byKind: kindGroupRows
                    }
                }
            }
            }
            // payments and documents are used less often: below the accounts, each under its heading
            SidebarCaption("Zahlungen", open: !paymentsCollapsed, toggle: toggler($paymentsCollapsed))
            if !paymentsCollapsed {
            // „Geplant“ (#240): ordered at the bank, not booked yet
            HStack(spacing: DisclosureChevron.gap) {
                SidebarLeadingSlot()
                Image(systemName: "calendar.badge.clock").foregroundStyle(Theme.secondary).frame(width: 20)
                Text("Geplant").fontWeight(.medium).appDefaultFont()
                Spacer()
            }
            .badge(model.plannedCount)
            .tag(SidebarItem.planned)
            .listRowInsets(Self.rowInsets)
            // „Lastschriften“ (#143): mandates and collections
            HStack(spacing: DisclosureChevron.gap) {
                SidebarLeadingSlot()
                Image(systemName: "arrow.down.left.arrow.up.right.square").foregroundStyle(Theme.secondary).frame(width: 20)
                Text("Lastschriften").fontWeight(.medium).appDefaultFont()
                Spacer()
            }
            .badge(model.mandateCount)
            .tag(SidebarItem.directDebits)
            .listRowInsets(Self.rowInsets)
            }
            SidebarCaption("Dokumente", open: !documentsCollapsed, toggle: toggler($documentsCollapsed))
            if !documentsCollapsed {
            HStack(spacing: DisclosureChevron.gap) {
                SidebarLeadingSlot()
                Image(systemName: "doc.on.doc").foregroundStyle(Theme.secondary).frame(width: 20)
                Text("Belege & PostBox").fontWeight(.medium).appDefaultFont()
                Spacer()
            }
            .badge(model.unseenDocumentCount)
            .contextMenu {
                Button("Alle als gelesen markieren") { model.markAllDocumentsSeen() }
                    .disabled(model.unseenDocumentCount == 0)
            }
            .tag(SidebarItem.documents)
            .listRowInsets(Self.rowInsets)
            }
            // The MoneyMoney import is an action, not a place: it lives in
            // Ablage › Vorhandene Daten importieren …, the command palette and
            // the welcome card (HIG: sidebar rows are navigation destinations).


            let pending = model.connections.filter { c in !model.allAccounts.contains { $0.connectionId == c.id } }
            if !pending.isEmpty {
                Section("Neue Bankzugänge") {
                ForEach(pending) { c in
                    HStack(spacing: 9) {
                        SymbolTile(symbol: c.sfSymbol, tint: c.tint, size: 24)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(c.displayName).lineLimit(1).appDefaultFont()
                            Text(model.missingSettings(for: c).isEmpty ? "Noch nicht abgerufen" : "Einrichtung fehlt")
                                .appFont(.caption2)
                                .foregroundStyle(model.missingSettings(for: c).isEmpty ? Theme.secondary : Theme.warning)
                        }
                        Spacer()
                        Button(model.missingSettings(for: c).isEmpty ? "Abrufen" : "Einrichten") { model.requestFullSync(c) }
                            .controlSize(.small)
                            .disabled(model.bankingGate(for: c) != nil)
                            .help(model.bankingGate(for: c)?.message ?? "")
                    }
                    .contextMenu {
                        Button("Bankzugang entfernen …", systemImage: "trash", role: .destructive) { dialog = .removeConnection(c.id) }
                    }
                }
                }
            }

            // a plain row styled like the group headers (not a List section
            // header, whose small caption style can't match them)
            CategoriesHeader()
            if !model.categoriesCollapsed, let note = model.categorySumNote {
                Text(note)
                    .appFont(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .padding(.leading, DisclosureChevron.iconInset)
                    .listRowInsets(Self.rowInsets)
                    .selectionDisabled()
            }
            if !model.categoriesCollapsed {
                Group {
                    HStack(spacing: DisclosureChevron.gap) {
                        SidebarLeadingSlot()
                        Image(systemName: "questionmark.folder").foregroundStyle(Theme.warning).frame(width: 20)
                        Text(model.uncategorizedLabel).appDefaultFont().sensitive(.labels)
                        Spacer()
                    }
                    .contextMenu {
                        Button("Umbenennen …", systemImage: "pencil") { model.groupDialog = .renameUncategorized }
                        if !model.uncategorizedName.isEmpty {
                            Button("Standardbezeichnung", systemImage: "arrow.uturn.backward") { model.setUncategorizedName("") }
                        }
                    }
                    .badge(model.uncategorizedCount)
                    .tag(SidebarItem.uncategorized)
            .listRowInsets(Self.rowInsets)
                    categoryRows(model.categoryTree, depth: 0)
                }
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .refreshable { await model.refreshCurrentView() }
        .background(Theme.sidebar)
        .navigationTitle("GiroCrab")
        #if os(macOS)
        // Keep traffic lights / sidebar toggle clear of the first row.
        .safeAreaInset(edge: .top, spacing: 0) { Color.clear.frame(height: 28) }
        #endif
        .safeAreaInset(edge: .bottom) {
            HStack(spacing: 6) {
                // Marketing screenshots (`--screenshot`) look like a real vault: no demo banner.
                if model.isDemo && LaunchOptions.current.screenshotPath == nil {
                    Image(systemName: "theatermasks")
                    Text("Demo-Daten – keine echte Bank verbunden")
                    Button(model.hasOwnVault ? "Eigener Tresor" : "Tresor öffnen") { model.showOwnVault() }
                        .buttonStyle(.borderless)
                } else {
                    Image(systemName: "lock.fill")
                    Text("Verschlüsselt · \(model.lastSyncText)")
                }
                Spacer(minLength: 0)
                Menu {
                    Button("Neues Konto …", systemImage: "building.columns") { model.showAddBank = true }
                    Button("Neues Offline-Konto …", systemImage: "banknote") { model.runInOwnVault { model.offlineStore.showNewAccount = true } }
                    Button("Neue Kontengruppe …", systemImage: "folder.badge.plus") { model.groupDialog = .create() }
                    Divider()
                    Button("Neue Kategorie …", systemImage: "tag") { model.newCategoryRequest = NewCategoryRequest(parent: selectedCategoryID) }
                    Button("Neue Kategoriengruppe …", systemImage: "tag.circle") { model.newCategoryRequest = NewCategoryRequest(parent: nil) }
                } label: {
                    Image(systemName: "plus").frame(width: 24, height: 18).contentShape(Rectangle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Hinzufügen …")
                .accessibilityLabel("Hinzufügen")
                .disabled(!model.isUnlocked)
            }
            .appFont(.caption)
            .foregroundStyle(Theme.secondary)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity)
            .background(.bar)
        }
        .confirmationDialog(
            dialog?.title ?? Text(verbatim: ""),
            isPresented: Binding(get: { dialog != nil }, set: { if !$0 { dialog = nil } }),
            titleVisibility: .visible,
            presenting: dialog
        ) { d in
            switch d {
            case .archiveAccount(let a): Button("Archivieren") { model.setAccountArchived(a, true) }
            case .deleteAccount(let a): AccountDeletionPrompt.confirmButton { model.deleteAccount(a.id) }
            case .removeConnection(let id): Button("Entfernen", role: .destructive) { model.removeConnection(id) }
            case .deleteCategory(let c): Button("Löschen", role: .destructive) { model.deleteCategory(c.id) }
            }
            Button("Abbrechen", role: .cancel) {}
        } message: { d in
            switch d {
            case .archiveAccount:
                Text("Das Konto wird nicht mehr abgerufen, von Regeln nicht mehr verändert und zählt nicht mehr zur Gesamtsumme. Buchungen, Kategorien, Belege und die Gruppe bleiben erhalten. Es lässt sich jederzeit reaktivieren.")
            case .deleteAccount(let a): AccountDeletionPrompt.message(a)
            case .deleteCategory:
                Text("Die Kategorie wird samt ihren Unterkategorien und Regeln unwiderruflich gelöscht. Zugeordnete Buchungen bleiben erhalten und stehen danach ohne Kategorie da.")
            case .removeConnection:
                Text("Alle Konten dieses Bankzugangs werden samt Buchungen, Belegen und gespeicherten Zugangsdaten aus dem Tresor gelöscht. Das lässt sich nicht widerrufen.")
            }
        }
        .capturableSheet(item: $editedAccount) { box in AccountSettingsView(accountID: box.id).environment(model) }
        .capturableSheet(item: $budgetCategory) { box in BudgetEditorSheet(categoryID: box.id).environment(model) }
    }

    /// A selected category becomes the parent of „Neue Kategorie“; otherwise a main category.
    private var selectedCategoryID: Int64? {
        if case .category(let id) = model.sidebarSelection { id } else { nil }
    }

    /// "Nach Kontoart": the fixed kind sections.
    @ViewBuilder
    private var kindGroupRows: some View {
        ForEach(model.groups) { group in
            let listed = group.accounts.filter(isShown)
            // Hide kind sections without a hit while searching, and ones holding
            // only archived accounts while those are hidden (#814).
            if model.isSidebarSectionListed(group.accounts) {
            let open = model.isKindGroupOpen(group.id)
            let toggle = {
                withAnimation(Motion.animation(.easeInOut(duration: 0.15))) {
                    if open { model.collapsedGroups.insert(group.id) } else { model.collapsedGroups.remove(group.id) }
                }
            }
            // Plain rows instead of List sections: macOS would otherwise
            // restore its own saved collapse state for sidebar sections.
            SectionHeader(
                name: group.name, sfSymbol: group.sfSymbol, balanceCents: group.balanceCents,
                item: .group(group.id),
                isLoanGroup: !group.accounts.isEmpty && group.accounts.allSatisfy { $0.kind == .loan },
                open: open, toggle: toggle
            )
                .padding(.top, 10)
                .listRowInsets(Self.rowInsets)
                .groupHeaderTap(SidebarItem.group(group.id), toggle: toggle)
                #if os(iOS)
                .contextMenu {
                    Button("Gruppenübersicht", systemImage: "list.bullet.rectangle") { model.interactiveSidebarSelection = .group(group.id) }
                }
                #endif
            ForEach(open ? listed : []) { account in
                accountRow(account)
            }
            }
        }
    }

    /// "Nach Bank": one section per bank name / connection.
    @ViewBuilder
    private var bankGroupRows: some View {
        ForEach(model.bankGroups) { group in
            let listed = group.accounts.filter(isShown)
            if model.isSidebarSectionListed(group.accounts) {
                let open = model.isBankGroupOpen(group.id)
                let toggle = {
                    withAnimation(Motion.animation(.easeInOut(duration: 0.15))) {
                        model.toggleCollapsedBank(group.id)
                    }
                }
                SectionHeader(
                    name: group.name, sfSymbol: group.sfSymbol, balanceCents: group.balanceCents,
                    item: .bank(group.id),
                    isLoanGroup: !group.accounts.isEmpty && group.accounts.allSatisfy { $0.kind == .loan },
                    nameRedaction: [],
                    open: open, toggle: toggle
                )
                .padding(.top, 10)
                .listRowInsets(Self.rowInsets)
                .groupHeaderTap(SidebarItem.bank(group.id), toggle: toggle)
                #if os(iOS)
                .contextMenu {
                    Button("Bankübersicht", systemImage: "list.bullet.rectangle") { model.interactiveSidebarSelection = .bank(group.id) }
                }
                #endif
                ForEach(open ? listed : []) { account in
                    accountRow(account)
                }
            }
        }
    }

    /// "Nach Gruppe": user groups, then "Ohne Gruppe".
    @ViewBuilder
    private var userGroupRows: some View {
        // a flattened tree: subgroups follow their group, indented by depth
        ForEach(model.userGroups.filter(model.isUserGroupListed)) { group in
            let open = model.isUserGroupOpen(group)
            let toggle = { withAnimation(Motion.animation(.easeInOut(duration: 0.15))) { model.toggleCollapsed(group) } }
            let indent = CGFloat(group.depth) * DisclosureChevron.indentStep
            UserGroupHeader(group: group, open: open, toggle: toggle)
                .padding(.vertical, 3) // taller hit area for the drop zones
                // an account goes to the top of this group; a group above it
                // (upper half) or into it as a subgroup (lower half)
                .modifier(DropSlot(key: "g:" + group.uuid, inset: 0, line: $dropLine, edge: { half in
                    guard let dragged = GroupDrag.draggedGroup else { return .below }
                    let into = half == .below && !group.isUngrouped
                    return model.canDropGroup(dragged, on: group, into: into) ? (into ? .into : .above) : nil
                }, perform: { items, edge in
                    handleDrop(items, into: group, row: nil, edge: edge)
                }, freeLeading: DisclosureChevron.width))  // the expand/collapse arrow
                .padding(.top, group.depth == 0 ? 10 : 4)
                .padding(.leading, indent)
                .listRowInsets(Self.rowInsets)
                .groupHeaderTap(SidebarItem.userGroup(group.uuid), toggle: toggle)
                .contextMenu {
                    UserGroupMenu(group: group)
                }
                .capturablePopover(isPresented: model.iconColorBinding(.group(group.uuid)), arrowEdge: .trailing) {
                    IconColorPopover(target: .group(group.uuid)).environment(model)
                }
                // no custom preview: the drag shows the row itself, like MoneyMoney
                .draggable(GroupDrag.group(group.uuid))
            if open {
                if !model.isSidebarAccountSearching,
                   group.accounts.isEmpty && !group.isUngrouped && model.userGroupSubtree(group.uuid).count == 1 {
                    Text(emptyHint)
                        .appFont(.caption)
                        .foregroundStyle(Theme.tertiary)
                        .padding(.leading, 33 + indent)
                        // an account goes here; a group into the group (header lights up)
                        .modifier(DropSlot(key: "e:" + group.uuid, inset: 33 + indent, line: $dropLine, edge: { _ in
                            if let dragged = GroupDrag.draggedGroup {
                                return model.canDropGroup(dragged, on: group, into: true) ? .into : nil
                            }
                            return .above
                        }, perform: { items, edge in
                            handleDrop(items, into: group, row: nil, edge: edge)
                        }, intoKey: "g:" + group.uuid))
                }
                // offsets of the full list: a drop position counts hidden archived accounts too
                ForEach(Array(group.accounts.enumerated()).filter { isShown($0.element) }, id: \.element.id) { i, account in
                    // an account: the line follows the half of the row under the pointer,
                    // also over the dragged row itself so it shows from the start;
                    // a group: into this group, its header lights up
                    accountRow(account, drop: DropSlot(key: "a:\(account.id)", inset: 0, line: $dropLine, edge: { half in
                        if let dragged = GroupDrag.draggedGroup {
                            return model.canDropGroup(dragged, on: group, into: true) ? .into : nil
                        }
                        return half
                    }, perform: { items, edge in
                        handleDrop(items, into: group, row: i, edge: edge)
                    }, intoKey: "g:" + group.uuid))
                        // icon on the group header's icon axis (behind the chevron column)
                        .padding(.leading, DisclosureChevron.iconInset + indent)
                        .draggable(GroupDrag.account(account.id))
                }
            }
        }
    }

    /// The account a confirmation dialog is about: its row stays highlighted (#668).
    private var dialogAccountID: Int64? {
        switch dialog {
        case .archiveAccount(let a), .deleteAccount(let a): a.id
        default: nil
        }
    }

    private var emptyHint: LocalizedStringKey {
        #if os(macOS)
        "Konten hierher ziehen"
        #else
        "Keine Konten – Konto lange drücken, um es zuzuordnen"
        #endif
    }

    /// A drop next to or onto a sidebar row. An account lands at the line
    /// next to row `row` (`row` nil = a header: top of the group). A group goes
    /// into `group` (`.into`, becomes its subgroup) or right above it as its
    /// sibling; onto "Ohne Gruppe" it moves to the top level, at the end.
    private func handleDrop(_ items: [String], into group: UserGroup, row: Int?, edge: DropLine.Edge = .above) -> Bool {
        var handled = false
        for item in items {
            if let id = GroupDrag.accountID(item) {
                let from = group.accounts.firstIndex { $0.id == id }
                let index = row.map { GroupDrag.insertionIndex(row: $0, below: edge == .below, from: from) } ?? 0
                model.moveAccount(id, to: group.uuid, at: index)
                handled = true
            } else if let uuid = GroupDrag.groupUUID(item) {
                handled = model.dropUserGroup(uuid, onto: group, before: edge != .into) || handled
            }
        }
        return handled
    }

    /// Archived accounts can be hidden; the sidebar search filters by name,
    /// Kontoinhaber, IBAN and account number (spaces ignored).
    private func isShown(_ account: Account) -> Bool {
        model.isSidebarAccountListed(account)
    }

    /// `drop` sits inside the context menu (and the caller's drag), so its drop
    /// zones don't cover the row for clicks.
    private func accountRow(_ account: Account, drop: DropSlot? = nil) -> some View {
        AccountRow(account: account, syncing: model.syncingConnectionID != nil && model.syncingConnectionID == account.connectionId)
            .dropSlot(drop)
            .background {
                if dialogAccountID == account.id {
                    RoundedRectangle(cornerRadius: 6).fill(Theme.accent.opacity(0.22)).allowsHitTesting(false)
                }
            }
            .listRowInsets(Self.rowInsets)
            .tag(SidebarItem.account(account.id))
            .contextMenu { accountMenu(account) }
            .capturablePopover(isPresented: model.iconColorBinding(.account(account.id)), arrowEdge: .trailing) {
                IconColorPopover(target: .account(account.id)).environment(model)
            }
            #if os(iOS)
            .swipeActions(edge: .trailing) {
                Button("Gruppe …", systemImage: "folder") { model.groupDialog = .moveAccount(account) }
                    .tint(Theme.accent)
            }
            #endif
    }

    @ViewBuilder
    private func accountMenu(_ account: Account) -> some View {
        // name, symbol & colour, Kontoart, Kontoinhaber, Startsaldo and the rest of the
        // account's data live in the settings window; the menu keeps the actions
        Button("Einstellungen …", systemImage: "gearshape") { editedAccount = IDBox(id: account.id) }
        CategoryChipToggle(accountID: account.id)
        AccountGroupMenu(account: account)
        // the account's own switch for the overall money totals (#337); the
        // group (or a group that group is in) and archiving can still keep it
        // out — the row's tooltip explains that precedence
        Toggle(isOn: Binding(
            get: { account.includeInTotal },
            set: { model.setAccountIncludeInTotal(account, $0) }
        )) {
            Label("In Gesamtsummen zählen", systemImage: "plus.forwardslash.minus")
        }
        Divider()
        Button("Alle Buchungen als gelesen markieren", systemImage: "checkmark.circle") { model.markAllChecked(accountIDs: [account.id]) }
        ManualLoanAccountMenu(model: model, account: account)
        OfflineAccountMenuItems(model: model, account: account)
        if !account.isArchived, let cid = account.connectionId, let conn = model.connections.first(where: { $0.id == cid }) {
            Button("\(account.name) abrufen", systemImage: "arrow.triangle.2.circlepath") { model.requestSyncAccount(account) }
                .disabled(model.bankingGate(for: conn) != nil || model.syncingConnectionID != nil)
        }
        AccountCopyMenu(account: account)
        Button("Belege anzeigen", systemImage: "doc.text.magnifyingglass") {
            model.documentsAccountFilter = account.id
            model.documentsConnectionFilter = nil
            model.interactiveSidebarSelection = .documents
        }
        Divider()
        if account.isArchived {
            Button("Reaktivieren", systemImage: "arrow.uturn.backward") { model.setAccountArchived(account, false) }
            // only archived accounts can be deleted (GiroCrab/GiroCrab#43)
            Button("Konto löschen …", systemImage: "trash", role: .destructive) { dialog = .deleteAccount(account) }
        } else {
            Button("Konto archivieren …", systemImage: "archivebox") { dialog = .archiveAccount(account) }
        }
    }

    /// The category tree at any depth (#101): a node with sub-categories is a
    /// disclosure group whose content recurses into its children.
    private func categoryRows(_ nodes: [CategoryNode], depth: Int) -> AnyView {
        let indent = CGFloat(depth) * DisclosureChevron.indentStep
        return AnyView(ForEach(nodes) { node in
            if let kids = node.children {
                let open = model.expandedCategories.contains(node.id)
                HStack(spacing: DisclosureChevron.gap) {
                    Button {
                        withAnimation(Motion.animation(.easeInOut(duration: 0.15))) {
                            if open { model.expandedCategories.remove(node.id) } else { model.expandedCategories.insert(node.id) }
                        }
                    } label: {
                        DisclosureChevron(open: open)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(open ? "„\(node.category.name)“ einklappen" : "„\(node.category.name)“ ausklappen")
                    .accessibilityValue(open ? "Ausgeklappt" : "Eingeklappt")
                    CategoryRow(category: node.category)
                }
                .padding(.leading, indent)
                .listRowInsets(Self.rowInsets)
                #if os(macOS)
                // a category dragged here goes above this row, or into it (lower half)
                .modifier(categoryDropSlot(node, indent: indent, takesBookings: false, chevron: true))
                #endif
                .tag(SidebarItem.category(node.id))
                .contextMenu { categoryMenu(node.category) }
                .modifier(CategoryIconColorPopover(id: node.id))
                #if os(macOS)
                .draggable(GroupDrag.category(node.id))
                #endif
                if open {
                    categoryRows(kids, depth: depth + 1)
                }
            } else {
                HStack(spacing: DisclosureChevron.gap) {
                    SidebarLeadingSlot()
                    CategoryRow(category: node.category)
                }
                .padding(.leading, indent)
                .listRowInsets(Self.rowInsets)
                #if os(macOS)
                // only leaves take dragged bookings, as only leaves are assigned;
                // a dragged category goes above or below, or into a main category (#977)
                .modifier(categoryDropSlot(node, indent: indent, takesBookings: true))
                #endif
                .tag(SidebarItem.category(node.id))
                .contextMenu { categoryMenu(node.category) }
                .modifier(CategoryIconColorPopover(id: node.id))
                #if os(macOS)
                .draggable(GroupDrag.category(node.id))
                #endif
            }
        })
    }

    #if os(macOS)
    /// Drop zones of a category row (#977): a category goes above the row (upper
    /// half), below it or – on a row with sub-categories or a main category – into
    /// it as its last sub-category (lower half). Bookings (`takesBookings`, leaves
    /// only) are assigned to the row's category. The slot sits outside the row's
    /// `indent` padding, so on a row with a `chevron` the zones leave the indent
    /// and the arrow free, else it takes no clicks below the first level (#1137).
    private func categoryDropSlot(_ node: CategoryNode, indent: CGFloat, takesBookings: Bool, chevron: Bool = false) -> DropSlot {
        DropSlot(key: "c:\(node.id)", inset: indent, line: $dropLine, edge: { half in
            guard let dragged = GroupDrag.draggedCategory else { return .into }
            guard model.canDropCategory(dragged, on: node.id) else { return nil }
            return half == .below && model.categoryTakesDropInto(node.id) ? .into : half
        }, perform: { items, edge in
            if items.contains(where: { GroupDrag.categoryID($0) != nil }) {
                return model.dropCategory(items, onto: node.id, place: edge == .into ? .into : edge == .below ? .after : .before)
            }
            return takesBookings && model.dropBookings(items, onCategory: node.id)
        }, freeLeading: chevron ? indent + DisclosureChevron.width : 0, takesBookings: takesBookings, takesCategories: true)
    }
    #endif

    @ViewBuilder
    private func categoryMenu(_ category: GiroCrabCore.Category) -> some View {
        // same order as the menus of accounts and groups (#183): rename, look,
        // place – then what only categories have – then delete
        Button("Umbenennen …", systemImage: "pencil") { model.groupDialog = .renameCategory(category) }
        Button("Symbol & Farbe …", systemImage: "paintpalette") { model.iconColorTarget = .category(category.id) }
        Menu("Verschieben nach", systemImage: "arrow.turn.down.right") {
            Toggle(isOn: Binding(
                get: { category.parentId == nil },
                set: { _ in model.moveCategory(category.id, to: nil) }
            )) {
                Label("Oberste Ebene", systemImage: "arrow.up.to.line")
            }
            Divider()
            ForEach(model.categoryMoveTargets(for: category.id)) { c in
                Toggle(isOn: Binding(
                    get: { category.parentId == c.id },
                    set: { _ in model.moveCategory(category.id, to: c.id) }
                )) {
                    Label(String(repeating: "\u{2003}", count: Int(c.depth)) + c.name, systemImage: c.sfSymbol)
                }
            }
        }
        Button("Nach oben", systemImage: "arrow.up") { model.shiftCategory(category.id, by: -1) }
            .disabled(!model.canShiftCategory(category.id, by: -1))
        Button("Nach unten", systemImage: "arrow.down") { model.shiftCategory(category.id, by: 1) }
            .disabled(!model.canShiftCategory(category.id, by: 1))
        Button("Duplizieren", systemImage: "plus.square.on.square") { model.duplicateCategory(category.id) }
        Divider()
        Button("Neue Unterkategorie …", systemImage: "plus") { model.newCategoryRequest = NewCategoryRequest(parent: category.id) }
        if showBudgets {
            Button("Monatsbudget festlegen …", systemImage: "gauge.with.dots.needle.67percent") { budgetCategory = IDBox(id: category.id) }
        }
        Divider()
        Button("Kategorie löschen …", systemImage: "trash", role: .destructive) { dialog = .deleteCategory(category) }
    }
}

/// Gives single-line rows ("Alle Konten") the height of a two-line account row,
/// text centred vertically in the selection pill (#658).
private struct AccountRowHeight: ViewModifier {
    @Environment(\.textScale) private var scale
    func body(content: Content) -> some View {
        content.frame(minHeight: 34 * scale, alignment: .center)
    }
}

/// The sidebar `List` and its selection. Reading the selection binding made the reading view
/// depend on `sidebarSelection`: in `SidebarView` every click re-ran its whole body and rebuilt
/// the outline. Here only this wrapper re-runs; its rows are the value `SidebarView` built last,
/// so they stay as they are and only the selection highlight moves.
private struct SidebarSelectionList<Content: View>: View {
    @Environment(AppModel.self) private var model
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        #if os(macOS)
        List(selection: Binding(get: { model.sidebarSelection }, set: { model.selectFromSidebar($0) })) { content }
        #else
        @Bindable var model = model
        List(selection: $model.interactiveSidebarSelection) { content }
        #endif
    }
}

/// Disclosure arrow of every collapsible sidebar header: `chevron.right` when
/// closed, `chevron.down` when open. One 12 pt column, so the icons of
/// all headers and accounts share one vertical axis.
struct DisclosureChevron: View {
    /// The one leading column of the sidebar: every chevron sits in it, and rows without one reserve it.
    static let leadingSlotWidth: CGFloat = 12
    /// Indent of a child account / sub-category / subgroup per depth level.
    static let indentStep: CGFloat = 12
    static let width: CGFloat = leadingSlotWidth
    /// Gap between the chevron column and the icon (HStack spacing of the headers).
    static let gap: CGFloat = 5
    /// Leading offset of an account row: its icon sits on the headers' icon axis.
    static let iconInset: CGFloat = width + gap
    let open: Bool
    var body: some View {
        Image(systemName: open ? "chevron.down" : "chevron.right")
            .appFont(.caption, weight: .bold)
            .frame(width: Self.width, height: 18)
            .contentShape(Rectangle())
    }
}

struct SectionHeader: View {
    @Environment(AppModel.self) private var model
    let name: String
    let sfSymbol: String
    let balanceCents: Int64
    /// The header's own sidebar item. Its selection flag (`sidebarRowSelection(for:)`) is read
    /// here, so a selection change re-runs only the old and the new header, not
    /// `SidebarView.body` (the whole outline).
    let item: SidebarItem
    var isLoanGroup: Bool = false
    /// Bug report screenshot (#301): category-like group names hide on request, bank names stay visible.
    var nameRedaction: ReportRedaction = .labels
    var open: Bool = true
    var toggle: () -> Void = {}

    /// Selected header: hierarchical styles so macOS can draw them light on the
    /// accent-coloured selection (fixed theme colours stay dark there).
    private var selected: Bool { model.sidebarRowSelection(for: item).isSelected }

    var body: some View {
        HStack(spacing: 5) {
            Button(action: toggle) {
                DisclosureChevron(open: open)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityLabel(open ? "„\(name)“ einklappen" : "„\(name)“ ausklappen")
            .accessibilityValue(open ? "Ausgeklappt" : "Eingeklappt")
            // styled like the "Gruppen" headers (UserGroupHeader), so both
            // views read the same and no header is smaller than its accounts (#171)
            Image(systemName: sfSymbol)
                .appFont(.body, weight: .semibold)
                .foregroundStyle(.secondary)
                .frame(width: 20)
                .accessibilityHidden(true)
            Text(name)
                .sensitive(nameRedaction)
                .lineLimit(2) // a second line instead of "…" at large text sizes (#571)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Text(Money.format(balanceCents))
                .sensitive(.amounts)
                .appFont(.footnote, weight: .semibold) // same size as the account balances
                .monospacedDigit()
                .fixedSize() // the name truncates, never the balance (#343)
                .foregroundStyle(selected ? AnyShapeStyle(.primary) : AnyShapeStyle(balanceCents < 0 && isLoanGroup ? Theme.loan : (balanceCents < 0 ? Theme.negative : Theme.secondary)))
        }
        .appFont(.body, weight: .semibold) // as big as the account names
        .foregroundStyle(.secondary)
    }
}

struct AccountRow: View {
    @Environment(AppModel.self) private var model
    /// Selected row: the amount uses the hierarchical `.primary` style, which
    /// macOS turns white on the accent-coloured selection (fixed theme colours
    /// stay dark there). `backgroundProminence` is not set in a macOS sidebar.
    private var onSelection: Bool { model.sidebarRowSelection(for: .account(account.id)).isSelected }
    @Environment(\.reportRedaction) private var redaction
    let account: Account
    var syncing: Bool = false

    private var repaid: Double? {
        guard account.kind == .loan, let p = account.initialBalanceCents, p > 0 else { return nil }
        return max(0, min(1, 1 - Double(-account.balanceCents) / Double(p)))
    }

    var body: some View {
        HStack(spacing: DisclosureChevron.gap) {
            // tinted like the account's marker in the booking list
            AccountSignet(account: account, size: 20, color: model.accountColor(account.id), onSelection: onSelection)
                .overlay(alignment: .topTrailing) {
                    // unread count pinned to the icon's corner, so the subtitle stays untruncated
                    if let text = NewCountBadge.label(for: Int(account.newCount)) {
                        Text(verbatim: text)
                            .appFont(.caption2, weight: .bold, monospacedDigit: true)
                            .foregroundStyle(.white)
                            .padding(.horizontal, 4)
                            .frame(minWidth: 14, minHeight: 14)
                            .background(Capsule().fill(Theme.accent))
                            .overlay(Capsule().strokeBorder(.background, lineWidth: 1.5))
                            .fixedSize()
                            .offset(x: 5, y: -4)
                            .accessibilityLabel(Text("\(Int(account.newCount)) neue Buchungen"))
                    }
                }
            VStack(alignment: .leading, spacing: 1) {
                // row 1: the whole width for the name
                HStack(spacing: 4) {
                    // the owner takes no room in the title line: tooltip only
                    Text(account.name).appFont(.callout, weight: .medium).lineLimit(1).sensitive()
                        .help(redaction.masking(account.owner))
                    if account.accessRevoked {
                        Image(systemName: "lock.slash")
                            .appFont(.caption2)
                            .foregroundStyle(Theme.warning)
                            .help("Zugriff entzogen: Die Bank liefert dieses Konto nicht mehr. Buchungen bleiben erhalten.")
                            .accessibilityLabel(Text("Zugriff entzogen"))
                    }
                    Spacer(minLength: 4)
                    if syncing {
                        ProgressView().controlSize(.mini)
                            .accessibilityLabel(Text("Wird synchronisiert"))
                    }
                    // outside the overall money totals (#337): like a group's
                    // eye.slash — the effective flag (own switch, group, archiving)
                    if account.showsTotalExclusionIndicator {
                        AccountTotalExclusionIndicator(
                            help: model.totalExclusionIconHelp(account, redaction: redaction))
                    }
                }
                // row 2: bank / loan progress left, balance right
                HStack(spacing: 6) {
                    if let repaid {
                        ProgressView(value: repaid)
                            .progressViewStyle(.linear)
                            .tint(Theme.loan.opacity(0.7))
                            .controlSize(.mini)
                            .frame(maxWidth: 44)
                            .sensitive(.amounts)
                            .help(redaction.masking("\(repaid.formatted(.percent.precision(.fractionLength(0)))) getilgt", .amounts))
                        Text("\(repaid.formatted(.percent.precision(.fractionLength(0)))) getilgt")
                            .sensitive(.amounts)
                            .appFont(.caption2).foregroundStyle(.secondary).lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                    } else if let date = account.archivedDate {
                        Text("Archiviert \(date.formatted(.dateTime.day(.twoDigits).month(.twoDigits).year(.twoDigits)))")
                            .appFont(.caption2).foregroundStyle(.secondary).lineLimit(1)
                            .help("Archiviert – wird nicht mehr abgerufen")
                    } else {
                        Text(searchAwareSubtitle)
                            .sensitive(searchAwareSubtitle == account.bankName ? [] : .personal) // owner / IBAN hits
                            .appFont(.caption2, design: ibanSubtitle ? .monospaced : .default)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    // archived accounts show no balance (#667)
                    if !account.isArchived {
                        Text(Money.format(account.balanceCents, currency: account.currency))
                            .sensitive(.amounts)
                            .appFont(.subheadline, monospacedDigit: true)
                            .fixedSize() // the subtitle truncates, never the balance (#343)
                            .foregroundStyle(onSelection ? AnyShapeStyle(.primary) : AnyShapeStyle(account.kind == .loan ? Theme.loan : (account.balanceCents < 0 ? Theme.negative : Theme.text)))
                    }
                }
            }
        }
        .padding(.vertical, 1)
        .opacity(account.isArchived ? 0.55 : (account.showsTotalExclusionIndicator ? 0.6 : 1))
        // reachable explanation (#337): why the account stays out of the totals
        // although its own switch is on — the group that wins, or archiving
        .helpIfPresent(model.totalExclusionReason(account, redaction: redaction))
        // one element per account: unread count, name, bank / loan progress, balance
        .accessibilityElement(children: .combine)
    }

    /// While searching, prefer Kontoinhaber or IBAN so the hit is obvious.
    /// Bank name and the account note (#679), e.g. "Bargeld" without a bank shows just its note.
    private var baseSubtitle: String {
        let note = model.accountNote(for: account.id).replacingOccurrences(of: "\n", with: " ")
        // the owner tells accounts of the same name apart
        let owner = model.accountDistinction(account)
        return [account.bankName, owner, note].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    private var searchAwareSubtitle: String {
        guard model.isSidebarAccountSearching else { return baseSubtitle }
        let q = model.sidebarAccountSearch.trimmingCharacters(in: .whitespacesAndNewlines)
        let compact = q.filter { !$0.isWhitespace }.lowercased()
        if !account.owner.isEmpty {
            let terms = q.split(whereSeparator: \.isWhitespace)
            if !terms.isEmpty, terms.allSatisfy({ account.owner.localizedCaseInsensitiveContains($0) }) {
                return account.owner
            }
        }
        if !compact.isEmpty, compact.count >= 3,
           account.compactAccountIds.contains(where: { $0.contains(compact) }) {
            return account.iban.isEmpty ? account.accountNumber : formatIBAN(account.iban)
        }
        return baseSubtitle
    }

    private var ibanSubtitle: Bool {
        model.isSidebarAccountSearching && searchAwareSubtitle == formatIBAN(account.iban)
    }
}

struct CategoryRow: View {
    @Environment(AppModel.self) private var model
    let category: GiroCrabCore.Category

    var body: some View {
        // "Zusätzlich" (#1049): what the category holds in currencies other than EUR, under its total.
        let other = (model.categoryOtherTotals[category.id] ?? []).map { Money.format($0.cents, currency: $0.currency) }.joined(separator: " · ")
        HStack(spacing: 8) {
            Image(systemName: category.sfSymbol)
                .foregroundStyle(Color.readable(hex: category.colorHex).opacity(0.85))
                .frame(width: 20)
            Text(category.name).lineLimit(1).appDefaultFont().sensitive(.labels)
            Spacer(minLength: 4)
            if category.totalCents != 0 || !other.isEmpty {
                VStack(alignment: .trailing, spacing: 1) {
                    if category.totalCents != 0 {
                        Text(Money.format(category.totalCents, currency: model.categorySumCurrency))
                            .sensitive(.amounts)
                            .appFont(.caption, monospacedDigit: true)
                            .fixedSize()
                            .foregroundStyle(.secondary) // follows selection, Dark Mode, Increase Contrast
                    }
                    if !other.isEmpty {
                        Text("Zusätzlich: \(other)")
                            .sensitive(.amounts)
                            .appFont(.caption2, monospacedDigit: true)
                            .fixedSize()
                            .foregroundStyle(.secondary)
                            .help("Weitere Währungen werden nicht umgerechnet und nicht in die Summe einbezogen.")
                    }
                }
            }
        }
    }
}

private extension View {
    /// macOS: clicking the header selects the group (the chevron collapses).
    /// iOS: the whole header collapses/expands – a bigger target than the
    /// chevron; the group overview is in the long-press menu.
    @ViewBuilder
    func groupHeaderTap(_ item: SidebarItem, toggle: @escaping () -> Void) -> some View {
        #if os(macOS)
        self.tag(item)
        #else
        self.contentShape(Rectangle()).onTapGesture(perform: toggle)
        #endif
    }
}

/// The insertion line a drag shows: above or below one sidebar row.
private struct DropLine: Equatable {
    /// `into`: onto the row itself (a group becoming a subgroup).
    enum Edge { case above, below, into }
    /// The row that draws the mark.
    let key: String
    let edge: Edge
    /// The drop zone under the pointer (one row half; may mark another row).
    let zone: String
}

/// A sidebar row as drop target. Like MoneyMoney and Finder it marks the
/// spot with a thin line between rows instead of outlining the row. The row
/// gets two drop zones, its upper and lower half, so the line can follow the
/// pointer; the drops go through `MoveStringDrop` (a `DropDelegate` that
/// proposes `.move`, so no green "+" shows, #659).
private struct DropSlot: ViewModifier {
    let key: String
    /// Where the line starts, so it lines up with the row's content.
    let inset: CGFloat
    @Binding var line: DropLine?
    /// The line's edge for the pointer over the given half; nil = no drop here.
    let edge: (_ half: DropLine.Edge) -> DropLine.Edge?
    let perform: (_ items: [String], _ edge: DropLine.Edge) -> Bool
    /// The row that lights up for `.into` (default: this one), e.g. the
    /// group's header while a group hovers over its accounts.
    var intoKey: String?
    /// Leading strip the drop zones leave free, so a button there still gets
    /// its clicks (the zones lie on top of the row).
    var freeLeading: CGFloat = 0
    /// Mark only booking drags (category rows), else only account and group
    /// drags. Gates just the mark: `perform` ignores foreign payloads itself.
    var takesBookings = false
    /// Also mark a category drag (#977; category rows).
    var takesCategories = false

    /// Whether the drag in progress is one this row marks: bookings only on
    /// `takesBookings` rows, categories only on `takesCategories` rows, else only
    /// account and group drags.
    private var acceptsDrag: Bool {
        if BookingDrag.isDragged { return takesBookings }
        if GroupDrag.draggedCategory != nil { return takesCategories }
        return !takesBookings && !takesCategories
    }

    /// Zones a drag is over right now. Not state: changing it must not redraw.
    @MainActor private static var hovered: Set<String> = []

    func body(content: Content) -> some View {
        content
            .overlay {
                VStack(spacing: 0) {
                    zone(.above)
                    zone(.below)
                }
                .padding(.leading, freeLeading)
            }
            .background {
                // dropping into the row: a highlight, like Finder on a folder
                if line?.key == key, line?.edge == .into {
                    RoundedRectangle(cornerRadius: 5)
                        .fill(Theme.accent.opacity(0.18))
                        .strokeBorder(Theme.accent, lineWidth: 1.5)
                        .padding(.leading, inset - 4)
                        .padding(.vertical, -2)
                        .allowsHitTesting(false)
                }
            }
            .overlay(alignment: line?.edge == .below ? .bottom : .top) {
                if line?.key == key, line?.edge != .into {
                    // a small ring, then a thin line, centred on the gap between two rows
                    HStack(spacing: 0) {
                        Circle().strokeBorder(Theme.secondary, lineWidth: 1.5).frame(width: 7, height: 7)
                        Rectangle().fill(Theme.secondary).frame(height: 1.5)
                    }
                    .padding(.leading, inset)
                    .offset(y: line?.edge == .below ? 3.5 : -3.5)
                    .allowsHitTesting(false)
                }
            }
    }

    private func zone(_ half: DropLine.Edge) -> some View {
        Color.clear
            .contentShape(Rectangle())
            .onDrop(of: [.plainText], delegate: MoveStringDrop(isTargeted: { targeted in
                setTargeted(targeted, half)
            }, perform: { items in
                line = nil
                guard let edge = edge(half) else { return false }
                return perform(items, edge)
            }))
    }

    private func setTargeted(_ targeted: Bool, _ half: DropLine.Edge) {
        // SwiftUI reports a quick off/on while the pointer moves within a
        // zone: write only real changes and clear a tick later, unless the
        // zone came back meanwhile, so the mark doesn't flicker.
        let zone = key + (half == .above ? "↑" : "↓")
        if targeted, let edge = edge(half), acceptsDrag {
            Self.hovered.insert(zone)
            let next = DropLine(key: edge == .into ? intoKey ?? key : key, edge: edge, zone: zone)
            if line != next { line = next }
        } else {
            Self.hovered.remove(zone)
            DispatchQueue.main.async {
                if !Self.hovered.contains(zone), line?.zone == zone { line = nil }
            }
        }
    }
}

extension View {
    @ViewBuilder
    fileprivate func dropSlot(_ slot: DropSlot?) -> some View {
        if let slot { modifier(slot) } else { self }
    }
}

/// Sidebar-only dialogs (account, bank login, category). One state instead
/// of one flag per dialog.
private enum SidebarDialog {
    case archiveAccount(Account)
    case deleteAccount(Account)
    case removeConnection(Int64)
    case deleteCategory(GiroCrabCore.Category)

    var title: Text {
        switch self {
        case .archiveAccount(let a): Text("Konto „\(a.name)“ archivieren?")
        case .deleteAccount(let a): AccountDeletionPrompt.title(a)
        case .removeConnection: Text("Bankzugang entfernen?")
        case .deleteCategory(let c): Text("Kategorie „\(c.name)“ löschen?")
        }
    }
}

/// The "Symbol & Farbe" popover of a category row, anchored to the row like
/// those of accounts and groups.
private struct CategoryIconColorPopover: ViewModifier {
    @Environment(AppModel.self) private var model
    let id: Int64

    func body(content: Content) -> some View {
        content.capturablePopover(isPresented: model.iconColorBinding(.category(id)), arrowEdge: .trailing) {
            IconColorPopover(target: .category(id)).environment(model)
        }
    }
}

/// Header of the category tree: chevron, symbol and name like a group header
/// (`UserGroupHeader`), the „+“ for a new main category on the right.
/// Heading of a group of sidebar pages, in the style of the „Konten“ heading. Not selectable.
private struct SidebarCaption: View {
    let title: LocalizedStringKey
    let open: Bool
    let toggle: () -> Void
    init(_ title: LocalizedStringKey, open: Bool, toggle: @escaping () -> Void) {
        self.title = title
        self.open = open
        self.toggle = toggle
    }

    var body: some View {
        HStack(spacing: DisclosureChevron.gap) {
            Button(action: toggle) { DisclosureChevron(open: open) }
                .buttonStyle(.plain)
                .accessibilityLabel(open ? Text("\(Text(title)) einklappen") : Text("\(Text(title)) ausklappen"))
                .accessibilityValue(open ? "Ausgeklappt" : "Eingeklappt")
            Text(title)
                .appFont(.caption, weight: .semibold)
                .accessibilityAddTraits(.isHeader)
            Spacer()
        }
        .foregroundStyle(Theme.secondary)
        .padding(.top, 10)
        .contentShape(Rectangle())
        .onTapGesture(perform: toggle)
        .listRowInsets(SidebarView.captionInsets)
        .selectionDisabled()
    }
}

/// Empty stand-in for the chevron on rows that cannot expand, so their icons share the headers' icon axis.
private struct SidebarLeadingSlot: View {
    var body: some View {
        Color.clear.frame(width: DisclosureChevron.leadingSlotWidth, height: 18).accessibilityHidden(true)
    }
}

private struct CategoriesHeader: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let open = !model.categoriesCollapsed
        let toggle = { withAnimation(Motion.animation(.easeInOut(duration: 0.15))) { model.categoriesCollapsed.toggle() } }
        HStack(spacing: DisclosureChevron.gap) {
            // folds the category tree away (rarely used, takes room)
            Button(action: toggle) {
                DisclosureChevron(open: open)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(open ? "Kategorien einklappen" : "Kategorien ausklappen")
            .accessibilityValue(open ? "Ausgeklappt" : "Eingeklappt")
            // caption style like „Konten“, „Zahlungen“ and „Dokumente“ (#676); „+“ lives in the bottom bar
            Text("Kategorien").lineLimit(1)
                .appFont(.caption, weight: .semibold)
                .accessibilityAddTraits(.isHeader)
            Spacer()
        }
        .foregroundStyle(Theme.secondary)
        .padding(.top, 10)
        .contentShape(Rectangle())
        .onTapGesture(perform: toggle)
        .listRowInsets(SidebarView.captionInsets)
        .selectionDisabled()
    }
}

/// Small bubble with the number of new bookings, next to an account's name.
struct NewCountBadge: View {
    let count: Int
    var onSelection = false

    /// The text in the bubble; nil hides it. Long numbers are capped.
    static func label(for count: Int) -> String? {
        guard count > 0 else { return nil }
        return count > 999 ? "999+" : "\(count)"
    }

    var body: some View {
        if let text = Self.label(for: count) {
            Text(verbatim: text)
                .appFont(.caption2, weight: .semibold, monospacedDigit: true)
                .foregroundStyle(onSelection ? AnyShapeStyle(.primary) : AnyShapeStyle(Theme.accent))
                .padding(.horizontal, 5)
                .padding(.vertical, 1)
                .background(Capsule().fill(Theme.accent.opacity(onSelection ? 0.35 : 0.18)))
                .overlay(Capsule().strokeBorder(onSelection ? AnyShapeStyle(.primary) : AnyShapeStyle(Theme.accent), lineWidth: 1))
                .fixedSize() // the name truncates, never the count
                .accessibilityLabel(Text("\(count) neue Buchungen"))
        }
    }
}

/// „Kategorie in der Buchungszeile anzeigen“ in an account's menu (GitHub #604).
/// Reads the setting through `@AppStorage` so the checkmark follows the stored value;
/// a Binding over `UserDefaults` is not observed and kept a stale checkmark in the menu.
private struct CategoryChipToggle: View {
    let accountID: Int64
    @AppStorage(ViewLayout.hiddenCategoryChipAccountsKey) private var hiddenRaw = ""

    var body: some View {
        Toggle(isOn: Binding(
            get: { !hiddenIDs.contains(accountID) },
            set: { show in
                var ids = hiddenIDs
                if show { ids.remove(accountID) } else { ids.insert(accountID) }
                hiddenRaw = ids.sorted().map(String.init).joined(separator: ",")
            }
        )) {
            Label("Kategorie in der Buchungszeile anzeigen", systemImage: "tag")
        }
    }

    private var hiddenIDs: Set<Int64> { ViewLayout.hiddenCategoryChipAccounts(raw: hiddenRaw) }
}
