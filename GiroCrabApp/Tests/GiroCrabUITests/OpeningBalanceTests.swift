import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Startsaldo (GiroCrab/GiroCrab#212): balance = Startsaldo + bookings from its date on.
@MainActor
final class OpeningBalanceTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws { model = nil }

    /// An imported account (no bank balance) with -50,00 on 2024-01-10 and +20,00 on 2024-03-01.
    private func importedAccount() throws -> Int64 {
        let csv = "Datum;Name;Verwendungszweck;Betrag\n10.01.2024;Bäcker;Brötchen;-50,00\n01.03.2024;Arbeitgeber;Bonus;20,00\n"
        let vault = try XCTUnwrap(model.vault)
        let r = try vault.importMoneymoneyData(data: Data(csv.utf8), fileName: "Altkonto.csv", targetAccountId: nil,
                                               preserveCategories: false, preserveNotes: false)
        model.reloadAccounts()
        return r.accountId
    }

    func testOpeningBalanceDrivesTheSidebarBalance() async throws {
        let id = try importedAccount()
        XCTAssertEqual(model.account(for: id)?.balanceDerived, true)

        try model.setOpeningBalance(id, cents: 1_000_00, date: "")
        XCTAssertEqual(model.account(for: id)?.balanceCents, 1_000_00 - 50_00 + 20_00)

        // from 2024-02-01 on, the January booking is part of the Startsaldo
        try model.setOpeningBalance(id, cents: 1_000_00, date: "2024-02-01")
        XCTAssertEqual(model.account(for: id)?.balanceCents, 1_000_00 + 20_00)
        XCTAssertEqual(model.account(for: id)?.openingBalanceDate, "2024-02-01")

        // "Kalibrieren" in the sheet: target − bookings since the date; nothing stored
        let loaded = try await model.previewOpeningBalance(id, date: "2024-02-01")
        let preview = try XCTUnwrap(loaded)
        XCTAssertEqual(preview.sumCents, 20_00)
        XCTAssertEqual([preview.counted, preview.older], [1, 1])
        XCTAssertEqual(model.account(for: id)?.openingBalanceCents, 1_000_00)
        try model.setOpeningBalance(id, cents: 500_00 - preview.sumCents, date: "2024-02-01")
        XCTAssertEqual(model.account(for: id)?.balanceCents, 500_00)
        XCTAssertEqual(model.account(for: id)?.openingBalanceCents, 480_00)

        try model.setOpeningBalance(id, cents: nil, date: "")
        XCTAssertNil(model.account(for: id)?.openingBalanceCents)
        // removed: back to the pure sum of all bookings (20 € since, −50 € before)
        XCTAssertEqual(model.account(for: id)?.balanceCents, -30_00)
    }

    func testDemoCardHasAComputedBalance() throws {
        let accounts = model.groups.flatMap(\.accounts)
        let card = try XCTUnwrap(accounts.first { $0.kind == .creditCard })
        XCTAssertTrue(card.balanceDerived)
        XCTAssertNotNil(card.openingBalanceCents)
        XCTAssertEqual(card.balanceCents, -642_80)
        XCTAssertEqual(accounts.filter(\.balanceDerived).map(\.id), [card.id])
    }

    func testBankBalancesAreNotOverridden() throws {
        let giro = try XCTUnwrap(model.groups.flatMap(\.accounts).first { $0.kind == .checking })
        XCTAssertFalse(giro.balanceDerived)
        XCTAssertThrowsError(try model.setOpeningBalance(giro.id, cents: 1, date: ""))
        XCTAssertEqual(model.account(for: giro.id)?.balanceCents, giro.balanceCents)
    }
}
