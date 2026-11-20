import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Sync (two-phase)

    /// Starts a sync (or the login it needs) once the banking policy is fresh
    /// and allows this login; otherwise explains why not.
    func requestSync(_ connection: BankConnection) {
        withFreshBankingPolicy { [self] in
            if let gate = bankingGate(for: connection) {
                clearSyncTargets()
                if syncBatchTotal > 1, !gate.all {
                    // A lock published while "Alle abrufen" runs: skip this
                    // login, go on with the rest, name it in the closing notice.
                    syncBatchSkipped.append(String(localized: "\(connection.displayName) (Sicherheitssperre)"))
                    syncNextInQueue()
                } else {
                    resetSyncBatch()
                    showError(gate.title, gate.detail)
                }
                return
            }
            requestSyncChecked(connection)
        }
    }

    private func requestSyncChecked(_ connection: BankConnection) {
        if connection.connector == .advanzia, !isDemo, !Self.isMagicDemoLogin(connection.loginName) {
            startAdvanzia(connection); return
        }
        // Finpension: no sync, the export file is imported from the connection's sheet.
        if usesFileImport(connection) {
            resetSyncBatch()
            toast = String(localized: "\(connection.displayName): Bitte den Export in den Einstellungen der Verbindung importieren.")
            return
        }
        // Amex / Amazon Visa: embedded web-portal login (no API keys needed).
        if usesPortal(connection) {
            webLoginConnection = connection
            return
        }
        // Trade Republic: AWS WAF blocks raw logins – without a stored
        // session the in-app browser login is the default flow.
        let magicDemo = Self.isMagicDemoLogin(connection.loginName)
        if !isDemo, !magicDemo, connection.connector == .tradeRepublic, !(connectionStatus(connection.id)?.hasSession ?? false) {
            webLoginConnection = connection
            return
        }
        if !isDemo, let st = connectionStatus(connection.id), (st.hasPin || ([.tradeRepublic, .kontist].contains(connection.connector) && st.hasSession)),
           missingSettings(for: connection).isEmpty {
            startSync(connection, secret: "")
            return
        }
        if !isDemo {
            let missing = missingSettings(for: connection)
            if !missing.isEmpty {
                setupRequest = SetupRequest(connection: connection, keys: missing)
                return
            }
        }
        let needsSecret = !(bankCatalog().first { $0.kind == connection.connector }?.secretLabel.hasPrefix("(") ?? false)
        if isDemo || !needsSecret {
            startSync(connection, secret: isDemo ? "demo" : "")
        } else {
            pendingLogin = PendingLogin(connection: connection)
        }
    }

    /// "Abrufen" of a login: all its accounts, also after an earlier
    /// "<Konto> abrufen" of it ended in an error.
    func requestFullSync(_ connection: BankConnection) {
        syncAccountID = nil
        requestSync(connection)
    }

    /// "<Konto> abrufen": only this account of its login (#1124). Login
    /// prompts and retries of this sync keep to it.
    func requestSyncAccount(_ account: Account) {
        guard syncingConnectionID == nil, !account.isArchived, let cid = account.connectionId,
              let connection = connections.first(where: { $0.id == cid }) else { return }
        // logins with their own flow (web portal, file import, Advanzia) fetch everything
        if !usesPortal(connection), !usesFileImport(connection), connection.connector != .advanzia {
            syncAccountID = account.id
        }
        requestSync(connection)
    }

    func requestSyncAll() {
        // portal connections need an interactive login – not part of "sync all"
        // logins whose accounts are all archived are not asked at all
        requestSyncBatch { [self] in connections.filter { !usesPortal($0) && !usesFileImport($0) && !$0.allAccountsArchived } }
    }

    /// "Konten abrufen" in a group's menu: the logins behind the group's
    /// accounts, subgroups included, each once – as "Alle abrufen" would.
    func groupSyncConnections(_ group: UserGroup) -> [BankConnection] {
        let ids = Set(userGroupSubtree(group.uuid).flatMap(\.accounts)
            .filter { !$0.isArchived }.compactMap(\.connectionId))
        return connections.filter { ids.contains($0.id) && !usesPortal($0) && !usesFileImport($0) }
    }

    func requestSyncGroup(_ group: UserGroup) {
        requestSyncBatch { [self] in groupSyncConnections(group) }
    }

    /// What a pull-to-refresh starts besides the iCloud merge.
    enum RefreshSyncAction: Equatable {
        case none
        case account(Int64)
        case group(String)
        case all
    }

    /// Context-aware bank sync of the current sidebar selection. Import-only
    /// and portal accounts (interactive login) need no bank sync: the iCloud
    /// merge and reload cover them.
    func refreshSyncAction() -> RefreshSyncAction {
        switch sidebarSelection {
        case .account(let id):
            guard let account = accountsByID[id], !account.isArchived,
                  let c = connections.first(where: { $0.id == account.connectionId }),
                  !usesPortal(c), !usesFileImport(c) else { return .none }
            return .account(id)
        case .userGroup(let uuid) where !uuid.isEmpty:
            return .group(uuid)
        case .documents, .wealth:
            return .none
        default:
            return .all
        }
    }

    /// Pull-to-refresh: merges the iCloud snapshots first (bookings synced on
    /// the Mac), then syncs what the current selection shows. Returns when the
    /// work is done, so the native spinner stays up. While a sync runs it only
    /// waits for it instead of starting a second one.
    func refreshCurrentView() async {
        if let v = ownVault, CloudSyncManager.shared.isEnabled {
            await CloudSyncManager.shared.sync(vault: v)
            reloadAll()
        }
        guard syncingConnectionID == nil else {
            await awaitSyncSettled()
            return
        }
        let action = refreshSyncAction()
        guard action != .none else { return }
        if let hook = refreshSyncOverride { hook(action); return }
        switch action {
        case .none: return
        case .account(let id):
            guard let account = accountsByID[id] else { return }
            requestSyncAccount(account)
        case .group(let uuid):
            guard let g = userGroups.first(where: { $0.uuid == uuid }) else { return }
            requestSyncGroup(g)
        case .all:
            requestSyncAll()
        }
        await awaitSyncSettled()
    }

    /// Waits until the running sync (batch included) is over, or stops at a
    /// PIN / TAN / setup prompt, which only the user can answer.
    private func awaitSyncSettled() async {
        var waited = 0
        // the banking policy check may delay the start a moment
        try? await Task.sleep(for: .milliseconds(150))
        while !Task.isCancelled, waited < 600 {
            // an error alert of "Alle abrufen" waits for the user too (#1041)
            let prompting = pendingLogin != nil || setupRequest != nil || webLoginConnection != nil || activeChallenge != nil
                || errorMessage != nil
            if prompting || (syncingConnectionID == nil && syncQueue.isEmpty) { return }
            try? await Task.sleep(for: .milliseconds(200))
            waited += 1
        }
    }

    /// Runs `logins` one after another; read once the banking policy is fresh.
    private func requestSyncBatch(_ logins: @escaping @MainActor () -> [BankConnection]) {
        guard syncingConnectionID == nil else { return }
        refreshAssetPrices()
        withFreshBankingPolicy { [self] in
            guard syncingConnectionID == nil else { return }
            let batch = logins()
            // Logins the banking policy blocks are skipped and named at the end.
            let blocked = batch.filter { bankingGate(for: $0) != nil }
            let allowed = batch.filter { bankingGate(for: $0) == nil }
            guard let head = allowed.first else {
                if let gate = blocked.first.flatMap({ bankingGate(for: $0) }) {
                    showError(gate.title, gate.detail)
                }
                return
            }
            syncQueue = Array(allowed.dropFirst())
            syncBatchTotal = allowed.count
            syncBatchSkipped = blocked.map { String(localized: "\($0.displayName) (Sicherheitssperre)") }
            syncAccountID = nil
            requestSyncChecked(head)
        }
    }

    /// Position in a running "Alle abrufen" over several logins, e.g. (2, 5).
    var syncBatchProgress: (current: Int, total: Int)? {
        guard syncBatchTotal > 1 else { return nil }
        return (syncBatchTotal - syncQueue.count, syncBatchTotal)
    }

    /// More logins wait after the current one: cancelling a PIN or TAN
    /// prompt then skips this login instead of stopping the whole batch.
    var canSkipInSyncBatch: Bool { !syncQueue.isEmpty }

    /// Cancel button of the TAN sheet. In a batch it drops this login and goes
    /// on with the next one (on the last one it ends the batch with its notice).
    func skipCurrentSync() {
        guard syncBatchTotal > 1, activeTransfer == nil else { cancelSync(); return }
        if let name = activeChallenge?.connection.displayName
            ?? connections.first(where: { $0.id == syncingConnectionID })?.displayName {
            syncBatchSkipped.append(name)
        }
        pollTask?.cancel()
        let s = activeSession
        activeSession = nil
        activeChallenge = nil
        clearSyncTargets()
        syncGeneration &+= 1
        let generation = syncGeneration
        Task {
            // end the bank dialog first: the next login may be the same bank.
            // syncingConnectionID stays set until then, so neither a scheduled
            // sync nor a new "Alle abrufen" can start in between.
            await s?.cancel()
            // "Alle abbrechen", a lock or a new sync meanwhile own the state now:
            // never end a session this skip did not start.
            guard self.syncGeneration == generation else { return }
            self.syncingConnectionID = nil
            self.syncNextInQueue()
        }
    }

    /// "Alle abbrechen": ends the running sync and closes whichever prompt of
    /// the batch is open (PIN, setup, browser login, TAN).
    func cancelAllSync() {
        advanziaWorkflow?.cancel()
        setupRequest = nil
        webLoginConnection = nil   // in a batch only the Trade Republic login
        cancelSync()
    }

    /// "Abbrechen" / "Überspringen" in the PIN sheet.
    func cancelPendingLogin() {
        let c = pendingLogin?.connection
        pendingLogin = nil
        skipPrompt(for: c)
    }

    /// "Abbrechen" / "Überspringen" in the setup sheet (missing settings).
    func cancelSetupRequest() {
        let c = setupRequest?.connection
        setupRequest = nil
        skipPrompt(for: c)
    }

    /// "Abbrechen" / "Überspringen" in the Trade Republic browser login.
    func cancelWebLogin() {
        let c = webLoginConnection
        webLoginConnection = nil
        skipPrompt(for: c)
    }

    /// A prompt before the bank dialog (PIN, setup, browser login) was
    /// cancelled: in a batch go on with the next login, otherwise end it –
    /// a stale queue would otherwise resume after a later single sync.
    private func skipPrompt(for connection: BankConnection?) {
        // A cancelled "Kontoauszüge abrufen" must never carry over to the next
        // sync – it would send that login's PIN for another bank's account.
        clearSyncTargets()
        guard syncBatchTotal > 1 else { resetSyncBatch(); return }
        if let name = connection?.displayName { syncBatchSkipped.append(name) }
        syncNextInQueue()
    }

    func resetSyncBatch() {
        syncQueue = []
        syncBatchTotal = 0
        syncBatchSkipped = []
    }

    /// Errors answered with a prompt (setup form, browser login, FinTS registration)
    /// instead of a failure: in "Alle abrufen" the batch waits for the prompt, whose
    /// "Überspringen" goes on with the next login (#1041). The Trade Republic PIN
    /// prompt (#1128) depends on the login, so `finishSync` handles it before this.
    nonisolated static func errorOpensPrompt(_ error: Error) -> Bool {
        if isMissingProductID(error) { return true }
        guard let ce = error as? CoreError else { return false }
        switch ce {
        case .BrowserLoginRequired, .MissingSetting: return true
        default: return false
        }
    }

    /// Toast suffix of a sync with the comdirect PostBox (#838) or the
    /// Consorsbank Online-Archiv (#1024).
    static func postboxNote(_ s: SyncSummary, _ c: BankConnection) -> String {
        if c.connector == .finTsGeneric, c.bankCode == "76030080" {
            guard let n = s.postboxDocuments else {
                return String(localized: "Online-Archiv nicht abgerufen. Bitte PIN und Freigabe in der Consorsbank App prüfen.")
            }
            return n == 0
                ? String(localized: "keine neuen Dokumente aus dem Online-Archiv")
                : String(localized: "\(n) neue Dokumente aus dem Online-Archiv")
        }
        guard let n = s.postboxDocuments else {
            return String(localized: "PostBox nicht abgerufen. Bitte Client-ID und Client-Secret der comdirect-API prüfen.")
        }
        return n == 0
            ? String(localized: "keine neuen PostBox-Dokumente")
            : String(localized: "\(n) neue PostBox-Dokumente")
    }

    /// Toast suffix: new bookings the bank delivered for archived accounts (not stored).
    static func skippedArchivedNote(_ s: SyncSummary) -> String {
        s.transactionsSkippedArchived == 0 ? "" : " · " + String(localized: "\(s.transactionsSkippedArchived) Buchungen archivierter Konten übersprungen")
    }

    /// Locale of the UI language (Settings › Allgemein › Sprache), which can
    /// differ from the system's – e.g. "und" vs. "and" in a list. Builds
    /// without the translations (`swift run`) fall back to the system locale.
    private static var uiLocale: Locale {
        LanguagePreference.isAvailable ? Locale(identifier: LanguagePreference.runningLanguage) : .current
    }

    private func syncNextInQueue() {
        guard syncQueue.isEmpty else {
            requestSync(syncQueue.removeFirst())
            return
        }
        // batch done: name what was skipped, after the last login's own notice
        if !syncBatchSkipped.isEmpty {
            let note = String(localized: "Übersprungen: \(syncBatchSkipped.formatted(.list(type: .and).locale(Self.uiLocale)))")
            toast = toast.map { $0 + " · " + note } ?? note
            // the notice disappears after a few seconds: VoiceOver reads it out
            AccessibilityNotification.Announcement(note).post()
        }
        resetSyncBatch()
    }

    /// Forgets what the next sync was asked to fetch besides (or instead of)
    /// the login's bookings: statements, PostBox, one account.
    func clearSyncTargets() {
        statementsAccountID = nil
        postboxConnectionID = nil
        syncAccountID = nil
    }

    /// `accountID` when it is an account of exactly this login.
    private func account(_ accountID: Int64?, of connection: BankConnection) -> Int64? {
        accountID.flatMap { accountsByID[$0]?.connectionId == connection.id ? $0 : nil }
    }

    /// Like `account(_:of:)`, and never in a scheduled sync.
    private func ownAccount(_ accountID: Int64?, of connection: BankConnection) -> Int64? {
        quietSyncConnectionID == connection.id ? nil : account(accountID, of: connection)
    }

    func startSync(_ connection: BankConnection, secret: String) {
        guard let vault else { return }
        if connection.connector == .advanzia, !isDemo, !Self.isMagicDemoLogin(connection.loginName) {
            startAdvanzia(connection); return
        }
        if usesPortal(connection) { webLoginConnection = connection; return }
        pendingLogin = nil
        // Statements only for an account of exactly this login: a stale value
        // would hand this login's PIN to another bank.
        if ownAccount(statementsAccountID, of: connection) == nil { statementsAccountID = nil }
        let statementsFor = statementsAccountID
        // "PostBox jetzt abrufen" only for exactly this login, never in an automatic sync
        if postboxConnectionID != connection.id || quietSyncConnectionID == connection.id {
            postboxConnectionID = nil
        }
        let postboxNow = postboxConnectionID != nil
        // kept when another login syncs first (e.g. a scheduled one)
        let accountOnly = ownAccount(syncAccountID, of: connection)
        let quiet = quietSyncConnectionID == connection.id
        syncingConnectionID = connection.id
        syncGeneration &+= 1
        Task {
            var started: SyncSession?
            do {
                let session: SyncSession
                if let accountID = statementsFor {
                    session = try vault.fetchAccountStatements(accountId: accountID, secret: secret, demo: isDemo)
                } else if postboxNow {
                    session = try vault.fetchComdirectPostbox(connectionId: connection.id, secret: secret, demo: isDemo)
                } else if let accountID = accountOnly {
                    session = try vault.beginAccountSync(accountId: accountID, secret: secret, demo: isDemo)
                } else if quiet {
                    // no new approval in the background (e.g. the comdirect PostBox, #838)
                    session = try vault.beginQuietSync(connectionId: connection.id, secret: secret, demo: isDemo)
                } else {
                    session = try vault.beginSync(connectionId: connection.id, secret: secret, demo: isDemo)
                }
                started = session
                self.activeSession = session
                let step = try await session.start()
                guard self.activeSession === session else { return }  // skipped / cancelled meanwhile
                self.handle(step, connection: connection)
            } catch {
                if let started, self.activeSession !== started { return }
                // Kept for the "use GiroCrab's registration" retry of a rejected product ID,
                // so the login is not asked again; finishSync takes it and clears it.
                if statementsFor == nil { self.failedSyncSecret = secret }
                self.finishSync(error: error)
            }
        }
    }

    func handle(_ step: SyncStep, connection: BankConnection) {
        // A scheduled sync never asks for a login: the bank wants a new
        // consent, so stop quietly – the connection view shows it.
        if quietSyncConnectionID == connection.id {
            switch step {
            case .needsChallenge, .stillWaiting:
                logSync(connectionID: connection.id, source: .automatic, outcome: .cancelled,
                        message: String(localized: "Automatischer Abruf übersprungen: Die Bank verlangt eine neue Freigabe."))
                quietSyncConnectionID = nil
                let s = activeSession
                activeSession = nil
                syncingConnectionID = nil
                Task { await s?.cancel() }
                checkConsentExpiry()
                return
            case .finished:
                break
            }
        }
        switch step {
        case .needsChallenge(let c):
            activeChallenge = ActiveChallenge(connection: connection, challenge: c, waiting: false)
            if c.kind == .decoupledPush || c.kind == .progress { schedulePoll(connection: connection, interval: c.pollIntervalSeconds) }
        case .stillWaiting(let c):
            if activeChallenge == nil {
                activeChallenge = ActiveChallenge(connection: connection, challenge: c, waiting: true)
            } else {
                activeChallenge?.challenge = c  // progress steps bring a new text
                activeChallenge?.waiting = true
                activeChallenge?.busy = false
            }
            schedulePoll(connection: connection, interval: c.pollIntervalSeconds)
        case .finished(let s):
            // a scheduled sync of another login leaves a pending "<Konto> abrufen" alone
            if account(syncAccountID, of: connection) != nil { syncAccountID = nil }
            logSync(connectionID: connection.id, source: syncLogSource(for: connection.id), outcome: .ok,
                    message: s.warnings.joined(separator: " / "), imported: Int(s.transactionsImported))
            activeChallenge = nil
            syncingConnectionID = nil
            activeSession = nil
            let statementsFetch = statementsAccountID != nil
            if let accountID = statementsAccountID {
                statementsAccountID = nil
                let before = statementsCountBefore
                let name = connection.displayName
                let warnings = s.warnings
                let report = { [self] (unplaced: Int) in
                    documentsVersion += 1
                    let n = max(0, accountDocuments(accountID).count - before)
                    var note = n > 0
                        ? String(localized: "\(name): \(n) neue Kontoauszüge")
                        : String(localized: "\(name): keine neuen Kontoauszüge. Falls die Bank keine elektronischen Kontoauszüge per FinTS anbietet, lassen sie sich hier nicht abrufen.")
                    // a partial download or a failed placement must not look like a finished fetch
                    if !warnings.isEmpty { note += " · " + warnings.joined(separator: " / ") }
                    if unplaced > 0 { note += " · " + String(localized: "\(unplaced) Dokumente konnten keinem Konto zugeordnet werden.") }
                    toast = note
                }
                if connection.connector == .bhw {
                    // BHW: mailbox PDFs reach their account only once their text was read.
                    Task { @MainActor in
                        report(await self.placeBhwDocuments().failed)
                    }
                } else {
                    report(0)
                }
            } else if postboxConnectionID == connection.id {
                postboxConnectionID = nil
                documentsVersion += 1
                toast = syncNotice(connection, s) + " · " + Self.postboxNote(s, connection)
            } else {
                let quiet = quietSyncConnectionID == connection.id
                // scheduled syncs only speak up when something changed
                if !quiet || s.transactionsImported > 0 || !s.newAccounts.isEmpty || !s.revokedAccounts.isEmpty
                    || !s.warnings.isEmpty {
                    var note = syncNotice(connection, s) + Self.skippedArchivedNote(s)
                    if let n = s.postboxDocuments, n > 0 { note += " · " + Self.postboxNote(s, connection) }
                    toast = note
                }
                if s.postboxDocuments.map({ $0 > 0 }) ?? false { documentsVersion += 1 }
            }
            let wasQuiet = quietSyncConnectionID == connection.id
            quietSyncConnectionID = nil
            if wasQuiet { reloadAllInBackground() } else { reloadAll() }
            // A regular BHW sync can bring mailbox PDFs too (automatic statements).
            if connection.connector == .bhw && !statementsFetch {
                Task { @MainActor in
                    if await self.placeBhwDocuments().placed > 0 { self.documentsVersion += 1 }
                }
            }
            checkPendingAccountSelection()
            syncNextInQueue()
        }
    }

    func schedulePoll(connection: BankConnection, interval: UInt32) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(interval, 1)) * 1_000_000_000)
            guard !Task.isCancelled, let self else { return }
            if let transfer = self.activeTransfer {
                do {
                    let step = try await transfer.poll()
                    guard self.activeTransfer === transfer else { return }  // cancelled meanwhile
                    self.handleTransfer(step, connection: connection)
                } catch {
                    guard self.activeTransfer === transfer else { return }
                    self.finishTransfer(error: error)
                }
                return
            }
            guard let session = self.activeSession else { return }
            do {
                let step = try await session.poll()
                guard self.activeSession === session else { return }  // skipped / cancelled meanwhile
                self.handle(step, connection: connection)
            } catch {
                guard self.activeSession === session else { return }
                self.finishSync(error: error)
            }
        }
    }

    /// Consorsbank Online-Archiv (#1024): the approval dialog's link switches between the
    /// app approval and a typed TAN ("TAN" / "APP", handled in `consorsbank_archive.rs`).
    func switchApproval(_ word: String) {
        pollTask?.cancel()
        submitCode(word)
    }

    func submitCode(_ code: String) {
        if let transfer = activeTransfer, let ch = activeChallenge {
            activeChallenge?.busy = true
            activeChallenge?.error = nil
            Task {
                do {
                    let step = try await transfer.submitCode(code: code)
                    // cancelled while the TAN was on its way: the cancel notice owns the sheet
                    guard self.activeTransfer === transfer else { return }
                    self.handleTransfer(step, connection: ch.connection)
                } catch {
                    guard self.activeTransfer === transfer else { return }
                    self.finishTransfer(error: error)
                }
            }
            return
        }
        guard let session = activeSession, let ch = activeChallenge else { return }
        activeChallenge?.busy = true
        activeChallenge?.error = nil
        Task {
            do {
                let step = try await session.submitCode(code: code)
                // "Überspringen" while the code was on its way: drop the late answer
                guard self.activeSession === session else { return }
                self.handle(step, connection: ch.connection)
            } catch {
                guard self.activeSession === session else { return }
                self.finishSync(error: error)
            }
        }
    }

    func cancelSync() {
        pollTask?.cancel()
        if let t = activeTransfer {
            activeTransfer = nil
            activeChallenge = nil
            transferBusy = false
            transferError = nil
            // A collection run or a batch: its own sheet comes back (lines and rows are still there),
            // and the flag must not make the next single transfer end as a batch or a collection.
            if activeTransferIsDebit {
                activeTransferIsDebit = false
                debitRun?.pin = ""
                debitRun = nil
                mandateError = String(localized: "Einzug abgebrochen – die TAN wurde nicht bestätigt. Bereits freigegebene Dateien bitte unter „Geplant“ prüfen, bevor Sie erneut einreichen.")
                showCollectionSheet = true
                mandatesVersion += 1
                cancelTransferSession(t, confirmedMessage: nil)
                return
            }
            if activeTransferIsBatch {
                activeTransferIsBatch = false
                batchDraft.pin = ""
                transferError = String(localized: "Sammelüberweisung abgebrochen – es wurde nichts gesendet, solange die TAN nicht bestätigt wurde.")
                showBatchSheet = true
                cancelTransferSession(t, confirmedMessage: nil)
                return
            }
            if activeTransferIsStanding {
                // not a transfer: the standing-order sheet comes back, not the transfer form
                cancelStandingOrderRun()
                cancelTransferSession(t, confirmedMessage: nil)
                return
            }
            // The bank has to confirm: a bunq draft stays approvable until it is rejected.
            cancelTransferSession(t, confirmedMessage: String(localized: "Überweisung abgebrochen – es wurde nichts gesendet, solange die TAN nicht bestätigt wurde."))
            return
        }
        let s = activeSession
        if s != nil, let id = syncingConnectionID {
            logSync(connectionID: id, source: syncLogSource(for: id), outcome: .cancelled,
                    message: String(localized: "Vom Benutzer abgebrochen"))
        }
        activeSession = nil
        activeChallenge = nil
        pendingLogin = nil
        syncingConnectionID = nil
        clearSyncTargets()
        syncGeneration &+= 1
        resetSyncBatch()
        Task { await s?.cancel() }
    }

    func finishSync(error: Error) {
        let failedID = quietSyncConnectionID ?? syncingConnectionID
        logSync(connectionID: failedID, source: syncLogSource(for: failedID), outcome: .error, message: describe(error))
        let retrySecret = failedSyncSecret
        failedSyncSecret = nil
        if let quietID = quietSyncConnectionID {
            // scheduled sync: a notice instead of dialogs / login prompts
            quietSyncConnectionID = nil
            pollTask?.cancel()
            activeSession = nil
            activeChallenge = nil
            syncingConnectionID = nil
            let name = connections.first { $0.id == quietID }?.displayName ?? ""
            toast = String(localized: "\(name): automatischer Abruf fehlgeschlagen – \(describe(error))")
            autoSyncStatus.failureDetail = describe(error)
            return
        }
        statementsAccountID = nil
        postboxConnectionID = nil
        // syncAccountID stays: the retry and the prompts below keep to the account
        syncingConnectionIDBeforeError = syncingConnectionID
        pollTask?.cancel()
        activeSession = nil
        activeChallenge = nil
        syncingConnectionID = nil
        // Trade Republic or Kontist on its stored session alone (no PIN given, none stored) whose
        // session expired (#1128): the core asks for the PIN/password, which only the PIN sheet
        // takes (for Trade Republic it also offers the browser login). One typed there that fails
        // stays an error.
        if let ce = error as? CoreError, case .Authentication = ce, retrySecret?.isEmpty ?? true,
           let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }),
           [.tradeRepublic, .kontist].contains(conn.connector), connectionStatus(conn.id)?.hasPin != true {
            toast = describe(error)
            pendingLogin = PendingLogin(connection: conn)
            return
        }
        // "Alle abrufen" (#1041): an error ends only this login. Its alert shows right away;
        // the batch goes on once it is closed ("Überspringen"), retries this login, or ends.
        if syncBatchTotal > 1, !Self.errorOpensPrompt(error),
           let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }) {
            showError(String(localized: "Abruf von „\(conn.displayName)“ fehlgeschlagen"), describe(error)) { [weak self] in
                guard let self, let c = self.connections.first(where: { $0.id == conn.id }) else { return }
                self.requestSync(c)   // the batch goes on after it
            }
            let alternatives = productIDAlternatives(for: conn, error: error, secret: retrySecret)
            if syncQueue.isEmpty {
                errorAlternatives = alternatives
            } else {
                // macOS shows at most three buttons: Überspringen, one retry, Alle abbrechen.
                // A rejected FinTS registration is retried with the other one, not the same.
                if let alt = alternatives.last { errorRetry = nil; errorAlternatives = [alt] }
                errorAlternatives.append((String(localized: "Alle abbrechen"), { [weak self] in self?.cancelAllSync() }))
                errorCloseTitle = String(localized: "Überspringen")
            }
            errorOnClose = { [weak self] in self?.syncNextInQueue() }
            return
        }
        // A prompt (browser login, setup form, retry with GiroCrab's registration) keeps
        // the batch: done, it syncs this login and goes on; cancelled, it skips it.
        if let ce = error as? CoreError, case .BrowserLoginRequired(let m) = ce,
           let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }) {
            toast = m
            webLoginConnection = conn
            return
        }
        if Self.isMissingProductID(error),
           let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }) {
            // Never a setup form for the optional FinTS registration: it would be an empty,
            // seemingly mandatory field. Fall back to GiroCrab's registration (once), else offer the own ID.
            if let vault, recoverFromMissingProductID(vault) {
                requestSync(conn)
                return
            }
            resetSyncBatch()
            showError(String(localized: "Abruf von „\(conn.displayName)“ fehlgeschlagen"),
                      String(localized: "Diese Version enthält keine FinTS-Registrierung von GiroCrab. Tragen Sie Ihre eigene Registrierung ein."))
            errorAlternatives = [(String(localized: "Eigene Registrierung eintragen …"), { [weak self] in self?.enterOwnProductID() })]
            return
        }
        if let ce = error as? CoreError, case .MissingSetting(let key) = ce,
           let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }) {
            var keys = missingSettings(for: conn)
            if !keys.contains(key) { keys.insert(key, at: 0) }
            setupRequest = SetupRequest(connection: conn, keys: keys)
            return
        }
        resetSyncBatch()
        if let conn = connections.first(where: { $0.id == syncingConnectionIDBeforeError }) {
            showError(String(localized: "Abruf von „\(conn.displayName)“ fehlgeschlagen"), describe(error)) { [weak self] in
                guard let self, let c = self.connections.first(where: { $0.id == conn.id }) else { return }
                self.requestSync(c)
            }
            errorAlternatives = productIDAlternatives(for: conn, error: error, secret: retrySecret)
        } else {
            showError(String(localized: "Abruf fehlgeschlagen"), describe(error))
        }
    }
}

extension AppModel {
    /// Opens the account choice for the first login whose new accounts overlap with
    /// another login (#346). No-op while a choice is already shown.
    func checkPendingAccountSelection() {
        guard accountSelectionConnection == nil else { return }
        Task { [weak self] in
            guard let self,
                  let ids = try? await self.vaultOffMain({ try $0.pendingAccountSelectionConnections() }),
                  let first = ids.first,
                  self.accountSelectionConnection == nil else { return }
            self.accountSelectionConnection = self.connections.first { $0.id == first }
        }
    }
}
