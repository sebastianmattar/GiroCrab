import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Assigning a category shows on the row at once; totals, counters and KPIs
/// follow from a background refresh and end up as a synchronous reload would.
@MainActor
final class CategoryAssignTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws { model = nil }

    /// An uncategorised booking of a non-loan account and a category to give it.
    private func uncategorizedBooking() throws -> (Booking, GiroCrabCore.Category) {
        let loans = Set(model.allAccounts.filter { $0.kind == .loan }.map(\.id))
        let b = try XCTUnwrap(model.bookings.first { $0.categoryId == nil && !loans.contains($0.accountId) })
        let c = try XCTUnwrap(model.categories.first { $0.depth > 0 })
        return (b, c)
    }

    /// Category totals, counters and KPIs as the vault has them now.
    private struct Derived: Equatable {
        var totals: [Int64]
        var uncategorized: Int
        var flow: [Int64]
    }

    private func derived() -> Derived {
        Derived(
            totals: model.categories.map(\.totalCents),
            uncategorized: model.uncategorizedCount,
            flow: [model.selectionFlow?.consumption, model.selectionFlow?.invested, model.selectionFlow?.income].map { $0 ?? 0 }
        )
    }

    /// What synchronous reloads produce, without changing the model.
    private func expected() throws -> Derived {
        let vault = try XCTUnwrap(model.vault)
        let (from, to) = model.periodRange
        let filter = model.currentFilter
        let flow = try filter.respectInTotal
            ? vault.selectionFlowForTotals(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
            : vault.selectionFlow(accountIds: filter.accountIds, dateFrom: from, dateTo: to)
        let loans = Set(model.allAccounts.filter { $0.kind == .loan }.map(\.id))
        let accounts = try vault.listAccounts().filter { !loans.contains($0.id) }
        return Derived(
            totals: try vault.listCategories(dateFrom: from, dateTo: to).map(\.totalCents),
            uncategorized: accounts.reduce(0) { $0 + Int($1.uncategorizedCount) },
            flow: [flow.consumptionCents, flow.investedCents, flow.incomeCents]
        )
    }

    private func awaitDerived(_ want: Derived, file: StaticString = #filePath, line: UInt = #line) async throws {
        for _ in 0..<300 {
            if derived() == want { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertEqual(derived(), want, "the background refresh did not publish", file: file, line: line)
    }

    func testAssignmentShowsAtOnceAndRefreshesInBackground() async throws {
        let (b, c) = try uncategorizedBooking()
        let before = derived()

        model.setCategory(c.id, for: b)
        let row = try XCTUnwrap(model.bookings.first { $0.id == b.id })
        XCTAssertEqual(row.categoryId, c.id, "the row shows the new category before the refresh")
        XCTAssertEqual(row.categoryName, c.name)
        XCTAssertEqual(row.categorySource, .manual)

        let want = try expected()
        XCTAssertNotEqual(want, before, "the assignment changes totals and counters")
        try await awaitDerived(want)
        XCTAssertEqual(model.bookings.first { $0.id == b.id }?.categoryId, c.id)
        XCTAssertNil(model.errorMessage)
    }

    /// A scope switch right after the assignment cancels the scope load it
    /// started; the category totals and counters must still arrive.
    func testScopeSwitchDuringRefreshKeepsTotals() async throws {
        let (b, c) = try uncategorizedBooking()
        model.setCategory(c.id, for: b)
        model.scope = .everyday
        try await awaitDerived(try expected())
        XCTAssertNil(model.errorMessage)
    }

    /// Quick successive assignments: the last one wins everywhere.
    func testRapidAssignmentsPublishTheLastState() async throws {
        let (b, c) = try uncategorizedBooking()
        let other = try XCTUnwrap(model.categories.first { $0.depth > 0 && $0.id != c.id })
        model.setCategory(c.id, for: b)
        model.setCategory(other.id, for: b)
        model.setCategory(nil, for: b)
        XCTAssertNil(model.bookings.first { $0.id == b.id }?.categoryId)
        try await awaitDerived(try expected())
        XCTAssertNil(model.bookings.first { $0.id == b.id }?.categoryId)
    }

    func testDragPayloadRoundTrips() {
        XCTAssertEqual(BookingDrag.ids(BookingDrag.payload([3, 14, 15])), [3, 14, 15])
        XCTAssertEqual(BookingDrag.ids(GroupDrag.account(3)), [], "an account drag carries no bookings")
        XCTAssertEqual(BookingDrag.ids("3,14"), [])
    }

    /// The row menu and the drag act on the multiple selection only when the
    /// row is part of it.
    func testTargetsFollowTheMultipleSelection() {
        let rows = Array(model.bookings.prefix(3))
        XCTAssertEqual(rows.count, 3)
        model.selectedBookingIDs = Set(rows.prefix(2).map(\.id))
        XCTAssertEqual(Set(model.categorizeTargets(for: rows[1]).map(\.id)), model.selectedBookingIDs)
        XCTAssertEqual(model.categorizeTargets(for: rows[2]).map(\.id), [rows[2].id])
    }

    /// Bookings dropped onto a sidebar category get it; foreign drags change nothing.
    func testDroppedBookingsGetTheCategory() async throws {
        let (b, c) = try uncategorizedBooking()
        XCTAssertFalse(model.dropBookings([GroupDrag.account(b.accountId)], onCategory: c.id))
        XCTAssertFalse(model.dropBookings([BookingDrag.payload([-1])], onCategory: c.id), "not a loaded booking")
        XCTAssertNil(model.bookings.first { $0.id == b.id }?.categoryId)

        XCTAssertTrue(model.dropBookings([BookingDrag.payload([b.id])], onCategory: c.id))
        XCTAssertEqual(model.bookings.first { $0.id == b.id }?.categoryId, c.id)
        try await awaitDerived(try expected())
        XCTAssertNil(model.errorMessage)
    }

    /// A booking dropped onto the list also arrives as a URL; like a web link
    /// it is no receipt to attach or match.
    func testDraggedBookingIsNoReceipt() throws {
        let b = try XCTUnwrap(model.bookings.first)
        let urls = try [BookingDrag.payload([b.id]), "https://example.com/beleg.pdf"].map { try XCTUnwrap(URL(string: $0)) }
        XCTAssertFalse(model.attach(urls, to: b))
        XCTAssertFalse(model.dropAnywhere(urls))
    }

    /// Rule counts are reloaded right away while the rule editor is open.
    func testOpenRuleEditorRefreshesRuleCounts() async throws {
        let vault = try XCTUnwrap(model.vault)
        let ruled = try XCTUnwrap(vault.listTransactions(filter: BookingFilter(
            accountIds: [], categoryId: nil, includeSubcategories: true, uncategorizedOnly: false,
            search: nil, dateFrom: nil, dateTo: nil, limit: 0, offset: 0,
            scope: .all, merchantKey: nil, direction: .all, checked: nil, wholeDays: false
        )).transactions.first { $0.ruleId != nil })
        let rule = try XCTUnwrap(ruled.ruleId)
        model.showRuleEditor = true
        let before = try XCTUnwrap(model.rules.first { $0.id == rule }?.matchCount)

        model.setCategory(nil, for: ruled)
        XCTAssertEqual(model.rules.first { $0.id == rule }?.matchCount, before - 1)
    }
}
