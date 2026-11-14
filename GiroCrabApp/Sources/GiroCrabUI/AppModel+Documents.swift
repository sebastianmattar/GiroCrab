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
            UserDefaults.standard.set(url.path, forKey: Self.mirrorDirKey)
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
            UserDefaults.standard.removeObject(forKey: Self.mirrorDirKey)
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

    // MARK: - Belege & PostBox

    func accountDocuments(_ accountID: Int64) -> [DocumentInfo] {
        _ = documentsVersion
        let f = DocumentFilter(accountId: accountID, docType: nil, search: nil, unlinkedOnly: false)
        return (try? vault?.listAllDocuments(filter: f)) ?? []
    }

    func canFetchStatements(_ account: Account) -> Bool {
        guard let c = account.connector, account.kind != .loan else { return false }
        return [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric].contains(c)
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

    func allDocuments(search: String = "", unlinkedOnly: Bool = false, accountId: Int64? = nil) -> [DocumentInfo] {
        _ = documentsVersion
        let f = DocumentFilter(accountId: accountId, docType: nil, search: search.isEmpty ? nil : search, unlinkedOnly: unlinkedOnly)
        return (try? vault?.listAllDocuments(filter: f)) ?? []
    }

    func documentData(_ doc: DocumentInfo) -> Data? {
        try? vault?.getDocumentBytes(docId: doc.id)
    }

    private func documentsChanged() {
        documentsVersion += 1
        reloadBookings()
    }

    /// Dropped onto a booking row / the inspector drop zone.
    func attach(_ urls: [URL], to booking: Booking) {
        guard let vault else { return }
        var n = 0
        for url in urls {
            let ok = url.startAccessingSecurityScopedResource()
            defer { if ok { url.stopAccessingSecurityScopedResource() } }
            guard let data = try? Data(contentsOf: url) else { continue }
            runCore { _ = try vault.attachDocumentToBooking(bookingId: booking.id, fileName: url.lastPathComponent, bytes: data); n += 1 }
        }
        if n > 0 { toast = String(localized: "\(n) Belege an „\(booking.merchantName)“ angehängt") }
        documentsChanged()
    }

    /// Dropped anywhere: Rust matches amount/date/merchant (with PDFKit/Vision text as hint).
    func dropAnywhere(_ urls: [URL]) {
        guard let vault else { return }
        Task {
            var linked: [String] = []
            var unmatched = 0
            for url in urls {
                let ok = url.startAccessingSecurityScopedResource()
                defer { if ok { url.stopAccessingSecurityScopedResource() } }
                guard let data = try? Data(contentsOf: url) else { continue }
                let text = await ReceiptText.extract(data: data, fileName: url.lastPathComponent)
                do {
                    if let hit = try vault.autoMatchDocument(fileName: url.lastPathComponent, bytes: data, textHint: text) {
                        linked.append(hit)
                    } else {
                        unmatched += 1
                    }
                } catch {
                    showError(String(localized: "Beleg konnte nicht zugeordnet werden"), describe(error))
                }
            }
            if !linked.isEmpty {
                toast = String(localized: "Beleg zugeordnet: \(linked.joined(separator: ", "))")
            } else if unmatched > 0 {
                toast = String(localized: "Kein passender Umsatz gefunden – in „Belege & PostBox“ abgelegt")
            }
            documentsChanged()
        }
    }

    func deleteDocument(_ doc: DocumentInfo) {
        guard let vault else { return }
        runCore { try vault.deleteDocument(docId: doc.id) }
        documentsChanged()
    }

    func linkDocument(_ doc: DocumentInfo, to booking: Booking?) {
        guard let vault else { return }
        runCore { try vault.linkDocument(docId: doc.id, bookingId: booking?.id) }
        documentsChanged()
    }

    /// Writes the document to a temp file and shows it in QuickLook.
    func quickLook(_ doc: DocumentInfo) {
        guard let data = documentData(doc) else { return }
        let ext = ["image/png": "png", "image/jpeg": "jpg", "image/heic": "heic"][doc.mimeType] ?? "pdf"
        _ = Self.quickLookCleanupObserver
        guard let dir = Self.preparedQuickLookDirectory() else { return }
        let url = dir.appendingPathComponent("GiroCrab-Beleg-\(doc.id).\(ext)")
        // Owner-only from the first byte: O_EXCL + O_NOFOLLOW, mode 0600.
        try? FileManager.default.removeItem(at: url)
        let fd = Darwin.open(url.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { return }
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
        guard written else { try? FileManager.default.removeItem(at: url); return }
        quickLookURL = url
    }

    func exportAllDocuments(to folder: URL) {
        guard let vault else { return }
        let ok = folder.startAccessingSecurityScopedResource()
        defer { if ok { folder.stopAccessingSecurityScopedResource() } }
        runCore {
            let n = try vault.exportAllDocuments(destDir: folder.path)
            toast = String(localized: "\(n) Belege nach „\(folder.lastPathComponent)/GiroCrab-Belege“ exportiert")
        }
    }
}
