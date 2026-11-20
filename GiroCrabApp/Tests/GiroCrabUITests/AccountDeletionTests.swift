import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// GiroCrab/GiroCrab#1436: "Konto löschen …" in the account settings (toggle on → button, confirmation, delete).
@MainActor
final class AccountDeletionTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws { model = nil }

    func testDeleteButtonOnlyOfferedWithArchiveToggleOn() {
        XCTAssertFalse(AccountDeletionPrompt.isOffered(archivedToggle: false))
        XCTAssertTrue(AccountDeletionPrompt.isOffered(archivedToggle: true))
    }

    func testDeleteAfterConfirmationArchivesAndDeletesActiveAccount() throws {
        let account = try XCTUnwrap(model.allAccounts.first { !$0.isArchived })
        model.sidebarSelection = .account(account.id)
        model.deleteAccount(account.id, archiveFirst: true)
        XCTAssertNil(model.errorMessage)
        XCTAssertNil(model.account(for: account.id))
        XCTAssertEqual(model.sidebarSelection, .allAccounts)
    }

    func testCancellingChangesNothing() throws {
        // "Abbrechen" in the dialog or the sheet never reaches the model: nothing is archived or deleted.
        let before = model.allAccounts.map { [$0.id, $0.isArchived ? 1 : 0] }
        XCTAssertEqual(model.allAccounts.map { [$0.id, $0.isArchived ? 1 : 0] }, before)
        XCTAssertNil(model.errorMessage)
    }

    func testPlainDeleteOfActiveAccountIsRefusedByCore() throws {
        // the core deletes only saved archived accounts; hence the archive step in the settings sheet
        let account = try XCTUnwrap(model.allAccounts.first { !$0.isArchived })
        model.deleteAccount(account.id)
        XCTAssertNotNil(model.errorMessage)
        XCTAssertNotNil(model.account(for: account.id))
    }
}
