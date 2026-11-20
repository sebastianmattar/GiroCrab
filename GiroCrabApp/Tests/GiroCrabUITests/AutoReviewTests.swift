// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
#if os(macOS)
import AppKit
#endif
@testable import GiroCrabUI

/// "Als geprüft markieren": a selected unchecked booking becomes checked
/// immediately, after 1 or 5 seconds, or only by hand (default).
@MainActor
final class AutoReviewTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        AppDefaults.store.removeObject(forKey: AutoReviewDelay.defaultsKey)
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        model.statusFilter = .all
    }

    override func tearDown() async throws {
        AppDefaults.store.removeObject(forKey: AutoReviewDelay.defaultsKey)
        model = nil
    }

    /// Two unchecked bookings of the demo list (made unchecked if needed);
    /// nothing selected yet (the demo selects its first booking on load).
    private func uncheckedBookings() throws -> (Booking, Booking) {
        model.selectedBookingID = nil
        let list = Array(model.visibleBookings.prefix(2))
        XCTAssertEqual(list.count, 2)
        for b in list where b.checked { model.setChecked(false, for: b) }
        let fresh = list.compactMap { b in model.visibleBookings.first { $0.id == b.id } }
        XCTAssertTrue(fresh.allSatisfy { !$0.checked })
        return (fresh[0], fresh[1])
    }

    private func isChecked(_ b: Booking) -> Bool {
        model.visibleBookings.first { $0.id == b.id }?.checked ?? false
    }

    func testDefaultIsManualAndNothingIsMarked() async throws {
        XCTAssertEqual(model.autoReviewDelay, .manual)
        let (a, _) = try uncheckedBookings()
        model.selectedBookingID = a.id
        await model.autoReviewTask?.value
        XCTAssertFalse(isChecked(a))
    }

    func testImmediatelyMarksTheSelectedBooking() async throws {
        model.autoReviewDelay = .immediately
        let (a, _) = try uncheckedBookings()
        model.selectedBookingID = a.id
        await model.autoReviewTask?.value
        XCTAssertTrue(isChecked(a))
    }

    func testMovingOnBeforeTheDelayLeavesTheBookingUnchecked() async throws {
        model.autoReviewDelay = .oneSecond
        let (a, b) = try uncheckedBookings()
        model.selectedBookingID = a.id
        model.selectedBookingID = b.id
        await model.autoReviewTask?.value
        XCTAssertFalse(isChecked(a), "left before the second was over")
        XCTAssertTrue(isChecked(b))
    }

    func testMultipleSelectionIsNeverMarked() async throws {
        model.autoReviewDelay = .immediately
        let (a, b) = try uncheckedBookings()
        model.selectedBookingIDs = [a.id, b.id]
        model.selectedBookingID = a.id
        await model.autoReviewTask?.value
        XCTAssertFalse(isChecked(a))
        XCTAssertFalse(isChecked(b))
    }

    func testChoiceIsStoredPerDevice() {
        model.autoReviewDelay = .fiveSeconds
        XCTAssertEqual(AppDefaults.store.string(forKey: AutoReviewDelay.defaultsKey), "5s")
        XCTAssertEqual(AutoReviewDelay.stored, .fiveSeconds)
        XCTAssertEqual([AutoReviewDelay.manual, .immediately, .oneSecond, .fiveSeconds].map(\.seconds), [nil, 0, 1, 5])
    }

    #if os(macOS)
    func testAccountSwitchMarksPreviousAccountAndClickMarksOneBooking() async throws {
        let vault = try XCTUnwrap(model.vault)
        model.sidebarSelection = .allAccounts
        model.period = .all
        let allBookings = try vault.listTransactions(filter: model.currentFilter).transactions
        let byAccount = Dictionary(grouping: allBookings, by: \.accountId)
        let account = try XCTUnwrap(byAccount.values.first { $0.count >= 3 })
        let other = try XCTUnwrap(allBookings.first { $0.accountId != account[0].accountId })
        let third = try XCTUnwrap(allBookings.first { $0.accountId != account[0].accountId && $0.accountId != other.accountId })
        let target = SidebarItem.account(account[0].accountId)
        let next = SidebarItem.account(other.accountId)
        let alreadyChecked = account[0].id
        let unread = account.dropFirst().map(\.id)
        try vault.setCheckedForIds(ids: [alreadyChecked], checked: true)
        try vault.setCheckedForIds(ids: unread + [other.id, third.id], checked: false)
        let unfiltered = model.currentFilter

        func checkedIDs() throws -> Set<Int64> {
            Set(try vault.listTransactions(filter: unfiltered).transactions.filter(\.checked).map(\.id))
        }

        _ = NSApplication.shared
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 160, height: 100),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        let undo = try XCTUnwrap(model.windowUndoManager)
        undo.groupsByEvent = false
        defer { undo.groupsByEvent = true }

        model.selectFromSidebar(target)
        model.selectFromSidebar(next)
        XCTAssertNil(model.checkedWriteTask, "The default mode must not mark an account")
        XCTAssertTrue(try checkedIDs().isDisjoint(with: unread))

        model.autoReviewDelay = .onAccountSwitch
        XCTAssertEqual(AutoReviewDelay.stored, .onAccountSwitch)
        XCTAssertNil(model.autoReviewDelay.seconds)
        model.sidebarSelection = target
        XCTAssertNil(model.checkedWriteTask, "Programmatic navigation must not mark accounts")
        model.selectFromSidebar(.allAccounts)
        XCTAssertNil(model.checkedWriteTask, "An aggregate selection must not mark accounts")
        model.selectFromSidebar(target)
        await model.selectionLoadTask?.value
        XCTAssertNil(model.checkedWriteTask, "Opening a single account must leave it unread")
        model.selectFromSidebar(target)
        XCTAssertNil(model.checkedWriteTask, "Selecting the same account must not mark it")

        let clicked = try XCTUnwrap(model.visibleBookings.first { unread.contains($0.id) })
        model.selectBooking(clicked.id)
        XCTAssertTrue(try checkedIDs().isDisjoint(with: unread), "Programmatic selection stays unread")
        model.selectBooking(clicked.id, fromListClick: true)
        let remaining = unread.filter { $0 != clicked.id }
        XCTAssertTrue(try checkedIDs().contains(clicked.id))
        XCTAssertTrue(try checkedIDs().isDisjoint(with: remaining))

        model.period = .custom
        model.customPeriodFrom = "2000-01-01"
        model.customPeriodTo = "2000-01-02"
        model.amountFilter = AmountFilter(minCents: Int64.max)

        undo.beginUndoGrouping()
        model.selectFromSidebar(next)
        let write = model.checkedWriteTask
        await write?.value
        undo.endUndoGrouping()
        XCTAssertNotNil(write)
        let checked = try checkedIDs()
        XCTAssertTrue(checked.isSuperset(of: unread + [alreadyChecked]))
        XCTAssertFalse(checked.contains(other.id))
        XCTAssertFalse(checked.contains(third.id))
        XCTAssertTrue(undo.canUndo)
        undo.undo()
        let afterUndo = try checkedIDs()
        XCTAssertTrue(afterUndo.isDisjoint(with: remaining))
        XCTAssertTrue(afterUndo.isSuperset(of: [clicked.id, alreadyChecked]))
        XCTAssertFalse(afterUndo.contains(other.id))

        undo.beginUndoGrouping()
        model.selectFromSidebar(.account(third.accountId))
        await model.checkedWriteTask?.value
        undo.endUndoGrouping()
        let afterNextSwitch = try checkedIDs()
        XCTAssertTrue(afterNextSwitch.contains(other.id), "The next switch marks the account just left")
        XCTAssertFalse(afterNextSwitch.contains(third.id))
        XCTAssertTrue(afterNextSwitch.isDisjoint(with: remaining))
    }
    #endif
}
