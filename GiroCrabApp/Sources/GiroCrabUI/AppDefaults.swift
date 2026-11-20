import Foundation

/// The settings store. In the app it is `UserDefaults.standard` (so `@AppStorage`
/// reads the same values). Under `swift test --parallel` several test processes
/// run at once and shared `.standard`, overwriting each other's settings: each
/// test process gets a store of its own (like the temp vaults,
/// `AppModel.tempVaultURL`). It lives at an absolute path in the temp folder, so
/// it never lands in ~/Library/Preferences and is deleted completely at exit.
public enum AppDefaults {
    public nonisolated static let store: UserDefaults = {
        guard let path = testStorePath(), let suite = UserDefaults(suiteName: path) else { return .standard }
        suite.removePersistentDomain(forName: path) // a reused pid must not see old values
        atexit {
            guard let path = AppDefaults.testStorePath() else { return }
            UserDefaults.standard.removePersistentDomain(forName: path)
            try? FileManager.default.removeItem(atPath: path + ".plist")
        }
        return suite
    }()

    /// `GiroCrabTests-<pid>` in unit tests, nil (= `.standard`) in the app.
    nonisolated static func suiteName(
        unitTests: Bool = AppModel.runningUnitTests,
        pid: Int32 = ProcessInfo.processInfo.processIdentifier
    ) -> String? {
        unitTests ? "GiroCrabTests-\(pid)" : nil
    }

    nonisolated static func testStorePath() -> String? {
        suiteName().map { (NSTemporaryDirectory() as NSString).appendingPathComponent($0) }
    }
}
