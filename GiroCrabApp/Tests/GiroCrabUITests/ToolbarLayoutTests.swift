// test: serial-defaults
import XCTest
@testable import GiroCrabUI

/// The stored main toolbar (#2049) and the one-time takeover of AppKit's saved layout of the former
/// customizable toolbar (#1832, #2010).
final class ToolbarLayoutTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suiteName: String!

    override func setUp() {
        super.setUp()
        suiteName = "ToolbarLayoutTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func setLegacy(_ items: [String]) {
        defaults.set(["TB Item Identifiers": items, "TB Display Mode": 2], forKey: ToolbarLayout.legacyDefaultsKey)
    }

    func testDefaultIsTheFormerDefaultToolbar() {
        XCTAssertEqual(ToolbarLayout.load(from: defaults), ToolbarLayout(
            showsScope: true, leading: [.transfer, .newBooking, .sync, .inspector, .privacy], trailing: []))
    }

    func testConvertsTheLayoutOf1832() {
        let layout = ToolbarLayout.converting(legacyItems: [
            "com.apple.SwiftUI.navigationSplitView.toggleSidebar", "com.apple.SwiftUI.splitViewSeparator-0",
            "export", "print", "sync", "privacy", "NSToolbarFlexibleSpaceItem", "settings", "inspector", "com.apple.SwiftUI.search",
        ])
        XCTAssertEqual(layout, ToolbarLayout(showsScope: false, leading: [.export, .print, .sync, .privacy], trailing: [.settings, .inspector]))
    }

    func testConvertsTheLayoutOf2010() {
        let layout = ToolbarLayout.converting(legacyItems: [
            "com.apple.SwiftUI.navigationSplitView.toggleSidebar", "print", "transfer", "sync", "newBooking", "privacy", "scope",
            "com.apple.SwiftUI.splitViewSeparator-0", "NSToolbarFlexibleSpaceItem", "com.apple.SwiftUI.search", "export", "inspector", "settings",
        ])
        XCTAssertEqual(layout, ToolbarLayout(
            showsScope: true, leading: [.print, .transfer, .sync, .newBooking, .privacy], trailing: [.export, .inspector, .settings]))
    }

    func testWithoutFlexibleSpaceEverythingIsLeading() {
        let layout = ToolbarLayout.converting(legacyItems: ["scope", "sync", "NSToolbarSpaceItem", "lock", "com.apple.SwiftUI.search"])
        XCTAssertEqual(layout, ToolbarLayout(showsScope: true, leading: [.sync, .lock], trailing: []))
    }

    func testUnknownIDsAndSpacesAreIgnored() {
        let layout = ToolbarLayout.converting(legacyItems: [
            "futureButton", "transfer", "NSToolbarSpaceItem", "transfer", "NSToolbarFlexibleSpaceItem",
            "com.apple.SwiftUI.unknown", "NSToolbarFlexibleSpaceItem", "palette", "somethingElse", "rules",
        ])
        XCTAssertEqual(layout, ToolbarLayout(showsScope: false, leading: [.transfer], trailing: [.palette, .rules]))
    }

    func testTakeoverStoresTheLayoutAndDeletesTheOldKey() {
        setLegacy(["sync", "NSToolbarFlexibleSpaceItem", "inspector"])
        ToolbarLayout.migrateLegacy(in: defaults)
        XCTAssertNil(defaults.object(forKey: ToolbarLayout.legacyDefaultsKey))
        XCTAssertEqual(ToolbarLayout.load(from: defaults), ToolbarLayout(showsScope: false, leading: [.sync], trailing: [.inspector]))
    }

    func testTakeoverWithoutItemsKeepsTheDefaultAndDeletesTheOldKey() {
        defaults.set(["TB Is Shown": 1], forKey: ToolbarLayout.legacyDefaultsKey)
        ToolbarLayout.migrateLegacy(in: defaults)
        XCTAssertNil(defaults.object(forKey: ToolbarLayout.legacyDefaultsKey))
        XCTAssertNil(defaults.object(forKey: ToolbarLayout.defaultsKey))
        XCTAssertEqual(ToolbarLayout.load(from: defaults), .standard)
    }

    func testTakeoverDoesNothingWhenOwnLayoutExists() {
        let own = ToolbarLayout(showsScope: false, leading: [.lock], trailing: [])
        own.save(to: defaults)
        setLegacy(["print", "NSToolbarFlexibleSpaceItem", "export"])
        ToolbarLayout.migrateLegacy(in: defaults)
        XCTAssertEqual(ToolbarLayout.load(from: defaults), own)
        XCTAssertNotNil(defaults.object(forKey: ToolbarLayout.legacyDefaultsKey))
    }

    @MainActor
    func testStoreRunsTheTakeoverAndSavesChanges() {
        setLegacy(["print", "NSToolbarFlexibleSpaceItem", "export"])
        let store = ToolbarLayoutStore(defaults: defaults)
        XCTAssertEqual(store.layout, ToolbarLayout(showsScope: false, leading: [.print], trailing: [.export]))
        store.layout.trailing.append(.rules)
        XCTAssertEqual(ToolbarLayout.load(from: defaults).trailing, [.export, .rules])
    }

    func testJSONRoundTrip() throws {
        let layout = ToolbarLayout(showsScope: false, leading: [.settings, .rules, .palette], trailing: [.lock, .print])
        let data = try JSONEncoder().encode(layout)
        XCTAssertEqual(try JSONDecoder().decode(ToolbarLayout.self, from: data), layout)
    }

    func testDecodingDropsUnknownAndDuplicateIDs() throws {
        let json = #"{"showsScope":true,"leading":["sync","teleport","sync"],"trailing":["export","sync"]}"#
        XCTAssertEqual(try JSONDecoder().decode(ToolbarLayout.self, from: Data(json.utf8)),
                       ToolbarLayout(showsScope: true, leading: [.sync], trailing: [.export]))
    }

    func testUnreadableLayoutFallsBackToDefault() {
        defaults.set(Data("nope".utf8), forKey: ToolbarLayout.defaultsKey)
        XCTAssertEqual(ToolbarLayout.load(from: defaults), .standard)
    }

    // MARK: List of the dialog „Symbolleiste anpassen“

    private let sample = ToolbarLayout(showsScope: true, leading: [.transfer, .sync], trailing: [.settings])

    private func layout(_ rows: [ToolbarListRow], shown: Set<ToolbarLayout.Item>? = nil) -> ToolbarLayout {
        .from(rows: rows, shown: shown ?? sample.shown, showsScope: sample.showsScope)
    }

    func testRowsMirrorTheToolbarThenHiddenButtons() {
        XCTAssertEqual(sample.listRows(oneGroup: false), [
            .item(.transfer), .item(.sync), .flexibleSpace, .item(.settings), .item(.newBooking), .item(.inspector),
            .item(.privacy), .item(.palette), .item(.export), .item(.print), .item(.rules), .item(.lock),
        ])
        XCTAssertEqual(layout(sample.listRows(oneGroup: false)), sample)
        XCTAssertEqual(ToolbarLayout.from(rows: ToolbarLayout.standard.listRows(oneGroup: false), shown: ToolbarLayout.standard.shown,
                                          showsScope: true), .standard)
    }

    func testMovingAButtonBelowTheSpacePutsItNextToTheSearch() {
        var rows = sample.listRows(oneGroup: false)
        rows.move(fromOffsets: [0], toOffset: 4) // Überweisen below Einstellungen
        XCTAssertEqual(layout(rows), ToolbarLayout(showsScope: true, leading: [.sync], trailing: [.settings, .transfer]))
    }

    func testMovingTheSpaceUpMovesButtonsToTheRight() {
        var rows = sample.listRows(oneGroup: false)
        rows.move(fromOffsets: [2], toOffset: 0)
        XCTAssertEqual(layout(rows), ToolbarLayout(showsScope: true, leading: [], trailing: [.transfer, .sync, .settings]))
    }

    func testHiddenButtonsKeepTheirRowAndShowWhereTheyStand() {
        var rows = sample.listRows(oneGroup: false)
        rows.move(fromOffsets: [5], toOffset: 1) // Informationen (hidden) between Überweisen and Abrufen
        XCTAssertEqual(layout(rows), sample, "a hidden row changes nothing in the toolbar")
        var shown = sample.shown
        shown.insert(.inspector)
        shown.remove(.settings)
        XCTAssertEqual(layout(rows, shown: shown), ToolbarLayout(showsScope: true, leading: [.transfer, .inspector, .sync], trailing: []))
    }

    func testOneGroupBeforeMacOS26() {
        var rows = sample.listRows(oneGroup: true)
        XCTAssertFalse(rows.contains(.flexibleSpace))
        rows.move(fromOffsets: [2], toOffset: 0) // Einstellungen first
        XCTAssertEqual(layout(rows), ToolbarLayout(showsScope: true, leading: [.settings, .transfer, .sync], trailing: []))
    }

    @MainActor
    func testStoreKeepsEditsResetAndScope() {
        let store = ToolbarLayoutStore(defaults: defaults)
        store.layout = .from(rows: store.layout.listRows(oneGroup: false), shown: [.lock, .sync], showsScope: false)
        XCTAssertEqual(ToolbarLayoutStore(defaults: defaults).layout, ToolbarLayout(showsScope: false, leading: [.sync], trailing: [.lock]))
        store.layout = .standard
        XCTAssertEqual(ToolbarLayoutStore(defaults: defaults).layout, .standard)
    }
}
