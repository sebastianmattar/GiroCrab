import SwiftUI
import ImageIO
import UniformTypeIdentifiers
import GiroCrabCore
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Hilfe › Problem melden … (GiroCrab/GiroCrab#301): a screenshot of the main
/// window with personal data drawn as placeholders, a scrubbed diagnostics
/// block, and a prefilled GitHub issue. Nothing leaves the device until the
/// user pastes the picture and submits the issue in the browser.
enum BugReport {
    static let newIssue = URL(string: "https://github.com/GiroCrab/GiroCrab/issues/new")!
    /// GitHub answers longer `issues/new` URLs with an error; measured on the
    /// percent-encoded URL (umlauts and "·" take 6 bytes there).
    static let maxURLLength = 7500
    /// Abrufprotokoll entries in the diagnostics.
    static let logEntries: UInt32 = 15

    // MARK: Screenshot

    enum CaptureResult {
        case image(CGImage)
        /// No main window showing the vault (closed, minimized, locked), or
        /// the redacted view was not confirmed as built: no picture rather
        /// than an unredacted one.
        case noWindow
        /// A newer capture took over; keep whatever is shown.
        case superseded
    }

    @MainActor private static var captureToken = 0

    /// Stands in for search text in the captured window.
    static let searchPlaceholder = "•••"

    #if os(macOS)
    typealias PlatformWindow = NSWindow
    #else
    typealias PlatformWindow = UIWindow
    #endif

    /// Trackers of the windows currently showing `MainSplitView` – the only
    /// view that gets `reportRedaction`. Any other window (Hilfe,
    /// Abruf-Protokoll, a locked main window) is never captured.
    @MainActor private static let trackers = NSHashTable<MainWindowTracker.TrackingView>.weakObjects()

    @MainActor static func track(_ tracker: MainWindowTracker.TrackingView) { trackers.add(tracker) }
    @MainActor static func untrack(_ tracker: MainWindowTracker.TrackingView) { trackers.remove(tracker) }

    /// The redaction `window`'s views were last built with (per window: on a
    /// Mac or an iPad several main windows can be open).
    @MainActor
    static func renderedRedaction(in window: PlatformWindow) -> ReportRedaction {
        trackers.allObjects.first { $0.window === window }?.renderedRedaction ?? []
    }

    /// How long a capture waits for the redacted views to be built.
    static let renderTimeout: Duration = .seconds(2)

    /// Redraws the main window with `redaction`, captures it and restores the
    /// normal view. A later call wins: an earlier one that is still waiting
    /// returns `.superseded`, so it can never reset the redaction under a
    /// newer capture.
    @MainActor
    static func captureMainWindow(model: AppModel, redaction: ReportRedaction,
                                  settle: Duration = .milliseconds(180)) async -> CaptureResult {
        captureToken += 1
        let token = captureToken
        model.reportRedaction = redaction
        defer { if captureToken == token { model.reportRedaction = [] } }
        // Lets SwiftUI lay out and draw the placeholders (lists need more than one frame).
        try? await Task.sleep(for: settle)
        // Fail closed: capture only once the views of the very window that is
        // captured report the redaction (a minimized window, a second main
        // window or a slow machine may not have rebuilt them yet).
        guard !Task.isCancelled, captureToken == token else { return .superseded }
        guard let window = mainWindow() else { return .noWindow }
        let rendered = { renderedRedaction(in: window) }
        let deadline = ContinuousClock.now + renderTimeout
        // `isSuperset`: `--redact` may add more than this capture asked for.
        while !rendered().isSuperset(of: redaction), ContinuousClock.now < deadline, !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(30))
        }
        guard !Task.isCancelled, captureToken == token, model.reportRedaction == redaction else { return .superseded }
        guard rendered().isSuperset(of: redaction) else { return .noWindow }
        return snapshot(window, hideSearch: redaction.contains(.personal)).map(CaptureResult.image) ?? .noWindow
    }

    #if os(macOS)
    /// The frontmost window that shows the vault. A minimized or hidden one
    /// does not count: it may not redraw before the capture.
    @MainActor
    static func mainWindow() -> NSWindow? {
        guard let app = NSApp else { return nil } // nil in unit tests
        let windows = trackers.allObjects.compactMap(\.window)
            .filter { $0.contentView != nil && $0.isVisible && !$0.isMiniaturized }
        return app.orderedWindows.first { windows.contains($0) }
    }


    /// `hideSearch`: search fields are drawn by AppKit, so `.sensitive()`
    /// cannot cover them. Every `NSSearchField` in the window (toolbar or
    /// not, whichever way SwiftUI hosts `.searchable`) shows a placeholder for
    /// the snapshot only; setting `stringValue` sends no action or
    /// text-change notification, so the search binding and the list stay.
    @MainActor
    private static func snapshot(_ window: NSWindow, hideSearch: Bool) -> CGImage? {
        guard window.isVisible, !window.isMiniaturized else { return nil }
        let fields = hideSearch ? searchFields(in: window) : []
        let saved = fields.map(\.stringValue)
        for f in fields where !f.stringValue.isEmpty { f.stringValue = searchPlaceholder }
        defer { for (f, text) in zip(fields, saved) where f.stringValue != text { f.stringValue = text } }
        return WindowScreenshot.bitmap(of: window)?.cgImage
    }

    @MainActor
    static func searchFields(in window: NSWindow) -> [NSSearchField] {
        func walk(_ v: NSView) -> [NSSearchField] { ((v as? NSSearchField).map { [$0] } ?? []) + v.subviews.flatMap(walk) }
        let toolbar = window.toolbar?.items.compactMap { ($0 as? NSSearchToolbarItem)?.searchField } ?? []
        let tree = (window.contentView?.superview ?? window.contentView).map(walk) ?? []
        var seen = Set<ObjectIdentifier>()
        return (toolbar + tree).filter { seen.insert(ObjectIdentifier($0)).inserted }
    }
    #else
    /// The window that shows the vault: the key one if it does, else the
    /// first one in a foreground scene (an iPad may show several).
    @MainActor
    static func mainWindow() -> UIWindow? {
        let windows = trackers.allObjects.compactMap(\.window)
            .filter { $0.windowScene?.activationState == .foregroundActive }
        return windows.first(where: \.isKeyWindow) ?? windows.first
    }

    /// `hideSearch`: UIKit draws the search field. It gets an opaque cover
    /// for the snapshot instead of new text, so `.searchable` never sees a change.
    @MainActor
    private static func snapshot(_ window: UIWindow, hideSearch: Bool) -> CGImage? {
        // The root view only: presented sheets (Settings, this report) are not part of it.
        guard let view = window.rootViewController?.view else { return nil }
        let covers: [UIView] = hideSearch ? searchFields(in: view).compactMap { field in
            guard !(field.text ?? "").isEmpty, field.window != nil else { return nil }
            let cover = UIView(frame: field.convert(field.bounds, to: view))
            cover.backgroundColor = .systemGray
            cover.layer.cornerRadius = 6
            view.addSubview(cover)
            return cover
        } : []
        defer { covers.forEach { $0.removeFromSuperview() } }
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.screen.scale
        let image = UIGraphicsImageRenderer(bounds: view.bounds, format: format).image { _ in
            view.drawHierarchy(in: view.bounds, afterScreenUpdates: true)
        }
        return image.cgImage
    }

    private static func searchFields(in view: UIView) -> [UISearchTextField] {
        (view as? UISearchTextField).map { [$0] } ?? view.subviews.flatMap(searchFields(in:))
    }
    #endif

    /// `base` with black boxes over `masks` (unit rectangles, origin top left).
    static func compose(_ base: CGImage, masks: [CGRect]) -> CGImage? {
        let width = base.width, height = base.height
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        let full = CGRect(x: 0, y: 0, width: width, height: height)
        ctx.draw(base, in: full)
        ctx.setFillColor(CGColor(gray: 0, alpha: 1))
        for m in masks {
            // CoreGraphics counts y from the bottom.
            let r = CGRect(x: m.minX * full.width, y: (1 - m.maxY) * full.height,
                           width: m.width * full.width, height: m.height * full.height)
            ctx.fill(r.integral.intersection(full))
        }
        return ctx.makeImage()
    }

    static func png(_ image: CGImage) -> Data? {
        let data = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(dest, image, nil)
        return CGImageDestinationFinalize(dest) ? data as Data : nil
    }

    @MainActor
    static func copyToPasteboard(_ png: Data) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setData(png, forType: .png)
        #else
        UIPasteboard.general.setData(png, forPasteboardType: UTType.png.identifier)
        #endif
    }

    // MARK: Diagnostics

    /// One Abrufprotokoll entry: `head` is built by the app from time,
    /// connector kind, trigger and result (no user data); `message` is the
    /// bank's text and the only part that passes the sanitizer.
    struct LogLine: Equatable, Sendable {
        var head: String
        var message: String
    }

    /// What the diagnostics block shows.
    struct Info: Equatable {
        var appVersion: String
        var system: String
        var architecture: String
        var language: String
        var demo: Bool
        var connectors: [String]
        var logLines: [LogLine]
    }

    /// The diagnostics block. Only the log lines carry user data: they pass
    /// the core sanitizer with `knownTerms`; the header is built from
    /// version, system and connector names alone.
    static func diagnostics(_ info: Info, knownTerms: [String]) -> String {
        render(info, log: sanitizedLog(info.logLines, knownTerms: knownTerms))
    }

    /// The log lines with sanitized messages (IBANs, e-mails, amounts,
    /// secrets, long numbers and the vault's own names are replaced). The
    /// head is left alone: an account named like its bank ("PayPal") must not
    /// blank out the connector column.
    /// Pure and vault-free: run it off the main thread (thousands of terms).
    static func sanitizedLog(_ lines: [LogLine], knownTerms: [String]) -> [String] {
        guard !lines.isEmpty else { return [] }
        // One call for all messages (one regex build). The closing "." keeps a
        // trailing empty message from being dropped by the line split.
        let joined = (lines.map(\.message) + ["."]).joined(separator: "\n")
        var messages = sanitizeReportText(text: joined, knownTerms: knownTerms).components(separatedBy: "\n")
        messages.removeLast()
        guard messages.count == lines.count else {
            // never expected; fall back to one call per message
            return lines.map { line(head: $0.head, message: sanitizeReportText(text: $0.message, knownTerms: knownTerms)) }
        }
        return zip(lines, messages).map { line(head: $0.head, message: $1) }
    }

    private static func line(head: String, message: String) -> String {
        message.isEmpty ? head : "\(head) · \(message)"
    }

    static func render(_ info: Info, log: [String]) -> String {
        var lines = [
            "GiroCrab \(info.appVersion)",
            "\(info.system) · \(info.architecture)",
            String(localized: "Sprache: \(info.language) · Demo-Modus: \(info.demo ? String(localized: "ja") : String(localized: "nein"))"),
            String(localized: "Anbindungen: \(info.connectors.isEmpty ? "–" : info.connectors.joined(separator: ", "))"),
        ]
        if !log.isEmpty {
            lines.append("")
            lines.append(String(localized: "Abruf-Protokoll (neueste zuerst):"))
            lines.append(contentsOf: log)
        }
        return lines.joined(separator: "\n")
    }

    /// Counterparty names read from the vault at most (newest first).
    static let maxCounterpartyTerms: UInt32 = 5000

    /// Everything personal the vault knows that might turn up in a log line:
    /// own accounts, owners, IBANs and logins, and the counterparties of all
    /// bookings (`counterparties`, from `Vault.counterpartyNames`: a rejected
    /// transfer names its payee, whatever the list currently shows).
    @MainActor
    static func knownTerms(model: AppModel, counterparties: [String],
                           catalog: [BankDescriptor] = bankCatalog()) -> [String] {
        let accounts = model.accountsByID.values.flatMap { [$0.name, $0.owner, $0.iban, $0.accountNumber] }
        let connections = model.connections.flatMap { [$0.displayName, $0.loginName] }
        // Bank and connector names are not personal, and an account named like
        // its bank ("PayPal") must not blank the bank out of every message.
        let banks = Set((catalog.map(\.name) + model.accountsByID.values.map(\.bankName)).map(normalizedTerm))
        return publicTermsRemoved(accounts + connections + counterpartyTerms(counterparties), banks: banks)
    }

    /// Booking texts that stand where a payee would ("VISA", "Zinsen",
    /// "Entgelt"): as terms they would blank ordinary words in every log line.
    static let bookingWords: Set<String> = [
        "abbuchung", "abrechnung", "abschluss", "auszahlung", "bank", "bargeld", "bonus", "dauerauftrag",
        "einzahlung", "entgelt", "gebühr", "gebühren", "gehalt", "girocard", "gutschrift", "habenzinsen",
        "kartenzahlung", "kasse", "konto", "kontoführung", "kreditkarte", "lastschrift", "lohn",
        "mastercard", "miete", "online", "paypal", "rechnungsabschluss", "rente", "rücklastschrift",
        "sepa", "sollzinsen", "steuer", "storno", "transfer", "umbuchung", "visa", "zins", "zinsen",
        "überweisung",
    ]

    /// Counterparties as redaction terms: every name of two words or more,
    /// and single words of four letters or more ("Müller", "Meier") unless
    /// they are booking words. Better a blanked word than a published payee.
    static func counterpartyTerms(_ names: [String]) -> [String] {
        names.filter { name in
            let words = name.split { !$0.isLetter && !$0.isNumber }
            if words.count >= 2 { return true }
            guard let word = words.first, word.count >= 4 else { return false }
            return !bookingWords.contains(word.lowercased())
        }
    }

    static func publicTermsRemoved(_ terms: [String], banks: Set<String>) -> [String] {
        var seen = Set<String>()
        return terms.filter { t in
            let key = normalizedTerm(t)
            return !key.isEmpty && !banks.contains(key) && seen.insert(key).inserted
        }
    }

    private static func normalizedTerm(_ s: String) -> String {
        s.trimmingCharacters(in: .whitespaces).lowercased()
    }

    @MainActor
    static func info(model: AppModel, log: [SyncLogEntry], catalog: [BankDescriptor] = bankCatalog()) -> Info {
        let connectorName = connectorNamer(catalog)
        return Info(
            appVersion: AppVersionDisplay.current(core: coreVersion()),
            system: systemDescription,
            architecture: architecture,
            language: Locale.current.identifier,
            demo: model.isDemo,
            connectors: Set(model.connections.map(\.connector)).map(connectorName).sorted(),
            logLines: logLines(model: model, entries: log, connectorName: connectorName)
        )
    }

    private static func connectorNamer(_ catalog: [BankDescriptor]) -> (ConnectorKind) -> String {
        { kind in
            guard let d = catalog.first(where: { $0.kind == kind }) else { return "\(kind)" }
            return d.protocol.isEmpty || d.protocol == d.name ? d.name : "\(d.name) (\(d.protocol))"
        }
    }

    @MainActor
    private static func logLines(model: AppModel, entries log: [SyncLogEntry],
                                 connectorName: (ConnectorKind) -> String) -> [LogLine] {
        let byConnection = Dictionary(model.connections.map { ($0.id, $0.connector) }, uniquingKeysWith: { a, _ in a })
        let time = DateFormatter()
        time.locale = Locale(identifier: "en_US_POSIX")
        time.dateFormat = "yyyy-MM-dd HH:mm"
        return log.map { e in
            // The connection's own name is the user's; the connector kind is enough.
            let kind = e.connectionId.flatMap { byConnection[$0] }.map(connectorName) ?? "–"
            let date = time.string(from: Date(timeIntervalSince1970: TimeInterval(e.timestampMs) / 1000))
            var head = "\(date) · \(kind) · \(e.source) · \(e.outcome)"
            if e.imported > 0 { head += " · +\(e.imported)" }
            return LogLine(head: head, message: e.message.replacingOccurrences(of: "\n", with: " "))
        }
    }

    @MainActor
    private static var systemDescription: String {
        let version = ProcessInfo.processInfo.operatingSystemVersionString
        #if os(macOS)
        return "macOS \(version)"
        #else
        if ProcessInfo.processInfo.isiOSAppOnMac { return "iOS-App auf macOS \(version)" }
        return "\(UIDevice.current.userInterfaceIdiom == .pad ? "iPadOS" : "iOS") \(version) · \(UIDevice.current.model)"
        #endif
    }

    private static var architecture: String {
        #if arch(arm64)
        "arm64"
        #elseif arch(x86_64)
        "x86_64"
        #else
        "unknown"
        #endif
    }

    // MARK: Issue

    static var pasteHint: String {
        #if os(macOS)
        String(localized: "📋 Der geschwärzte Screenshot liegt in deiner Zwischenablage – klicke hier ins Textfeld und drücke **⌘V**, um ihn einzufügen.")
        #else
        String(localized: "📋 Der geschwärzte Screenshot liegt in deiner Zwischenablage – tippe hier lange ins Textfeld und wähle **Einsetzen**.")
        #endif
    }

    /// Markdown body of the new issue. With `withScreenshot` false the paste
    /// hint is left out.
    static func issueBody(diagnostics: String, withScreenshot: Bool) -> String {
        var parts = [
            "### " + String(localized: "Was ist passiert?"),
            "",
            String(localized: "<!-- Beschreibe kurz das Problem: Was hast du getan, was hast du erwartet, was ist stattdessen passiert? Bitte keine echten IBANs, Namen oder Buchungsdaten. -->"),
            "",
        ]
        if withScreenshot {
            parts += ["### " + String(localized: "Screenshot"), "", pasteHint, ""]
        }
        // A fence longer than any backtick run in the text: a log line with
        // "```" must not end the block (and turn "@name" or "#123" live).
        let fence = String(repeating: "`", count: max(3, longestBacktickRun(diagnostics) + 1))
        parts += ["### " + String(localized: "Diagnose (automatisch bereinigt)"), "", fence + "text", diagnostics, fence]
        return parts.joined(separator: "\n")
    }

    static func longestBacktickRun(_ s: String) -> Int {
        var longest = 0, run = 0
        for c in s {
            run = c == "`" ? run + 1 : 0
            longest = max(longest, run)
        }
        return longest
    }

    /// The prefilled `issues/new` URL. Drops log lines from the end (the
    /// oldest) until the encoded URL fits `maxURLLength`.
    static func issueURL(info: Info, knownTerms: [String], withScreenshot: Bool) -> URL {
        issueURL(info: info, sanitizedLog: sanitizedLog(info.logLines, knownTerms: knownTerms), withScreenshot: withScreenshot)
    }

    /// Same, with log lines already passed through `sanitizedLog`.
    static func issueURL(info: Info, sanitizedLog: [String], withScreenshot: Bool) -> URL {
        var log = sanitizedLog
        var body: String
        var url: URL
        repeat {
            body = issueBody(diagnostics: render(info, log: log), withScreenshot: withScreenshot)
            url = issueURL(body: body)
            if url.absoluteString.utf8.count <= maxURLLength || log.isEmpty { break }
            log.removeLast()
        } while true
        // Without any log line still too long (not expected): cut the text.
        while url.absoluteString.utf8.count > maxURLLength, !body.isEmpty {
            body = String(body.dropLast(max(1, body.count / 10)))
            url = issueURL(body: body)
        }
        return url
    }

    private static func issueURL(body: String) -> URL {
        var c = URLComponents(url: newIssue, resolvingAgainstBaseURL: false)!
        c.queryItems = [URLQueryItem(name: "title", value: "[BUG] "), URLQueryItem(name: "body", value: body)]
        // URLComponents leaves "+" alone, but GitHub decodes it as a space.
        c.percentEncodedQuery = c.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return c.url!
    }
}

struct PNGFileDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.png] }
    var data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}
