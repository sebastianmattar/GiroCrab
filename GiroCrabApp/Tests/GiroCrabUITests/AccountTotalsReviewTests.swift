import XCTest
import GiroCrabCore
@testable import GiroCrabUI

@MainActor
final class AccountTotalsReviewTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        model.sidebarSelection = .allAccounts
    }

    override func tearDown() async throws { model = nil }

    private func startCategoryRefresh() throws -> (Account, Task<Void, Never>) {
        let target = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking && $0.inTotal })
        let booking = try XCTUnwrap(model.bookings.first { $0.accountId == target.id })
        let category = try XCTUnwrap(model.categories.first { $0.depth > 0 && $0.id != booking.categoryId })
        model.setCategory(category.id, for: booking)
        return (target, try XCTUnwrap(model.categoryRefreshTask))
    }

    private func assertFreshTotals(file: StaticString = #filePath, line: UInt = #line) throws {
        let vault = try XCTUnwrap(model.vault)
        let (from, to) = model.periodRange
        let flow = try vault.selectionFlowForTotals(accountIds: [], dateFrom: from, dateTo: to)
        XCTAssertEqual(model.selectionFlow?.income, flow.incomeCents, file: file, line: line)
        XCTAssertEqual(model.selectionFlow?.consumption, flow.consumptionCents, file: file, line: line)
        let categories = try vault.listCategories(dateFrom: from, dateTo: to)
        XCTAssertEqual(model.categories.map(\.totalCents), categories.map(\.totalCents), file: file, line: line)
    }

    func testAccountToggleStopsOlderCategoryRefreshAndKeepsDerivedDataFresh() async throws {
        let (target, pending) = try startCategoryRefresh()
        model.setAccountIncludeInTotal(target, false)
        XCTAssertTrue(pending.isCancelled, "the older refresh must not publish its captured account and KPI snapshot")
        await pending.value
        XCTAssertFalse(try XCTUnwrap(model.allAccounts.first { $0.id == target.id }).includeInTotal)
        try assertFreshTotals()
        XCTAssertNil(model.errorMessage)
    }

    func testGroupToggleStopsOlderCategoryRefreshAndKeepsDerivedDataFresh() async throws {
        let target = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking && $0.inTotal })
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Review-Testgruppe"))
        model.moveAccount(target.id, to: uuid)
        let (_, pending) = try startCategoryRefresh()
        let group = try XCTUnwrap(model.userGroups.first { $0.uuid == uuid })
        model.updateUserGroup(group, includeInTotal: false)
        XCTAssertTrue(pending.isCancelled)
        await pending.value
        XCTAssertFalse(try XCTUnwrap(model.allAccounts.first { $0.id == target.id }).inTotal)
        try assertFreshTotals()
    }

    func testArchivingAndReactivatingInvalidatesCashflowWithoutChangingProofCount() throws {
        let target = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking && $0.inTotal })
        let vault = try XCTUnwrap(model.vault)
        let proofCount = model.bookings.count
        // the overview card is an overall money figure (#337): totals context
        let before = try vault.cashflowByPeriod(granularity: .month, count: 6, respectInTotal: true).map(\.incomeCents)
        var version = model.totalsVersion
        model.setAccountArchived(target, true)
        let archived = try vault.cashflowByPeriod(granularity: .month, count: 6, respectInTotal: true).map(\.incomeCents)
        XCTAssertNotEqual(archived, before, "the account contributes real demo cashflow")
        XCTAssertEqual(model.bookings.count, proofCount)
        XCTAssertGreaterThan(model.totalsVersion, version, "a visible cashflow card needs a new request")
        version = model.totalsVersion
        model.setAccountArchived(target, false)
        XCTAssertEqual(model.bookings.count, proofCount)
        XCTAssertEqual(try vault.cashflowByPeriod(granularity: .month, count: 6, respectInTotal: true).map(\.incomeCents), before)
        XCTAssertGreaterThan(model.totalsVersion, version)
    }

    func testFullReloadInvalidatesCashflowAfterExternalFlagChange() throws {
        let vault = try XCTUnwrap(model.vault)
        let target = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking && $0.inTotal })
        let proofCount = model.bookings.count
        let version = model.totalsVersion
        // A flag-only sync merge changes the vault but adds no bookings.
        try vault.setAccountIncludeInTotal(accountId: target.id, includeInTotal: false)
        model.reloadAll()
        XCTAssertEqual(model.bookings.count, proofCount)
        XCTAssertFalse(try XCTUnwrap(model.allAccounts.first { $0.id == target.id }).inTotal)
        XCTAssertGreaterThan(model.totalsVersion, version)
        try assertFreshTotals()
    }

    func testGroupExclusionTooltipHidesNamesOnlyForLabelRedaction() throws {
        let target = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        let name = "Vertrauliche Testgruppe 337"
        let uuid = try XCTUnwrap(model.createUserGroup(name: name))
        model.moveAccount(target.id, to: uuid)
        model.updateUserGroup(try XCTUnwrap(model.userGroups.first { $0.uuid == uuid }), includeInTotal: false)
        let account = try XCTUnwrap(model.allAccounts.first { $0.id == target.id })
        model.reportRedaction = .personal
        XCTAssertTrue(try XCTUnwrap(model.totalExclusionReason(account)).contains(name))
        model.reportRedaction = [.personal, .labels]
        let hidden = try XCTUnwrap(model.totalExclusionReason(account))
        XCTAssertFalse(hidden.contains(name), "help text must not reveal a redacted group name")
        XCTAssertTrue(hidden.contains("ausgeschlossen"), "the exclusion explanation stays available")
        model.reportRedaction = []
        let environmentOnly = try XCTUnwrap(model.totalExclusionReason(account, redaction: .labels))
        XCTAssertFalse(environmentOnly.contains(name), "the view environment also supplies command-line redaction")
    }
}
