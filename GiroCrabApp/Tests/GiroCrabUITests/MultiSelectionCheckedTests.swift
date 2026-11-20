// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// #725: "Geprüft" / "Als neu markieren" acts on every selected booking when
/// the booking is part of a multiple selection, otherwise on that booking alone.
@MainActor
final class MultiSelectionCheckedTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        AppDefaults.store.removeObject(forKey: AutoReviewDelay.defaultsKey)
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
        model.statusFilter = .all
    }

    override func tearDown() async throws { model = nil }

    /// The first `count` bookings of the demo list, all unchecked; nothing selected.
    private func uncheckedBookings(_ count: Int) throws -> [Booking] {
        model.selectedBookingID = nil
        model.selectedBookingIDs = []
        let list = Array(model.visibleBookings.prefix(count))
        XCTAssertEqual(list.count, count)
        for b in list where b.checked { model.setChecked(false, for: b) }
        XCTAssertTrue(list.allSatisfy { !isChecked($0) })
        return list
    }

    /// Checked state as stored in the vault (not just the patched list row).
    private func storedChecked(_ ids: [Int64]) throws -> [Bool] {
        let vault = try XCTUnwrap(model.vault)
        var filter = model.currentFilter
        filter.limit = 0; filter.offset = 0
        let rows = try vault.listTransactions(filter: filter).transactions
        return ids.map { id in rows.first { $0.id == id }?.checked ?? false }
    }

    private func isChecked(_ b: Booking) -> Bool {
        model.visibleBookings.first { $0.id == b.id }?.checked ?? false
    }

    private func select(_ list: [Booking]) {
        model.selectedBookingIDs = Set(list.map(\.id))
        model.selectedBookingID = list.first?.id
        XCTAssertTrue(model.isMultiSelection)
    }

    func testCheckingOneSelectedBookingChecksTheWholeSelection() async throws {
        let list = try uncheckedBookings(3)
        select(list)

        model.setChecked(true, for: list[1])
        XCTAssertTrue(list.allSatisfy(isChecked), "rows change at once")
        await model.checkedWriteTask?.value
        XCTAssertEqual(try storedChecked(list.map(\.id)), [true, true, true])
    }

    func testUncheckingWorksForTheWholeSelectionToo() async throws {
        let list = try uncheckedBookings(3)
        select(list)
        model.setChecked(true, for: list[0])
        await model.checkedWriteTask?.value

        model.setChecked(false, for: list[2])
        await model.checkedWriteTask?.value
        XCTAssertFalse(list.contains(where: isChecked))
        XCTAssertEqual(try storedChecked(list.map(\.id)), [false, false, false])
    }

    func testBookingOutsideTheSelectionChangesAlone() async throws {
        let list = try uncheckedBookings(4)
        select(Array(list.prefix(2)))

        model.setChecked(true, for: list[3])
        await model.checkedWriteTask?.value
        XCTAssertEqual(try storedChecked(list.map(\.id)), [false, false, false, true])
    }

    func testQuickTogglesEndInTheLastState() async throws {
        let list = try uncheckedBookings(3)
        select(list)

        model.setChecked(true, for: list[0])
        model.setChecked(false, for: list[0])
        model.setChecked(true, for: list[0])
        await model.checkedWriteTask?.value
        XCTAssertEqual(try storedChecked(list.map(\.id)), [true, true, true])
    }
}
