// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// GiroCrab/GiroCrab#814: with "Archivierte Konten ausblenden", groups, banks and kinds
/// that hold nothing but archived accounts leave the sidebar; entries with at least one
/// active account and groups without any account stay.
@MainActor
final class ArchivedSectionsHiddenTests: XCTestCase {
    private static let defaultsKeys = [
        "sidebar.showArchived", "sidebar.accountView", "sidebar.collapsedUserGroups",
        "sidebar.collapsedKindGroups", "sidebar.collapsedBankGroups",
    ]
    private var model: AppModel!

    override func setUp() async throws {
        Self.clearDefaults()
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertNil(model.errorMessage)
        model.sidebarAccountSearch = ""
    }

    override func tearDown() async throws {
        model = nil
        Self.clearDefaults()
    }

    private static func clearDefaults() {
        for k in defaultsKeys { AppDefaults.store.removeObject(forKey: k) }
    }

    // MARK: helpers

    private func group(_ uuid: String) throws -> UserGroup {
        try XCTUnwrap(model.userGroups.first { $0.uuid == uuid }, "group \(uuid)")
    }

    private func listed(_ uuid: String) throws -> Bool {
        model.isUserGroupListed(try group(uuid))
    }

    private func archive(_ id: Int64) throws {
        let account = try XCTUnwrap(model.allAccounts.first { $0.id == id })
        model.setAccountArchived(account, true)
    }

    /// A new group holding the first `count` active accounts.
    private func makeGroup(_ name: String, accounts count: Int, parent: String? = nil) throws -> (uuid: String, ids: [Int64]) {
        let ids = Array(model.allAccounts.filter { !$0.isArchived }.prefix(count).map(\.id))
        XCTAssertEqual(ids.count, count)
        let uuid = try XCTUnwrap(model.createUserGroup(name: name, parent: parent))
        for id in ids { model.moveAccount(id, to: uuid) }
        return (uuid, ids)
    }

    // MARK: user groups

    func testGroupWithOnlyArchivedAccountsIsHiddenWhileArchivedAreHidden() throws {
        let (uuid, ids) = try makeGroup("Alt", accounts: 1)
        try archive(ids[0])
        model.showArchivedAccounts = true
        XCTAssertTrue(try listed(uuid), "shown while archived accounts are shown")
        model.showArchivedAccounts = false
        XCTAssertFalse(try listed(uuid), "only archived accounts: hidden")
    }

    func testGroupWithAnActiveAccountStaysUnchanged() throws {
        let (uuid, ids) = try makeGroup("Gemischt", accounts: 2)
        try archive(ids[0])
        model.showArchivedAccounts = false
        XCTAssertTrue(try listed(uuid))
    }

    func testGroupWithoutAccountsStaysListed() throws {
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Leer"))
        model.showArchivedAccounts = false
        XCTAssertTrue(try listed(uuid))
    }

    func testParentOfOnlyArchivedSubgroupsIsHiddenWithItsSubgroups() throws {
        let parent = try XCTUnwrap(model.createUserGroup(name: "Eltern"))
        let (child, ids) = try makeGroup("Kind", accounts: 1, parent: parent)
        let emptyChild = try XCTUnwrap(model.createUserGroup(name: "Leeres Kind", parent: parent))
        try archive(ids[0])
        model.showArchivedAccounts = false
        XCTAssertFalse(try listed(parent), "everything below is archived")
        XCTAssertFalse(try listed(child))
        XCTAssertFalse(try listed(emptyChild), "no subgroup stays behind under a hidden parent")
        model.showArchivedAccounts = true
        XCTAssertTrue(try listed(parent))
        XCTAssertTrue(try listed(emptyChild))
    }

    func testParentWithoutOwnAccountsStaysWhileASubgroupHasAnActiveAccount() throws {
        let parent = try XCTUnwrap(model.createUserGroup(name: "Eltern"))
        let (child, _) = try makeGroup("Kind", accounts: 1, parent: parent)
        model.showArchivedAccounts = false
        XCTAssertTrue(try listed(parent))
        XCTAssertTrue(try listed(child))
    }

    func testSearchStillFindsGroupsByName() throws {
        let (uuid, ids) = try makeGroup("Altbestand", accounts: 1)
        try archive(ids[0])
        model.showArchivedAccounts = false
        model.sidebarAccountSearch = "Altbestand"
        XCTAssertTrue(try listed(uuid), "a name hit stays reachable while searching")
    }

    // MARK: banks and kinds

    func testBankWithOnlyArchivedAccountsIsHidden() throws {
        let bank = try XCTUnwrap(model.bankGroups.first { $0.accounts.count >= 1 && !$0.id.isEmpty })
        model.showArchivedAccounts = false
        XCTAssertTrue(model.isSidebarSectionListed(bank.accounts))
        for a in bank.accounts { try archive(a.id) }
        let after = try XCTUnwrap(model.bankGroup(for: bank.id))
        XCTAssertFalse(model.isSidebarSectionListed(after.accounts), "only archived accounts: hidden")
        model.showArchivedAccounts = true
        XCTAssertTrue(model.isSidebarSectionListed(after.accounts))
    }

    func testBankKeepsListedWithOneActiveAccount() throws {
        let bank = try XCTUnwrap(model.bankGroups.first { $0.accounts.count >= 2 })
        try archive(bank.accounts[0].id)
        model.showArchivedAccounts = false
        let after = try XCTUnwrap(model.bankGroup(for: bank.id))
        XCTAssertTrue(model.isSidebarSectionListed(after.accounts))
    }

    func testKindSectionWithOnlyArchivedAccountsIsHidden() throws {
        let kind = try XCTUnwrap(model.groups.first { !$0.accounts.isEmpty })
        for a in kind.accounts { try archive(a.id) }
        let after = try XCTUnwrap(model.groups.first { $0.id == kind.id })
        model.showArchivedAccounts = false
        XCTAssertFalse(model.isSidebarSectionListed(after.accounts))
        model.showArchivedAccounts = true
        XCTAssertTrue(model.isSidebarSectionListed(after.accounts))
    }

    func testSectionWithoutAccountsStaysListed() {
        model.showArchivedAccounts = false
        XCTAssertTrue(model.isSidebarSectionListed([]))
    }

    func testSearchHidesSectionsWithoutHitsAsBefore() throws {
        let bank = try XCTUnwrap(model.bankGroups.first { !$0.accounts.isEmpty })
        model.showArchivedAccounts = true
        model.sidebarAccountSearch = "zzz-kein-treffer-zzz"
        XCTAssertFalse(model.isSidebarSectionListed(bank.accounts))
    }
}
