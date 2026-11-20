// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Unlocking loads the own vault's first lists off the main thread and shows
/// the main view only with them (the lock screen's crab keeps moving meanwhile).
@MainActor
final class UnlockLoadTests: XCTestCase {
    private let realDataKey = "girocrab.hasRealData"
    private var hadRealData: Any?
    private var url: URL!

    override func setUp() async throws {
        hadRealData = AppDefaults.store.object(forKey: realDataKey)
        url = FileManager.default.temporaryDirectory.appendingPathComponent("unlock-load-\(UUID().uuidString).girocrab")
    }

    override func tearDown() async throws {
        AppDefaults.store.set(hadRealData, forKey: realDataKey)
        for suffix in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: url.path + suffix) }
    }

    private func makeVault() throws -> Vault {
        let v = try Vault.open(path: url.path, key: Data(repeating: 7, count: 32))
        _ = try v.seedDemoData()
        return v
    }

    func testMainViewAppearsOnlyWithTheLoadedLists() async throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let v = try makeVault()
        model.sidebarSelection = .newBookings
        model.vaultState = .unlocking

        let load = Task { await model.useOwnVaultAfterUnlock(v, hop: Task {}) }
        // the synchronous part ran no booking scan and left the lock screen up
        await Task.yield()
        XCTAssertEqual(model.vaultState, .unlocking)
        XCTAssertTrue(model.bookings.isEmpty)
        await load.value
        model.stopAutoSync()

        XCTAssertEqual(model.vaultState, .open)
        XCTAssertFalse(model.isDemo)
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(model.sidebarSelection, .allAccounts)
        XCTAssertFalse(model.isLoadingSelection)
        let expected = try v.listTransactions(filter: model.currentFilter).transactions.map(\.id)
        XCTAssertFalse(expected.isEmpty)
        XCTAssertEqual(model.bookings.map(\.id), expected)
        XCTAssertEqual(model.selectedBookingID, expected.first)

        // same state as the synchronous switch to that vault
        let history = model.history.map(\.balanceCents), categories = model.categories.map(\.totalCents)
        let recurring = model.recurring.map(\.baseKey), groups = model.groups.map(\.id)
        XCTAssertFalse(history.isEmpty)
        model.useVault(v, demo: false)
        model.stopAutoSync()
        XCTAssertEqual(model.bookings.map(\.id), expected)
        XCTAssertEqual(model.history.map(\.balanceCents), history)
        XCTAssertEqual(model.categories.map(\.totalCents), categories)
        XCTAssertEqual(model.recurring.map(\.baseKey), recurring)
        XCTAssertEqual(model.groups.map(\.id), groups)
    }

    /// Locked again (or switched to the demo) while loading: the main view must not open.
    func testLockDuringLoadKeepsTheLockScreen() async throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let v = try makeVault()
        model.vaultState = .unlocking
        let load = Task { await model.useOwnVaultAfterUnlock(v, hop: Task {}) }
        await Task.yield()
        model.vaultState = .locked
        await load.value
        model.stopAutoSync()
        XCTAssertEqual(model.vaultState, .locked)
    }
}
