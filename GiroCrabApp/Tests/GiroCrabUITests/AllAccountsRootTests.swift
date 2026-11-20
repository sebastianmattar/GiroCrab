// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// „Alle Konten“: the sidebar's root row with the total per currency.
@MainActor
final class AllAccountsRootTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        AppDefaults.store.removeObject(forKey: AppModel.allAccountsCollapsedKey)
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws {
        model = nil
        AppDefaults.store.removeObject(forKey: AppModel.allAccountsCollapsedKey)
    }

    private func account(_ base: Account, cents: Int64, currency: String, inTotal: Bool = true) -> Account {
        var a = base
        a.balanceCents = cents
        a.currency = currency
        a.inTotal = inTotal
        return a
    }

    func testTotalsPerCurrencyForeignFirstAndOnlyCountedAccounts() throws {
        let base = try XCTUnwrap(model.allAccounts.first)
        let accounts = [
            account(base, cents: 100_00, currency: "EUR"),
            account(base, cents: 6_92, currency: "USD"),
            account(base, cents: -50_00, currency: "EUR"),
            account(base, cents: 999_00, currency: "EUR", inTotal: false),
            account(base, cents: 1_00, currency: "CHF"),
        ]
        let totals = AllAccountsTotal.totals(of: accounts)
        XCTAssertEqual(totals.map(\.currency), ["CHF", "USD", "EUR"])
        XCTAssertEqual(totals.map(\.cents), [1_00, 6_92, 50_00])
    }

    func testNoCountedAccountsHasNoTotal() throws {
        let base = try XCTUnwrap(model.allAccounts.first)
        XCTAssertTrue(AllAccountsTotal.totals(of: [account(base, cents: 5, currency: "EUR", inTotal: false)]).isEmpty)
    }

    func testCollapsedStateIsRemembered() {
        XCTAssertFalse(model.allAccountsCollapsed)
        model.allAccountsCollapsed = true
        let again = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertTrue(again.allAccountsCollapsed)
    }

    func testTitleIsAlleKonten() {
        model.sidebarSelection = .allAccounts
        XCTAssertEqual(model.title, "Alle Konten")
    }

    /// Like collapsed groups, a collapsed root opens while the sidebar search runs.
    func testSearchShowsTheTreeEvenWhenCollapsed() {
        model.allAccountsCollapsed = true
        XCTAssertFalse(model.showsAccountTree)
        model.sidebarAccountSearch = "a"
        XCTAssertTrue(model.showsAccountTree)
        model.sidebarAccountSearch = ""
        model.allAccountsCollapsed = false
        XCTAssertTrue(model.showsAccountTree)
    }
}
