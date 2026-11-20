import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    static func expandedCategoriesKey(demo: Bool) -> String {
        demo ? "sidebar.expandedCategories.demo" : "sidebar.expandedCategories"
    }

    static func storedExpandedCategories(demo: Bool) -> Set<Int64> {
        Set(AppDefaults.store.array(forKey: expandedCategoriesKey(demo: demo)) as? [Int64] ?? [])
    }

    #if os(macOS)
    func selectFromSidebar(_ selection: SidebarItem?) {
        let previous = sidebarSelection
        interactiveSidebarSelection = selection
        if autoReviewDelay == .onAccountSwitch, previous != selection,
           case .account(let id)? = previous, case .account? = selection {
            markAllChecked(accountIDs: [id], quietWhenEmpty: true)
        }
    }
    #endif

    /// Marks the selected booking as checked after `autoReviewDelay`, unless
    /// the selection moved on (or became a multiple one) in the meantime.
    func scheduleAutoReview() {
        autoReviewTask?.cancel()
        autoReviewTask = nil
        guard let seconds = autoReviewDelay.seconds, let id = selectedBookingID, !isMultiSelection,
              let booking = selectedBooking, !booking.checked else { return }
        autoReviewTask = Task { @MainActor [weak self] in
            if seconds > 0 { try? await Task.sleep(for: .seconds(seconds)) }
            guard let self, !Task.isCancelled, self.selectedBookingID == id, !self.isMultiSelection,
                  let booking = self.selectedBooking, !booking.checked else { return }
            self.setChecked(true, for: booking)
        }
    }

    var isMultiSelection: Bool { selectedBookingIDs.count > 1 }

    /// The selected-state flag of one sidebar row; a row reads `isSelected` instead of
    /// `sidebarSelection`, so a click re-runs only the rows whose state changes.
    func sidebarRowSelection(for item: SidebarItem) -> SidebarRowSelection {
        if let flag = sidebarRowSelections[item] { return flag }
        let flag = SidebarRowSelection(isSelected: untrackedSidebarSelection == item)
        sidebarRowSelections[item] = flag
        return flag
    }

    /// Sum of the selected bookings over all accounts, one entry per currency (#878).
    /// Currencies are never added together; the entries are sorted by currency code.
    var selectionTotals: [SelectionTotal] {
        Dictionary(grouping: selectedBookings, by: \.currency)
            .map { SelectionTotal(currency: $0.key, cents: $0.value.reduce(Int64(0)) { $0 + $1.amountCents }) }
            .sorted { $0.currency < $1.currency }
    }
    /// The selected bookings in list order.
    var selectedBookings: [Booking] { visibleBookings.filter { selectedBookingIDs.contains($0.id) } }

    /// Click on a booking row: plain = only this one, `toggle` (⌘) adds or
    /// removes it, `extend` (⇧) selects the range from the current booking.
    func selectBooking(_ id: Booking.ID, toggle: Bool = false, extend: Bool = false, fromListClick: Bool = false) {
        let list = visibleBookings.map(\.id)
        if extend, let anchor = selectedBookingID, let a = list.firstIndex(of: anchor), let b = list.firstIndex(of: id) {
            selectedBookingIDs = Set(list[min(a, b)...max(a, b)])
            selectedBookingID = id
        } else if toggle {
            if selectedBookingIDs.contains(id) {
                selectedBookingIDs.remove(id)
                if selectedBookingID == id {
                    selectedBookingID = list.first { selectedBookingIDs.contains($0) }
                }
            } else {
                selectedBookingIDs.insert(id)
                selectedBookingID = id
            }
        } else {
            selectedBookingIDs = [id]
            selectedBookingID = id
        }
        #if os(macOS)
        if fromListClick, autoReviewDelay == .onAccountSwitch, !isMultiSelection,
           selectedBookingID == id, let booking = selectedBooking, !booking.checked {
            setChecked(true, for: booking)
        }
        #endif
    }

    /// Bearbeiten › Alles auswählen (⌘A) in the booking list.
    func selectAllBookings() {
        let ids = visibleBookings.map(\.id)
        guard !ids.isEmpty else { return }
        selectedBookingIDs = Set(ids)
        if selectedBookingID.map({ !selectedBookingIDs.contains($0) }) ?? true { selectedBookingID = ids.first }
    }

    // MARK: - Selection change (account, group, category, period)

    /// A selection or period change now loads in the background (`reloadSelection`).
    var selectionLoadsInBackground: Bool { loadsSelectionInBackground && vault != nil }

    func reloadSelection(periodChanged: Bool) {
        if selectionLoadsInBackground {
            startSelectionLoad()
        } else if selectionLoadTask != nil {
            settlePendingSelectionLoad() // reloads everything
        } else {
            reloadSelectionNow(periodChanged: periodChanged)
        }
    }

    private func reloadSelectionNow(periodChanged: Bool) {
        refreshSalaryCycle()
        if periodChanged { reloadCategories() } else { refreshRecurring() }
        reloadBookings()
        reloadHistory()
        showSelection() // the data is in: header and title follow at once (#1413)
    }

    /// A synchronous reload while a background selection load is still running:
    /// drop that load and bring everything it would have delivered up to date
    /// now, so its older result can never overwrite fresher data.
    func settlePendingSelectionLoad() {
        guard selectionLoadTask != nil else { return }
        endSelectionLoad()
        showSelection()
        if streamTopPending { scrollStreamTop() }
        refreshSalaryCycle()
        refreshRecurring()
        reloadCategories()
        reloadBookings()
        reloadHistory()
    }

    /// Stops a running selection load and its placeholder timer (no data is touched).
    func endSelectionLoad() {
        selectionLoadTask?.cancel()
        selectionLoadTask = nil
        selectionPlaceholderTask?.cancel()
        selectionPlaceholderTask = nil
        isLoadingSelection = false
        selectionLoadStamp &+= 1
    }

    /// Back to the top of the booking stream for a new sidebar selection or period. When the
    /// list loads in the background the scroll waits for the new data (`streamTopPending`, taken
    /// where the load publishes or a synchronous reload takes over). Scrolling the old list at
    /// the click cost a layout pass of its own, and clearing the chart marker's day then re-ran
    /// every day header on screen, all for data about to be replaced.
    func requestStreamTop() {
        if selectionLoadsInBackground { streamTopPending = true } else { scrollStreamTop() }
    }

    private func scrollStreamTop() {
        streamTopPending = false
        // before the publish selects a booking: its day becomes the marker's day (#593)
        scrollFocusDay = nil
        streamTopRequest &+= 1
    }

    /// The load is slow: the column switches to the new selection with an empty
    /// list and chart, so nothing of the previous selection stays under its title.
    private func showSelectionPlaceholder() {
        isLoadingSelection = true
        showSelectionWithoutData()
    }

    /// The column switches to the new selection with nothing of the previous one in it.
    private func showSelectionWithoutData() {
        showSelection()
        page = nil
        bookings = []
        listBalances = ListBalances()
        history = []
        categoryHistory = []
        categoryOtherCurrencies = []
    }

    /// Everything a selection change needs, loaded off the main thread and
    /// published in one go. The previous data stays until then, at most for
    /// `selectionPlaceholderDelay`; after that list and chart show the placeholder.
    /// `keepingShown`: the data changed under the same selection (sync, merge), so the old
    /// list stays until the new one arrives instead of showing the placeholder.
    func startSelectionLoad(keepingShown: Bool = false) {
        selectionPlaceholderTask?.cancel()
        selectionPlaceholderTask = nil
        selectionLoadTask?.cancel()
        scopeLoadTask?.cancel()
        dropPendingPage()
        guard let vault else { return }
        if keepingShown { categoriesAwaitRefresh = true }
        let ids = selectionAccountIDs
        let month = Calendar(identifier: .gregorian).dateComponents([.year, .month], from: Date())
        var placeholderDelay: Duration?
        if keepingShown && !isLoadingSelection {
            // the old data stays until the new one arrives
        } else if selectionPlaceholderDelay <= .zero || isLoadingSelection {
            showSelectionPlaceholder()
        } else {
            placeholderDelay = selectionPlaceholderDelay
        }
        let task = Task { [weak self] in
            let load = Perf.signposter.beginInterval("selectionLoad")
            defer { Perf.signposter.endInterval("selectionLoad", load) }
            // The grace period starts with the load, not with the click: until this task
            // runs, the main thread is busy with the selection change itself (sidebar, title,
            // header). Counted from the click, that alone used up the 200 ms, so even a fast
            // load emptied the list for the placeholder and then built it a second time.
            if let placeholderDelay, !Task.isCancelled {
                self?.selectionPlaceholderTask = Task { [weak self] in
                    try? await Task.sleep(for: placeholderDelay)
                    guard !Task.isCancelled, let self, self.selectionLoadTask != nil else { return }
                    self.selectionPlaceholderTask = nil
                    self.showSelectionPlaceholder()
                }
            }
            var cycleLoaded = false
            do {
                // The pay cycle comes first: `.sinceSalary` derives the period from it.
                let cycle = try await Self.offMain { try vault.salaryCycle(accountIds: ids) }
                guard !Task.isCancelled, let self else { return }
                self.salaryCycle = cycle
                cycleLoaded = true
                let filter = self.listFilter
                let (from, to) = self.periodRange
                let chart = self.historyRequest
                let categoryChart = self.categoryHistoryRequest
                let sums = self.categorySumFilter
                let uncategorizedQuery = self.uncategorizedCountFilter(sums)
                let listHasFlow = filter.listHasFlow || self.kpiFollowsList
                let loaded = try await Self.offMain {
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
                    let recurring = try vault.recurringSnapshot(accountIds: ids, year: Int32(month.year ?? 2026), month: UInt32(month.month ?? 1))
                    let categories = try vault.listCategoriesFilteredAmount(dateFrom: from, dateTo: to, accountIds: sums.accounts, checked: sums.checked, direction: sums.direction, amountMinCents: sums.amount.minCents, amountMaxCents: sums.amount.maxCents)
                    let uncategorized = try uncategorizedQuery.map { Int(try vault.countTransactions(filter: $0)) }
                    try Task.checkCancellation()
                    let history = try vault.balanceHistory(accountIds: chart.ids, days: chart.days)
                    let categoryHistory = try categoryChart.map {
                        try vault.categoryHistoryByUnit(categoryId: $0.id, accountIds: $0.accounts, dateFrom: $0.from, dateTo: $0.to, keyLength: $0.keyLength)
                    } ?? []
                    let otherCurrencies = try categoryChart.map {
                        try vault.categoryOtherCurrencies(categoryId: $0.id, accountIds: $0.accounts, dateFrom: $0.from, dateTo: $0.to)
                    } ?? []
                    return (page: page, flow: flow, recurring: recurring, categories: categories, uncategorized: uncategorized, history: history, categoryHistory: categoryHistory, otherCurrencies: otherCurrencies)
                }
                // A newer selection or a synchronous reload cancelled this request.
                guard !Task.isCancelled else { return }
                self.selectionLoadTask = nil
                self.endSelectionLoad()
                self.showSelection()
                if self.streamTopPending { self.scrollStreamTop() }
                let publish = Perf.signposter.beginInterval("publishSelection")
                self.publish(loaded.page)
                self.selectionFlow = (loaded.flow.consumptionCents, loaded.flow.investedCents, loaded.flow.incomeCents)
                self.recurring = loaded.recurring.series
                self.recurringHidden = loaded.recurring.ignored
                self.recurringMonth = loaded.recurring.month
                self.categories = loaded.categories
                self.uncategorizedFilteredCount = loaded.uncategorized
                self.categoryTree = Self.buildTree(loaded.categories)
                self.history = Self.trimmedHistory(loaded.history, from: from, to: to)
                self.categoryHistory = loaded.categoryHistory
                self.categoryOtherCurrencies = loaded.otherCurrencies
                Perf.signposter.endInterval("publishSelection", publish)
            } catch {
                guard !Task.isCancelled, let self else { return }
                self.failSelectionLoad(error, cycleLoaded: cycleLoaded)
            }
        }
        selectionLoadTask = task
    }

    /// The selection load threw. A failed refresh keeps what the column shows; a new selection
    /// or period must not show the previous one's data under its title (#1413).
    /// `cycleLoaded`: the load set `salaryCycle` itself; otherwise it may still be a superseded load's.
    func failSelectionLoad(_ error: Error, cycleLoaded: Bool = false) {
        selectionLoadTask = nil
        endSelectionLoad()
        if shownSelection != sidebarSelection || shownPeriod != requestedPeriod {
            showSelectionWithoutData()
        } else if !cycleLoaded {
            salaryCycle = shownSalaryCycle
        }
        errorMessage = describe(error)
    }

    // MARK: - Merchant list (GitHub #495)

    /// Shows every booking of the booking's merchant in the current period: all
    /// accounts, no category, status, direction, tag, scope or search filter. The
    /// merchant key covers differing counterparty spellings of one merchant.
    func focusMerchant(of booking: Booking) {
        let key = booking.merchant.key
        guard !key.isEmpty else { return }
        // A second merchant from within a merchant list still returns to the original view.
        let back = merchantFocusReturn ?? MerchantFocusReturn(
            selection: sidebarSelection, scope: scope, status: statusFilter,
            direction: directionFilter, search: searchText, tags: tagFilter)
        // Filters first: their reloads still run on the old, cheaper selection.
        scope = .all
        statusFilter = .all
        directionFilter = .all
        tagFilter = BookingTagFilter()
        if !searchText.isEmpty { searchText = "" }
        searchTask?.cancel()
        merchantFocus = MerchantFocus(key: key, name: booking.merchantName)
        merchantFocusReturn = back
        applyMerchantFocusSelection(.allAccounts)
        #if os(iOS)
        showBookingSheet = false
        #endif
    }

    /// Back to the sidebar selection and filters from before `focusMerchant`.
    func leaveMerchantFocus() {
        guard merchantFocus != nil else { return }
        let back = merchantFocusReturn
        merchantFocus = nil
        merchantFocusReturn = nil
        guard let back else { reloadBookings(); return }
        applyMerchantFocusSelection(back.selection)
        scope = back.scope
        statusFilter = back.status
        directionFilter = back.direction
        tagFilter = back.tags
        if searchText != back.search { searchText = back.search }
    }

    private func applyMerchantFocusSelection(_ selection: SidebarItem?) {
        appliesMerchantFocus = true
        defer { appliesMerchantFocus = false }
        interactiveSidebarSelection = selection
    }

    func scheduleSearch() {
        searchTask?.cancel()
        let query = searchText
        searchTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 150_000_000)
            guard !Task.isCancelled, let self else { return }
            guard self.naturalSearch?.query != query, query != self.literalSearchQuery,
                  self.naturalSearchEnabled() else {
                self.reloadListInBackground() // the literal search, as ever
                return
            }
            // One or two words naming a category ("mieten", "mietzahlungen", "kredite"): the category
            // filter, deterministic and without the model.
            if self.categories.isEmpty { self.reloadCategories() }
            if let parsed = NaturalSearch.categoryOnly(query, categories: self.categories) {
                if !self.applyParsedSearch(parsed) { self.reloadListInBackground() }
                return
            }
            guard NaturalSearch.looksNatural(query) else {
                self.reloadListInBackground()
                return
            }
            // A keyword wrapped in filler words ("zeige alle amazon") needs no model and no literal
            // search first (that one would only flash "Keine Treffer").
            if let keyword = NaturalSearch.keywordOnly(query, categoryNames: self.categories.map(\.name)) {
                if !self.applyParsedSearch(ParsedSearchQuery(text: keyword)) { self.reloadListInBackground() }
                return
            }
            self.reloadListInBackground() // the literal search while the model reads the phrase
            try? await Task.sleep(nanoseconds: 650_000_000) // ~800 ms after the last keystroke
            guard !Task.isCancelled else { return }
            await self.applyNaturalSearch(query)
        }
    }

    /// The text the list searches: the natural reading of the typed phrase, else the phrase itself.
    var effectiveSearchText: String { naturalSearch?.keyword ?? searchText }

    /// What the core searches for: the model's keyword, or the typed text with German plurals cut
    /// back to the stem ("kredite" → "kredit") so the substring search also finds the singular.
    /// Always a superset of the literal search. The field keeps showing what was typed.
    var coreSearchText: String {
        naturalSearch?.keyword ?? NaturalSearch.literalSearchTerm(searchText)
    }

    /// Refines the literal search (opt-in, Einstellungen › Allgemein) through the on-device model.
    /// Period, category and amount range become the existing filters, the rest the effective search
    /// text. Without a model answer the phrase loses only its filler words; a stale result changes nothing.
    private func applyNaturalSearch(_ query: String) async {
        let parsed = await NaturalSearch.understand(
            query, categoryNames: categories.map(\.name), today: DateText.iso(Date()))
        // The user may have typed on, cleared the field or locked the vault meanwhile.
        guard !Task.isCancelled, searchText == query, naturalSearch == nil, isUnlocked else { return }
        applyParsedSearch(parsed)
    }

    private func naturalSearchState(text: String) -> NaturalSearchState {
        var category: Int64?
        if case .category(let id)? = sidebarSelection { category = id }
        return NaturalSearchState(searchText: text, period: period, customFrom: customPeriodFrom,
                                  customTo: customPeriodTo, categoryID: category, amountFilter: amountFilter)
    }

    /// Applies a parsed reading of the typed phrase next to it: `searchText` (the field) stays as typed,
    /// the list searches `effectiveSearchText` and the parsed filters. Reloads the list once; false
    /// (nothing changed, nothing reloaded) when the reading adds nothing to the literal search.
    @discardableResult
    func applyParsedSearch(_ reading: ParsedSearchQuery, today: Date = Date()) -> Bool {
        let query = searchText
        // A phrase that names no period never filters by date, whatever the reading says.
        let parsed = NaturalSearch.withoutUnnamedDates(reading, for: query)
        revertNaturalSearch() // a new reading starts from the user's own filters
        let old = naturalSearchState(text: query)
        var new = old
        NaturalSearch.apply(parsed: parsed, to: &new, categories: categories, today: today)
        guard new != old else { return false }

        // Set first: the reloads the filters below start already search for the keyword.
        let selection = sidebarSelection
        naturalSearch = NaturalSearchOverride(query: query, keyword: new.searchText, before: old,
                                              beforeSelection: selection, applied: new, appliedSelection: selection)
        listLimit = Self.defaultListLimit
        if new.amountFilter != old.amountFilter {
            let suspended = suspendsFilterReloads
            suspendsFilterReloads = true // the reload at the end covers it
            amountFilter = new.amountFilter
            suspendsFilterReloads = suspended
        }
        // The category first: it may bring its own default period, which the parsed one then replaces.
        if new.categoryID != old.categoryID, let id = new.categoryID {
            // Without a period in the phrase the category's own default period must not move it.
            let namesPeriod = parsed.fromDate != nil || parsed.toDate != nil
            keepsPeriodOnCategorySelection = !namesPeriod
            interactiveSidebarSelection = .category(id)
            keepsPeriodOnCategorySelection = false
        }
        if new.period != old.period || new.customFrom != old.customFrom || new.customTo != old.customTo,
           let from = DateText.date(new.customFrom), let to = DateText.date(new.customTo) {
            loadsSelectionInBackground = true
            applyCustomPeriod(from: from, to: to)
            loadsSelectionInBackground = false
        }
        naturalSearch?.applied = naturalSearchState(text: new.searchText)
        naturalSearch?.appliedSelection = sidebarSelection
        reloadListInBackground()
        return true
    }

    /// Ends the natural reading (typed on, field cleared, „wörtlich suchen“): the list searches the
    /// typed text again and every filter the reading set and the user left alone returns to its
    /// previous value. The caller's search change reloads the list; this only reloads what it restores.
    func revertNaturalSearch() {
        guard let natural = naturalSearch else { return }
        naturalSearch = nil
        let before = natural.before, applied = natural.applied
        if amountFilter == applied.amountFilter, amountFilter != before.amountFilter {
            let suspended = suspendsFilterReloads
            suspendsFilterReloads = true // the caller's search reload covers it
            amountFilter = before.amountFilter
            suspendsFilterReloads = suspended
        }
        if sidebarSelection == natural.appliedSelection, sidebarSelection != natural.beforeSelection {
            interactiveSidebarSelection = natural.beforeSelection
        }
        let periodNow = (period, customPeriodFrom, customPeriodTo)
        if periodNow == (applied.period, applied.customFrom, applied.customTo),
           periodNow != (before.period, before.customFrom, before.customTo) {
            customPeriodFrom = before.customFrom
            customPeriodTo = before.customTo
            UserDefaults.standard.set(customPeriodFrom, forKey: "period.customFrom")
            UserDefaults.standard.set(customPeriodTo, forKey: "period.customTo")
            loadsSelectionInBackground = true
            if period == before.period { periodDidChange() } else { period = before.period }
            loadsSelectionInBackground = false
            rememberPeriod()
        }
    }

    /// The filter chip's „wörtlich suchen“: drops the natural reading of the current phrase for good.
    func searchLiterally() {
        guard naturalSearch != nil else { return }
        literalSearchQuery = searchText
        revertNaturalSearch()
        scheduleSearch()
    }

    // MARK: - Pay cycle (GitHub #37)

    /// Accounts of the sidebar selection; empty = all accounts.
    var selectionAccountIDs: [Int64] {
        switch sidebarSelection {
        case .account(let id): return [id]
        case .group(let gid): return groups.first(where: { $0.id == gid })?.accounts.map(\.id) ?? []
        case .bank(let key): return bankGroup(for: key)?.accounts.map(\.id) ?? []
        case .userGroup(let uuid): return userGroupAccountIDs(uuid)
        default: return []
        }
    }

    /// Date range of the selected period; `.sinceSalary` starts at the latest salary.
    var periodRange: (String?, String?) { requestedPeriod.range(salaryCycle: salaryCycle) }

    /// Title of the selected period: the preset name or "15.03.2025 – 30.06.2025".
    var periodTitle: String { requestedPeriod.title }

    /// Default of the date dialog before the user ever picked a range: last month.
    static var defaultCustomPeriod: (from: String, to: String) {
        let (from, to) = Period.previousMonth().range
        return (from ?? DateText.iso(Date()), to ?? DateText.iso(Date()))
    }

    /// Switches to the user-defined period (GitHub #53); swaps the dates if needed.
    func applyCustomPeriod(from: Date, to: Date) {
        let (a, b) = from <= to ? (from, to) : (to, from)
        customPeriodFrom = DateText.iso(a)
        customPeriodTo = DateText.iso(b)
        AppDefaults.store.set(customPeriodFrom, forKey: "period.customFrom")
        AppDefaults.store.set(customPeriodTo, forKey: "period.customTo")
        if period == .custom { periodDidChange() } else { period = .custom }
        rememberPeriod()
    }

    static let lastPeriodKey = "period.last"

    /// The period last chosen in the period menu (#840); `.custom` uses the saved start/end.
    static var storedPeriod: Period? {
        AppDefaults.store.string(forKey: lastPeriodKey).flatMap { Period(lastChoiceKey: $0) }
    }

    func rememberPeriod() {
        AppDefaults.store.set(period.lastChoiceKey(), forKey: Self.lastPeriodKey)
    }

    private static let categoryPeriodsKey = "period.byCategory"

    /// The period the user last chose for this category (#425); `nil` = follows the current one.
    func categoryPeriod(_ id: Int64) -> Period? {
        let map = AppDefaults.store.dictionary(forKey: Self.categoryPeriodsKey) as? [String: String]
        return map?[String(id)].flatMap { Period(storageKey: $0) }
    }

    func setCategoryPeriod(_ period: Period, for id: Int64) {
        guard let key = period.storageKey() else { return }
        var map = AppDefaults.store.dictionary(forKey: Self.categoryPeriodsKey) as? [String: String] ?? [:]
        map[String(id)] = key
        AppDefaults.store.set(map, forKey: Self.categoryPeriodsKey)
    }

    func periodDidChange() {
        listLimit = Self.defaultListLimit
        reloadSelection(periodChanged: true)
    }

    /// Subtitle of the period-based KPI tiles: the period of the figures they show.
    var periodSubtitle: String {
        guard shownPeriod.period == .sinceSalary else { return shownPeriod.title }
        guard let c = shownSalaryCycle else { return String(localized: "Monat (kein Gehalt)") }
        return String(localized: "Seit Gehalt am \(DateText.medium(c.lastDate))")
    }

    func refreshSalaryCycle() {
        settlePendingSelectionLoad()
        guard let vault else { salaryCycle = nil; return }
        let ids = selectionAccountIDs
        runCore { salaryCycle = try vault.salaryCycle(accountIds: ids) }
    }

    /// `true` = salary, `false` = never salary, `nil` = automatic detection.
    func setSalaryFlag(_ booking: Booking, _ flag: Bool?) {
        guard let vault else { return }
        runCore { try vault.setBookingSalaryFlag(bookingId: booking.id, flag: flag) }
        // keep the edited booking selected even if it leaves the period
        if selectedBookingID == booking.id { stickyBooking = freshBooking(booking) }
        refreshSalaryCycle()
        if period == .sinceSalary { reloadCategories(); reloadHistory() }
        reloadBookings()
    }

    /// Re-reads one booking from the vault (independent of the list filter).
    func freshBooking(_ b: Booking) -> Booking? {
        guard let vault else { return nil }
        let f = BookingFilter(accountIds: [b.accountId], categoryId: nil, includeSubcategories: true, uncategorizedOnly: false,
                              search: nil, dateFrom: b.bookingDate, dateTo: b.bookingDate, limit: 0, offset: 0,
                              scope: .all, merchantKey: nil, direction: .all, checked: nil, wholeDays: false)
        return (try? vault.listTransactions(filter: f))?.transactions.first { $0.id == b.id }
    }

    // MARK: - Keyboard navigation

    func moveSelection(_ delta: Int) {
        let list = visibleBookings
        guard !list.isEmpty else { return }
        let idx = selectedBookingID.flatMap { id in list.firstIndex { $0.id == id } } ?? -1
        let next = max(0, min(list.count - 1, idx + delta))
        selectedBookingIDs = []
        selectedBookingID = list[next].id
    }
}
