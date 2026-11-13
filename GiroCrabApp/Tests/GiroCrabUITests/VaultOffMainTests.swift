import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Core calls from UI code run off the main thread (GitHub #262).
@MainActor
final class VaultOffMainTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws { model = nil }

    func testVaultOffMainReturnsTheCoreResultAndRunsOffTheMainThread() async throws {
        let expected = try XCTUnwrap(model.vault).listCategories(dateFrom: nil, dateTo: nil).count
        let result = try await model.vaultOffMain { v in
            (try v.listCategories(dateFrom: nil, dateTo: nil).count, Thread.isMainThread)
        }
        let (count, onMain) = try XCTUnwrap(result)
        XCTAssertEqual(count, expected)
        XCTAssertFalse(onMain)
        XCTAssertTrue(model.isUnlocked)
    }

    func testLoansReloadInTheBackground() async throws {
        // created in the core directly: saveLoan needs NSApp for its undo manager
        let id = try XCTUnwrap(model.vault).createManualLoan(setup: LoanSetup(
            name: "Autokredit", originalCents: 20_000_00, anchorDate: "2024-01-31",
            anchorBalanceCents: 15_000_00, interestRatePpm: nil, matchExpr: ""))
        XCTAssertNil(model.manualLoan(id))
        // reloadAccounts → reloadLoans: published once the background read is done
        model.reloadAccounts()
        await model.loanStore.loadTask?.value
        XCTAssertEqual(model.manualLoan(id)?.setup.name, "Autokredit")
    }

    func testLoanEditsAreVisibleAtOnce() throws {
        // undo entries and the sidebar read the store right after an edit
        let id = try model.saveLoan(LoanSetup(
            name: "Autokredit", originalCents: 20_000_00, anchorDate: "2024-01-31",
            anchorBalanceCents: 15_000_00, interestRatePpm: nil, matchExpr: ""), accountId: nil)
        XCTAssertEqual(model.manualLoan(id)?.setup.name, "Autokredit")
        XCTAssertNil(model.loanStore.loadTask)
    }
}
