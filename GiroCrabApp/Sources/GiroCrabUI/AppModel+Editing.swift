import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Editing

    func setCategory(_ categoryID: Int64?, for booking: Booking) {
        setCategory(categoryID, for: [booking])
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
        merchantStatsCache = [:]
        loadMerchantStats(keepShown: true)
        // Rule counts and the investment dashboard: reload only when on screen,
        // otherwise when the rule editor opens / the Investments scope is shown.
        if showRuleEditor { reloadRules() } else { rulesStale = true }
        if scope == .investments { loadDashboard() } else { dashboardStale = true }
        refreshAfterCategoryChange()
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
        var rows = bookings
        for i in rows.indices where ids.contains(rows[i].id) { patch(&rows[i]) }
        bookings = rows
        if var s = stickyBooking, ids.contains(s.id) { patch(&s); stickyBooking = s }
    }

    /// The synchronous reloads a category change needs (fallback when the write
    /// or the background refresh failed).
    private func reloadAfterCategoryChange() {
        categoryRefreshTask?.cancel()
        reloadBookings(); reloadCategories(); reloadAccounts()
    }

    /// Everything a category change affects, loaded off the main thread:
    /// - the list page, published unless another list load started meanwhile
    ///   (`listGeneration`; that load already reads the new category),
    /// - KPIs and category totals, published if the accounts / period are unchanged,
    /// - the account counters ("ohne Kategorie"), published always.
    /// Quick successive assignments only publish the last result.
    private func refreshAfterCategoryChange() {
        // A selection still loading publishes everything itself.
        if selectionLoadTask != nil { settlePendingSelectionLoad(); reloadAccounts(); return }
        categoryRefreshTask?.cancel()
        // An older list load in flight would publish rows without the new category.
        scopeLoadTask?.cancel()
        dropPendingPage()
        guard let vault else { return }
        let generation = listGeneration
        let filter = currentFilter
        let (from, to) = periodRange
        let listHasFlow = filter.listHasFlow
        categoryRefreshTask = Task { [weak self] in
            do {
                let r = try await Self.offMain { () -> CategoryRefresh in
                    let page = try Perf.interval("listTransactions") { try vault.listTransactions(filter: filter) }
                    try Task.checkCancellation()
                    let flow = listHasFlow ? page.flow : try vault.selectionFlow(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
                    try Task.checkCancellation()
                    return CategoryRefresh(
                        page: page,
                        flow: flow,
                        categories: try vault.listCategories(dateFrom: from, dateTo: to),
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
                if nowFrom == from, nowTo == to {
                    if self.currentFilter.accountIds == filter.accountIds {
                        self.selectionFlow = (r.flow.consumptionCents, r.flow.investedCents, r.flow.incomeCents)
                    }
                    self.categories = r.categories
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
        let groups: [AccountGroup]
        let userGroups: [UserGroup]
    }

    func setChecked(_ checked: Bool, for booking: Booking) {
        guard let vault else { return }
        runCore { try vault.setTransactionChecked(transactionId: booking.id, checked: checked) }
        // With "Neu" / "Geprüft" active the booking leaves the list: keep it in the inspector.
        if statusFilter != .all, selectedBookingID == booking.id { stickyBooking = freshBooking(booking) }
        reloadBookings(); reloadAccounts()
    }

    /// "Alle als gelesen markieren" (GitHub #236): everything new in the list as
    /// filtered now, or – with `accountIDs`, from a sidebar entry – in those
    /// accounts regardless of period, search and scope. One SQL update off the
    /// main thread; undoable via Bearbeiten › Widerrufen.
    /// The Buchung menu entry: nothing to do without new bookings, and not in
    /// Belege (there the list would silently mean every account).
    var canMarkAllChecked: Bool { vault != nil && newCount > 0 && sidebarSelection != .documents }

    func markAllChecked(accountIDs: [Int64]? = nil) {
        guard let vault else { return }
        if accountIDs == nil, sidebarSelection == .documents { return }
        var f = currentFilter
        if let accountIDs {
            f.accountIds = accountIDs; f.categoryId = nil; f.uncategorizedOnly = false
            f.search = nil; f.dateFrom = nil; f.dateTo = nil; f.direction = .all
            f.scope = .all; f.merchantKey = nil
        }
        f.checked = false; f.limit = 0; f.offset = 0; f.wholeDays = false
        let filter = f
        Task { [weak self] in
            do {
                let ids = try await Self.offMain { try vault.setCheckedForFilter(filter: filter, checked: true) }
                guard let self, self.vault === vault else { return }
                self.finishMarkChecked(ids)
            } catch {
                self?.errorMessage = describe(error)
            }
        }
    }

    private func finishMarkChecked(_ ids: [Int64]) {
        guard !ids.isEmpty else {
            toast = String(localized: "Keine neuen Umsätze")
            return
        }
        if let um = windowUndoManager {
            um.registerUndo(withTarget: self) { m in m.setChecked(ids, to: false) }
            um.setActionName(String(localized: "Alle als gelesen markieren"))
        }
        toast = String(localized: "\(ids.count) Umsätze als gelesen markiert")
        // With "Neu" / "Geprüft" active the bookings leave the list: keep the selected one in the inspector (like setChecked)
        if statusFilter != .all, let b = selectedBooking, ids.contains(b.id) { stickyBooking = freshBooking(b) }
        reloadBookings(); reloadAccounts()
    }

    /// Undo / redo of `markAllChecked` for exactly the bookings it changed.
    private func setChecked(_ ids: [Int64], to checked: Bool) {
        guard let vault else { return }
        do { try vault.setCheckedForIds(ids: ids, checked: checked) } catch { errorMessage = describe(error); return }
        if let um = windowUndoManager {
            um.registerUndo(withTarget: self) { m in m.setChecked(ids, to: !checked) }
            um.setActionName(String(localized: "Alle als gelesen markieren"))
        }
        reloadBookings(); reloadAccounts()
    }

    func updateText(counterparty: String, purpose: String, for booking: Booking) {
        guard let vault else { return }
        runCore { try vault.updateTransactionText(transactionId: booking.id, counterparty: counterparty, purpose: purpose) }
        reloadBookings(); reloadMerchantStats()
    }

    /// Select the booking and put the cursor into its note field in the inspector.
    func editNote(for booking: Booking) {
        #if os(macOS)
        // the note lives in the inspector: show it if it was hidden (the field claims the focus when it appears)
        UserDefaults.standard.set(true, forKey: ViewLayout.inspectorKey)
        #endif
        selectedBookingID = booking.id
        pendingNoteFocus = booking.id
    }

    func updateNote(_ note: String, for booking: Booking) {
        guard let vault else { return }
        runCore { try vault.updateTransactionNote(transactionId: booking.id, note: note) }
        reloadBookings()
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
        if let parent { expandedCategories.insert(parent) } // keep it in view
    }

    /// "Symbol & Farbe …" of a category.
    func setCategorySymbol(_ id: Int64, symbol: String) { editCategory(id) { $0.sfSymbol = symbol } }
    func setCategoryColor(_ id: Int64, hex: String) { editCategory(id) { $0.colorHex = hex } }

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
            minAmountCents: nil,
            maxAmountCents: booking.map { $0.amountCents < 0 ? 0 : nil } ?? nil,
            accountId: nil,
            priority: Int32((rules.map(\.priority).max() ?? 0) + 10),
            enabled: true,
            matchCount: 0
        )
    }

    func openRuleEditor(from booking: Booking? = nil) {
        ruleDraft = booking.map { newRuleDraft(from: $0) }
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
