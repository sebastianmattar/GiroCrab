import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - PDF statements & document folder (GitHub #57)

    private static let mirrorDirKey = "documents.mirrorDir"

    /// Suggested mirror folder: iCloud Drive/GiroCrab/Kontoauszüge (if iCloud Drive exists).
    static var suggestedMirrorURL: URL? {
        #if os(macOS)
        let docs = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Mobile Documents/com~apple~CloudDocs", isDirectory: true)
        guard FileManager.default.fileExists(atPath: docs.path) else { return nil }
        return docs.appendingPathComponent("GiroCrab/Kontoauszüge", isDirectory: true)
        #else
        return nil
        #endif
    }

    /// Turns the document folder on (all documents are copied once) or off.
    func setDocumentMirror(_ url: URL?) {
        if let url {
            documentMirrorPath = url.path
            AppDefaults.store.set(url.path, forKey: Self.mirrorDirKey)
            // Creating the folder (iCloud Drive can be slow) and resetting the
            // copy markers (vault mutex) stay off the main thread. `mirrorRunning`
            // keeps syncs from starting a copy in between.
            let vault = self.vault
            mirrorRunning = true
            Task { @MainActor [weak self] in
                let failure: Error? = await Task.detached(priority: .userInitiated) { () -> Error? in
                    try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
                    do { try vault?.resetDocumentMirror(); return nil } catch { return error }
                }.value
                guard let self else { return }
                mirrorRunning = false
                if let failure { errorMessage = describe(failure) }
                // Switched off or changed again while resetting: nothing to copy here.
                guard documentMirrorPath == url.path else { return }
                mirrorDocumentsIfEnabled()
            }
        } else {
            documentMirrorPath = nil
            documentMirrorStatus = nil
            AppDefaults.store.removeObject(forKey: Self.mirrorDirKey)
        }
    }

    /// Copies documents that are not in the folder yet (after syncs, imports, drops).
    func mirrorDocumentsIfEnabled() {
        guard let vault, let dir = documentMirrorPath, !isDemo, !mirrorRunning else { return }
        mirrorRunning = true
        Task { @MainActor [weak self] in
            let result: Result<UInt32, Error> = await Task.detached(priority: .utility) {
                Result { try vault.mirrorNewDocuments(destDir: dir) }
            }.value
            guard let self else { return }
            mirrorRunning = false
            switch result {
            case .success(let n):
                let time = Date().formatted(date: .omitted, time: .shortened)
                documentMirrorStatus = n == 0
                    ? String(localized: "Aktuell (geprüft \(time))")
                    : String(localized: "\(n) Dokument(e) gesichert (\(time))")
            case .failure(let e):
                documentMirrorStatus = describe(e)
            }
        }
    }

    func autoStatements(_ account: Account) -> Bool {
        guard let vault else { return false }
        return (try? vault.autoStatementsEnabled(accountId: account.id)) ?? false
    }

    func setAutoStatements(_ account: Account, _ on: Bool) {
        guard let vault else { return }
        runCore { try vault.setAutoStatements(accountId: account.id, enabled: on) }
    }

    /// comdirect via FinTS: also fetch the PostBox through comdirect's API (#838).
    func comdirectPostboxEnabled(_ connectionID: Int64) -> Bool {
        (try? vault?.comdirectPostboxEnabled(connectionId: connectionID)) ?? false
    }

    func setComdirectPostbox(_ connectionID: Int64, _ on: Bool) {
        guard let vault else { return }
        runCore { try vault.setComdirectPostbox(connectionId: connectionID, enabled: on) }
    }

    /// "PostBox jetzt abrufen": syncs the login with the PostBox, however recent the last fetch.
    func fetchComdirectPostbox(_ connection: BankConnection) {
        guard syncingConnectionID == nil else { return }
        postboxConnectionID = connection.id
        requestSync(connection)
    }

    // MARK: - Belege & PostBox

    func accountDocuments(_ accountID: Int64) -> [DocumentInfo] {
        _ = documentsVersion
        let f = DocumentFilter(accountId: accountID, docType: nil, search: nil, unlinkedOnly: false, connectionId: nil)
        return (try? vault?.listAllDocuments(filter: f)) ?? []
    }

    /// Connections whose bank delivers documents but whose retrieval is switched off, for the whole
    /// list or (`accountFilter`) the one account on screen. "Belege & PostBox" explains why nothing new comes.
    func connectionsWithDocumentsOff(accountFilter: Int64?, connectionFilter: Int64? = nil) -> [BankConnection] {
        var ids: Set<Int64>?
        if let accountFilter {
            guard accountFilter != Self.documentsWithoutAccount else { return [] }
            guard let cid = accountsByID[accountFilter]?.connectionId else { return [] }
            ids = [cid]
        } else if let connectionFilter {
            guard connectionFilter != Self.documentsWithoutAccount else { return [] }
            ids = [connectionFilter]
        }
        return connections.filter { c in
            (ids?.contains(c.id) ?? true) && documentsOffConnectionIDs.contains(c.id)
        }
    }

    func canFetchStatements(_ account: Account) -> Bool {
        guard let c = account.connector, account.kind != .loan else { return false }
        // switched off per connection (bank connection dialog): the core would refuse it
        if let cid = account.connectionId, documentsOffConnectionIDs.contains(cid) { return false }
        return [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric, .easybank, .fordMoney, .opelDirektbank, .ffb].contains(c)
    }

    /// Explicit "Kontoauszüge abrufen": regular syncs never fetch statements (speed, no extra TAN).
    func fetchStatements(for account: Account) {
        guard syncingConnectionID == nil,
              let cid = account.connectionId,
              let connection = connections.first(where: { $0.id == cid }) else { return }
        statementsAccountID = account.id
        statementsCountBefore = accountDocuments(account.id).count
        requestSync(connection)
    }

    /// Private folder (0700) for the decrypted QuickLook copies.
    nonisolated static var quickLookDirectory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("GiroCrab-QuickLook", isDirectory: true)
    }

    /// Deletes every decrypted QuickLook copy (lock, quit, next launch).
    public nonisolated static func removeQuickLookFiles() {
        try? FileManager.default.removeItem(at: quickLookDirectory)
    }

    /// Quit: the decrypted copies go too (launch cleans up after a crash). Both
    /// entry points (SwiftPM and Xcode target) share this, so it lives here.
    static let quickLookCleanupObserver: NSObjectProtocol = {
        #if os(macOS)
        let name = NSApplication.willTerminateNotification
        #else
        let name = UIApplication.willTerminateNotification
        #endif
        return NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { _ in
            AppModel.removeQuickLookFiles()
        }
    }()

    /// Creates the QuickLook folder owner-only; refuses a symlink or a
    /// folder of another user in its place.
    private nonisolated static func preparedQuickLookDirectory() -> URL? {
        let fm = FileManager.default
        let dir = quickLookDirectory
        var st = Darwin.stat()
        if lstat(dir.path, &st) == 0 {
            guard (st.st_mode & S_IFMT) == S_IFDIR, st.st_uid == getuid() else { return nil }
            if st.st_mode & 0o077 != 0 { chmod(dir.path, 0o700) }
            return dir
        }
        do {
            try fm.createDirectory(at: dir, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            return dir
        } catch {
            return nil
        }
    }

    func documents(for booking: Booking) -> [DocumentInfo] {
        _ = documentsVersion
        return (try? vault?.listDocumentsForBooking(bookingId: booking.id)) ?? []
    }

    func allDocuments(search: String = "", unlinkedOnly: Bool = false, accountId: Int64? = nil, connectionId: Int64? = nil, scope: DocumentsScope = .all) -> [DocumentInfo] {
        _ = documentsVersion
        let withoutAccount = accountId == Self.documentsWithoutAccount || connectionId == Self.documentsWithoutAccount
        let f = DocumentFilter(accountId: accountId == Self.documentsWithoutAccount ? nil : accountId, docType: nil, search: search.isEmpty ? nil : search,
                               unlinkedOnly: unlinkedOnly, connectionId: connectionId == Self.documentsWithoutAccount ? nil : connectionId)
        return ((try? vault?.listAllDocuments(filter: f)) ?? [])
            .filter { scope.includes($0) && (!withoutAccount || $0.accountId == nil) }
    }

    /// Bank connections to offer in the "Belege & PostBox" filter: those with at least one account.
    var documentsFilterConnections: [BankConnection] {
        let used = Set(accountsByID.values.compactMap(\.connectionId))
        return connections.filter { used.contains($0.id) }
    }

    /// The account menu of "Belege & PostBox", narrowed to the chosen bank connection (#1042).
    var documentsAccountSections: [AccountMenuSection] {
        guard let cid = documentsConnectionFilter, cid != Self.documentsWithoutAccount else { return accountMenuSections }
        return accountMenuSections.compactMap { section in
            let accounts = section.accounts.filter { $0.connectionId == cid }
            return accounts.isEmpty ? nil : AccountMenuSection(id: section.id, title: section.title, accounts: accounts)
        }
    }

    /// Picks the bank connection of the document list. An account filter that does not belong to it
    /// (or "Ohne Konto") is reset, otherwise the list would be empty.
    func selectDocumentsConnection(_ id: Int64?) {
        documentsConnectionFilter = id
        guard let id, let account = documentsAccountFilter else { return }
        if id == Self.documentsWithoutAccount || accountsByID[account]?.connectionId != id {
            documentsAccountFilter = nil
        }
    }

    func documentData(_ doc: DocumentInfo) -> Data? {
        try? vault?.getDocumentBytes(docId: doc.id)
    }

    /// Marks documents read or unread (opened in the list, or by hand).
    func markDocumentsSeen(_ ids: [Int64], seen: Bool) {
        guard let vault, !ids.isEmpty else { return }
        runCore {
            _ = try vault.markDocumentsSeen(docIds: ids, seen: seen)
            unseenDocumentCount = Int(try vault.unseenDocumentCount())
        }
        documentsVersion += 1
    }

    /// Marks every unread document read (sidebar: „Belege & PostBox“).
    func markAllDocumentsSeen() {
        guard let vault else { return }
        runCore {
            _ = try vault.markAllDocumentsSeen(accountId: nil)
            unseenDocumentCount = Int(try vault.unseenDocumentCount())
        }
        documentsVersion += 1
    }

    private func documentsChanged() {
        documentsVersion += 1
        reloadBookings()
    }

    /// Dropped onto a booking row / the inspector drop zone. Takes files only:
    /// a dragged booking (`BookingDrag`) and a web link also arrive as URLs.
    /// False when there is no file to attach.
    @discardableResult
    func attach(_ urls: [URL], to booking: Booking) -> Bool {
        let urls = urls.filter(\.isFileURL)
        guard let vault, !urls.isEmpty else { return false }
        var n = 0
        for url in urls {
            let ok = url.startAccessingSecurityScopedResource()
            defer { if ok { url.stopAccessingSecurityScopedResource() } }
            guard let data = try? Data(contentsOf: url) else { continue }
            runCore { _ = try vault.attachDocumentToBooking(bookingId: booking.id, fileName: url.lastPathComponent, bytes: data); n += 1 }
        }
        if n > 0 { toast = String(localized: "\(n) Belege an „\(booking.merchantName)“ angehängt") }
        documentsChanged()
        return true
    }

    /// Dropped anywhere: Rust matches amount/date/merchant (with PDFKit/Vision text as hint).
    /// Files only, as in `attach`; false when there is none.
    @discardableResult
    func dropAnywhere(_ urls: [URL]) -> Bool {
        let urls = urls.filter(\.isFileURL)
        guard !urls.isEmpty, let pin = pinVault() else { return false }
        Task {
            var linked: [String] = []
            var unmatched = 0
            var failure: Error?
            for url in urls {
                let ok = url.startAccessingSecurityScopedResource()
                defer { if ok { url.stopAccessingSecurityScopedResource() } }
                guard let data = try? Data(contentsOf: url) else { continue }
                let fileName = url.lastPathComponent
                let text = await ReceiptText.extract(data: data, fileName: fileName)
                do {
                    // Parses the PDF and scores every booking: too slow for the main thread.
                    if let hit = try await pin.run({ try $0.autoMatchDocument(fileName: fileName, bytes: data, textHint: text) }) {
                        linked.append(hit)
                    } else {
                        unmatched += 1
                    }
                } catch {
                    failure = error
                }
            }
            guard isCurrent(pin) else { return }
            if let failure {
                showError(String(localized: "Beleg konnte nicht zugeordnet werden"), describe(failure))
            }
            if !linked.isEmpty {
                toast = String(localized: "Beleg zugeordnet: \(linked.joined(separator: ", "))")
            } else if unmatched > 0 {
                toast = String(localized: "Kein passender Umsatz gefunden – in „Belege & PostBox“ abgelegt")
            }
            documentsChanged()
        }
        return true
    }

    func deleteDocument(_ doc: DocumentInfo) {
        guard let vault else { return }
        runCore { try vault.deleteDocument(docId: doc.id) }
        documentsChanged()
    }

    func linkDocument(_ doc: DocumentInfo, to booking: Booking?) {
        guard let vault else { return }
        var linked = false
        runCore { try vault.linkDocument(docId: doc.id, bookingId: booking?.id); linked = true }
        documentsChanged()
        if linked && booking == nil { toast = String(localized: "Zuordnung aufgehoben") }
    }

    /// Moves a document to another account, or to none (#838).
    func setDocumentAccount(_ doc: DocumentInfo, _ accountID: Int64?) {
        guard let vault else { return }
        runCore { try vault.setDocumentAccount(docId: doc.id, accountId: accountID) }
        documentsChanged()
    }

    /// Writes the document to a temp file and shows it in QuickLook.
    func quickLook(_ doc: DocumentInfo) {
        guard let url = writeTemporaryFile(for: doc) else { return }
        quickLookURL = url
    }

    #if os(macOS)
    /// Writes the document to the temp directory and reveals it in Finder.
    func showInFinder(_ doc: DocumentInfo) {
        guard let url = writeTemporaryFile(for: doc) else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    /// Writes the document to the temp directory and copies the file path to the pasteboard
    /// (without opening Finder or the document). The copy is deleted on lock and quit.
    func copyDocumentPath(_ doc: DocumentInfo) {
        guard let url = writeTemporaryFile(for: doc) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url.path, forType: .string)
        toast = String(localized: "Pfad kopiert")
    }
    #endif

    /// Decrypts the document into an owner-only temp file; nil when it cannot be read or written.
    private func writeTemporaryFile(for doc: DocumentInfo) -> URL? {
        guard let data = documentData(doc) else { return nil }
        let ext = ["image/png": "png", "image/jpeg": "jpg", "image/heic": "heic"][doc.mimeType] ?? "pdf"
        _ = Self.quickLookCleanupObserver
        guard let dir = Self.preparedQuickLookDirectory() else { return nil }
        let url = dir.appendingPathComponent("GiroCrab-Beleg-\(doc.id).\(ext)")
        // Owner-only from the first byte: O_EXCL + O_NOFOLLOW, mode 0600.
        try? FileManager.default.removeItem(at: url)
        let fd = Darwin.open(url.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { return nil }
        let written = data.withUnsafeBytes { buf -> Bool in
            var off = 0
            while off < buf.count {
                let n = Darwin.write(fd, buf.baseAddress! + off, buf.count - off)
                if n <= 0 { return false }
                off += n
            }
            return true
        }
        Darwin.close(fd)
        guard written else { try? FileManager.default.removeItem(at: url); return nil }
        return url
    }

    /// Exports the whole collection, or only the documents with `ids` (the list the user filtered).
    /// The year folders go straight into `folder`, or into `subfolder` inside it (the account).
    func exportAllDocuments(to folder: URL, ids: [Int64]? = nil, subfolder: String? = nil) {
        guard let vault else { return }
        let ok = folder.startAccessingSecurityScopedResource()
        defer { if ok { folder.stopAccessingSecurityScopedResource() } }
        runCore {
            let n = try vault.exportDocuments(destDir: folder.path, docIds: ids ?? [], subfolder: subfolder ?? "")
            let target = subfolder.map { "\(folder.lastPathComponent)/\($0)" } ?? folder.lastPathComponent
            toast = String(localized: "\(n) Belege nach „\(target)“ exportiert")
        }
    }
}
