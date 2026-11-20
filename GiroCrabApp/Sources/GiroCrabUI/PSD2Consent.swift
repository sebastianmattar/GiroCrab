import Foundation
import GiroCrabCore
#if canImport(UserNotifications)
@preconcurrency import UserNotifications
#endif

/// PSD2 (Enable Banking) consents: expiry display, scheduled background
/// syncs while the app runs, and a notification before a consent expires.
enum ConsentStage: Equatable {
    case none          // no stored consent
    case valid         // more than `warnDays` left
    case expiringSoon  // within `warnDays`
    case expired
}

enum PSD2Consent {
    /// Badge in the connection view from this many days before expiry.
    static let warnDays = 14
    /// System notification from this many days before expiry.
    static let notifyDays = 7

    /// `valid_until` as stored by the connector (RFC 3339, optional fraction).
    static func parse(_ s: String?) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: s) { return d }
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: s)
    }

    /// Whole days left (negative once expired).
    static func daysLeft(until end: Date, now: Date = Date()) -> Int {
        Int((end.timeIntervalSince(now) / 86_400).rounded(.down))
    }

    static func stage(until end: Date?, now: Date = Date(), warnDays: Int = warnDays) -> ConsentStage {
        guard let end else { return .none }
        if end <= now { return .expired }
        return daysLeft(until: end, now: now) < warnDays ? .expiringSoon : .valid
    }

    /// A scheduled sync may run: setting on, consent still valid (it must
    /// never pop up a bank login), and the last sync at least `hours` ago –
    /// manual syncs reset the clock, so the bank's daily limit is respected.
    static func autoSyncDue(lastSyncUnix: Int64?, consentEnd: Date?, hours: Int, now: Date = Date()) -> Bool {
        guard hours > 0, let consentEnd, consentEnd > now else { return false }
        guard let last = lastSyncUnix else { return true }
        return now.timeIntervalSince1970 - TimeInterval(last) >= TimeInterval(hours) * 3600
    }
}

extension AppModel {
    // MARK: consent per connection

    func consentEnd(_ connection: BankConnection) -> Date? {
        guard connection.connector == .enableBanking else { return nil }
        return PSD2Consent.parse(connectionStatus(connection.id)?.consentValidUntil)
    }

    /// "Neu verbinden …": drop the stored consent and start a new one (the bank's
    /// login opens). Accounts and bookings stay; accounts the new consent no
    /// longer includes are marked "Zugriff entzogen" by the sync.
    func reconnect(_ connection: BankConnection) {
        guard let vault else { return }
        do {
            try vault.clearConnectionSession(connectionId: connection.id)
        } catch {
            errorMessage = describe(error)
            return
        }
        requestFullSync(connections.first { $0.id == connection.id } ?? connection)
    }

    // MARK: scheduled sync

    /// Hours between automatic fetches (Einstellungen › Allgemein, `AutoSync.hoursKey`,
    /// 0 = off). Falls back to the former hidden vault setting `sync.psd2_auto_hours`
    /// until the picker has been used once.
    var autoSyncHours: Int {
        if let stored = AppDefaults.store.object(forKey: AutoSync.hoursKey) as? Int {
            return AutoSync.hours(stored: stored)
        }
        return Int(settingEntry("sync.psd2_auto_hours")?.value.trimmingCharacters(in: .whitespaces) ?? "") ?? 0
    }

    func startAutoSync() {
        autoSyncTask?.cancel()
        guard !isDemo else { return }
        autoSyncTask = Task { [weak self] in
            // let the launch / unlock settle first
            try? await Task.sleep(nanoseconds: 60 * 1_000_000_000)
            while !Task.isCancelled {
                self?.autoSyncTick()
                self?.checkConsentExpiry()
                try? await Task.sleep(nanoseconds: 15 * 60 * 1_000_000_000)
            }
        }
    }

    func stopAutoSync() {
        autoSyncTask?.cancel()
        autoSyncTask = nil
    }

    /// Starts at most one quiet sync when nothing else is going on.
    func autoSyncTick(now: Date = Date()) {
        // a transfer waiting for its TAN shows `activeChallenge` / `transferBusy`
        guard vault != nil, !isDemo, syncingConnectionID == nil, activeChallenge == nil,
              !transferBusy, pendingLogin == nil, webLoginConnection == nil,
              setupRequest == nil, syncQueue.isEmpty else { return }
        let hours = autoSyncHours
        guard hours > 0 else { return }
        // Sachwerte: missing gold prices (at most one request a day, none without gold items)
        refreshAssetPrices()
        let defaults = AppDefaults.store
        guard let due = connections.first(where: { c in
            // never a login prompt: PSD2 needs a valid consent, the others a stored PIN or session
            guard !usesPortal(c), !c.allAccountsArchived, bankingGate(for: c) == nil else { return false }
            if c.connector == .enableBanking {
                guard let end = consentEnd(c), end > now else { return false }
            } else {
                guard let st = connectionStatus(c.id), st.hasPin || st.hasSession,
                      missingSettings(for: c).isEmpty else { return false }
            }
            let attempt = defaults.double(forKey: AutoSync.attemptKey(c.id))
            return AutoSync.decide(hours: hours, isUnlocked: vault != nil, lastSyncUnix: c.lastSyncUnix,
                                   lastAttemptUnix: attempt > 0 ? Int64(attempt) : nil, now: now) == .run
        }) else { return }
        // Quiet as well: a blocked login is simply not synced (the banner explains why).
        withFreshBankingPolicy { [self] in
            guard bankingGate(for: due) == nil, syncingConnectionID == nil else { return }
            quietSyncConnectionID = due.id
            defaults.set(now.timeIntervalSince1970, forKey: AutoSync.attemptKey(due.id))
            autoSyncStatus = AutoSyncStatus(lastRun: now, failureDetail: nil)
            startSync(due, secret: "")
        }
    }

    // MARK: expiry notification

    /// Posts one system notification per consent when it expires within
    /// `notifyDays`, and one when it has expired.
    func checkConsentExpiry(now: Date = Date()) {
        guard !isDemo else { return }
        for c in connections where c.connector == .enableBanking {
            guard let raw = connectionStatus(c.id)?.consentValidUntil, let end = PSD2Consent.parse(raw) else { continue }
            let stage = PSD2Consent.stage(until: end, now: now, warnDays: PSD2Consent.notifyDays)
            guard stage == .expiringSoon || stage == .expired else { continue }
            let key = "consentNotice.\(c.id).\(raw).\(stage == .expired ? "expired" : "soon")"
            guard !AppDefaults.store.bool(forKey: key) else { continue }
            AppDefaults.store.set(true, forKey: key)
            let body = stage == .expired
                ? String(localized: "Die PSD2-Zustimmung für \(c.displayName) ist abgelaufen. Bitte in GiroCrab neu verbinden.")
                : String(localized: "Die PSD2-Zustimmung für \(c.displayName) läuft in \(max(0, PSD2Consent.daysLeft(until: end, now: now))) Tagen ab. Bitte in GiroCrab neu verbinden.")
            postNotification(title: String(localized: "Bankzugang erneuern"), body: body)
            toast = body
        }
    }

    private func postNotification(title: String, body: String) {
        #if canImport(UserNotifications)
        // Only inside a real app bundle (not `swift run` / unit tests).
        guard Bundle.main.bundleURL.pathExtension == "app" else { return }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, _ in
            guard granted else { return }
            let content = UNMutableNotificationContent()
            content.title = title
            content.body = body
            UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
        #endif
    }

    // MARK: sync notice

    /// Toast text after a sync: bookings plus accounts that appeared or lost access, and
    /// a pointer to the log when parts failed (#1083; the texts name accounts, so not here).
    func syncNotice(_ connection: BankConnection, _ s: SyncSummary) -> String {
        var parts = [String(localized: "\(connection.displayName): \(s.transactionsImported) neue Buchungen, \(s.transactionsCategorized) kategorisiert")]
        if !s.newAccounts.isEmpty {
            parts.append(String(localized: "Neue Konten: \(s.newAccounts.joined(separator: ", "))"))
        }
        if !s.revokedAccounts.isEmpty {
            parts.append(String(localized: "Kein Zugriff mehr: \(s.revokedAccounts.joined(separator: ", "))"))
        }
        if !s.warnings.isEmpty {
            #if os(macOS)
            parts.append(String(localized: "Nicht alles abgerufen, Details im Abruf-Protokoll"))
            #else
            parts.append(String(localized: "Nicht alles abgerufen"))
            #endif
        }
        return parts.joined(separator: " · ")
    }
}
