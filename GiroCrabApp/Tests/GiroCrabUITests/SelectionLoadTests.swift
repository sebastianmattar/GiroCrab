import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// A selection made in the UI loads in the background; everything else stays synchronous.
@MainActor
final class SelectionLoadTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        // placeholder at once, so the loading state can be checked deterministically
        model.selectionPlaceholderDelay = .zero
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws {
        model = nil
        // `interactivePeriod` remembers the period for the next launch (#840)
        AppDefaults.store.removeObject(forKey: AppModel.lastPeriodKey)
    }

    private func expectedIDs() throws -> [Int64] {
        try XCTUnwrap(model.vault).listTransactions(filter: model.currentFilter).transactions.map(\.id)
    }

    /// Waits for the background load to publish `expected`; `whileWaiting` looks at each state before.
    private func awaitBookings(_ expected: [Int64], whileWaiting: () -> Void = {}) async throws {
        for _ in 0..<1500 {
            if model.bookings.map(\.id) == expected { return }
            whileWaiting()
            try await Task.sleep(for: .milliseconds(2))
        }
        XCTFail("The background selection load did not publish its bookings")
    }

    /// The state a synchronous change to the same selection produces.
    private func assertMatchesSynchronousLoad(file: StaticString = #filePath, line: UInt = #line) throws {
        let bookings = model.bookings.map(\.id), flow = model.selectionFlow, history = model.history.map(\.balanceCents)
        let recurring = model.recurring.map(\.baseKey), categories = model.categories.map(\.totalCents)
        let selection = model.sidebarSelection, period = model.period
        model.sidebarSelection = selection
        model.period = period
        XCTAssertEqual(model.bookings.map(\.id), bookings, file: file, line: line)
        XCTAssertEqual(model.selectionFlow?.consumption, flow?.consumption, file: file, line: line)
        XCTAssertEqual(model.selectionFlow?.invested, flow?.invested, file: file, line: line)
        XCTAssertEqual(model.selectionFlow?.income, flow?.income, file: file, line: line)
        XCTAssertEqual(model.history.map(\.balanceCents), history, file: file, line: line)
        XCTAssertEqual(model.recurring.map(\.baseKey), recurring, file: file, line: line)
        XCTAssertEqual(model.categories.map(\.totalCents), categories, file: file, line: line)
    }

    /// A load that ends before the placeholder shows never changes `isLoadingSelection`; a view that
    /// asked `selectionSettled` (the Sachwerte chart) must still be told that the load is over.
    func testSettledStateIsObservableWhenAFastLoadEndsWithoutAPlaceholder() async throws {
        model.selectionPlaceholderDelay = .seconds(60)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertFalse(model.selectionSettled, "the load is running")
        XCTAssertFalse(model.isLoadingSelection, "and no placeholder shows")
        let changed = expectation(description: "the settled state notifies its observers")
        withObservationTracking { _ = model.selectionSettled } onChange: { changed.fulfill() }
        await fulfillment(of: [changed], timeout: 5)
        XCTAssertTrue(model.selectionSettled)
    }

    func testInteractiveAccountSwitchLoadsInBackground() async throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        XCTAssertFalse(model.bookings.isEmpty)

        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertEqual(model.sidebarSelection, .account(account.id))
        XCTAssertTrue(model.isLoadingSelection)
        XCTAssertEqual(model.shownSelection, .account(account.id), "the placeholder shows under the new title")
        XCTAssertTrue(model.bookings.isEmpty, "nothing of the previous selection shows while loading")
        XCTAssertTrue(model.dayGroups.isEmpty)
        XCTAssertTrue(model.history.isEmpty)
        XCTAssertFalse(model.hasMoreBookings)

        let expected = try expectedIDs()
        XCTAssertFalse(expected.isEmpty)
        try await awaitBookings(expected)
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertTrue(model.bookings.allSatisfy { $0.accountId == account.id })
        XCTAssertNil(model.errorMessage)
        try assertMatchesSynchronousLoad()
    }

    /// Default grace period: a fast load swaps old for new data in one step,
    /// without the empty list / chart / "–" tiles in between (no flicker).
    func testFastLoadKeepsPreviousDataUntilItPublishes() async throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let before = model.bookings.map(\.id)
        let history = model.history.map(\.balanceCents)
        XCTAssertFalse(before.isEmpty)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })

        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertEqual(model.bookings.map(\.id), before)
        XCTAssertEqual(model.history.map(\.balanceCents), history)
        XCTAssertFalse(model.hasMoreBookings, "no paging into the previous selection while loading")
        // #1396: the old data keeps its own title and header, not the new one over it
        XCTAssertEqual(model.shownSelection, .allAccounts)
        XCTAssertEqual(model.title, String(localized: "Alle Konten"))
        XCTAssertTrue(model.showsOverview)

        let expected = try expectedIDs()
        var sawEmpty = false, sawMixed = false
        for _ in 0..<300 where model.bookings.map(\.id) != expected {
            sawEmpty = sawEmpty || model.bookings.isEmpty || model.isLoadingSelection
            sawMixed = sawMixed || model.shownSelection != .allAccounts
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTAssertEqual(model.bookings.map(\.id), expected)
        XCTAssertFalse(sawEmpty)
        XCTAssertFalse(sawMixed, "the title switched before the data")
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertEqual(model.shownSelection, .account(account.id))
        XCTAssertEqual(model.title, account.name)
        XCTAssertFalse(model.showsOverview)
        try assertMatchesSynchronousLoad()
    }

    func testInteractivePeriodChangeLoadsInBackground() async throws {
        model.interactivePeriod = .sinceSalary
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(model.bookings.map(\.id), try expectedIDs())
        XCTAssertNil(model.errorMessage)
        try assertMatchesSynchronousLoad()
    }

    /// #1413: the period menu and the tile subtitles name the old period until the new data is in.
    func testPeriodHeaderSwitchesWithTheData() async throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let before = model.bookings.map(\.id)
        let year = Period.previousYear()
        model.interactivePeriod = year
        XCTAssertEqual(model.period, year)
        XCTAssertEqual(model.shownPeriod.period, .all)
        XCTAssertEqual(model.periodSubtitle, Period.all.title)
        XCTAssertEqual(model.bookings.map(\.id), before)

        let expected = try expectedIDs()
        XCTAssertNotEqual(expected, before)
        var sawMixed = false
        try await awaitBookings(expected) { sawMixed = sawMixed || model.shownPeriod.period != .all }
        XCTAssertFalse(sawMixed, "the header switched before the data")
        XCTAssertEqual(model.shownPeriod.period, year)
        XCTAssertEqual(model.periodSubtitle, year.title)
    }

    /// A synchronous period change (also new custom dates) shows its period at once.
    func testSynchronousPeriodChangeShowsItsPeriod() {
        model.period = .days(30)
        XCTAssertEqual(model.shownPeriod, model.requestedPeriod)
        // the dates as `applyCustomPeriod` sets them, without writing them to the defaults
        model.customPeriodFrom = "2025-03-15"
        model.customPeriodTo = "2025-06-30"
        model.period = .custom
        XCTAssertEqual(model.shownPeriod, model.requestedPeriod)
        XCTAssertEqual(model.shownPeriod.customTo, "2025-06-30")
        model.customPeriodTo = "2025-03-15"
        model.periodDidChange()
        XCTAssertEqual(model.shownPeriod, model.requestedPeriod, "new dates of `.custom`")
    }

    /// #1413: a period change that fails before the placeholder shows nothing of the old period.
    func testFailedPeriodChangeShowsNoPreviousData() throws {
        model.selectionPlaceholderDelay = .seconds(30)
        XCTAssertFalse(model.bookings.isEmpty)
        model.interactivePeriod = .days(30)
        XCTAssertEqual(model.shownPeriod.period, .all)

        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.shownSelection, .allAccounts)
        XCTAssertEqual(model.shownPeriod.period, .days(30))
        XCTAssertTrue(model.bookings.isEmpty)
        XCTAssertTrue(model.history.isEmpty)
        XCTAssertTrue(model.selectionFiguresMissing)
        XCTAssertNotNil(model.errorMessage)
    }

    /// „Seit Gehalt“ for all accounts, then a background switch to an account whose pay cycle differs,
    /// stopped where its load has set that cycle and still runs its other queries.
    private func switchToAccountWithOtherPayCycle(placeholderDelay: Duration = .seconds(30)) throws
        -> (shown: SalaryCycle, account: Account, accountCycle: SalaryCycle?) {
        model.period = .sinceSalary
        let vault = try XCTUnwrap(model.vault)
        let shown = try XCTUnwrap(model.salaryCycle)
        let account = try XCTUnwrap(model.allAccounts.first {
            $0.kind != .loan && (try? vault.salaryCycle(accountIds: [$0.id]))?.lastDate != shown.lastDate
        })
        let accountCycle = try vault.salaryCycle(accountIds: [account.id])
        model.selectionPlaceholderDelay = placeholderDelay
        model.interactiveSidebarSelection = .account(account.id)
        model.salaryCycle = accountCycle
        return (shown, account, accountCycle)
    }

    /// #1413: the tile subtitle names the pay cycle of the data shown, not that of a selection still loading.
    func testPayCycleSubtitleSwitchesWithTheData() async throws {
        model.period = .sinceSalary
        let subtitle = model.periodSubtitle
        let (shownCycle, account, accountCycle) = try switchToAccountWithOtherPayCycle()
        XCTAssertEqual(model.shownSalaryCycle?.lastDate, shownCycle.lastDate)
        XCTAssertEqual(model.periodSubtitle, subtitle)

        try await awaitBookings(try expectedIDs())
        XCTAssertEqual(model.shownSelection, .account(account.id))
        XCTAssertEqual(model.shownSalaryCycle?.lastDate, accountCycle?.lastDate)
        XCTAssertNotEqual(model.periodSubtitle, subtitle)
    }

    /// #1413: a superseded load left its pay cycle behind; a failed load of the selection shown
    /// keeps that selection's cycle for the period, print and export.
    func testFailedRefreshRestoresThePayCycleOfTheShownData() throws {
        model.period = .sinceSalary
        let bookings = model.bookings.map(\.id)
        let (shownCycle, _, _) = try switchToAccountWithOtherPayCycle()
        model.interactiveSidebarSelection = .allAccounts
        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.bookings.map(\.id), bookings)
        XCTAssertEqual(model.salaryCycle?.lastDate, shownCycle.lastDate)
        XCTAssertEqual(model.periodRange.0, shownCycle.lastDate)
    }

    /// #1413: behind the placeholder the new selection shows, with its pay cycle once loaded,
    /// which a failed load then keeps.
    func testPlaceholderShowsThePayCycleOfItsSelection() throws {
        let (_, _, accountCycle) = try switchToAccountWithOtherPayCycle(placeholderDelay: .zero)
        XCTAssertTrue(model.isLoadingSelection)
        XCTAssertEqual(model.shownSalaryCycle?.lastDate, accountCycle?.lastDate)

        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.salaryCycle?.lastDate, accountCycle?.lastDate)
        XCTAssertEqual(model.shownSalaryCycle?.lastDate, accountCycle?.lastDate)
    }

    /// #1413: the header's expense pie queries the accounts of the selection shown, not one still loading.
    func testHeaderPieFollowsTheShownSelection() async throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let before = model.headerChartAccountIDs
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertEqual(model.headerChartAccountIDs, before)
        try await awaitBookings(try expectedIDs())
        XCTAssertEqual(model.headerChartAccountIDs, [account.id])
    }

    /// A failed refresh that already read its pay cycle (a sync brought a new salary) keeps it.
    func testFailedRefreshKeepsTheCycleItLoaded() throws {
        model.period = .sinceSalary
        let vault = try XCTUnwrap(model.vault)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        let fresh = try vault.salaryCycle(accountIds: [account.id])
        model.startSelectionLoad(keepingShown: true)
        model.salaryCycle = fresh
        model.failSelectionLoad(LoadFailure(), cycleLoaded: true)
        XCTAssertEqual(model.salaryCycle?.lastDate, fresh?.lastDate)
    }

    /// #1413: a non-booking column cancels a pending period change; the sidebar sums still follow the period.
    func testNonBookingColumnLoadsSumsOfAPendingPeriod() throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let before = model.categories.map(\.totalCents)
        model.interactivePeriod = Period.previousYear()
        XCTAssertEqual(model.categories.map(\.totalCents), before)

        model.sidebarSelection = .planned
        XCTAssertEqual(model.shownPeriod.period, Period.previousYear())
        let sums = model.categories.map(\.totalCents)
        XCTAssertNotEqual(sums, before)
        model.reloadCategories()
        XCTAssertEqual(model.categories.map(\.totalCents), sums)
    }

    func testRapidSwitchesKeepTheLatestSelection() async throws {
        let accounts = model.allAccounts.prefix(3)
        for a in accounts {
            model.interactiveSidebarSelection = .account(a.id)
            await Task.yield()
        }
        let expected = try expectedIDs()
        try await awaitBookings(expected)
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.bookings.map(\.id), expected)
        XCTAssertEqual(model.page?.transactions.map(\.id), expected)
    }

    func testSynchronousReloadSupersedesPendingLoad() async throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        // e.g. an edit right after the click: the data must be current on return
        model.reloadBookings()
        let expected = try expectedIDs()
        XCTAssertEqual(model.bookings.map(\.id), expected)
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertEqual(model.shownSelection, .account(account.id))
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.bookings.map(\.id), expected)
        try assertMatchesSynchronousLoad()
    }

    func testScopeChangeDuringPendingLoadIsPickedUp() async throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        model.scope = .everyday
        let expected = try expectedIDs()
        try await awaitBookings(expected)
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.bookings.map(\.id), expected)
    }

    /// Geplant shows no booking list: it loads none, and the list of the account before stays.
    /// Back to an account, the column keeps Geplant until that account's data is in (#1396).
    func testNonBookingColumnLoadsNoBookingsAndSwitchesBackWithItsData() async throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.sidebarSelection = .account(account.id)
        let accountBookings = model.bookings.map(\.id)

        model.interactiveSidebarSelection = .planned
        XCTAssertNil(model.selectionLoadTask)
        XCTAssertEqual(model.mainColumn, .planned)
        XCTAssertEqual(model.shownSelection, .planned)
        XCTAssertEqual(model.bookings.map(\.id), accountBookings)

        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertEqual(model.mainColumn, .planned, "no booking column before its data")
        XCTAssertEqual(model.shownSelection, .planned)
        for _ in 0..<300 where model.shownSelection != .account(account.id) {
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTAssertEqual(model.mainColumn, .bookings)
        XCTAssertEqual(model.bookings.map(\.id), try expectedIDs())
    }

    /// A click on Geplant drops a booking load still running for the selection before it.
    func testNonBookingColumnCancelsPendingLoad() async throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let before = model.bookings.map(\.id)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertNotNil(model.selectionLoadTask)
        model.interactiveSidebarSelection = .directDebits
        XCTAssertNil(model.selectionLoadTask)
        XCTAssertEqual(model.mainColumn, .directDebits)
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.bookings.map(\.id), before)
    }

    /// The account filter of a category view narrows the sidebar sums only there: on Geplant,
    /// which loads no bookings, the sums still drop it (#406).
    func testNonBookingColumnDropsCategoryAccountFilterFromSums() throws {
        let category = try XCTUnwrap(model.categories.first)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.sidebarSelection = .category(category.id)
        model.categoryAccountFilter = [account.id]
        XCTAssertNotNil(model.categorySumNote)
        let filtered = model.categories.map(\.totalCents)

        model.interactiveSidebarSelection = .planned
        XCTAssertNil(model.categorySumNote)
        let shown = model.categories.map(\.totalCents)
        XCTAssertNotEqual(shown, filtered, "the demo vault has bookings outside the filtered account")
        model.reloadCategories()
        XCTAssertEqual(model.categories.map(\.totalCents), shown)
    }

    /// Category view with an account filter → account (still loading) → Geplant: the cancelled
    /// account load would have dropped the filter from the sums, so Geplant does it.
    func testNonBookingColumnDropsFilterOfCategoryStillShown() throws {
        try assertGeplantDropsFilterAfterAccountClick(placeholderDelay: .seconds(30))
    }

    /// The same once the placeholder is up: the column already shows the account, the sums
    /// are still the category's.
    func testNonBookingColumnDropsFilterBehindThePlaceholder() throws {
        try assertGeplantDropsFilterAfterAccountClick(placeholderDelay: .zero)
    }

    private func assertGeplantDropsFilterAfterAccountClick(placeholderDelay: Duration) throws {
        model.selectionPlaceholderDelay = placeholderDelay
        let category = try XCTUnwrap(model.categories.first)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.sidebarSelection = .category(category.id)
        model.categoryAccountFilter = [account.id]
        let filtered = model.categories.map(\.totalCents)

        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertNotNil(model.selectionLoadTask)
        model.interactiveSidebarSelection = .planned
        let shown = model.categories.map(\.totalCents)
        XCTAssertNotEqual(shown, filtered)
        model.reloadCategories()
        XCTAssertEqual(model.categories.map(\.totalCents), shown)
    }

    /// „Neue Umsätze“ orders the stream by account card; leaving it regroups the same
    /// bookings by day even when the list itself is reloaded before the column switches.
    func testLeavingNewBookingsRegroupsTheStreamByDay() {
        model.sidebarSelection = .newBookings
        model.sidebarSelection = .allAccounts
        XCTAssertEqual(model.dayGroups.flatMap(\.bookings).map(\.id), model.bookings.map(\.id))
        XCTAssertEqual(Set(model.dayGroups.map(\.date)).count, model.dayGroups.count, "one header per day")
    }

    private struct LoadFailure: Error {}

    /// A switch that fails before the placeholder: the new title over an empty list and
    /// „–“ tiles, never the previous selection's figures.
    func testFailedSwitchBeforePlaceholderShowsNoPreviousData() throws {
        model.selectionPlaceholderDelay = .seconds(30)
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertEqual(model.shownSelection, .allAccounts)
        XCTAssertFalse(model.selectionFiguresMissing)

        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.shownSelection, .account(account.id))
        XCTAssertTrue(model.bookings.isEmpty)
        XCTAssertTrue(model.history.isEmpty)
        XCTAssertTrue(model.selectionFiguresMissing, "the tiles show „–“")
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertNotNil(model.errorMessage)
    }

    /// The same after the placeholder took over: it stays empty, without the loading state.
    func testFailedSwitchBehindPlaceholderShowsNoPreviousData() throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.interactiveSidebarSelection = .account(account.id)
        XCTAssertTrue(model.isLoadingSelection)

        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.shownSelection, .account(account.id))
        XCTAssertTrue(model.bookings.isEmpty)
        XCTAssertTrue(model.selectionFiguresMissing)
        XCTAssertFalse(model.isLoadingSelection)
        XCTAssertNotNil(model.errorMessage)
    }

    /// A failed refresh of the selection shown (sync, merge) keeps list, chart and tiles.
    func testFailedRefreshKeepsShownData() throws {
        let bookings = model.bookings.map(\.id), history = model.history.map(\.balanceCents)
        XCTAssertFalse(bookings.isEmpty)
        model.startSelectionLoad(keepingShown: true)
        XCTAssertNotNil(model.selectionLoadTask)

        model.failSelectionLoad(LoadFailure())
        XCTAssertEqual(model.shownSelection, .allAccounts)
        XCTAssertEqual(model.bookings.map(\.id), bookings)
        XCTAssertEqual(model.history.map(\.balanceCents), history)
        XCTAssertFalse(model.selectionFiguresMissing)
        XCTAssertNotNil(model.errorMessage)
    }

    /// Geplant loads no bookings, but the recurring data and salary cycle are those of all accounts.
    func testNonBookingColumnRecomputesRecurringForAllAccounts() throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.sidebarSelection = .allAccounts
        let allSeries = model.recurring.map(\.lastBookingId)
        let allCycle = model.salaryCycle?.lastDate
        let allOpen = model.recurringMonth?.openExpenseCents

        model.sidebarSelection = .account(account.id)
        model.sidebarSelection = .planned
        XCTAssertEqual(model.recurring.map(\.lastBookingId), allSeries)
        XCTAssertEqual(model.salaryCycle?.lastDate, allCycle)
        XCTAssertEqual(model.recurringMonth?.openExpenseCents, allOpen)
    }
}
