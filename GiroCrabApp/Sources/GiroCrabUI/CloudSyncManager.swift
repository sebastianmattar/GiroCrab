import Foundation
import Observation
import SwiftUI
import GiroCrabCore
#if os(iOS)
import UIKit
#endif

@Observable @MainActor
public final class CloudSyncManager {
    public static let shared = CloudSyncManager()

    public enum Location: String { case container, folder }

    private enum K {
        static let enabled = "girocrab.sync.enabled"
        static let location = "girocrab.sync.location"
        static let bookmark = "girocrab.sync.bookmark"
        static let chosen = "girocrab.sync.folderChosen"
        static let last = "girocrab.sync.last"
        static let stats = "girocrab.sync.stats"
        static let seen = "girocrab.sync.seen"
        static let failed = "girocrab.sync.failed"
        static let migrated = "girocrab.sync.folderMigrated"
        static let migratedHashes = "girocrab.sync.folderMigratedHashes"
    }
    /// Legacy plaintext key file of v0.4.0. Never written anymore; removed on sync.
    private let legacyKeyFileName = "tresor.key"
    private let defaults = AppDefaults.store
    /// Modification date of each peer snapshot when it was last merged.
    @ObservationIgnored private var seen: [String: Double]
    /// Same for peers this vault's key didn't open (not retried until they change).
    @ObservationIgnored private var failed: [String: Double]
    @ObservationIgnored private var cachedDeviceID: String?
    @ObservationIgnored private var folderOverride: URL?

    public var isEnabled: Bool {
        didSet {
            defaults.set(isEnabled, forKey: K.enabled)
            if !isEnabled { stopAutoSync() }
            #if os(macOS)
            if isEnabled && folderName == nil {
                ensureDefaultMacICloudFolder()
            }
            #endif
        }
    }
    public var location: Location { didSet { defaults.set(location.rawValue, forKey: K.location) } }
    public private(set) var folderName: String?
    /// "GiroCrab" in iCloud Drive holds foreign files: the previous folder keeps being used.
    public private(set) var folderNotice = false
    /// Another device still writes into the old folder after the migration (older app version).
    public private(set) var legacyFolderWritten = false
    /// iOS: the chosen folder is the former one; choosing "GiroCrab" again joins the new folder.
    public private(set) var repickFolderHint = false
    /// macOS: both folders hold a vault; the previous one stays in use.
    public private(set) var folderBothVaults = false

    /// Whether this app copied the data to "GiroCrab" (set at migration).
    static var migratedByThisApp: Bool { AppDefaults.store.object(forKey: K.migrated) != nil }
    public private(set) var lastSync: Date?
    public private(set) var lastStats: String?
    public private(set) var isSyncing = false
    public private(set) var lastError: String?
    public private(set) var keyMismatch = false
    /// The last merge read at least one snapshot of another device.
    public private(set) var lastMergeFoundSnapshot = false
    /// The user picked the sync folder („Ordner wählen …“): it beats the app's
    /// own iCloud container, so a device with a container can join a device
    /// without one, e.g. the iPhone a Mac that syncs via iCloud Drive (#484).
    public private(set) var folderChosen = false
    /// The sync folder can't be read (macOS: no permission for iCloud Drive yet).
    public private(set) var folderAccessDenied = false

    private init() {
        isEnabled = defaults.bool(forKey: K.enabled)
        folderChosen = defaults.bool(forKey: K.chosen)
        location = .folder
        lastSync = defaults.object(forKey: K.last) as? Date
        lastStats = defaults.string(forKey: K.stats)
        seen = defaults.dictionary(forKey: K.seen) as? [String: Double] ?? [:]
        failed = defaults.dictionary(forKey: K.failed) as? [String: Double] ?? [:]
        let bookmarked = try? resolveBookmark()
        #if os(macOS)
        let legacyBookmark = bookmarked?.lastPathComponent == ICloudFolderMigration.legacyFolderName
        #else
        let legacyBookmark = false
        #endif
        if let url = bookmarked, !(isEnabled && legacyBookmark) {
            folderName = ICloudFolderMigration.displayName(for: url)
            #if os(iOS)
            // iOS only sees the picked folder, not its siblings: it cannot migrate by itself.
            repickFolderHint = isEnabled && Self.folderWasMoved(url)
            #endif
        } else if isEnabled {
            // Only touch iCloud Drive when the user has actually enabled sync.
            #if os(macOS)
            ensureDefaultMacICloudFolder()
            #endif
        }
    }

    /// Whether this build has its own iCloud container (iCloud entitlement).
    public private(set) var containerAvailable = false
    @ObservationIgnored private var containerLookup: Task<URL?, Never>?

    #if os(macOS)
    private var defaultMacICloudURL: URL? {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let cloudDocs = home.appendingPathComponent("Library/Mobile Documents/com~apple~CloudDocs", isDirectory: true)
        guard FileManager.default.fileExists(atPath: cloudDocs.path) else { return nil }
        // Only reached once sync is enabled (init / isEnabled / sync call it gated).
        let r = ICloudFolderMigration.resolve(cloudDocs: cloudDocs, migratedByThisApp: defaults.object(forKey: K.migrated) != nil)
        if folderBothVaults != r.bothHaveVault { folderBothVaults = r.bothHaveVault }
        if r.didMigrate {
            defaults.set(r.legacyHashes, forKey: K.migratedHashes)
            defaults.set(Date(), forKey: K.migrated)
        }
        if folderNotice != r.conflict { folderNotice = r.conflict }
        return r.url
    }

    private static var cloudDocsURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Mobile Documents/com~apple~CloudDocs", isDirectory: true)
    }

    private func ensureDefaultMacICloudFolder() {
        if let dir = defaultMacICloudURL {
            _ = try? setFolder(dir)
            folderName = "iCloud Drive / " + ICloudFolderMigration.displayName(for: dir)
        }
    }
    #endif

    /// Looks (read-only) for writes into the old folder after the migration.
    public func refreshLegacyNotice() async {
        #if os(macOS)
        guard isEnabled, let since = defaults.object(forKey: K.migrated) as? Date else {
            legacyFolderWritten = false
            return
        }
        let docs = Self.cloudDocsURL
        let hashes = defaults.dictionary(forKey: K.migratedHashes) as? [String: String] ?? [:]
        legacyFolderWritten = await Task.detached {
            ICloudFolderMigration.legacyChanged(cloudDocs: docs, since: since, hashes: hashes)
        }.value
        #endif
    }

    /// iOS: whether the marker lies in the (security-scoped) folder `url`.
    nonisolated static func folderWasMoved(_ url: URL) -> Bool {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        return ICloudFolderMigration.hasMovedMarker(in: url)
    }

    /// `chosen`: picked by the user, see `folderChosen`.
    public func setFolder(_ url: URL, chosen: Bool = false) throws {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        #if os(macOS)
        // A folder picked in the open panel: keep the user's grant across launches.
        let scopedData = try? url.bookmarkData(options: [.withSecurityScope], includingResourceValuesForKeys: nil, relativeTo: nil)
        #else
        let scopedData: Data? = nil
        #endif
        if let data = scopedData ?? (try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)) {
            defaults.set(data, forKey: K.bookmark)
        }
        folderName = ICloudFolderMigration.displayName(for: url)
        #if os(iOS)
        repickFolderHint = ICloudFolderMigration.hasMovedMarker(in: url)
        #else
        repickFolderHint = false
        #endif
        location = .folder
        lastError = nil
        folderAccessDenied = false
        if chosen {
            folderChosen = true
            defaults.set(true, forKey: K.chosen)
        }
    }

    /// Back from a chosen folder to the app's own iCloud container (or, without
    /// one, to the default folder).
    public func useContainerAgain() {
        folderChosen = false
        defaults.removeObject(forKey: K.chosen)
        defaults.removeObject(forKey: K.bookmark)
        folderName = nil
        lastError = nil
        folderAccessDenied = false
    }

    /// Merges the other devices' snapshots into `vault`, then writes this
    /// device's own snapshot. Uses the open vault's key: no keychain prompt.
    /// - Parameters:
    ///   - remoteKey: key pasted from another device, for snapshots this
    ///     vault's key doesn't open.
    ///   - mergeOnly: merge but don't write (used while adopting a pasted key).
    public func sync(vault: Vault, remoteKey: Data? = nil, mergeOnly: Bool = false) async {
        // Dev bypass (`tresor-dev.girocrab`, throw-away key): never write into the user's iCloud folder.
        guard !VaultKeyManager.keychainBypassed else { return }
        guard !isSyncing else { return }
        isSyncing = true; lastError = nil; keyMismatch = false; folderAccessDenied = false
        defer { isSyncing = false }
        do {
            let (dir, release) = try await syncDirectory()
            defer { release() }
            let folder = SyncFolder(dir: dir, deviceID: deviceID)
            let legacyKeyFile = dir.appendingPathComponent(legacyKeyFileName)
            let seen = self.seen
            let (result, written) = try await Task.detached { () throws -> (SyncMergeResult, SyncWrite) in
                // Security: never leave the raw vault key next to the vault in iCloud Drive.
                if let e = folder.accessError() { throw Self.folderAccessError(dir, e) }
                Self.removeLegacyKeyFile(legacyKeyFile)
                // Someone is waiting for this sync: give iCloud time to download.
                folder.waitForDownloads(timeout: 30)
                let r = folder.merge(vault: vault, peers: folder.peers(), remoteKey: remoteKey)
                let w = mergeOnly ? SyncWrite() : try folder.write(vault: vault, merged: seen.merging(r.seen) { $1 })
                return (r, w)
            }.value
            record(result, written)
            await refreshLegacyNotice()
            #if os(iOS)
            if location == .folder { repickFolderHint = ICloudFolderMigration.hasMovedMarker(in: dir) }
            #endif
        } catch {
            lastError = error.localizedDescription
            folderAccessDenied = Self.isFolderAccessError(error)
        }
    }

    /// Stores what a merge did: last sync time and summary, the peers'
    /// modification dates, a key mismatch or per-file errors.
    private func record(_ result: SyncMergeResult, _ written: SyncWrite = SyncWrite()) {
        lastMergeFoundSnapshot = !result.stats.isEmpty
        seen.merge(result.seen) { $1 }
        if let m = written.legacyModified { seen[SyncFolder.legacyName] = m }
        failed.merge(result.failed) { $1 }
        for name in result.seen.keys { failed[name] = nil }
        defaults.set(seen, forKey: K.seen)
        defaults.set(failed, forKey: K.failed)
        lastSync = Date()
        defaults.set(lastSync, forKey: K.last)
        if result.stats.isEmpty {
            lastStats = Self.writtenText(location)
        } else {
            let added = result.stats.reduce(UInt32(0)) { $0 + $1.bookingsAdded }
            let updated = result.stats.reduce(UInt32(0)) { $0 + $1.bookingsUpdated }
            let accounts = result.stats.reduce(UInt32(0)) { $0 + $1.accountsAdded }
            lastStats = String(localized: "\(added) neue / \(updated) aktualisierte Buchungen, \(accounts) Konten")
        }
        defaults.set(lastStats, forKey: K.stats)
        if result.keyMismatch {
            keyMismatch = true
            lastError = Self.keyMismatchError.localizedDescription
        } else if !result.errors.isEmpty {
            lastError = result.errors.joined(separator: "\n")
        }
    }

    /// This installation's id in the sync folder's file names.
    private var deviceID: String {
        if let cachedDeviceID { return cachedDeviceID }
        let id = SyncDeviceID.current(defaults: defaults, hardwareID: SyncDeviceID.hardwareID())
        cachedDeviceID = id
        return id
    }

    /// Tests point the manager at a temporary folder; nil restores the user's.
    func setFolderForTesting(_ url: URL?) {
        folderOverride = url
    }

    // Automatic sync without its timer, for tests.
    func prepareAutoSyncForTesting(vault: Vault) {
        autoVault = vault
        autoState = AutoSyncState()
        _ = autoState.tick(changeCount: vault.changeCount(), now: Date())
    }

    func runForTesting(write: Bool, poll: Bool) async {
        guard let autoVault else { return }
        await run(vault: autoVault, write: write, poll: poll)
    }

    func flushForTesting() async {
        guard let autoVault else { return }
        await flush(autoVault)
    }

    var isAutoBusy: Bool { autoBusy }

    // MARK: automatic sync

    @ObservationIgnored private var autoVault: Vault?
    @ObservationIgnored private var onMerged: (@MainActor () -> Void)?
    @ObservationIgnored private var autoState = AutoSyncState()
    @ObservationIgnored private var autoTask: Task<Void, Never>?
    @ObservationIgnored private var autoBusy = false
    /// The automatic run in progress; a flush waits for it instead of being dropped.
    @ObservationIgnored private var inFlight: Task<Void, Never>?

    /// Keeps `vault` in sync while the app runs: writes this device's
    /// snapshot after local changes and merges other devices' snapshots
    /// when they change. `onMerged` runs after a merge that brought changes.
    public func startAutoSync(vault: Vault, onMerged: @escaping @MainActor () -> Void) {
        stopAutoSync()
        guard isEnabled, !VaultKeyManager.keychainBypassed else { return }
        autoVault = vault
        self.onMerged = onMerged
        autoState = AutoSyncState()
        startLoop()
    }

    /// „App zurücksetzen“ (#241): forgets folder, state and bookmark of this device
    /// (the UserDefaults domain is removed right after). Snapshots of other
    /// devices in the folder are left alone.
    public func resetForAppReset() {
        stopAutoSync()
        isEnabled = false
        folderOverride = nil
        folderName = nil
        folderNotice = false
        legacyFolderWritten = false
        repickFolderHint = false
        folderBothVaults = false
        lastSync = nil
        lastStats = nil
        lastError = nil
        keyMismatch = false
        seen = [:]
        failed = [:]
    }

    /// Ends automatic sync; with `flushing`, a pending local change is
    /// written first (e.g. when the vault gets locked).
    public func stopAutoSync(flushing: Bool = false) {
        autoTask?.cancel()
        autoTask = nil
        if flushing, let vault = autoVault {
            Task { await flush(vault) }
        }
        autoVault = nil
        onMerged = nil
    }

    /// The app came to the front or went to the background.
    public func sceneChanged(_ phase: ScenePhase) {
        guard autoVault != nil else { return }
        switch phase {
        case .active:
            autoState.pollSoon()
            if autoTask == nil { startLoop() }
            Task { await autoTick() }
        #if os(iOS)
        case .background:
            // iOS suspends the app: write what's pending, then stop until it's back.
            // The background time covers a run still in progress, too.
            guard let vault = autoVault else { return }
            let background = BackgroundTask(name: "GiroCrab sync")
            autoTask?.cancel()
            autoTask = nil
            Task {
                await flush(vault)
                background.end()
            }
        #endif
        default:
            break
        }
    }

    private func startLoop() {
        autoTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.autoTick()
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }

    private func flush(_ vault: Vault) async {
        while let running = inFlight { await running.value }
        _ = autoState.tick(changeCount: vault.changeCount(), now: Date())
        guard autoState.hasPendingWrite else { return }
        await run(vault: vault, write: true, poll: false)
    }

    private func autoTick() async {
        guard let vault = autoVault, !isSyncing, !autoBusy else { return }
        let actions = autoState.tick(changeCount: vault.changeCount(), now: Date())
        guard !actions.isEmpty else { return }
        await run(vault: vault, write: actions.contains(.write), poll: actions.contains(.poll))
    }

    private func run(vault: Vault, write: Bool, poll: Bool) async {
        guard inFlight == nil else { return }
        // Cleared inside the task, before anyone awaiting it resumes.
        let task = Task {
            await self.perform(vault: vault, write: write, poll: poll)
            self.inFlight = nil
        }
        inFlight = task
        await task.value
    }

    private func perform(vault: Vault, write: Bool, poll: Bool) async {
        guard !autoBusy, let (dir, release) = try? await syncDirectory() as (URL, () -> Void) else { return }
        autoBusy = true
        defer { autoBusy = false; release() }
        let folder = SyncFolder(dir: dir, deviceID: deviceID)
        if let e = await Task.detached(operation: { folder.accessError() }).value {
            lastError = Self.folderAccessError(dir, e).localizedDescription
            folderAccessDenied = true
            return
        }
        folderAccessDenied = false
        let seen = self.seen
        let skip = seen.merging(failed) { $1 }
        let count = vault.changeCount()
        let outcome = await Task.detached { () -> (written: SyncWrite, merge: SyncMergeResult?, error: String?) in
            var written = SyncWrite()
            var error: String?
            if write {
                do { written = try folder.write(vault: vault, merged: seen) } catch let e { error = e.localizedDescription }
            }
            var merge: SyncMergeResult?
            if poll {
                let changed = folder.changedPeers(seen: skip)
                if !changed.isEmpty { merge = folder.merge(vault: vault, peers: changed, remoteKey: nil) }
            }
            return (written, merge, error)
        }.value
        if outcome.written.wrote {
            autoState.wrote(changeCount: count)
            if let m = outcome.written.legacyModified {
                self.seen[SyncFolder.legacyName] = m
                defaults.set(self.seen, forKey: K.seen)
            }
            lastSync = Date()
            defaults.set(lastSync, forKey: K.last)
        }
        if let error = outcome.error { lastError = error }
        if let merge = outcome.merge {
            autoState.merged(changes: merge.stats.reduce(UInt64(0)) { $0 + $1.changes })
            lastError = nil
            record(merge)
            let changed = merge.stats.contains {
                $0.accountsAdded + $0.categoriesAdded + $0.rulesAdded + $0.bookingsAdded + $0.bookingsUpdated + $0.groupsUpdated > 0
            }
            if changed { onMerged?() }
        }
    }

    /// Deletes `tresor.key` (and its iCloud placeholder) from the sync folder.
    nonisolated private static func removeLegacyKeyFile(_ keyFile: URL) {
        let fm = FileManager.default
        let placeholder = keyFile.deletingLastPathComponent()
            .appendingPathComponent("." + keyFile.lastPathComponent + ".icloud")
        for url in [keyFile, placeholder] where fm.fileExists(atPath: url.path) {
            try? fm.removeItem(at: url)
        }
    }

    /// The sync folder can't be read. On macOS that usually means access to
    /// iCloud Drive hasn't been granted, which macOS doesn't always ask for.
    nonisolated private static func folderAccessError(_ dir: URL, _ underlying: Error) -> NSError {
        let name = dir.lastPathComponent
        #if os(macOS)
        let message = String(localized: "GiroCrab darf den Sync-Ordner „\(name)“ nicht lesen. Wählen Sie ihn mit „Ordner wählen …“ aus oder erlauben Sie den Zugriff in den Systemeinstellungen unter Datenschutz & Sicherheit › Dateien und Ordner (iCloud Drive).")
        #else
        let message = String(localized: "GiroCrab kann den Sync-Ordner „\(name)“ nicht lesen. Wählen Sie den Ordner in den Einstellungen unter iCloud Sync neu aus.")
        #endif
        return NSError(domain: "CloudSync", code: 4, userInfo: [NSLocalizedDescriptionKey: message, NSUnderlyingErrorKey: underlying])
    }

    nonisolated private static func isFolderAccessError(_ error: Error) -> Bool {
        let e = error as NSError
        return e.domain == "CloudSync" && e.code == 4
    }

    nonisolated private static var keyMismatchError: NSError {
        NSError(domain: "CloudSync", code: 3, userInfo: [NSLocalizedDescriptionKey: String(localized: "Der Tresor in iCloud nutzt einen anderen Schlüssel. Aktivieren Sie den iCloud-Schlüsselbund oder fügen Sie den Tresor-Schlüssel von Ihrem anderen Gerät ein (Einstellungen › iCloud Sync).")])
    }

    private func resolveBookmark() throws -> URL? {
        guard let data = defaults.data(forKey: K.bookmark) else { return nil }
        var stale = false
        #if os(macOS)
        if let url = try? URL(resolvingBookmarkData: data, options: [.withSecurityScope], relativeTo: nil, bookmarkDataIsStale: &stale) {
            return url
        }
        #endif
        if let url = try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale) {
            return url
        }
        defaults.removeObject(forKey: K.bookmark)
        return nil
    }

    /// Status after writing, naming the place that was written.
    nonisolated static func writtenText(_ location: Location) -> String {
        location == .container
            ? String(localized: "Tresor in iCloud aktualisiert")
            : String(localized: "Tresor in iCloud Drive aktualisiert")
    }

    /// Where to sync, in this order: a test folder; a folder the user picked
    /// (`folderChosen`); the app's own iCloud container (needs no permission
    /// and is the same on every device with this app); a folder set up before;
    /// the default folder in iCloud Drive.
    nonisolated static func syncLocation(override: URL?, chosen: URL? = nil, container: URL?, bookmark: URL?, fallback: URL?) -> (url: URL, location: Location)? {
        if let override { return (override, .folder) }
        if let chosen { return (chosen, .folder) }
        if let container { return (container, .container) }
        if let bookmark { return (bookmark, .folder) }
        if let fallback { return (fallback, .folder) }
        return nil
    }

    /// The `Documents` folder of the app's iCloud container, or nil when
    /// the build has no iCloud entitlement (ad-hoc builds, CI). Looked up
    /// once, off the main thread: Apple documents the call as blocking.
    private func ubiquityContainer() async -> URL? {
        if containerLookup == nil {
            containerLookup = Task.detached {
                guard let base = FileManager.default.url(forUbiquityContainerIdentifier: nil) else { return nil }
                let docs = base.appendingPathComponent("Documents", isDirectory: true)
                try? FileManager.default.createDirectory(at: docs, withIntermediateDirectories: true)
                return docs
            }
        }
        let url = await containerLookup?.value
        containerAvailable = url != nil
        return url
    }

    private func syncDirectory() async throws -> (URL, () -> Void) {
        let container = folderOverride == nil ? await ubiquityContainer() : nil
        var bookmark: URL?
        var release: () -> Void = {}
        if folderOverride == nil, container == nil || folderChosen, let url = try resolveBookmark() {
            let ok = url.startAccessingSecurityScopedResource()
            bookmark = url
            release = { if ok { url.stopAccessingSecurityScopedResource() } }
            #if os(macOS)
            if !ok && !FileManager.default.isReadableFile(atPath: url.path) {
                defaults.removeObject(forKey: K.bookmark)
                bookmark = nil
                release = {}
            }
            #endif
        }
        var fallback: URL?
        #if os(macOS)
        if folderOverride == nil, container == nil, bookmark == nil { fallback = defaultMacICloudURL }
        #endif
        guard let choice = Self.syncLocation(override: folderOverride, chosen: folderChosen ? bookmark : nil, container: container, bookmark: bookmark, fallback: fallback) else {
            throw NSError(domain: "CloudSync", code: 2, userInfo: [NSLocalizedDescriptionKey: String(localized: "Bitte einmalig den Ordner „GiroCrab“ in iCloud Drive auswählen.")])
        }
        location = choice.location
        if choice.location == .container {
            folderName = "iCloud (GiroCrab)"
        } else if choice.url == fallback {
            folderName = "iCloud Drive / " + ICloudFolderMigration.displayName(for: choice.url)
        }
        return (choice.url, release)
    }
}

#if os(iOS)
/// Background time for the last write before iOS suspends the app; ended
/// when the write is done or when the time runs out, whichever comes first.
@MainActor
private final class BackgroundTask {
    private var id = UIBackgroundTaskIdentifier.invalid

    init(name: String) {
        id = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
            self?.end()
        }
    }

    func end() {
        guard id != .invalid else { return }
        UIApplication.shared.endBackgroundTask(id)
        id = .invalid
    }
}
#endif
