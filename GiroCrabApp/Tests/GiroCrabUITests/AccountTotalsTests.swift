import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// #337 „Doppelzählung vermeiden": overall money contexts sum the effective
/// total set (account switch, group, archiving) while proof views — single
/// account, category, documents — keep their sums and complete lists.
@MainActor
final class AccountTotalsTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    // ---------------------------------------------------------- calculation context

    func testTotalsContextOfSelections() {
        XCTAssertTrue(SidebarItem.allAccounts.usesTotalsContext)
        XCTAssertTrue(SidebarItem.group(1).usesTotalsContext)
        XCTAssertTrue(SidebarItem.bank("Bank").usesTotalsContext)
        XCTAssertTrue(SidebarItem.userGroup("u").usesTotalsContext)
        let none: SidebarItem? = nil
        XCTAssertTrue(none?.usesTotalsContext ?? true, "no selection = overview")
        XCTAssertFalse(SidebarItem.account(1).usesTotalsContext)
        XCTAssertFalse(SidebarItem.category(1).usesTotalsContext)
        XCTAssertFalse(SidebarItem.uncategorized.usesTotalsContext)
        XCTAssertFalse(SidebarItem.documents.usesTotalsContext)
        XCTAssertFalse(SidebarItem.newBookings.usesTotalsContext)
        XCTAssertFalse(SidebarItem.planned.usesTotalsContext)
        XCTAssertFalse(SidebarItem.directDebits.usesTotalsContext)
        XCTAssertFalse(SidebarItem.wealth.usesTotalsContext)
    }

    func testBookingFilterDefaultsToProofContext() {
        let f = BookingFilter(accountIds: [], categoryId: nil, includeSubcategories: true,
                              uncategorizedOnly: false, search: nil, dateFrom: nil, dateTo: nil,
                              limit: 0, offset: 0, scope: .all, merchantKey: nil, direction: .all,
                              checked: nil, wholeDays: false)
        XCTAssertFalse(f.respectInTotal, "existing callers keep their proof sums")
    }

    func testCurrentFilterFollowsTheSelectionContext() throws {
        let group = try XCTUnwrap(model.groups.first)
        let account = try XCTUnwrap(model.allAccounts.first)
        model.sidebarSelection = .allAccounts
        XCTAssertTrue(model.currentFilter.respectInTotal)
        model.sidebarSelection = .group(group.id)
        XCTAssertTrue(model.currentFilter.respectInTotal, "kind group is a totals context")
        model.sidebarSelection = .account(account.id)
        XCTAssertFalse(model.currentFilter.respectInTotal, "single account is a proof view")
        model.sidebarSelection = .category(1)
        XCTAssertFalse(model.currentFilter.respectInTotal)
        model.sidebarSelection = .newBookings
        XCTAssertFalse(model.currentFilter.respectInTotal)
    }

    // ---------------------------------------------------------- stock figures

    private func account(_ base: Account, cents: Int64, currency: String = "EUR",
                         includeInTotal: Bool = true, inTotal: Bool = true) -> Account {
        var a = base
        a.balanceCents = cents
        a.currency = currency
        a.includeInTotal = includeInTotal
        a.inTotal = inTotal
        return a
    }

    func testAllAccountsTotalAndWealthSkipSwitchedOffAccounts() throws {
        let base = try XCTUnwrap(model.allAccounts.first)
        let on = account(base, cents: 100_00)
        let off = account(base, cents: 999_00, includeInTotal: false, inTotal: false)
        XCTAssertEqual(AllAccountsTotal.totals(of: [on, off]).map(\.cents), [100_00])

        let wealth = WealthSummary(accounts: [on, off])
        XCTAssertEqual(wealth.cash.count + wealth.depots.count + wealth.liabilities.count, 1,
                       "the switched-off account lands in no bucket")
        XCTAssertEqual(WealthSummary(accounts: [on, off]).grossCents,
                       WealthSummary(accounts: [on]).grossCents)
    }

    // ---------------------------------------------------------- effective flag through the vault

    /// The own switch travels through the FFI, moves the overview figures and
    /// leaves the single-account proof view untouched.
    func testAccountSwitchMovesOverviewFiguresOnly() throws {
        let target = try XCTUnwrap(
            model.allAccounts.first { [.checking, .savings, .paymentService, .creditCard, .cash].contains($0.kind) && $0.inTotal }
        )
        let liquidBefore = model.liquidAccounts.map(\.id)
        XCTAssertTrue(liquidBefore.contains(target.id))

        // the app's own way (context menu): full refresh incl. the totals signal
        let versionBefore = model.totalsVersion
        model.setAccountIncludeInTotal(target, false)
        let reloaded = try XCTUnwrap(model.allAccounts.first { $0.id == target.id })
        XCTAssertFalse(reloaded.includeInTotal, "own value stored (FFI round trip)")
        XCTAssertFalse(reloaded.inTotal, "effective value follows")
        XCTAssertFalse(model.liquidAccounts.map(\.id).contains(target.id),
                       "„Verfügbar“ and the overview history skip it")
        XCTAssertGreaterThan(model.totalsVersion, versionBefore,
                             "the cashflow card reloads via the totals signal")

        // proof view of the same account keeps its sums (respectInTotal false)
        model.sidebarSelection = .account(target.id)
        XCTAssertFalse(model.currentFilter.respectInTotal)

        // … and switching it back restores the overview figure
        model.setAccountIncludeInTotal(reloaded, true)
        XCTAssertTrue(model.liquidAccounts.map(\.id).contains(target.id))
        XCTAssertGreaterThan(model.totalsVersion, versionBefore + 1)
    }

    /// The exclusion reason names the group that wins over the account's own
    /// switch; archiving is its own reason; a plain switched-off account has none.
    func testExclusionReasonNamesWinningGroupOrArchiving() throws {
        let vault = try XCTUnwrap(model.vault)
        let base = try XCTUnwrap(model.allAccounts.first { $0.kind == .checking })
        let uuid = try XCTUnwrap(model.createUserGroup(name: "Test-Gruppe #337"))
        model.moveAccount(base.id, to: uuid)
        model.reloadAccounts()
        model.updateUserGroup(
            model.userGroups.first { $0.uuid == uuid }!, includeInTotal: false)
        model.reloadAccounts()

        let placed = try XCTUnwrap(model.allAccounts.first { $0.id == base.id })
        XCTAssertTrue(placed.includeInTotal, "own switch still on (default)")
        XCTAssertFalse(placed.inTotal, "group wins")
        let reason = try XCTUnwrap(model.totalExclusionReason(placed))
        XCTAssertTrue(reason.contains("Test-Gruppe #337"), "names the winning group: \(reason)")

        // archived is its own reason, even with the own switch on
        try vault.setAccountArchived(accountId: base.id, archived: true)
        model.reloadAccounts()
        let archived = try XCTUnwrap(model.allAccounts.first { $0.id == base.id })
        let archivedReason = try XCTUnwrap(model.totalExclusionReason(archived))
        XCTAssertTrue(archivedReason.contains("archiviert"))

        // a plain switched-off account carries no explanation (the toggle shows
        // it) — with the group back in and nothing else in the way
        try vault.setAccountArchived(accountId: base.id, archived: false)
        model.updateUserGroup(model.userGroups.first { $0.uuid == uuid }!, includeInTotal: true)
        model.reloadAccounts()
        let plain = try XCTUnwrap(model.allAccounts.first { $0.id == base.id })
        XCTAssertNil(model.totalExclusionReason(plain))
    }
}
