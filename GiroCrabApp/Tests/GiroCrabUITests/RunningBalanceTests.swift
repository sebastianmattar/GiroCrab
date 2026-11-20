// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
@testable import GiroCrabUI
import GiroCrabCore

/// Ansicht › Saldo anzeigen (#577): the list loads balances only for account selections.
@MainActor
final class RunningBalanceTests: XCTestCase {
    private var model: AppModel!
    private var saved: Any?

    override func setUp() async throws {
        saved = AppDefaults.store.object(forKey: ViewLayout.runningBalanceKey)
        AppDefaults.store.set(false, forKey: ViewLayout.runningBalanceKey)
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws {
        AppDefaults.store.set(saved, forKey: ViewLayout.runningBalanceKey)
        model = nil
    }

    /// The setting is observed, so a change from the settings window reloads too.
    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<300 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    func testSettingLoadsBalancesForAccountsOnly() async throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.sidebarSelection = .account(account.id)
        XCTAssertTrue(model.listBalances.bookings.isEmpty, "off by default")
        XCTAssertFalse(model.currentFilter.withBalances)

        AppDefaults.store.set(true, forKey: ViewLayout.runningBalanceKey)
        try await waitUntil { !model.listBalances.bookings.isEmpty }
        XCTAssertFalse(model.listBalances.days.isEmpty)
        // the newest booked row carries today's balance of the account
        let newest = try XCTUnwrap(model.bookings.first { !$0.isPending })
        let balance = try XCTUnwrap(model.listBalances.bookings[newest.id])
        XCTAssertEqual(balance.text, Money.format(balance.cents, currency: account.currency))
        XCTAssertEqual(model.listBalances.currency, account.currency)
        // only the list's own pages pay for the balances
        XCTAssertTrue(model.listFilter.withBalances)
        XCTAssertFalse(model.currentFilter.withBalances)

        // a category lists rows of accounts it does not name: no balances, also not in the CSV
        let category = try XCTUnwrap(model.categories.first)
        model.sidebarSelection = .category(category.id)
        XCTAssertTrue(model.listBalances.bookings.isEmpty)
        XCTAssertFalse(model.listFilter.withBalances)
        let categoryExport = try await model.loadExportPage(
            accountIDs: nil, period: .all, bookingIDs: nil, options: nil, withBalances: true)
        XCTAssertTrue(try XCTUnwrap(categoryExport).balances.isEmpty)

        model.sidebarSelection = .account(account.id)
        XCTAssertFalse(model.listBalances.bookings.isEmpty)
        let accountExport = try await model.loadExportPage(
            accountIDs: nil, period: .all, bookingIDs: nil, options: nil, withBalances: true)
        XCTAssertFalse(try XCTUnwrap(accountExport).balances.isEmpty)
        AppDefaults.store.set(false, forKey: ViewLayout.runningBalanceKey)
        try await waitUntil { model.listBalances.bookings.isEmpty }
        XCTAssertTrue(model.listBalances.bookings.isEmpty)
        XCTAssertTrue(model.listBalances.days.isEmpty)
    }
}
