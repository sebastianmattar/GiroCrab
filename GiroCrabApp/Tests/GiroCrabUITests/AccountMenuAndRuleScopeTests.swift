// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// #412: account menus follow the sidebar; #295: a rule made from a booking
/// carries that booking's direction.
@MainActor
final class AccountMenuAndRuleScopeTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        for k in ["sidebar.accountView"] { AppDefaults.store.removeObject(forKey: k) }
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws {
        AppDefaults.store.removeObject(forKey: "sidebar.accountView")
        model = nil
    }

    func testMenuSectionsFollowTheSidebarInEveryMode() {
        for mode in AccountSidebarMode.allCases {
            model.accountView = mode
            let sections = model.accountMenuSections
            XCTAssertFalse(sections.isEmpty, "\(mode)")
            XCTAssertTrue(sections.allSatisfy { !$0.accounts.isEmpty }, "empty sections are dropped (\(mode))")
            let expected: [[Int64]]
            switch mode {
            case .byKind: expected = model.groups.map { $0.accounts.map(\.id) }
            case .byBank: expected = model.bankGroups.map { $0.accounts.map(\.id) }
            case .byGroup: expected = model.userGroups.map { $0.accounts.map(\.id) }
            }
            XCTAssertEqual(sections.map { $0.accounts.map(\.id) }, expected.filter { !$0.isEmpty }, "\(mode)")
            // every account appears exactly once
            XCTAssertEqual(sections.flatMap(\.accounts).map(\.id).sorted(), model.allAccounts.map(\.id).sorted(), "\(mode)")
        }
    }

    private func booking(_ amount: Int64) -> Booking? {
        model.bookings.first { ($0.amountCents > 0) == (amount > 0) && $0.amountCents != 0 }
    }

    func testRuleFromIncomeBookingIsIncomeOnly() throws {
        let b = try XCTUnwrap(booking(1))
        let r = model.newRuleDraft(from: b)
        XCTAssertEqual(r.minAmountCents, 1)
        XCTAssertNil(r.maxAmountCents)
    }

    func testRuleFromExpenseBookingIsExpenseOnly() throws {
        let b = try XCTUnwrap(booking(-1))
        let r = model.newRuleDraft(from: b)
        XCTAssertNil(r.minAmountCents)
        XCTAssertEqual(r.maxAmountCents, 0)
    }

    func testBlankRuleHasNoDirection() {
        let r = model.newRuleDraft()
        XCTAssertNil(r.minAmountCents)
        XCTAssertNil(r.maxAmountCents)
    }
}
