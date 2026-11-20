// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Account groups ("Kontogruppen") on the demo vault, driven through the same
/// AppModel calls the sidebar, menus, drag & drop and the editor sheet use.
/// Platform-independent: runs on macOS and the iOS Simulator.
@MainActor
final class AccountGroupsTests: XCTestCase {
    private static let defaultsKeys = ["sidebar.accountView", "sidebar.collapsedUserGroups", "sidebar.collapsedKindGroups", "sidebar.collapsedBankGroups"]
    private var model: AppModel!

    override func setUp() async throws {
        Self.clearDefaults()
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertTrue(model.isDemo)
        XCTAssertNil(model.errorMessage)
    }

    override func tearDown() async throws {
        model = nil
        Self.clearDefaults()
    }

    private static func clearDefaults() {
        for k in defaultsKeys { AppDefaults.store.removeObject(forKey: k) }
    }

    // MARK: helpers

    private var groups: [UserGroup] { model.namedUserGroups }
    private var ungrouped: UserGroup? { model.userGroups.first { $0.isUngrouped } }

    private func group(_ uuid: String) throws -> UserGroup {
        try XCTUnwrap(model.userGroups.first { $0.uuid == uuid }, "group \(uuid)")
    }

    private func account(_ kind: AccountKind) throws -> Account {
        try XCTUnwrap(model.allAccounts.first { $0.kind == kind }, "demo account of kind \(kind)")
    }

    private func members(_ uuid: String) throws -> [Int64] {
        try group(uuid).accounts.map(\.id)
    }

    /// Every account is listed exactly once in the "Nach Gruppe" view, and
    /// sums match their accounts – whatever was done before.
    private func assertConsistent(file: StaticString = #filePath, line: UInt = #line) {
        let listed = model.userGroups.flatMap(\.accounts).map(\.id)
        XCTAssertEqual(listed.sorted(), model.allAccounts.map(\.id).sorted(), "each account exactly once", file: file, line: line)
        for g in model.userGroups {
            // like the core's sum (#337): effectively included accounts — but an
            // excluded group's header keeps its own sum (review point 1 on #1100)
            let summands = g.inTotal
                ? g.accounts.filter { $0.currency == "EUR" && $0.inTotal }
                : g.accounts.filter { $0.currency == "EUR" && $0.includeInTotal && !$0.isArchived }
            let sum = summands.reduce(Int64(0)) { $0 + $1.balanceCents }
            XCTAssertEqual(g.balanceCents, sum, "sum of \(g.name)", file: file, line: line)
            for a in g.accounts {
                XCTAssertEqual(a.userGroupUuid ?? "", g.uuid, "\(a.name) belongs to \(g.name)", file: file, line: line)
            }
        }
        XCTAssertEqual(model.userGroups.filter(\.isUngrouped).count <= 1, true, file: file, line: line)
        XCTAssertNil(model.errorMessage, file: file, line: line)
    }

    // MARK: demo data

    func testDemoStartsGroupedByGroup() {
        XCTAssertEqual(model.accountView, .byGroup, "groups exist → 'Nach Gruppe' without an explicit choice")
        XCTAssertEqual(groups.count, 3)
        XCTAssertEqual(groups.map(\.sfSymbol), ["person", "chart.pie", "house"])
        XCTAssertNil(ungrouped, "every demo account is in a group")
        XCTAssertEqual(model.allAccounts.count, 14)
        assertConsistent()
    }

    // MARK: create

    func testCreateGroupAndMoveAccountIntoIt() throws {
        let giro = try account(.checking)
        let before = try XCTUnwrap(model.userGroup(of: giro))
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Selbstständig", symbol: "briefcase", moving: giro.id))

        let created = try group(uuid)
        XCTAssertEqual(created.name, "Selbstständig")
        XCTAssertEqual(created.sfSymbol, "briefcase")
        XCTAssertEqual(created.accounts.map(\.id), [giro.id])
        XCTAssertFalse(try members(before.uuid).contains(giro.id), "moved out of its old group")
        XCTAssertEqual(groups.last?.uuid, uuid, "new groups go to the end")
        assertConsistent()
    }

    func testCreateEmptyGroupIsListed() throws {
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Kinder"))
        XCTAssertTrue(try group(uuid).accounts.isEmpty)
        XCTAssertEqual(groups.count, 4, "empty groups stay visible as drop targets")
        assertConsistent()
    }

    func testDuplicateOrEmptyNameIsRejected() throws {
        let existing = try XCTUnwrap(groups.first).name
        XCTAssertNil(model.createUserGroup(name: existing.uppercased()))
        XCTAssertNotNil(model.errorMessage, "duplicate names (ignoring case) show an error")
        model.errorMessage = nil
        XCTAssertNil(model.createUserGroup(name: "   "))
        XCTAssertNotNil(model.errorMessage)
        model.errorMessage = nil
        XCTAssertEqual(groups.count, 3)
    }

    func testRenameToExistingNameAsksToMerge() throws {
        let from = groups[2], into = groups[0], other = groups[1].uuid
        let moved = Set(from.accounts.map(\.id)), kept = into.accounts.map(\.id)
        model.sidebarSelection = .userGroup(from.uuid)
        // a clash changes nothing yet, it asks
        let ask = try XCTUnwrap(model.renameUserGroup(from, to: " \(into.name.uppercased()) "))
        guard case .merge(let f, let i) = ask else { return XCTFail("expected .merge, got \(ask)") }
        XCTAssertEqual([f.uuid, i.uuid], [from.uuid, into.uuid])
        XCTAssertEqual(groups.count, 3)
        XCTAssertNil(model.errorMessage)
        // confirming combines them: accounts appended, selection follows
        model.mergeUserGroups(f, into: i)
        XCTAssertEqual(groups.map(\.uuid), [into.uuid, other])
        let now = try members(into.uuid)
        XCTAssertEqual(Array(now.prefix(kept.count)), kept)
        XCTAssertEqual(Set(now.dropFirst(kept.count)), moved)
        XCTAssertEqual(model.sidebarSelection, .userGroup(into.uuid))
        // a rename without a clash just renames; saving the same name works
        XCTAssertNil(model.renameUserGroup(try group(into.uuid), to: "Familie"))
        model.updateUserGroup(try group(into.uuid), symbol: "house")
        XCTAssertEqual(try group(into.uuid).name, "Familie")
        XCTAssertEqual(try group(into.uuid).sfSymbol, "house")
        assertConsistent()
    }

    func testSelectionOfAGroupGoneViaSyncFallsBack() throws {
        let g = groups[1]
        model.sidebarSelection = .userGroup(g.uuid)
        // removed behind the model's back, as a sync would
        try model.vault?.deleteUserGroup(uuid: g.uuid)
        model.reloadAccounts()
        XCTAssertEqual(model.sidebarSelection, .allAccounts)
    }

    func testCreatingFirstGroupSwitchesToGroupView() throws {
        model.accountView = .byKind
        XCTAssertEqual(model.accountView, .byKind)
        model.createUserGroup(name: "Neu")
        XCTAssertEqual(model.accountView, .byGroup)
    }

    // MARK: move & reorder

    func testMoveBetweenGroupsAndToUngrouped() throws {
        let visa = try account(.creditCard)
        let target = try XCTUnwrap(groups.first { $0.uuid != visa.userGroupUuid })

        model.moveAccount(visa.id, to: target.uuid)
        XCTAssertEqual(try members(target.uuid).last, visa.id, "appended at the end")

        model.moveAccount(visa.id, to: nil)
        XCTAssertEqual(ungrouped?.accounts.map(\.id), [visa.id], "'Ohne Gruppe' appears")
        XCTAssertNil(model.allAccounts.first { $0.id == visa.id }?.userGroupUuid)

        model.moveAccount(visa.id, to: "")   // the "Ohne Gruppe" entry's uuid
        XCTAssertEqual(ungrouped?.accounts.map(\.id), [visa.id])

        model.moveAccount(visa.id, to: target.uuid)
        XCTAssertNil(ungrouped, "'Ohne Gruppe' disappears when empty")
        assertConsistent()
    }

    func testDropOnAccountRowInsertsAtThatPosition() throws {
        // drag & drop: dropping on row i calls moveAccount(_, to: group, at: i)
        let g = try XCTUnwrap(groups.first { $0.accounts.count >= 2 })
        let outsider = try XCTUnwrap(model.allAccounts.first { $0.userGroupUuid != g.uuid })
        model.moveAccount(outsider.id, to: g.uuid, at: 0)
        XCTAssertEqual(try members(g.uuid).first, outsider.id)
        model.moveAccount(outsider.id, to: g.uuid, at: 1)
        XCTAssertEqual(try members(g.uuid)[1], outsider.id, "reorder inside the same group")
        model.moveAccount(outsider.id, to: g.uuid, at: 99)
        XCTAssertEqual(try members(g.uuid).last, outsider.id, "index past the end clamps")
        assertConsistent()
    }

    func testDropLinePositionBecomesMoveIndex() {
        // from another group: above row 2 → 2, below row 2 → 3
        XCTAssertEqual(GroupDrag.insertionIndex(row: 2, below: false, from: nil), 2)
        XCTAssertEqual(GroupDrag.insertionIndex(row: 2, below: true, from: nil), 3)
        // same group, dragged down: the moved account leaves a gap above
        XCTAssertEqual(GroupDrag.insertionIndex(row: 2, below: false, from: 0), 1)
        XCTAssertEqual(GroupDrag.insertionIndex(row: 2, below: true, from: 0), 2)
        // same group, dragged up: nothing shifts
        XCTAssertEqual(GroupDrag.insertionIndex(row: 0, below: false, from: 2), 0)
        XCTAssertEqual(GroupDrag.insertionIndex(row: 0, below: true, from: 2), 1)
    }

    func testDropBelowNextRowMovesAccountDownOne() throws {
        let g = try XCTUnwrap(groups.first { $0.accounts.count >= 2 })
        let ids = g.accounts.map(\.id)
        // drag the first account onto the lower half of the second
        model.moveAccount(ids[0], to: g.uuid, at: GroupDrag.insertionIndex(row: 1, below: true, from: 0))
        XCTAssertEqual(try Array(members(g.uuid).prefix(2)), [ids[1], ids[0]])
        assertConsistent()
    }

    func testDropGroupIntoItselfOrItsSubgroupIsRefused() throws {
        let a = try XCTUnwrap(model.createUserGroup(name: "A (drop)"))
        let sub = try XCTUnwrap(model.createUserGroup(name: "Sub (drop)", parent: a))
        XCTAssertFalse(model.canDropGroup(a, on: try group(a), into: true))
        XCTAssertFalse(model.canDropGroup(a, on: try group(sub), into: true))
        XCTAssertFalse(model.canDropGroup(a, on: try group(sub), into: false))
        XCTAssertTrue(model.canDropGroup(sub, on: try group(a), into: false))
    }

    func testAddableAccountsExcludeMembersUngroupedFirst() throws {
        let g = try XCTUnwrap(groups.first { !$0.accounts.isEmpty })
        let outsider = try XCTUnwrap(model.allAccounts.first { $0.userGroupUuid != g.uuid })
        model.moveAccount(outsider.id, to: nil)
        let sections = model.addableAccounts(for: g.uuid)
        XCTAssertEqual(sections.first?.id, "", "Ohne Gruppe first")
        XCTAssertTrue(sections.first?.accounts.contains { $0.id == outsider.id } ?? false)
        XCTAssertFalse(sections.flatMap(\.accounts).contains { $0.userGroupUuid == g.uuid })
        XCTAssertFalse(sections.contains { $0.id == g.uuid })
        XCTAssertFalse(sections.contains { $0.accounts.isEmpty })
    }

    func testAddableAccountsListsSubgroupMembersUnderTheirGroup() throws {
        let parent = try XCTUnwrap(model.createUserGroup(name: "Eltern (229)"))
        let sub = try XCTUnwrap(model.createUserGroup(name: "Kind (229)", parent: parent))
        let a = try XCTUnwrap(model.allAccounts.first)
        model.moveAccount(a.id, to: sub)
        let section = try XCTUnwrap(model.addableAccounts(for: parent).first { $0.id == sub })
        XCTAssertEqual(section.title, "Kind (229)")
        XCTAssertEqual(section.accounts.map(\.id), [a.id])
    }

    func testAddableAccountsEmptyWhenAllMembers() throws {
        let g = try XCTUnwrap(model.createUserGroup(name: "Alle (229)"))
        for a in model.allAccounts { model.moveAccount(a.id, to: g) }
        XCTAssertTrue(model.addableAccounts(for: g).isEmpty)
        assertConsistent()
    }

    func testMoveAccountsInGroupByOffsets() throws {
        let g = try XCTUnwrap(groups.first { $0.accounts.count >= 2 })
        let ids = g.accounts.map(\.id)
        model.moveAccounts(in: g.uuid, from: IndexSet(integer: 0), to: ids.count)
        XCTAssertEqual(try members(g.uuid), Array(ids.dropFirst()) + [ids[0]])
        assertConsistent()
    }

    func testAccountDetailTellsSameNamedAccountsApart() throws {
        var a = try XCTUnwrap(model.allAccounts.first)
        a.iban = "DE12500105170648489890"
        a.accountNumber = "648489890"
        XCTAssertEqual(accountDetail(a), "DE12 5001 0517 0648 4898 90", "IBAN first, grouped")
        a.iban = ""
        XCTAssertEqual(accountDetail(a), String(localized: "Konto \("648489890")"), "no IBAN: account number")
        a.accountNumber = ""
        XCTAssertEqual(accountDetail(a), a.bankName, "neither: the bank")
    }

    func testAccountDetailMarksArchivedAccounts() throws {
        var a = try XCTUnwrap(model.allAccounts.first { !$0.isArchived })
        a.iban = "DE12500105170648489890"
        a.archivedUnix = 1_700_000_000
        XCTAssertEqual(accountDetail(a), String(localized: "Archiviert · \("DE12 5001 0517 0648 4898 90")"))
    }

    func testUngroupedForSettingsExistsEvenWhenEmpty() throws {
        let a = try XCTUnwrap(model.allAccounts.first)
        model.moveAccount(a.id, to: nil)
        XCTAssertTrue(model.ungroupedForSettings.isUngrouped)
        XCTAssertTrue(model.ungroupedForSettings.accounts.contains { $0.id == a.id })
        // every account in a group: "Ohne Gruppe" stays listable, just empty
        let g = try XCTUnwrap(model.createUserGroup(name: "Alle (229)"))
        for b in model.allAccounts { model.moveAccount(b.id, to: g) }
        XCTAssertNil(model.userGroups.first { $0.isUngrouped })
        XCTAssertTrue(model.ungroupedForSettings.isUngrouped)
        XCTAssertEqual(model.ungroupedForSettings.uuid, "")
        XCTAssertTrue(model.ungroupedForSettings.accounts.isEmpty)
        // adding to "Ohne Gruppe" from its settings = taking the account out of its group
        let section = try XCTUnwrap(model.addableAccounts(for: "").first)
        XCTAssertEqual(section.id, g)
    }

    func testAddableAccountsLeaveOutArchived() throws {
        let g = try XCTUnwrap(model.createUserGroup(name: "Ziel (229)"))
        let a = try XCTUnwrap(model.allAccounts.first { !$0.isArchived })
        model.setAccountArchived(a, true)
        XCTAssertTrue(try XCTUnwrap(model.allAccounts.first { $0.id == a.id }).isArchived)
        XCTAssertFalse(model.addableAccounts(for: g).flatMap(\.accounts).contains { $0.id == a.id })
    }

    func testReorderAccountsInGroup() throws {
        let g = try XCTUnwrap(groups.first { $0.accounts.count >= 2 })
        let reversed = Array(g.accounts.map(\.id).reversed())
        model.reorderAccounts(in: g.uuid, ids: reversed)
        XCTAssertEqual(try members(g.uuid), reversed)
        assertConsistent()
    }

    func testReorderGroupsLikeTheEditorAndDragAndDrop() throws {
        let order = groups.map(\.uuid)
        // editor sheet: List.onMove – last group to the top
        model.moveUserGroups(from: IndexSet(integer: order.count - 1), to: 0)
        XCTAssertEqual(groups.map(\.uuid), [order[2], order[0], order[1]])
        // explicit order (sidebar header drop)
        model.reorderUserGroups(order)
        XCTAssertEqual(groups.map(\.uuid), order)
        assertConsistent()
    }

    /// Sidebar drag & drop of a group: into another group, before one, and
    /// back to the top level – never into its own subtree.
    func testDropGroupIntoBeforeAndOut() throws {
        let (a, b, c) = (groups[0].uuid, groups[1].uuid, groups[2].uuid)
        let g = { (u: String) in self.model.userGroups.first { $0.uuid == u }! }

        // middle of a row: B becomes a subgroup of A
        XCTAssertTrue(model.dropUserGroup(b, onto: g(a), before: false))
        XCTAssertEqual(g(b).parentUuid, a)
        XCTAssertEqual(g(b).depth, 1)

        // A into its own subgroup B: refused, nothing changes
        XCTAssertFalse(model.dropUserGroup(a, onto: g(b), before: false))
        XCTAssertNil(g(a).parentUuid)

        // another main group: B moves from A into C
        XCTAssertTrue(model.dropUserGroup(b, onto: g(c), before: false))
        XCTAssertEqual(g(b).parentUuid, c)

        // top edge of A: B leaves C and sits right above A, at the top level
        XCTAssertTrue(model.dropUserGroup(b, onto: g(a), before: true))
        XCTAssertNil(g(b).parentUuid)
        XCTAssertEqual(groups.map(\.uuid).prefix(2), [b, a])

        // onto "Ohne Gruppe": top level, at the end
        XCTAssertTrue(model.dropUserGroup(c, onto: g(a), before: false))
        let ungrouped = UserGroup(uuid: "", name: "", sfSymbol: "", colorHex: "", sortOrder: 0, includeInTotal: true,
                                  inTotal: true, isUngrouped: true, parentUuid: nil, depth: 0,
                                  balanceCents: 0, accounts: [])
        XCTAssertTrue(model.dropUserGroup(c, onto: ungrouped, before: false))
        XCTAssertNil(g(c).parentUuid)
        XCTAssertEqual(groups.last?.uuid, c)
        assertConsistent()
    }

    // MARK: edit

    func testRenameAndChangeSymbol() throws {
        let g = try XCTUnwrap(groups.first)
        model.updateUserGroup(g, name: "Familie Mustermann")
        model.updateUserGroup(try group(g.uuid), symbol: "figure.and.child.holdinghands")
        let now = try group(g.uuid)
        XCTAssertEqual(now.name, "Familie Mustermann")
        XCTAssertEqual(now.sfSymbol, "figure.and.child.holdinghands")
        XCTAssertEqual(now.accounts.map(\.id), g.accounts.map(\.id), "members unchanged")
        // renaming to another group's name is refused
        model.updateUserGroup(now, name: groups[1].name)
        XCTAssertNotNil(model.errorMessage)
        model.errorMessage = nil
        XCTAssertEqual(try group(g.uuid).name, "Familie Mustermann")
    }

    func testExcludedGroupLeavesAvailableTotal() throws {
        // the group holding the Tagesgeld (a liquid account) in the demo
        let tagesgeld = try account(.savings)
        let g = try XCTUnwrap(model.userGroup(of: tagesgeld))
        let liquidBefore = model.liquidBalance
        let excludedLiquid = g.accounts
            .filter { [.checking, .savings, .paymentService, .creditCard, .cash].contains($0.kind) }
            .reduce(Int64(0)) { $0 + $1.balanceCents }
        XCTAssertNotEqual(excludedLiquid, 0)
        let ownSumBefore = g.balanceCents

        model.updateUserGroup(g, includeInTotal: false)
        XCTAssertFalse(try group(g.uuid).includeInTotal)
        XCTAssertTrue(try group(g.uuid).accounts.allSatisfy { !$0.inTotal })
        XCTAssertEqual(model.liquidBalance, liquidBefore - excludedLiquid, "'Verfügbar' drops by the group's liquid accounts")
        XCTAssertEqual(try group(g.uuid).balanceCents, ownSumBefore, "an excluded group keeps showing its own sum (review point 1 on #1100)")

        model.updateUserGroup(try group(g.uuid), includeInTotal: true)
        XCTAssertEqual(model.liquidBalance, liquidBefore)
        XCTAssertEqual(try group(g.uuid).balanceCents, ownSumBefore)
    }

    /// GiroCrab/GiroCrab#208: an "everything" group with an "old" subgroup. Archived
    /// accounts and subgroups taken out of the total don't count in the group sum,
    /// and the balance chart of the group follows the same accounts.
    func testGroupSumSkipsArchivedAccountsAndExcludedSubgroups() throws {
        let eur = model.allAccounts.filter { $0.currency == "EUR" && $0.balanceCents != 0 }
        XCTAssertGreaterThanOrEqual(eur.count, 3)
        let (kept, archived, old) = (eur[0], eur[1], eur[2])
        let alle = try XCTUnwrap(model.createUserGroup(name: "Alle (208)"))
        let alt = try XCTUnwrap(model.createUserGroup(name: "Alt (208)", parent: alle))
        model.moveAccount(kept.id, to: alle)
        model.moveAccount(archived.id, to: alle)
        model.moveAccount(old.id, to: alt)
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(try group(alle).balanceCents, kept.balanceCents + archived.balanceCents + old.balanceCents)

        model.setAccountArchived(archived, true)
        model.updateUserGroup(try group(alt), includeInTotal: false)
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(try group(alle).balanceCents, kept.balanceCents, "archived account and excluded subgroup don't count")
        XCTAssertEqual(try group(alt).balanceCents, old.balanceCents, "an excluded subgroup keeps showing its own sum (review point 1 on #1100)")
        XCTAssertEqual(Set(try group(alle).accounts.map(\.id)), [kept.id, archived.id], "archived account stays listed")
        XCTAssertEqual(model.userGroupBalanceAccountIDs(alle), [kept.id], "chart follows the sum")
        XCTAssertEqual(model.userGroupBalanceAccountIDs(alt), [old.id], "the excluded group's chart follows its own sum (review point 1 on #1100)")
        XCTAssertEqual(Set(model.userGroupAccountIDs(alle)), [kept.id, archived.id, old.id], "bookings still cover everything")

        model.updateUserGroup(try group(alt), includeInTotal: true)
        XCTAssertEqual(try group(alle).balanceCents, kept.balanceCents + old.balanceCents)
        XCTAssertEqual(Set(model.userGroupBalanceAccountIDs(alle)), [kept.id, old.id])
    }

    // MARK: delete

    func testDeleteGroupKeepsAccounts() throws {
        let g = try XCTUnwrap(groups.first)
        let ids = g.accounts.map(\.id)
        model.sidebarSelection = .userGroup(g.uuid)

        model.deleteUserGroup(g)
        XCTAssertNil(model.userGroups.first { $0.uuid == g.uuid })
        XCTAssertEqual(Set(ungrouped?.accounts.map(\.id) ?? []), Set(ids), "accounts go to 'Ohne Gruppe'")
        XCTAssertEqual(model.allAccounts.count, 14, "no account deleted")
        XCTAssertEqual(model.sidebarSelection, .allAccounts, "selection leaves the deleted group")
        XCTAssertNotNil(model.toast)
        assertConsistent()
    }

    func testDeleteAllGroupsThenGroupAgain() throws {
        for g in groups { model.deleteUserGroup(g) }
        XCTAssertTrue(groups.isEmpty)
        XCTAssertEqual(ungrouped?.accounts.count, 14)
        XCTAssertFalse(model.hasUserGroups)
        XCTAssertEqual(model.accountView, .byKind, "no groups left → 'Nach Kontoart' by default")
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Alles", moving: try account(.checking).id))
        XCTAssertEqual(try members(uuid).count, 1)
        assertConsistent()
    }

    // MARK: selection, header, bookings

    func testSelectingGroupShowsItsAccounts() throws {
        let g = try XCTUnwrap(groups.first { $0.accounts.count >= 2 })
        model.sidebarSelection = .userGroup(g.uuid)
        XCTAssertEqual(model.title, g.name)
        XCTAssertEqual(model.headerBalance?.cents, g.balanceCents)
        XCTAssertEqual(Set(model.currentFilter.accountIds), Set(g.accounts.map(\.id)))
        XCTAssertFalse(model.bookings.isEmpty)
        XCTAssertTrue(model.bookings.allSatisfy { b in g.accounts.contains { $0.id == b.accountId } }, "only the group's bookings")

        // "Ohne Gruppe" (empty uuid) is selectable as well
        let visa = try account(.creditCard)
        model.moveAccount(visa.id, to: nil)
        model.sidebarSelection = .userGroup("")
        XCTAssertEqual(model.currentFilter.accountIds, [visa.id])
        XCTAssertTrue(model.bookings.allSatisfy { $0.accountId == visa.id })
    }

    // MARK: view switch & per-device state

    func testViewChoiceAndCollapseStatePersist() throws {
        let g = try XCTUnwrap(groups.first)
        model.toggleCollapsed(g)
        XCTAssertTrue(model.collapsedUserGroups.contains(g.uuid))
        model.accountView = .byKind

        // a new model (next app launch) restores both
        let next = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(next.accountView, .byKind)
        XCTAssertEqual(next.accountViewChoice, .byKind)
        // the demo vault is recreated with new uuids, so compare the persisted set
        XCTAssertEqual(AppDefaults.store.stringArray(forKey: "sidebar.collapsedUserGroups"), [g.uuid])

        model.toggleCollapsed(g)
        XCTAssertFalse(model.collapsedUserGroups.contains(g.uuid))
    }

    func testKindViewIsUnaffectedByUserGroups() throws {
        let kindBefore = model.groups.map { ($0.name, $0.accounts.map(\.id)) }
        let visa = try account(.creditCard)
        model.createUserGroup(name: "Karten", moving: visa.id)
        model.deleteUserGroup(try XCTUnwrap(groups.first))
        XCTAssertEqual(model.groups.map(\.name), kindBefore.map(\.0))
        XCTAssertEqual(model.groups.map { $0.accounts.map(\.id) }, kindBefore.map(\.1), "'Nach Kontoart' never changes")
    }

    // MARK: Gruppen bearbeiten (Sheet)

    func testEditGroupsOpensGroupManagerSheet() {
        XCTAssertFalse(model.showGroupManager)
        model.openGroupManager()
        XCTAssertTrue(model.showGroupManager)
        XCTAssertEqual(model.settingsTab, .general, "no Settings tab involved any more")
        #if os(macOS)
        XCTAssertFalse(model.settingsRequested)
        #else
        XCTAssertFalse(model.showSettings)
        #endif
    }

    /// GitHub #33: the merchant-logo tip's "Einstellungen …" opens "Allgemein" (section Händler-Logos).
    func testOpenSettingsOnLogoPane() {
        model.openSettings(.general)
        XCTAssertEqual(model.settingsTab, .general)
        #if os(macOS)
        XCTAssertTrue(model.settingsRequested)
        #else
        XCTAssertTrue(model.showSettings)
        #endif
    }

    func testAccountCountAndDeleteTexts() throws {
        XCTAssertEqual(accountCountText(0), String(localized: "Keine Konten"))
        XCTAssertEqual(accountCountText(1), String(localized: "1 Konto"))
        XCTAssertEqual(accountCountText(3), String(localized: "3 Konten"))
        let single = try XCTUnwrap(groups.first { $0.accounts.count == 1 })
        XCTAssertFalse(deleteMessage(single).contains("1 Konten"))
    }

    // MARK: drag payloads

    func testDragPayloadsRoundTrip() {
        XCTAssertEqual(GroupDrag.accountID(GroupDrag.account(42)), 42)
        XCTAssertEqual(GroupDrag.groupUUID(GroupDrag.group("abc-123")), "abc-123")
        XCTAssertNil(GroupDrag.accountID(GroupDrag.group("1")))
        XCTAssertNil(GroupDrag.groupUUID(GroupDrag.account(1)))
        XCTAssertNil(GroupDrag.accountID("hello"))
    }

    // MARK: a longer session

    func testRandomisedSessionStaysConsistent() throws {
        var rng = SeededRandom(seed: 2026)
        var created = 0
        for step in 0..<60 {
            let accounts = model.allAccounts
            switch rng.next() % 6 {
            case 0 where groups.count < 6:
                created += 1
                let symbols = SymbolCatalog.sections.flatMap { $0.items.map(\.symbol) }
                model.createUserGroup(name: "G\(created)", symbol: symbols[Int(rng.next() % 12)])
            case 1 where !groups.isEmpty:
                let a = accounts[Int(rng.next() % UInt64(accounts.count))]
                let g = groups[Int(rng.next() % UInt64(groups.count))]
                model.moveAccount(a.id, to: g.uuid, at: Int(rng.next() % 4))
            case 2:
                model.moveAccount(accounts[Int(rng.next() % UInt64(accounts.count))].id, to: nil)
            case 3 where groups.count > 1:
                model.moveUserGroups(from: IndexSet(integer: Int(rng.next() % UInt64(groups.count))), to: 0)
            case 4 where groups.count > 2:
                model.deleteUserGroup(groups[Int(rng.next() % UInt64(groups.count))])
            case 5 where !groups.isEmpty:
                let g = groups[Int(rng.next() % UInt64(groups.count))]
                model.updateUserGroup(g, includeInTotal: !g.includeInTotal)
            default:
                break
            }
            assertConsistent()
            if model.errorMessage != nil { XCTFail("step \(step): \(model.errorMessage!)"); return }
        }
        XCTAssertEqual(model.allAccounts.count, 14)
        let excluded = model.allAccounts.filter { !$0.inTotal }.map(\.id)
        XCTAssertTrue(model.liquidAccounts.allSatisfy { !excluded.contains($0.id) })
    }

    // MARK: sidebar search

    func testSidebarSearchFindsOwnerAndSpacedIban() throws {
        let giro = try account(.checking)
        XCTAssertEqual(giro.owner, "Erika Mustermann")
        XCTAssertFalse(giro.iban.isEmpty)

        model.sidebarAccountSearch = "erika"
        XCTAssertTrue(model.isSidebarAccountSearching)
        let byOwner = model.allAccounts.filter(model.isSidebarAccountListed)
        XCTAssertTrue(byOwner.contains(where: { $0.id == giro.id }))
        XCTAssertGreaterThanOrEqual(byOwner.count, 5, "bank accounts share the demo owner")
        XCTAssertFalse(
            byOwner.contains(where: { $0.name == "Autokredit" }),
            "manual loan has no Kontoinhaber"
        )

        model.sidebarAccountSearch = "DE89 3704 0044 0532 0130 00"
        let byIban = model.allAccounts.filter(model.isSidebarAccountListed)
        XCTAssertEqual(byIban.map(\.id), [giro.id], "spaced IBAN finds the stored value")

        model.sidebarAccountSearch = "0532 013000"
        XCTAssertEqual(
            model.allAccounts.filter(model.isSidebarAccountListed).map(\.id),
            [giro.id],
            "account number with spaces"
        )

        model.sidebarAccountSearch = "Girokonto"
        XCTAssertEqual(model.allAccounts.filter(model.isSidebarAccountListed).map(\.id), [giro.id])

        model.sidebarAccountSearch = "xyz-gibt-es-nicht"
        XCTAssertTrue(model.allAccounts.filter(model.isSidebarAccountListed).isEmpty)
        XCTAssertTrue(model.userGroups.filter(model.isUserGroupListed).isEmpty)

        model.sidebarAccountSearch = ""
        XCTAssertFalse(model.isSidebarAccountSearching)
        XCTAssertEqual(model.allAccounts.filter(model.isSidebarAccountListed).count, model.allAccounts.count)
    }

    func testSidebarSearchOpensCollapsedGroupsWithHits() throws {
        let giro = try account(.checking)
        let parent = try XCTUnwrap(model.userGroup(of: giro))
        model.collapsedUserGroups.insert(parent.uuid)
        XCTAssertFalse(model.isUserGroupOpen(parent))

        model.sidebarAccountSearch = giro.iban
        XCTAssertTrue(model.isUserGroupListed(parent))
        XCTAssertTrue(model.isUserGroupOpen(parent), "search expands groups with hits")
    }

    // MARK: by bank

    func testBankViewGroupsDemoAccounts() throws {
        model.accountView = .byBank
        XCTAssertEqual(model.accountView, .byBank)
        let banks = model.bankGroups
        XCTAssertEqual(banks.map(\.name), ["Amazon", "Musterbank", "PayPal", "Ohne Bank"])
        let muster = try XCTUnwrap(banks.first { $0.id == "Musterbank" })
        let orphan = try XCTUnwrap(banks.first { $0.id.isEmpty })
        XCTAssertEqual(banks.reduce(0) { $0 + $1.accounts.count }, model.allAccounts.count)
        XCTAssertTrue(orphan.accounts.contains(where: { $0.name == "Autokredit" }), "manual loan has no bank name")

        let giro = try account(.checking)
        model.sidebarSelection = .bank(muster.id) // synchronous: the title follows the loaded data (#1396)
        XCTAssertEqual(model.sidebarSelection, .bank("Musterbank"))
        XCTAssertEqual(Set(model.selectionAccountIDs), Set(muster.accounts.map(\.id)))
        XCTAssertEqual(model.title, "Musterbank")
        XCTAssertEqual(model.bankGroupKey(for: giro), "Musterbank")

        model.toggleCollapsedBank("Musterbank")
        XCTAssertFalse(model.isBankGroupOpen("Musterbank"))
        model.sidebarAccountSearch = "Girokonto"
        XCTAssertTrue(model.isBankGroupOpen("Musterbank"), "search expands bank sections")
    }
}

/// Deterministic random numbers (SplitMix64), so a failure is reproducible.
struct SeededRandom {
    private var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}
