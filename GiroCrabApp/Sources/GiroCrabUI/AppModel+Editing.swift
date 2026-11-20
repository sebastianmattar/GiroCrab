import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Editing

    func setCategory(_ categoryID: Int64?, for booking: Booking) {
        setCategory(categoryID, for: [booking])
    }

    /// What a row's category menu and drag act on: the multiple selection
    /// when the row is part of it, otherwise the row alone (like Finder).
    /// Looked up by ID: every selected row's menu asks, and `selectedBookings`
    /// filters the whole list.
    func categorizeTargets(for booking: Booking) -> [Booking] {
        guard isMultiSelection, selectedBookingIDs.contains(booking.id) else { return [booking] }
        return selectedBookingIDs.compactMap(self.booking(withID:))
    }

    /// Bookings dragged onto a sidebar category (#366); false when `items`
    /// carry no loaded booking.
    func dropBookings(_ items: [String], onCategory categoryID: Int64) -> Bool {
        let dropped = items.flatMap(BookingDrag.ids).compactMap(booking(withID:))
        guard !dropped.isEmpty else { return false }
        setCategory(categoryID, for: dropped)
        return true
    }

    /// One category for several bookings (multiple selection).
    /// The write is cheap; the reloads behind it are not (~0.5 s on 30k bookings).
    /// So the rows show the new category at once and everything derived from it
    /// (totals, counts, KPIs) follows from a background refresh.
    func setCategory(_ categoryID: Int64?, for bookings: [Booking]) {
        guard let vault, !bookings.isEmpty else { return }
        do {
            for b in bookings { try vault.setTransactionCategory(transactionId: b.id, categoryId: categoryID) }
        } catch {
            // Partly written: show the database state as before (synchronous).
            errorMessage = describe(error)
            reloadAfterCategoryChange()
            return
        }
        showCategory(categoryID, on: Set(bookings.map(\.id)))
        // Merchant histories count categories: keep the shown one until the new one is in.
        refreshSelectedBookingHistory()
        // Rule counts and the investment dashboard: reload only when on screen,
        // otherwise when the rule editor opens / the Investments scope is shown.
        if showRuleEditor { reloadRules() } else { rulesStale = true }
        reloadDashboard()
        refreshAfterEdit()
    }

    /// Optimistic update: the new category on the loaded rows (and a sticky
    /// inspector booking) before the background refresh brings the real data.
    private func showCategory(_ categoryID: Int64?, on ids: Set<Booking.ID>) {
        let c = categoryID.flatMap { id in categories.first { $0.id == id } }
        func patch(_ b: inout Booking) {
            b.categoryId = c?.id
            b.categoryName = c?.name ?? ""
            b.categoryPath = c?.path ?? ""
            b.categoryColorHex = c?.colorHex ?? ""
            b.categorySymbol = c?.sfSymbol ?? ""
            b.categorySource = c == nil ? .none : .manual
        }
        patchLoadedRows(ids, patch)
    }

    /// Shows an edit on the loaded rows (and a sticky inspector booking) before the background
    /// refresh brings the real data; derived fields (merchant, flow) follow from the refresh.
    private func patchLoadedRows(_ ids: Set<Booking.ID>, _ change: (inout Booking) -> Void) {
        var rows = bookings
        var touched = false
        for i in rows.indices where ids.contains(rows[i].id) { change(&rows[i]); touched = true }
        if touched { bookings = rows }
        if var sticky = stickyBooking, ids.contains(sticky.id) { change(&sticky); stickyBooking = sticky }
    }

    /// The synchronous reloads a category change needs (fallback when the write
    /// or the background refresh failed).
    private func reloadAfterCategoryChange() {
        categoryRefreshTask?.cancel()
        reloadBookings(); reloadCategories(); reloadAccounts()
    }

    /// Everything a category change (or another edit of a booking: checked, text, note) affects, loaded off the main thread:
    /// - the list page, published unless another list load started meanwhile
    ///   (`listGeneration`; that load already reads the new category),
    /// - KPIs and category totals, published if the accounts / period are unchanged,
    /// - the account counters ("ohne Kategorie"), published always.
    /// Quick successive assignments only publish the last result.
    private func refreshAfterEdit() {
        // A selection still loading publishes everything itself.
        if selectionLoadTask != nil { settlePendingSelectionLoad(); reloadAccounts(); return }
        categoryRefreshTask?.cancel()
        // An older list load in flight would publish rows without the new category.
        scopeLoadTask?.cancel()
        dropPendingPage()
        guard let vault else { return }
        let generation = listGeneration
        let filter = listFilter
        let (from, to) = periodRange
        let sums = categorySumFilter
        let uncategorizedQuery = uncategorizedCountFilter(sums)
        let listHasFlow = filter.listHasFlow || kpiFollowsList
        categoryRefreshTask = Task { [weak self] in
            do {
                let r = try await Self.offMain { () -> CategoryRefresh in
                    let page = try Perf.interval("listTransactions") { try vault.listTransactions(filter: filter) }
                    try Task.checkCancellation()
                    // fallback in the filter's own calculation context (#337)
                    let flow: SelectionFlow
                    if listHasFlow {
                        flow = page.flow
                    } else if filter.respectInTotal {
                        flow = try vault.selectionFlowForTotals(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
                    } else {
                        flow = try vault.selectionFlow(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
                    }
                    try Task.checkCancellation()
                    return CategoryRefresh(
                        page: page,
                        flow: flow,
                        categories: try vault.listCategoriesFilteredAmount(dateFrom: from, dateTo: to, accountIds: sums.accounts, checked: sums.checked, direction: sums.direction, amountMinCents: sums.amount.minCents, amountMaxCents: sums.amount.maxCents),
                        uncategorized: try uncategorizedQuery.map { Int(try vault.countTransactions(filter: $0)) },
                        groups: try vault.listAccountGroups(),
                        userGroups: try vault.listUserGroups()
                    )
                }
                guard !Task.isCancelled, let self, self.vault === vault else { return }
                if generation == self.listGeneration {
                    self.dropPendingPage()
                    self.publish(r.page)
                }
                let (nowFrom, nowTo) = self.periodRange
                let nowSums = self.categorySumFilter
                if nowFrom == from, nowTo == to, nowSums.accounts == sums.accounts, nowSums.checked == sums.checked, nowSums.direction == sums.direction, nowSums.amount == sums.amount {
                    // same calculation context as the captured filter, not only
                    // the same accounts: the totals switch changes the KPIs
                    // without changing the account list (#337)
                    if self.currentFilter.accountIds == filter.accountIds,
                       self.currentFilter.respectInTotal == filter.respectInTotal {
                        self.selectionFlow = (r.flow.consumptionCents, r.flow.investedCents, r.flow.incomeCents)
                    }
                    self.categories = r.categories
                    self.uncategorizedFilteredCount = r.uncategorized
                    self.categoryTree = Self.buildTree(r.categories)
                }
                self.applyAccountGroups(r.groups, r.userGroups)
            } catch {
                guard !Task.isCancelled, !(error is CancellationError), let self else { return }
                self.errorMessage = describe(error)
                self.reloadAfterCategoryChange()
            }
        }
    }

    private struct CategoryRefresh: @unchecked Sendable {
        let page: BookingPage
        let flow: SelectionFlow
        let categories: [GiroCrabCore.Category]
        let uncategorized: Int?
        let groups: [AccountGroup]
        let userGroups: [UserGroup]
    }

    /// The list filters by checked state: a changed booking leaves it.
    private var leavesListWhenChecked: Bool { statusFilter != .all || sidebarSelection == .newBookings }

    /// "Geprüft" for a booking – or, when it is part of a multiple selection, for
    /// every selected booking (#725, like `categorizeTargets`).
    func setChecked(_ checked: Bool, for booking: Booking) {
        let targets = categorizeTargets(for: booking)
        if targets.count > 1 {
            setChecked(Set(targets.map(\.id)), to: checked)
            return
        }
        guard let vault else { return }
        runCore { try vault.setTransactionChecked(transactionId: booking.id, checked: checked) }
        // With "Neu" / "Geprüft" active the booking leaves the list: keep it in the inspector.
        if leavesListWhenChecked, selectedBookingID == booking.id {
            stickyBooking = freshBooking(booking)
            lingeringBookingID = checked ? booking.id : nil
        }
        patchLoadedRows([booking.id]) { $0.checked = checked }
        refreshAfterEdit()
    }

    /// "Alle Buchungen als gelesen markieren" (GitHub #236): everything new in the list as
    /// filtered now, or – with `accountIDs`, from a sidebar entry – in those
    /// accounts regardless of period, search and scope. One SQL update off the
    /// main thread; undoable via Bearbeiten › Widerrufen.
    /// The Buchung menu entry: nothing to do without new bookings, and not in
    /// Belege (there the list would silently mean every account).
    var canMarkAllChecked: Bool { vault != nil && newCount > 0 && sidebarSelection != .documents }

    func markAllChecked(accountIDs: [Int64]? = nil, quietWhenEmpty: Bool = false) {
        guard let vault else { return }
        if accountIDs == nil, sidebarSelection == .documents { return }
        var f: BookingFilter
        if let accountIDs {
            guard !accountIDs.isEmpty else { return }
            f = BookingFilter(accountIds: accountIDs, categoryId: nil, includeSubcategories: false,
                              uncategorizedOnly: false, search: nil, dateFrom: nil, dateTo: nil,
                              limit: 0, offset: 0, scope: .all, merchantKey: nil, direction: .all,
                              checked: false, wholeDays: false)
        } else {
            f = currentFilter
            f.checked = false; f.limit = 0; f.offset = 0; f.wholeDays = false
        }
        let filter = f
        let previous = checkedWriteTask
        checkedWriteTask = Task { [weak self] in
            await previous?.value
            guard let self, self.vault === vault else { return }
            do {
                let ids = try await Self.offMain { try vault.setCheckedForFilter(filter: filter, checked: true) }
                guard self.vault === vault, !ids.isEmpty || !quietWhenEmpty else { return }
                self.finishMarkChecked(ids)
            } catch {
                if self.vault === vault { self.errorMessage = describe(error) }
            }
        }
    }

    private func finishMarkChecked(_ ids: [Int64]) {
        guard !ids.isEmpty else {
            toast = String(localized: "Keine neuen Umsätze")
            return
        }
        if let um = windowUndoManager {
            um.registerUndo(withTarget: self) { m in m.setChecked(ids, to: false, actionName: Self.markAllActionName) }
            um.setActionName(Self.markAllActionName)
        }
        toast = String(localized: "\(ids.count) Umsätze als gelesen markiert")
        // With "Neu" / "Geprüft" active the bookings leave the list: keep the selected one in the inspector (like setChecked)
        if leavesListWhenChecked, let b = selectedBooking, ids.contains(b.id) { stickyBooking = freshBooking(b) }
        patchLoadedRows(Set(ids)) { $0.checked = true }
        refreshAfterEdit()
    }

    private static var markAllActionName: String { String(localized: "Alle Buchungen als gelesen markieren") }

    /// Undo / redo of `markAllChecked` for exactly the bookings it changed.
    private func setChecked(_ ids: [Int64], to checked: Bool, actionName: String) {
        guard let vault else { return }
        do { try vault.setCheckedForIds(ids: ids, checked: checked) } catch { errorMessage = describe(error); return }
        if let um = windowUndoManager {
            um.registerUndo(withTarget: self) { m in m.setChecked(ids, to: !checked, actionName: actionName) }
            um.setActionName(actionName)
        }
        patchLoadedRows(Set(ids)) { $0.checked = checked }
        refreshAfterEdit()
    }

    /// "Geprüft" for a multiple selection (#725): the rows change at once, the
    /// write runs off the main thread (⌘A can select thousands) after the
    /// previous one, and Bearbeiten › Widerrufen restores each booking's state.
    private func setChecked(_ ids: Set<Booking.ID>, to checked: Bool) {
        guard let vault else { return }
        let changed = bookings.filter { ids.contains($0.id) && $0.checked != checked }.map(\.id)
        guard !changed.isEmpty else { return }
        let actionName = checked ? String(localized: "Als geprüft markieren") : String(localized: "Als neu markieren")
        // With "Neu" / "Geprüft" active the bookings leave the list: keep the selected one in the inspector.
        if leavesListWhenChecked, let b = selectedBooking, changed.contains(b.id) { stickyBooking = freshBooking(b) }
        patchLoadedRows(Set(changed)) { $0.checked = checked }
        let previous = checkedWriteTask
        checkedWriteTask = Task { [weak self] in
            await previous?.value
            do {
                try await Self.offMain { try vault.setCheckedForIds(ids: changed, checked: checked) }
                guard let self, self.vault === vault else { return }
                if let um = self.windowUndoManager {
                    um.registerUndo(withTarget: self) { m in m.setChecked(changed, to: !checked, actionName: actionName) }
                    um.setActionName(actionName)
                }
                self.refreshAfterEdit()
            } catch {
                guard let self, self.vault === vault else { return }
                self.errorMessage = describe(error)
                self.reloadBookings()
            }
        }
    }

    func updateText(counterparty: String, purpose: String, for booking: Booking) {
        guard let vault else { return }
        runCore { try vault.updateTransactionText(transactionId: booking.id, counterparty: counterparty, purpose: purpose) }
        patchLoadedRows([booking.id]) { $0.counterparty = counterparty; $0.purpose = purpose }
        refreshAfterEdit(); reloadMerchantStats()
    }

    /// Select the booking and put the cursor into its note field in the inspector.
    func editNote(for booking: Booking) {
        #if os(macOS)
        AppDefaults.store.set(true, forKey: ViewLayout.inspectorKey)
        #else
        showBookingSheet = true
        #endif
        selectedBookingIDs = [booking.id]
        selectedBookingID = booking.id
        pendingNoteFocus = booking.id
    }

    func updateNote(_ note: String, for booking: Booking) {
        guard let vault else { return }
        runCore { try vault.updateTransactionNote(transactionId: booking.id, note: note) }
        // Search, flow and sums ignore the note: showing it on the rows is the whole refresh.
        patchLoadedRows([booking.id]) { $0.note = note }
    }

    func createCategory(name: String, parent: Int64?, symbol: String, color: String) {
        guard let vault else { return }
        runCore { _ = try vault.createCategory(name: name, parentId: parent, sfSymbol: symbol, colorHex: color) }
        reloadCategories()
    }

    /// Changes one property of a category and keeps the others (#168) –
    /// the core only updates a category as a whole.
    private func editCategory(_ id: Int64, _ change: (inout GiroCrabCore.Category) -> Void) {
        guard let vault, var c = categories.first(where: { $0.id == id }) else { return }
        change(&c)
        runCore {
            try vault.updateCategory(id: c.id, name: c.name, parentId: c.parentId,
                                     sfSymbol: c.sfSymbol, colorHex: c.colorHex)
        }
        reloadCategories()
        reloadBookings() // the booking list shows a category's name, symbol and colour
    }

    /// "Umbenennen …" of a category.
    func renameCategory(_ id: Int64, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        editCategory(id) { $0.name = trimmed }
    }

    /// "Verschieben nach ▸" of a category: under another category, or to the
    /// top level (nil). The core refuses moving it below itself.
    func moveCategory(_ id: Int64, to parent: Int64?) {
        editCategory(id) { $0.parentId = parent }
        if let parent { expandCategoryPath(to: parent) } // keep it in view
    }

    /// Whether category `id` may be dropped on `target` (#977): never on itself
    /// or on one of its own sub-categories, which would put it below itself.
    func canDropCategory(_ id: Int64, on target: Int64) -> Bool {
        categoryMoveTargets(for: id).contains { $0.id == target }
    }

    /// Whether a drop on the lower half of `target` goes into it (it has
    /// sub-categories, or is a main category that may still get its first one)
    /// instead of right below it.
    func categoryTakesDropInto(_ target: Int64) -> Bool {
        guard let c = categories.first(where: { $0.id == target }) else { return false }
        return c.parentId == nil || categories.contains { $0.parentId == target }
    }

    /// Drag and drop of a sidebar category (#977): reorders it among its siblings
    /// or hangs it under another category, in one core call. `false` when the
    /// payload is no category or the drop is not allowed.
    func dropCategory(_ items: [String], onto target: Int64, place: CategoryDropPlace) -> Bool {
        guard let id = items.lazy.compactMap(GroupDrag.categoryID).first,
              canDropCategory(id, on: target) else { return false }
        placeCategory(id, relativeTo: target, place: place)
        return true
    }

    /// Puts category `id` before/after `target` among its siblings, or last
    /// below it (`.into`).
    func placeCategory(_ id: Int64, relativeTo target: Int64, place: CategoryDropPlace) {
        guard let vault, let t = categories.first(where: { $0.id == target }) else { return }
        let parent: Int64? = place == .into ? target : t.parentId
        // `categories` is in tree order, so a filter keeps the siblings' order
        var ids = categories.filter { $0.parentId == parent && $0.id != id }.map(\.id)
        switch place {
        case .into: ids.append(id)
        case .before: ids.insert(id, at: ids.firstIndex(of: target) ?? ids.endIndex)
        case .after: ids.insert(id, at: ids.firstIndex(of: target).map { $0 + 1 } ?? ids.endIndex)
        }
        reorderCategories(parent, ids: ids, vault: vault)
        if let parent { expandCategoryPath(to: parent) } // keep it in view
    }

    /// "Nach oben" / "Nach unten" of a category: one place among its siblings
    /// (the menu twin of the drag, also for the keyboard and iOS).
    func shiftCategory(_ id: Int64, by step: Int) {
        guard let vault, let c = categories.first(where: { $0.id == id }) else { return }
        var ids = categories.filter { $0.parentId == c.parentId }.map(\.id)
        guard let i = ids.firstIndex(of: id), ids.indices.contains(i + step) else { return }
        ids.swapAt(i, i + step)
        reorderCategories(c.parentId, ids: ids, vault: vault)
    }

    /// Whether category `id` has a sibling above (`step` -1) or below (+1) it.
    func canShiftCategory(_ id: Int64, by step: Int) -> Bool {
        guard let c = categories.first(where: { $0.id == id }) else { return false }
        let ids = categories.filter { $0.parentId == c.parentId }.map(\.id)
        return ids.firstIndex(of: id).map { ids.indices.contains($0 + step) } ?? false
    }

    private func reorderCategories(_ parent: Int64?, ids: [Int64], vault: Vault) {
        runCore { try vault.reorderCategories(parentId: parent, ids: ids) }
        reloadCategories()
        reloadBookings() // the booking list shows a category's path
    }

    /// Opens `id` and every category above it, so what is placed under `id`
    /// shows in the sidebar even inside a folded main category.
    func expandCategoryPath(to id: Int64) {
        var path: Set<Int64> = []
        var next: Int64? = id
        while let c = next, path.insert(c).inserted {
            next = categories.first { $0.id == c }?.parentId
        }
        expandedCategories.formUnion(path)
    }

    /// "Symbol & Farbe …" of a category.
    func setCategorySymbol(_ id: Int64, symbol: String) { editCategory(id) { $0.sfSymbol = symbol } }
    func setCategoryColor(_ id: Int64, hex: String) { editCategory(id) { $0.colorHex = hex } }

    /// „Eigene Farben“ › Entfernen: accounts with colour `old` go back to
    /// their default colour, groups to none, and a category (it always has a
    /// colour) takes its parent's, a main category slate grey.
    func removeColor(_ old: String) {
        let old = old.uppercased()
        let matches = { (hex: String) in hex.uppercased() == old }
        for a in allAccounts where matches(a.colorHex) { setAccountColor(a.id, hex: nil) }
        for g in userGroups where matches(g.colorHex) { setUserGroupColor(g, hex: nil) }
        let byID = Dictionary(categories.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        for c in categories where matches(c.colorHex) {
            let parent = c.parentId.flatMap { byID[$0]?.colorHex }.flatMap { $0.isEmpty || matches($0) ? nil : $0 }
            setCategoryColor(c.id, hex: parent ?? ColorChoices.extras[2])
        }
    }

    /// Where a category can move: every category except itself and its own
    /// sub-categories.
    func categoryMoveTargets(for id: Int64) -> [GiroCrabCore.Category] {
        let byID = Dictionary(categories.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        func isOwnOrBelow(_ c: GiroCrabCore.Category) -> Bool {
            var cur: Int64? = c.id
            var steps = 0
            while let i = cur, steps < 64 {
                if i == id { return true }
                cur = byID[i]?.parentId
                steps += 1
            }
            return false
        }
        return categories.filter { !isOwnOrBelow($0) }
    }

    /// "Hauptkategorie › Unterkategorie" for a category id (nil = top level).
    func categoryPath(_ id: Int64?) -> String? {
        id.flatMap { id in categories.first { $0.id == id }?.path }
    }

    func deleteCategory(_ id: Int64) {
        guard let vault else { return }
        runCore { try vault.deleteCategory(id: id) }
        if sidebarSelection == .category(id) { sidebarSelection = .allAccounts }
        reloadAll()
    }

    func newRuleDraft(from booking: Booking? = nil) -> Rule {
        Rule(
            id: 0,
            name: booking.map { "\($0.merchant.name)" } ?? String(localized: "Neue Regel"),
            categoryId: booking?.categoryId ?? categories.first(where: { $0.depth > 0 })?.id ?? categories.first?.id ?? 0,
            field: booking == nil ? .any : .counterparty,
            matchKind: .keyword,
            pattern: booking.map { $0.merchant.known ? $0.merchant.name : $0.counterparty.split(separator: " ").prefix(2).joined(separator: " ") } ?? "",
            caseSensitive: false,
            // Direction follows the booking (#295): a rule made from an income
            // must not also categorise the outgoing transfers to the same payee.
            minAmountCents: booking.flatMap { $0.amountCents > 0 ? 1 : nil },
            maxAmountCents: booking.flatMap { $0.amountCents < 0 ? 0 : nil },
            accountIds: [],
            priority: Int32((rules.map(\.priority).max() ?? 0) + 10),
            enabled: true,
            matchCount: 0
        )
    }

    func openRuleEditor(from booking: Booking? = nil) {
        openRuleEditor(draft: booking.map { newRuleDraft(from: $0) })
    }

    /// The rule editor on `draft` (nil: the rule list).
    func openRuleEditor(draft: Rule?) {
        ruleDraft = draft
        showRuleEditor = true
    }

    @discardableResult
    func saveRule(_ rule: Rule) -> Int64? {
        guard let vault else { return nil }
        do {
            let id = try vault.saveRule(rule: rule)
            _ = try vault.applyRules(onlyUncategorized: false)
            reloadAll()
            return id
        } catch {
            showError(String(localized: "Regel konnte nicht gespeichert werden"), describe(error))
            return nil
        }
    }

    func deleteRule(_ id: Int64) {
        guard let vault else { return }
        runCore { try vault.deleteRule(ruleId: id) }
        reloadAll()
    }

    /// Reassigns sequential priorities (10, 20, 30, ...) matching the given rule order
    /// and re-applies rules once (#434).
    func reorderRules(_ ordered: [Rule]) {
        guard let vault else { return }
        do {
            for (idx, var rule) in ordered.enumerated() {
                let newPriority = Int32((idx + 1) * 10)
                if rule.priority != newPriority {
                    rule.priority = newPriority
                    _ = try vault.saveRule(rule: rule)
                }
            }
            _ = try vault.applyRules(onlyUncategorized: false)
            reloadAll()
        } catch {
            showError(String(localized: "Regel konnte nicht gespeichert werden"), describe(error))
        }
    }

    func preview(_ rule: Rule) -> (matches: [Booking], error: String?) {
        guard let vault else { return ([], nil) }
        do { return (try vault.previewRule(rule: rule, limit: 200), nil) } catch { return ([], describe(error)) }
    }

    func applyAllRules() {
        guard let vault else { return }
        runCore {
            let n = try vault.applyRules(onlyUncategorized: false)
            toast = String(localized: "Regeln angewendet – \(n) Buchungen neu zugeordnet")
        }
        reloadAll()
    }
}
