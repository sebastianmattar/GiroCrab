// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Account markers in the booking list (GiroCrab/GiroCrab#26) on our data
/// model: stored colours, when markers show, short labels.
@MainActor
final class AccountMarkerTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        for k in ["sidebar.accountView", "sidebar.collapsedUserGroups", "sidebar.collapsedKindGroups"] {
            AppDefaults.store.removeObject(forKey: k)
        }
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws { model = nil }

    private func account(_ kind: AccountKind) throws -> Account {
        try XCTUnwrap(model.allAccounts.first { $0.kind == kind })
    }

    func testEveryAccountHasAPaletteColourAndLookup() {
        let palette = Set(accountColorPalette())
        XCTAssertEqual(palette.count, 12)
        for a in model.allAccounts {
            XCTAssertTrue(palette.contains(a.colorHex), "\(a.name): \(a.colorHex)")
            XCTAssertEqual(model.account(for: a.id)?.id, a.id)
            XCTAssertFalse(model.accountShortLabel(a.id).isEmpty)
        }
        XCTAssertEqual(model.accountMarkers.count, model.allAccounts.count)
        XCTAssertNil(model.account(for: -1))
        XCTAssertEqual(model.accountShortLabel(-1), "")
    }

    func testChosenColourAndReset() throws {
        let giro = try account(.checking)
        let initial = giro.colorHex
        let other = try XCTUnwrap(accountColorPalette().first { $0 != initial })
        model.setAccountColor(giro.id, hex: other)
        XCTAssertEqual(model.account(for: giro.id)?.colorHex, other)
        model.setAccountColor(giro.id, hex: nil)
        XCTAssertEqual(model.account(for: giro.id)?.colorHex, initial, "back to the default")
        model.setAccountColor(giro.id, hex: "blue")
        XCTAssertNotNil(model.errorMessage, "invalid colours are refused")
    }

    func testColourDoesNotDependOnOtherAccounts() throws {
        let before = Dictionary(uniqueKeysWithValues: model.allAccounts.map { ($0.id, $0.colorHex) })
        // moving accounts between groups or deleting a group never changes colours
        let visa = try account(.creditCard)
        model.moveAccount(visa.id, to: nil)
        if let g = model.namedUserGroups.first { model.deleteUserGroup(g) }
        for a in model.allAccounts { XCTAssertEqual(a.colorHex, before[a.id], a.name) }
    }

    func testMarkersOnlyWhenTheListMixesAccounts() throws {
        model.sidebarSelection = .allAccounts
        XCTAssertTrue(model.showsAccountMarkers)
        let giro = try account(.checking)
        model.sidebarSelection = .account(giro.id)
        XCTAssertFalse(model.showsAccountMarkers)

        // user groups: one account → no markers, two → markers
        let single = try XCTUnwrap(model.namedUserGroups.first { $0.accounts.count == 1 })
        model.sidebarSelection = .userGroup(single.uuid)
        XCTAssertFalse(model.showsAccountMarkers)
        let multi = try XCTUnwrap(model.namedUserGroups.first { $0.accounts.count > 1 })
        model.sidebarSelection = .userGroup(multi.uuid)
        XCTAssertTrue(model.showsAccountMarkers)

        // kind groups likewise
        if let one = model.groups.first(where: { $0.accounts.count == 1 }) {
            model.sidebarSelection = .group(one.id)
            XCTAssertFalse(model.showsAccountMarkers)
        }
        if let many = model.groups.first(where: { $0.accounts.count > 1 }) {
            model.sidebarSelection = .group(many.id)
            XCTAssertTrue(model.showsAccountMarkers)
        }
    }

    func testDigitsOnlyWhenNamesCollide() throws {
        let giro = try account(.checking)
        let savings = try account(.savings)
        XCTAssertFalse(model.accountShortLabel(giro.id).contains("··"), "unique names stay short")

        // two accounts with the same name (e.g. the holder's name) → last digits
        try XCTUnwrap(model.vault).renameAccount(accountId: savings.id, name: giro.name)
        model.reloadAccounts()
        let a = model.accountShortLabel(giro.id)
        let b = model.accountShortLabel(savings.id)
        XCTAssertNotEqual(a, b, "same-named accounts stay distinguishable")
        XCTAssertTrue(a.hasPrefix(giro.name))
        XCTAssertTrue(b.hasPrefix(giro.name))
    }
}
