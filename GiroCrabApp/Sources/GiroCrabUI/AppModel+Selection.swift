import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
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
    /// The selected bookings in list order.
    var selectedBookings: [Booking] { visibleBookings.filter { selectedBookingIDs.contains($0.id) } }

    /// Click on a booking row: plain = only this one, `toggle` (⌘) adds or
    /// removes it, `extend` (⇧) selects the range from the current booking.
    func selectBooking(_ id: Booking.ID, toggle: Bool = false, extend: Bool = false) {
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
    }

    /// Bearbeiten › Alles auswählen (⌘A) in the booking list.
    func selectAllBookings() {
        let ids = visibleBookings.map(\.id)
        guard !ids.isEmpty else { return }
        selectedBookingIDs = Set(ids)
        if selectedBookingID.map({ !selectedBookingIDs.contains($0) }) ?? true { selectedBookingID = ids.first }
    }

    // MARK: - Selection change (account, group, category, period)

    func reloadSelection(periodChanged: Bool) {
        if loadsSelectionInBackground, vault != nil {
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
    }

    /// A synchronous reload while a background selection load is still running:
    /// drop that load and bring everything it would have delivered up to date
    /// now, so its older result can never overwrite fresher data.
    func settlePendingSelectionLoad() {
        guard selectionLoadTask != nil else { return }
        endSelectionLoad()
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
    }

    /// The load is slow: empty list and chart, so nothing of the previous
    /// selection stays under the new title.
    private func showSelectionPlaceholder() {
        isLoadingSelection = true
        page = nil
        bookings = []
        history = []
    }

    /// Everything a selection change needs, loaded off the main thread and
    /// published in one go. The previous data stays until then, at most for
    /// `selectionPlaceholderDelay`; after that list and chart show the placeholder.
    func startSelectionLoad() {
        selectionPlaceholderTask?.cancel()
        selectionPlaceholderTask = nil
        selectionLoadTask?.cancel()
        scopeLoadTask?.cancel()
        dropPendingPage()
        guard let vault else { return }
        let ids = selectionAccountIDs
        let month = Calendar(identifier: .gregorian).dateComponents([.year, .month], from: Date())
        if selectionPlaceholderDelay <= .zero || isLoadingSelection {
            showSelectionPlaceholder()
        } else {
            let delay = selectionPlaceholderDelay
            selectionPlaceholderTask = Task { [weak self] in
                try? await Task.sleep(for: delay)
                guard !Task.isCancelled, let self, self.selectionLoadTask != nil else { return }
                self.selectionPlaceholderTask = nil
                self.showSelectionPlaceholder()
            }
        }
        let task = Task { [weak self] in
            let load = Perf.signposter.beginInterval("selectionLoad")
            defer { Perf.signposter.endInterval("selectionLoad", load) }
            do {
                // The pay cycle comes first: `.sinceSalary` derives the period from it.
                let cycle = try await Self.offMain { try vault.salaryCycle(accountIds: ids) }
                guard !Task.isCancelled, let self else { return }
                self.salaryCycle = cycle
                let filter = self.currentFilter
                let (from, to) = self.periodRange
                let chart = self.historyRequest
                let listHasFlow = filter.listHasFlow
                let loaded = try await Self.offMain {
                    let page = try Perf.interval("listTransactions") { try vault.listTransactions(filter: filter) }
                    try Task.checkCancellation()
                    let flow = listHasFlow ? page.flow : try vault.selectionFlow(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
                    try Task.checkCancellation()
                    let recurring = try vault.recurringSnapshot(accountIds: ids, year: Int32(month.year ?? 2026), month: UInt32(month.month ?? 1))
                    let categories = try vault.listCategories(dateFrom: from, dateTo: to)
                    try Task.checkCancellation()
                    let history = try vault.balanceHistory(accountIds: chart.ids, days: chart.days)
                    return (page: page, flow: flow, recurring: recurring, categories: categories, history: history)
                }
                // A newer selection or a synchronous reload cancelled this request.
                guard !Task.isCancelled else { return }
                self.selectionLoadTask = nil
                self.endSelectionLoad()
                let publish = Perf.signposter.beginInterval("publishSelection")
                self.publish(loaded.page)
                self.selectionFlow = (loaded.flow.consumptionCents, loaded.flow.investedCents, loaded.flow.incomeCents)
                self.recurring = loaded.recurring.series
                self.recurringHidden = loaded.recurring.ignored
                self.recurringMonth = loaded.recurring.month
                self.categories = loaded.categories
                self.categoryTree = Self.buildTree(loaded.categories)
                self.history = Self.trimmedHistory(loaded.history, from: from, to: to)
                Perf.signposter.endInterval("publishSelection", publish)
            } catch {
                guard !Task.isCancelled, let self else { return }
                self.selectionLoadTask = nil
                self.endSelectionLoad()
                self.errorMessage = describe(error)
            }
        }
        selectionLoadTask = task
    }

    func scheduleSearch() {
        searchTask?.cancel()
        searchTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 150_000_000)
            guard !Task.isCancelled else { return }
            self?.reloadBookings()
        }
    }

    // MARK: - Pay cycle (GitHub #37)

    /// Accounts of the sidebar selection; empty = all accounts.
    var selectionAccountIDs: [Int64] {
        switch sidebarSelection {
        case .account(let id): return [id]
        case .group(let gid): return groups.first(where: { $0.id == gid })?.accounts.map(\.id) ?? []
        case .userGroup(let uuid): return userGroupAccountIDs(uuid)
        default: return []
        }
    }

    /// Date range of the selected period; `.sinceSalary` starts at the latest salary.
    var periodRange: (String?, String?) {
        if period == .sinceSalary, let c = salaryCycle { return (c.lastDate, nil) }
        if period == .custom { return (customPeriodFrom, customPeriodTo) }
        return period.range
    }

    /// Label of the period menu: the preset name or "15.03.2025 – 30.06.2025".
    var periodTitle: String {
        guard period == .custom else { return period.title }
        return "\(DateText.medium(customPeriodFrom)) – \(DateText.medium(customPeriodTo))"
    }

    /// Default of the date dialog before the user ever picked a range: last month.
    static var defaultCustomPeriod: (from: String, to: String) {
        let (from, to) = Period.lastMonth.range
        return (from ?? DateText.iso(Date()), to ?? DateText.iso(Date()))
    }

    /// Switches to the user-defined period (GitHub #53); swaps the dates if needed.
    func applyCustomPeriod(from: Date, to: Date) {
        let (a, b) = from <= to ? (from, to) : (to, from)
        customPeriodFrom = DateText.iso(a)
        customPeriodTo = DateText.iso(b)
        UserDefaults.standard.set(customPeriodFrom, forKey: "period.customFrom")
        UserDefaults.standard.set(customPeriodTo, forKey: "period.customTo")
        if period == .custom { periodDidChange() } else { period = .custom }
    }

    func periodDidChange() {
        listLimit = Self.defaultListLimit
        reloadSelection(periodChanged: true)
    }

    /// Subtitle of the period-based KPI tiles.
    var periodSubtitle: String {
        guard period == .sinceSalary else { return periodTitle }
        guard let c = salaryCycle else { return String(localized: "Monat (kein Gehalt)") }
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
