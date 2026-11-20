// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// After a pasted key opens the synced vault, the app leaves the demo and remembers real data (#308).
@MainActor
final class KeyAdoptionTests: XCTestCase {
    private let realDataKey = "girocrab.hasRealData"
    private var saved: Any?

    override func setUp() {
        saved = AppDefaults.store.object(forKey: realDataKey)
        AppDefaults.store.set(false, forKey: realDataKey)
    }

    override func tearDown() {
        if let saved { AppDefaults.store.set(saved, forKey: realDataKey) } else { AppDefaults.store.removeObject(forKey: realDataKey) }
    }

    func test_adoption_with_synced_data_leaves_demo_and_marks_real_data() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertTrue(model.demoActive)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("adopt-\(UUID().uuidString).girocrab")
        defer { for s in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: url.path + s) } }
        let v = try Vault.open(path: url.path, key: Data(repeating: 7, count: 32))
        _ = try v.seedDemoData() // stands in for the merged snapshot: accounts exist
        model.finishKeyAdoption(vault: v)
        XCTAssertFalse(model.demoActive)
        XCTAssertEqual(model.vaultState, .open)
        XCTAssertEqual(AppDefaults.store.object(forKey: realDataKey) as? Bool, true)
        model.stopAutoSync()
        CloudSyncManager.shared.stopAutoSync()
    }

    func test_adoption_without_data_stays_in_demo() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("adopt-\(UUID().uuidString).girocrab")
        defer { for s in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: url.path + s) } }
        let v = try Vault.open(path: url.path, key: Data(repeating: 7, count: 32))
        model.finishKeyAdoption(vault: v)
        XCTAssertTrue(model.demoActive)
        XCTAssertEqual(AppDefaults.store.object(forKey: realDataKey) as? Bool, false)
    }

    func test_invalid_key_fails_inline_without_leaving_the_sheet_state() {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        model.showSettings = true
        XCTAssertEqual(model.keyAdoption, .idle)
        model.adoptSyncKey("kein schluessel")
        guard case .failed = model.keyAdoption else { return XCTFail("expected .failed, got \(model.keyAdoption)") }
        XCTAssertTrue(model.showSettings)
        XCTAssertTrue(model.demoActive)
        XCTAssertEqual(model.vaultState, .open)
    }

    func test_paste_in_demo_without_own_vault_opens_own_vault_and_defers_adoption() {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        var unlockRequested = false
        model.requestOwnVaultUnlock = { unlockRequested = true }
        model.adoptSyncKey(String(repeating: "ab", count: 32))
        XCTAssertTrue(unlockRequested, "the own vault is created first, not a silent toast + dismiss (#646)")
        XCTAssertEqual(model.keyAdoption, .working)
        XCTAssertNotNil(model.pendingOwnVaultAction)
    }

    func test_paste_while_locked_outside_demo_reports_error() throws {
        let model = AppModel(options: LaunchOptions(arguments: [], environment: [:]))
        guard !model.demoActive, !model.hasOwnVault else { throw XCTSkip("needs a locked, non-demo model") }
        model.adoptSyncKey(String(repeating: "ab", count: 32))
        guard case .failed = model.keyAdoption else { return XCTFail("expected .failed") }
    }
}
