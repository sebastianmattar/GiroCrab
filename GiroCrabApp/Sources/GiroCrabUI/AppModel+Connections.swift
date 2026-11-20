import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    /// Mirrors the core's FinTS registration choice. New users start with GiroCrab's
    /// registration; users who already entered their own are asked once.
    func reloadProductIDState(_ vault: Vault) {
        var mode = (try? vault.fintsProductIdMode()) ?? ""
        if fintsBuiltinAvailable, mode != "builtin", !isDemo {
            try? vault.setFintsProductIdMode(mode: "builtin")
            mode = "builtin"
        }
        fintsProductIDMode = mode
        fintsProductIDSource = (try? vault.fintsProductIdSource()) ?? ""
        offerProductIDChoice = false
    }

    /// Stores the choice. The user's own ID always stays in the vault.
    func setFintsProductIDMode(_ mode: String) {
        guard let vault else { return }
        runCore { try vault.setFintsProductIdMode(mode: mode) }
        offerProductIDChoice = false
        reloadSettings()
    }

    func saveSetting(_ key: String, _ value: String) {
        guard let vault else { return }
        runCore { try vault.setSetting(key: key, value: value) }
        reloadSettings()
        enableBankingSettingSaved(key)
    }

    /// Same as `saveSetting`, but the write and the list reload run off the main
    /// thread (they hold the vault mutex). For edits made in the Settings window,
    /// where nothing right after depends on the value being stored; flows that
    /// continue with the vault straight away (add-bank, import) keep `saveSetting`.
    func saveSettingInBackground(_ key: String, _ value: String) {
        guard let vault else { return }
        let previous = settingsWriteTask
        settingsWriteTask = Task { @MainActor [weak self] in
            await previous?.value
            do {
                let result: SettingsSnapshot = try await Self.offMain { () throws -> SettingsSnapshot in
                    try vault.setSetting(key: key, value: value)
                    let entries = try vault.listSettings()
                    let provider = try vault.onlineLogoProvider()
                    return SettingsSnapshot(entries: entries, logoProvider: provider)
                }
                // The vault may have been switched or locked meanwhile.
                guard let self, self.vault === vault else { return }
                self.settings = result.entries
                self.logoProvider = result.logoProvider
                self.reloadProductIDState(vault)
                self.enableBankingSettingSaved(key)
            } catch {
                guard let self else { return }
                self.errorMessage = describe(error)
            }
        }
    }

    private struct SettingsSnapshot: Sendable {
        let entries: [SettingEntry]
        let logoProvider: String
    }

    /// Adds a bank login. While demo data is shown, the login goes into the
    /// personal vault (switching to it), then setup / PIN entry starts.
    func addConnection(_ kind: ConnectorKind, login: String) {
        withFreshBankingPolicy { [self] in
            guard Self.isMagicDemoLogin(login) || bankingAllowsAdding(bankingGate(forNew: kind)) else { return }
            addConnectionChecked(kind, login: login)
        }
    }

    private func addConnectionChecked(_ kind: ConnectorKind, login: String) {
        runInOwnVault(allowDemo: Self.isMagicDemoLogin(login)) { [self] in
            guard let vault else { return }
            var newID: Int64?
            runCore { newID = try vault.addBankConnection(connector: kind, displayName: "", loginName: login) }
            reloadAccounts()
            if let id = newID, let conn = connections.first(where: { $0.id == id }) {
                toast = String(localized: "\(conn.displayName) hinzugefügt")
                requestSync(conn)
            }
        }
    }

    /// Adds a login at any FinTS 3.0 bank (BLZ + https URL), then starts setup / PIN entry.
    func addFinTsConnection(name: String, blz: String, url: String, login: String, tanMedium: String = "") {
        withFreshBankingPolicy { [self] in
            guard Self.isMagicDemoLogin(login) || bankingAllowsAdding(bankingGate(forNewFinTS: blz)) else { return }
            addFinTsConnectionChecked(name: name, blz: blz, url: url, login: login, tanMedium: tanMedium)
        }
    }

    private func addFinTsConnectionChecked(name: String, blz: String, url: String, login: String, tanMedium: String) {
        runInOwnVault(allowDemo: Self.isMagicDemoLogin(login)) { [self] in
            guard let vault else { return }
            var newID: Int64?
            runCore { newID = try vault.addFintsConnection(blz: blz, url: url, displayName: name, loginName: login, tanMedium: tanMedium) }
            reloadSettings()
            reloadAccounts()
            if let id = newID, let conn = connections.first(where: { $0.id == id }) {
                toast = String(localized: "\(conn.displayName) hinzugefügt")
                requestSync(conn)
            }
        }
    }

    /// Adds a login at a bank reached via Enable Banking (PSD2), then starts the
    /// setup (Enable Banking credentials, if missing) and the consent redirect.
    func addEnableBankingConnection(aspsp: String, country: String, name: String, label: String) {
        withFreshBankingPolicy { [self] in
            guard Self.isMagicDemoLogin(label) || bankingAllowsAdding(bankingGate(forNew: .enableBanking)) else { return }
            addEnableBankingConnectionChecked(aspsp: aspsp, country: country, name: name, label: label)
        }
    }

    private func addEnableBankingConnectionChecked(aspsp: String, country: String, name: String, label: String) {
        runInOwnVault(allowDemo: Self.isMagicDemoLogin(label)) { [self] in
            guard let vault else { return }
            var newID: Int64?
            runCore { newID = try vault.addEnableBankingConnection(aspsp: aspsp, country: country, displayName: name, loginName: label) }
            reloadAccounts()
            if let id = newID, let conn = connections.first(where: { $0.id == id }) {
                toast = String(localized: "\(conn.displayName) hinzugefügt")
                requestSync(conn)
            }
        }
    }

    /// Loads the live ASPSP list when the user's Enable Banking credentials are set.
    func loadLivePsd2Banks() {
        guard let vault, livePsd2Banks.isEmpty, !isDemo,
              settingEntry("enablebanking.application_id")?.hasValue == true,
              settingEntry("enablebanking.private_key_pem")?.hasValue == true else { return }
        Task { [weak self] in
            let list = (try? await vault.listEnableBankingAspsps(country: "")) ?? []
            await MainActor.run { self?.livePsd2Banks = list }
        }
    }

    /// Setting keys a bank still needs (FinTS product ID, API keys …).
    func missingSettings(for connection: BankConnection) -> [String] {
        if Self.isMagicDemoLogin(connection.loginName) { return [] }
        guard let d = bankCatalog().first(where: { $0.kind == connection.connector }) else { return [] }
        if connection.connector == .payPal, payPalUsesSignature { return missingPayPalSignatureKeys }
        return userRequiredKeys(for: d).filter { key in !(settingEntry(key)?.hasValue ?? false) }
    }

    /// PayPal via API signature (classic NVP API, #752): all three are required.
    nonisolated static let payPalSignatureKeys = ["paypal.api_username", "paypal.api_password", "paypal.api_signature"]

    /// One of the API signature fields is filled in: PayPal is set up that way, not via a REST app.
    var payPalUsesSignature: Bool {
        Self.payPalSignatureKeys.contains { settingEntry($0)?.hasValue == true }
    }

    /// The API signature fields still empty (none: complete).
    var missingPayPalSignatureKeys: [String] {
        Self.payPalSignatureKeys.filter { !(settingEntry($0)?.hasValue ?? false) }
    }

    /// A setting to enter masked: the core's flag, else guessed from the key (not loaded yet).
    nonisolated static func isSecretSetting(_ key: String, _ entry: SettingEntry?) -> Bool {
        entry?.isSecret ?? (key.contains("secret") || key.hasSuffix("password") || key.hasSuffix("signature"))
    }

    /// Settings the core lists as required that are only an optional override for the user.
    /// A custom FinTS product ID is one: GiroCrab ships its own, and a value entered once is a
    /// global setting that covers every bank.
    nonisolated static let optionalSettingKeys: Set<String> = ["fints.product_id"]

    /// The ONLY place that turns `requiredSettings` into what the user must provide
    /// (`scripts/lint-required-settings.sh`). Add-bank forms, the assistant and setup use it.
    nonisolated static func userRequiredKeys(_ requiredSettings: [String]) -> [String] {
        requiredSettings.filter { !optionalSettingKeys.contains($0) }
    }

    /// The core's `MissingSetting` for a key that is only an optional override.
    nonisolated static func isMissingProductID(_ error: Error) -> Bool {
        guard let ce = error as? CoreError, case .MissingSetting(let key) = ce else { return false }
        return optionalSettingKeys.contains(key)
    }

    func userRequiredKeys(for descriptor: BankDescriptor) -> [String] {
        Self.userRequiredKeys(descriptor.requiredSettings)
    }

    /// The core could not resolve a FinTS registration (no embedded ID and no own one).
    /// Never a setup form: with an embedded ID the mode returns to `builtin` and the caller
    /// retries once; otherwise the user is offered the own-ID alternative in an alert.
    /// Returns true when a retry should follow.
    func recoverFromMissingProductID(_ vault: Vault) -> Bool {
        guard fintsBuiltinAvailable, fintsProductIDMode != "builtin" else { return false }
        try? vault.setFintsProductIdMode(mode: "builtin")
        reloadProductIDState(vault)
        return true
    }

    /// "Mit … erneut versuchen" after a bank rejected the FinTS registration in use.
    /// The bank refused the FinTS product registration (9078, or 9010 as some banks answer).
    /// The core reports the FIRST code of the reply (often 9050 "Nachricht enthält Fehler"),
    /// so 9078/9010 are also looked for in the joined message.
    nonisolated static func isProductIDRejection(_ error: Error) -> Bool {
        guard let ce = error as? CoreError, case .Bank(let code, let message) = ce else { return false }
        let codes = ["9010", "9078"]
        return codes.contains(code) || codes.contains { message.contains($0) }
    }

    func productIDAlternatives(for conn: BankConnection, error: Error, secret: String?) -> [(title: String, action: @MainActor () -> Void)] {
        guard Self.isProductIDRejection(error),
              bankCatalog().first(where: { $0.kind == conn.connector })?.requiredSettings.contains("fints.product_id") == true
        else { return [] }
        let hasOwn = settingEntry("fints.product_id")?.hasValue == true
        var out: [(title: String, action: @MainActor () -> Void)] = []
        switch fintsProductIDSource {
        case "builtin":
            // GiroCrab's registration was refused: the own one goes into Settings (no extra
            // field here), then the account is fetched again by hand.
            out.append((hasOwn ? String(localized: "Eigene Registrierung ersetzen …") : String(localized: "Eigene Registrierung eintragen …"),
                        { [weak self] in self?.enterOwnProductID() }))
        case "own":
            // The own registration is wrong: replace it, or use GiroCrab's instead.
            out.append((String(localized: "Eigene Registrierung ersetzen …"), { [weak self] in self?.enterOwnProductID() }))
            if fintsBuiltinAvailable {
                out.append((String(localized: "Mit GiroCrab-Registrierung erneut versuchen"), { [weak self] in
                    self?.setFintsProductIDMode("builtin")
                    guard let self, let c = self.connections.first(where: { $0.id == conn.id }) else { return }
                    // Same login and PIN as the failed attempt: only the registration changes.
                    if let secret, !secret.isEmpty, self.bankingGate(for: c) == nil {
                        self.startSync(c, secret: secret)
                    } else {
                        self.requestSync(c)
                    }
                }))
            }
        default: break
        }
        return out
    }

    /// After a rejected registration: switch to the own one, open Settings › Banks (FinTS registration)
    /// and put the cursor into its field (no extra field in the alert).
    func enterOwnProductID() {
        // Off to Settings: a waiting "Alle abrufen" ends here instead of stalling (#1041);
        // the login is fetched again by hand once the registration is in.
        resetSyncBatch()
        // Mode `own` only once an ID is stored (`OwnProductIDField.commit`), or an existing one is replaced:
        // an empty field must not leave the user in a mode without a registration.
        if settingEntry("fints.product_id")?.hasValue == true { setFintsProductIDMode("own") }
        focusOwnProductID = true
        openSettings(.banks)
    }

    func settingEntry(_ key: String) -> SettingEntry? { settings.first { $0.key == key } }

    /// Saves the setup form and continues with the sync.
    func completeSetup(_ request: SetupRequest, values: [String: String]) {
        for (k, v) in values where !v.trimmingCharacters(in: .whitespaces).isEmpty {
            saveSetting(k, v)
        }
        setupRequest = nil
        let still = missingSettings(for: request.connection)
        if still.isEmpty {
            requestSync(request.connection)
        } else {
            setupRequest = SetupRequest(connection: request.connection, keys: still)
        }
    }

    func holdings(for accountID: Int64) -> [Holding] {
        (try? vault?.listHoldings(accountId: accountID)) ?? []
    }

    /// Archives ("Das Konto wurde aufgelöst") or reactivates an account. Archived accounts keep
    /// everything, but are skipped by syncs and rule runs and leave the overall total.
    func setAccountArchived(_ account: Account, _ archived: Bool) {
        guard let vault else { return }
        runCore {
            try vault.setAccountArchived(accountId: account.id, archived: archived)
            toast = archived
                ? String(localized: "„\(account.name)“ archiviert")
                : String(localized: "„\(account.name)“ reaktiviert")
        }
        reloadAll()
    }

    /// Deletes one archived account incl. bookings/documents/holdings (and its connection if it was the last account).
    ///
    /// The core deletes only accounts that are saved as archived. `archiveFirst` (account settings, where the
    /// archive toggle may still be unsaved) archives and deletes in one go; the archive step does not show in
    /// the UI. If the delete fails afterwards, the account stays archived and the error is shown as usual.
    func deleteAccount(_ id: Int64, archiveFirst: Bool = false) {
        guard let vault else { return }
        var deleted = false
        runCore {
            if archiveFirst { try vault.setAccountArchived(accountId: id, archived: true) }
            try vault.deleteAccount(accountId: id)
            deleted = true
        }
        if deleted, sidebarSelection == .account(id) { sidebarSelection = .allAccounts }
        reloadAll()
    }

    func removeConnection(_ id: Int64) {
        guard let vault else { return }
        if advanziaWorkflow?.connection.id == id { advanziaWorkflow?.cancel() }
        // Trade Republic: also drop the browser session (tr_session, tr_refresh, website data).
        if connections.first(where: { $0.id == id })?.connector == .tradeRepublic {
            Task { await WebLoginSheet.clearAllTradeRepublicWebData() }
        }
        runCore { try vault.deleteBankConnection(connectionId: id) }
        reloadAll()
    }

    // MARK: - Connection details

    func connectionStatus(_ id: Int64) -> ConnectionStatus? {
        try? vault?.connectionStatus(connectionId: id)
    }

    func updateConnection(_ id: Int64, name: String, login: String) {
        guard let vault else { return }
        runCore { try vault.updateBankConnection(connectionId: id, displayName: name, loginName: login) }
        reloadAccounts()
    }

    /// comdirect: switches the login between FinTS and comdirect's own API,
    /// keeping accounts, categories and notes (see `Vault.switchComdirectAccess`).
    func switchComdirectAccess(_ id: Int64) {
        guard let vault else { return }
        runCore { _ = try vault.switchComdirectAccess(connectionId: id) }
        reloadAll()
    }

    /// Vault setting with the portal user name of a login (Amex); the password is
    /// the login's PIN (`setConnectionPin`). `pin.` keys are stored as secrets.
    static func portalLoginKey(_ id: Int64) -> String { "pin.conn.\(id).login" }

    func setConnectionPin(_ id: Int64, _ pin: String?) {
        guard let vault else { return }
        runCore { try vault.setConnectionPin(connectionId: id, pin: pin) }
    }

    func clearSession(_ id: Int64) {
        guard let vault else { return }
        runCore { try vault.clearConnectionSession(connectionId: id) }
        toast = String(localized: "Sitzung gelöscht")
    }

    /// The connection's bank delivers documents (the switch is offered only then).
    func documentsFetchSupported(_ id: Int64) -> Bool {
        (try? vault?.documentsFetchSupported(connectionId: id)) ?? false
    }

    /// Syncs of this connection fetch documents (default on).
    func documentsFetchEnabled(_ id: Int64) -> Bool {
        (try? vault?.documentsFetchEnabled(connectionId: id)) ?? true
    }

    func setDocumentsFetch(_ id: Int64, _ on: Bool) {
        guard let vault else { return }
        runCore { try vault.setDocumentsFetch(connectionId: id, enabled: on) }
        refreshDocumentsOff()
    }

    /// Reads the retrieval switch of every connection once (not from a view body): on a change of the
    /// connections and of the switch.
    func refreshDocumentsOff() {
        documentsOffConnectionIDs = Set(connections.filter {
            documentsFetchSupported($0.id) && !documentsFetchEnabled($0.id)
        }.map(\.id))
    }

    func revealSetting(_ key: String) -> String {
        ((try? vault?.getSetting(key: key)) ?? nil) ?? ""
    }

    /// Cookies from the in-app browser login -> vault -> sync without 2FA.
    func applyBrowserSession(_ connection: BankConnection, cookies: String) {
        guard let vault else { return }
        webLoginConnection = nil
        do {
            try vault.setConnectionSession(connectionId: connection.id, cookies: cookies)
            toast = String(localized: "Trade-Republic-Sitzung übernommen")
            startSync(connection, secret: "")
        } catch {
            resetSyncBatch()   // like any sync error: ends "Alle abrufen" instead of stalling it
            showError(String(localized: "Anmeldung konnte nicht übernommen werden"), describe(error))
        }
    }
}
