import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Offline accounts and manual bookings (#679, #81): which accounts take bookings, and what the
/// editor's calls change. Run against the demo vault, which holds a cash account "Bargeld".
@MainActor
final class OfflineAccountsTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws { model = nil }

    private var cash: Account { model.allAccounts.first { $0.name == "Bargeld" }! }

    private func setup(_ cents: Int64, _ who: String) -> ManualBookingSetup {
        ManualBookingSetup(bookingDate: DateText.iso(Date()), amountCents: cents, counterparty: who,
                           purpose: "", categoryId: nil, note: "")
    }

    /// All bookings of one account (the filter of the current view, narrowed to that account).
    private func bookings(of id: Int64) throws -> [Booking] {
        var filter = model.currentFilter
        filter.accountIds = [id]
        filter.limit = 1000
        return try XCTUnwrap(model.vault).listTransactions(filter: filter).transactions
    }

    private func awaitBalance(of id: Int64, _ cents: Int64) async throws {
        for _ in 0..<300 {
            if model.account(for: id)?.balanceCents == cents { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("The sidebar did not show the new balance")
    }

    func testOnlyAccountsWithoutBankConnectionTakeBookings() {
        XCTAssertEqual(model.offlineAccounts.map(\.name), ["Bargeld"])
        XCTAssertTrue(model.isOffline(cash.id))
        let giro = model.allAccounts.first { $0.name == "Musterbank Girokonto" }!
        XCTAssertFalse(model.isOffline(giro.id))
        XCTAssertTrue(model.canAddManualBooking)
    }

    func testSelectingTheOfflineAccountOffersItsBookingForNewEntries() {
        XCTAssertNil(model.selectedOfflineAccount)
        model.sidebarSelection = .account(cash.id)
        XCTAssertEqual(model.selectedOfflineAccount?.id, cash.id)
        model.openManualBooking()
        XCTAssertEqual(model.offlineStore.bookingRequest?.accountId, cash.id)
        XCTAssertNil(model.offlineStore.bookingRequest?.booking)
    }

    func testSavingABookingMovesTheBalanceAndTheBookingCanBeDeleted() async throws {
        let before = cash.balanceCents
        let id = try model.saveManualBooking(accountId: cash.id, bookingId: nil, setup: setup(-10_00, "Kiosk"))
        try await awaitBalance(of: cash.id, before - 10_00)
        let b = try XCTUnwrap(try bookings(of: cash.id).first { $0.id == id })
        XCTAssertEqual(b.counterparty, "Kiosk")
        XCTAssertTrue(model.isManuallyEditable(b))

        model.deleteManualBooking(b)
        try await awaitBalance(of: cash.id, before)
    }

    func testBookingsOfBankAccountsAreNotEditableButCanBeMirrored() async throws {
        let giro = model.allAccounts.first { $0.name == "Musterbank Girokonto" }!
        let booking = try XCTUnwrap(try bookings(of: giro.id).first { $0.amountCents < 0 })
        XCTAssertFalse(model.isManuallyEditable(booking))
        let before = cash.balanceCents
        model.mirror(booking, to: cash)
        XCTAssertNil(model.errorMessage)
        try await awaitBalance(of: cash.id, before - booking.amountCents)
    }

    func testBalanceCorrectionBooksTheDifference() async throws {
        let before = cash.balanceCents
        model.correctBalance(cash.id, to: before + 7_50)
        try await awaitBalance(of: cash.id, before + 7_50)
        model.correctBalance(cash.id, to: before + 7_50)
        XCTAssertEqual(model.toast, String(localized: "Der Saldo stimmt bereits."))
    }

    func testAnInvalidBookingReportsTheCoreError() {
        XCTAssertThrowsError(try model.saveManualBooking(accountId: cash.id, bookingId: nil, setup: setup(0, "x")))
    }

    func testAccountNoteSurvivesAReloadOfTheAccounts() async throws {
        let id = cash.id
        model.setAccountNote("Reisekasse", for: id)
        model.reloadAccounts()
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(model.accountNote(for: id), "Reisekasse")
    }

    /// #1049: kind and currency chosen in the sheet reach the new account, and its bookings carry the currency.
    /// Goes through the vault directly: `createOfflineAccount` would switch the demo to the own vault first.
    func testNewOfflineAccountTakesKindAndCurrency() async throws {
        let vault = try XCTUnwrap(model.vault)
        let id = try vault.createOfflineAccount(name: "Tagesgeld", kind: .savings, currency: "CHF", openingCents: 100_00)
        model.offlineStore.accountIDs.insert(id)
        model.reloadAccounts()
        try await awaitBalance(of: id, 100_00)
        let account = try XCTUnwrap(model.account(for: id))
        XCTAssertEqual(account.kind, .savings)
        XCTAssertEqual(account.currency, "CHF")
        XCTAssertTrue(model.isOffline(id))
        let booking = try model.saveManualBooking(accountId: id, bookingId: nil, setup: setup(-5_00, "Gebühr"))
        let row = try XCTUnwrap(try bookings(of: id).first { $0.id == booking })
        XCTAssertEqual(row.currency, "CHF")
        try await awaitBalance(of: id, 95_00)
    }

    func testSheetOffersCashAndSavingsWithEuroFirst() {
        XCTAssertEqual(OfflineAccountOptions.kinds, [.cash, .savings])
        XCTAssertEqual(OfflineAccountOptions.currencies.first, "EUR")
        XCTAssertEqual(OfflineAccountOptions.defaultCurrency, "EUR")
        XCTAssertNotEqual(OfflineAccountOptions.title(.cash), OfflineAccountOptions.title(.savings))
    }

    /// #1049: header, chart axis and bookings of a USD account show dollars, not euros.
    func testHeaderChartAndBookingsFollowTheAccountCurrency() async throws {
        let vault = try XCTUnwrap(model.vault)
        let id = try vault.createOfflineAccount(name: "Dollar", kind: .cash, currency: "USD", openingCents: 0)
        model.offlineStore.accountIDs.insert(id)
        model.reloadAccounts()
        try await awaitBalance(of: id, 0)
        model.sidebarSelection = .account(id)
        XCTAssertEqual(model.balanceCurrency, "USD")
        XCTAssertTrue(Money.format(-30_00, currency: model.balanceCurrency).contains("$"))
        XCTAssertTrue(BalanceChart.compactEuros(-30, currency: "USD").hasSuffix("$"))
        XCTAssertTrue(BalanceChart.compactEuros(-30).hasSuffix("€"))
        model.sidebarSelection = .allAccounts
        XCTAssertEqual(model.balanceCurrency, "EUR")
        XCTAssertTrue(Money.format(100, currency: "").contains("€"))
    }

    /// #1049 (review): sums follow the currency of exactly one account, whether it is chosen by the
    /// selection or by the account filter; several accounts sum in EUR only (as the core does).
    func testSumCurrencyFollowsOneAccountOnly() async throws {
        let vault = try XCTUnwrap(model.vault)
        let id = try vault.createOfflineAccount(name: "Dollar", kind: .cash, currency: "USD", openingCents: 0)
        model.offlineStore.accountIDs.insert(id)
        model.reloadAccounts()
        try await awaitBalance(of: id, 0)
        XCTAssertEqual(model.sumCurrency(forAccounts: [id]), "USD")
        XCTAssertEqual(model.sumCurrency(forAccounts: []), "EUR")
        XCTAssertEqual(model.sumCurrency(forAccounts: [id, try XCTUnwrap(model.allAccounts.first { $0.id != id }).id]), "EUR")
        model.sidebarSelection = .allAccounts
        model.categoryAccountFilter = [id]
        XCTAssertEqual(model.balanceCurrency, "USD")
        XCTAssertEqual(model.categorySumCurrency, "EUR") // the sidebar sums ignore the filter outside the category views
        model.sidebarSelection = .uncategorized
        XCTAssertEqual(model.categorySumCurrency, "USD")
        model.categoryAccountFilter = []
        XCTAssertEqual(model.balanceCurrency, "EUR")
        XCTAssertEqual(model.categorySumCurrency, "EUR")
    }

    /// #1049 (maintainer decision): curated list plus an own ISO code, and the header line "Sparkonto · USD".
    func testCurrencyChoiceAndSubtitle() async throws {
        XCTAssertEqual(OfflineAccountOptions.currencies,
                       ["EUR", "USD", "CHF", "GBP", "SEK", "DKK", "NOK", "PLN", "CZK"])
        XCTAssertEqual(OfflineAccountOptions.currency(picked: "SEK", typed: "xxx"), "SEK")
        let other = OfflineAccountOptions.otherTag
        XCTAssertEqual(OfflineAccountOptions.currency(picked: other, typed: " jpy "), "JPY")
        for bad in ["", "JP", "JPYY", "J1Y", "€€€"] {
            XCTAssertNil(OfflineAccountOptions.currency(picked: other, typed: bad), bad)
        }
        let vault = try XCTUnwrap(model.vault)
        let id = try vault.createOfflineAccount(name: "Reserve", kind: .savings, currency: "JPY", openingCents: 0)
        model.offlineStore.accountIDs.insert(id)
        model.reloadAccounts()
        try await awaitBalance(of: id, 0)
        let account = try XCTUnwrap(model.account(for: id))
        XCTAssertEqual(OfflineAccountOptions.subtitle(account), "\(OfflineAccountOptions.title(.savings)) · JPY")
        XCTAssertNotEqual(account.sfSymbol, cash.sfSymbol, "kinds differ by symbol")
    }

    /// #1049: a category over all accounts counts EUR only and lists the other currencies as "Zusätzlich".
    func testCategoryListsOtherCurrenciesSeparately() async throws {
        let vault = try XCTUnwrap(model.vault)
        let usd = try vault.createOfflineAccount(name: "Dollar", kind: .cash, currency: "USD", openingCents: 0)
        model.offlineStore.accountIDs.insert(usd)
        model.reloadAccounts()
        try await awaitBalance(of: usd, 0)
        let cat = try vault.createCategory(name: "Reisen \(UUID().uuidString.prefix(6))", parentId: nil, sfSymbol: "airplane", colorHex: "#336699")
        var s = setup(-30_00, "Hotel")
        s.categoryId = cat
        _ = try model.saveManualBooking(accountId: usd, bookingId: nil, setup: s)

        model.sidebarSelection = .category(cat)
        model.reloadCategoryHistory()
        var other: [CurrencyAmount] = []
        for _ in 0..<300 {
            other = model.categoryOtherCurrencies
            if !other.isEmpty { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertEqual(other.map(\.currency), ["USD"])
        XCTAssertEqual(other.first?.cents, -30_00)
        XCTAssertEqual(model.categoryHistory.map(\.cents).reduce(0, +), 0, "EUR only: the dollars are not summed in")
        XCTAssertEqual(model.balanceCurrency, "EUR")

        model.sidebarSelection = .allAccounts
        model.reloadCategoryHistory()
        XCTAssertTrue(model.categoryOtherCurrencies.isEmpty)
    }

    /// #1049: "Zusätzlich" next to the sidebar total of a category and under the KPI tiles.
    func testOtherCurrenciesAppearNextToTotalsAndKPIs() async throws {
        let vault = try XCTUnwrap(model.vault)
        let usd = try vault.createOfflineAccount(name: "Dollar", kind: .cash, currency: "USD", openingCents: 0)
        model.offlineStore.accountIDs.insert(usd)
        model.reloadAccounts()
        try await awaitBalance(of: usd, 0)
        let cat = try vault.createCategory(name: "Reisen \(UUID().uuidString.prefix(6))", parentId: nil, sfSymbol: "airplane", colorHex: "#336699")
        var s = setup(-30_00, "Hotel")
        s.categoryId = cat
        _ = try model.saveManualBooking(accountId: usd, bookingId: nil, setup: s)

        model.sidebarSelection = .allAccounts
        model.refreshOtherCurrencies()
        for _ in 0..<300 where model.categoryOtherTotals[cat] == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertEqual(model.categoryOtherTotals[cat]?.map(\.currency), ["USD"])
        XCTAssertEqual(model.categoryOtherTotals[cat]?.first?.cents, -30_00)
        XCTAssertEqual(model.otherCurrencyFlows.map(\.currency), ["USD"])
        let text = try XCTUnwrap(model.otherCurrencyText { -$0.consumptionCents })
        XCTAssertTrue(text.contains("$"), text)

        // one account: its own currency is the main figure, nothing is "zusätzlich"
        model.sidebarSelection = .account(usd)
        model.refreshOtherCurrencies()
        for _ in 0..<300 where !model.otherCurrencyFlows.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(model.otherCurrencyFlows.isEmpty)
    }
}
