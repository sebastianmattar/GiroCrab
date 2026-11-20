import Foundation
import GiroCrabCore

// MARK: Enable Banking key via iCloud Keychain

extension AppModel {
    nonisolated static let enableBankingKeySettings: Set<String> = ["enablebanking.application_id", "enablebanking.private_key_pem"]

    /// Only with the opt-in "Tresor-Schlüssel über iCloud-Schlüsselbund", and
    /// never for the demo vault.
    private var sharesEnableBankingKey: Bool {
        VaultKeyManager.shared.syncViaICloudKeychain && !demoActive && vault != nil
    }

    private func vaultEnableBankingApp() -> EnableBankingApp? {
        guard let vault else { return nil }
        let id = ((try? vault.getSetting(key: "enablebanking.application_id")) ?? nil) ?? ""
        let pem = ((try? vault.getSetting(key: "enablebanking.private_key_pem")) ?? nil) ?? ""
        return EnableBankingApp(applicationID: id, pem: pem)
    }

    /// After the own vault opened: take over an application another device
    /// shared, or share the one set up here.
    func reconcileEnableBankingKey() {
        guard sharesEnableBankingKey, let vault else {
            enableBankingKeyLog.info("open: not shared (opt-in \(VaultKeyManager.shared.syncViaICloudKeychain, privacy: .public), demo \(self.demoActive, privacy: .public))")
            return
        }
        let store = enableBankingKeyStore
        // The keychain query can block (iCloud Keychain): never on the main thread at start.
        Task { [weak self] in
            let stored = await Task.detached(priority: .utility) { store.load() }.value
            guard let self, self.vault === vault, self.sharesEnableBankingKey else { return }
            // read only now: a key saved here meanwhile is what gets compared
            let here = self.vaultEnableBankingApp()
            self.applyEnableBankingKey(EnableBankingKeySync.reconcile(vault: here, store: stored), here: here, vault: vault)
        }
    }

    private func applyEnableBankingKey(_ action: EnableBankingKeySync.Action, here: EnableBankingApp?, vault: Vault) {
        enableBankingKeyLog.info("open: \(String(describing: action).prefix(8), privacy: .public)")
        switch action {
        case .none:
            break
        case .adopt(let app):
            do {
                try vault.setSetting(key: "enablebanking.application_id", value: app.applicationID)
                try vault.setSetting(key: "enablebanking.private_key_pem", value: app.pem)
                reloadSettings()
                if !AppDefaults.store.bool(forKey: EnableBankingKeySync.noticeSuppressedKey) {
                    // logins with their own application (#824) keep it
                    let own = Set(((try? vault.listConnectionEnableBankingApps()) ?? []).map(\.connectionId))
                    let banks = connections.filter { $0.connector == .enableBanking && !own.contains($0.id) }.map(\.displayName)
                    enableBankingAdoptNotice = EnableBankingKeySync.notice(for: app, replaced: here?.isComplete == true, banks: banks)
                }
            } catch {
                Self.debugLog("enablebanking key adopt failed: \(error)")
            }
        case .upload(let app):
            uploadEnableBankingKey(app)
        }
    }

    /// After one of the two settings was saved here: share the new state (no
    /// reconcile – the store still holds the old key and must not win).
    func enableBankingSettingSaved(_ key: String) {
        guard Self.enableBankingKeySettings.contains(key), sharesEnableBankingKey,
              let app = vaultEnableBankingApp(), app.isComplete else { return }
        uploadEnableBankingKey(app)
    }

    private func uploadEnableBankingKey(_ app: EnableBankingApp) {
        do {
            try enableBankingKeyStore.save(app)
        } catch {
            // no iCloud Keychain here: the setting itself is stored anyway
            Self.debugLog("enablebanking key upload failed: \(error)")
        }
    }
}

// MARK: Enable Banking application per bank login (#824)

extension AppModel {
    /// The application this login signs with (its own, else the default one).
    func connectionEnableBankingApp(_ id: Int64) -> ConnectionEnableBankingApp? {
        (try? vault?.connectionEnableBankingApp(connectionId: id)) ?? nil
    }

    /// Private key of that application, for the login's settings.
    func connectionEnableBankingKey(_ id: Int64) -> String {
        ((try? vault?.connectionEnableBankingKey(connectionId: id)) ?? nil) ?? ""
    }

    /// Stores the application of this login only; the default and all other
    /// logins keep theirs. Both empty: back to the default application.
    func setConnectionEnableBankingApp(_ id: Int64, applicationID: String, pem: String) {
        guard let vault else { return }
        var changed = false
        runCore { changed = try vault.setConnectionEnableBankingApp(connectionId: id, applicationId: applicationID, privateKeyPem: pem) }
        if changed {
            toast = String(localized: "Anwendung für diesen Bankzugang gespeichert. Beim nächsten Abruf ist eine neue Zustimmung nötig.")
        }
    }
}
