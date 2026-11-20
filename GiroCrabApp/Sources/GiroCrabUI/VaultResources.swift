import Foundation

/// Everything the app persists for one vault (GitHub #241). „App zurücksetzen“
/// works through this list instead of hard-coded paths, so a later
/// multi-vault setup only has to build one `VaultResources` per vault.
///
/// Deliberately NOT listed: the keychain item of the banking policy
/// (`de.girocrab.banking-policy`). It is the anti-rollback anchor of the
/// banking lock (#126) and must survive a reset, like it survives a
/// reinstall. The user's document mirror folder and the peers' snapshots in
/// iCloud Drive are not ours to delete either.
@MainActor
struct VaultResources {
    /// The vault file; its SQLite side files (`-wal`, `-shm`, `-journal`) are derived.
    var vaultURL: URL
    /// Further files and folders (legacy key file, decrypted temp copies, …).
    var extraPaths: [URL] = []
    /// Resets UserDefaults and in-memory copies of settings.
    var clearSettings: () -> Void
    /// Deletes keychain items other than the vault key.
    var clearKeychainItems: () -> Void
    /// Deletes the vault key. Runs last: with the key gone an existing file is unreadable.
    var clearKey: () -> Void

    var filePaths: [URL] {
        ["", "-wal", "-shm", "-journal"].map { URL(fileURLWithPath: vaultURL.path + $0) } + extraPaths
    }

    /// The resources of the app's own (single) vault.
    static func current() -> VaultResources {
        let bypass = VaultKeyManager.keychainBypassed
        return VaultResources(
            vaultURL: AppModel.vaultURL,
            extraPaths: [
                AppModel.vaultURL.deletingLastPathComponent().appendingPathComponent("tresor.key"),
                AppModel.quickLookDirectory,
            ],
            clearSettings: {
                CloudSyncManager.shared.resetForAppReset()
                if let id = Bundle.main.bundleIdentifier {
                    AppDefaults.store.removePersistentDomain(forName: id)
                }
            },
            clearKeychainItems: {
                // Dev runs with the keychain bypass never touched the real items.
                if !bypass { KeychainEnableBankingKeyStore.deleteStoredApplication() }
            },
            clearKey: { if !bypass { VaultKeyManager.shared.destroyKey() } }
        )
    }
}

@MainActor
enum AppReset {
    /// Deletes the resources. Order: files, settings, keychain items, key. A
    /// failed file deletion throws before anything else is touched, so the
    /// vault and its key stay together.
    static func perform(_ resources: VaultResources) throws {
        let fm = FileManager.default
        for url in resources.filePaths where fm.fileExists(atPath: url.path) || (try? url.checkResourceIsReachable()) == true {
            try fm.removeItem(at: url)
        }
        resources.clearSettings()
        resources.clearKeychainItems()
        resources.clearKey()
    }
}
