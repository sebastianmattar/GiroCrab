import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// "Konto bearbeiten": Kontoart, Notiz (#639, #605, #656).
@MainActor
final class AccountEditTests: XCTestCase {
    func testKindChangeMovesAccountBetweenKindGroups() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let account = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        model.setAccountKind(account.id, to: .brokerage)
        let group = try XCTUnwrap(model.groups.first { $0.accounts.contains { $0.id == account.id } })
        XCTAssertTrue(group.accounts.allSatisfy { $0.kind == .brokerage || $0.id != account.id })
        XCTAssertEqual(model.accountsByID[account.id]?.kind, .brokerage)
        XCTAssertFalse(model.groups.filter { $0.name == "Girokonten" }.contains { $0.accounts.contains { $0.id == account.id } })
    }

    func testAccountNoteRoundTrip() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let id = try XCTUnwrap(model.allAccounts.first).id
        model.setAccountNote("  Zweitkonto  ", for: id)
        XCTAssertEqual(model.accountNote(for: id), "Zweitkonto")
    }

    /// Kontoeinstellungen (#980): what a sync changed while the sheet was open is no edit there.
    func testSavingKeepsWhatASyncChangedWhileTheSheetWasOpen() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let original = try XCTUnwrap(model.allAccounts.first)
        // another device renamed the account and wrote a note; the merge reloaded the accounts
        let vault = try XCTUnwrap(model.vault)
        try vault.renameAccount(accountId: original.id, name: "Haushalt")
        try vault.setAccountNote(accountId: original.id, note: "Vom iPad")
        model.reloadAccounts()
        // the sheet still shows what it loaded; only the owner was changed
        model.saveAccountEdits(original, name: original.name, owner: "Anna", kind: original.kind, note: original.note)
        let saved = try XCTUnwrap(model.account(for: original.id))
        XCTAssertEqual(saved.name, "Haushalt")
        XCTAssertEqual(saved.note, "Vom iPad")
        XCTAssertEqual(saved.owner, "Anna")
    }

    func testUntrimmedSnapshotDoesNotOverwriteSyncedFields() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let base = try XCTUnwrap(model.allAccounts.first)
        var original = base
        original.name = "  \(base.name)  "
        original.owner = "  \(base.owner)  "
        original.note = "  Alt\n"

        let vault = try XCTUnwrap(model.vault)
        try vault.renameAccount(accountId: base.id, name: "Neu per Sync")
        try vault.setAccountNote(accountId: base.id, note: "Vom iPhone")
        model.reloadAccounts()

        // The form submits trimmed versions of the untouched fields while changing only owner.
        model.saveAccountEdits(
            original,
            name: original.name.trimmingCharacters(in: .whitespaces),
            owner: "Anna",
            kind: original.kind,
            note: original.note.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let saved = try XCTUnwrap(model.account(for: base.id))
        XCTAssertEqual(saved.name, "Neu per Sync")
        XCTAssertEqual(saved.note, "Vom iPhone")
        XCTAssertEqual(saved.owner, "Anna")
    }
}
