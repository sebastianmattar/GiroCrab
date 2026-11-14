import Foundation
import SwiftUI
import UniformTypeIdentifiers
import GiroCrabCore

enum SidebarItem: Hashable {
    case allAccounts
    case group(Int64)
    /// User-defined group ("Nach Gruppe"); empty uuid = "Ohne Gruppe".
    case userGroup(String)
    case account(Int64)
    case category(Int64)
    case uncategorized
    /// "Belege & PostBox"
    case documents
}

enum Period: String, CaseIterable, Identifiable {
    case all = "Gesamter Zeitraum"
    case thisMonth = "Dieser Monat"
    case lastMonth = "Letzter Monat"
    case last3Months = "Letzte 3 Monate"
    case last12Months = "Letzte 12 Monate"
    case thisQuarter = "Dieses Quartal"
    case thisYear = "Dieses Jahr"
    case lastYear = "Letztes Jahr"
    /// Pay cycle: from the latest detected salary inflow (GitHub #37).
    case sinceSalary = "Seit letztem Gehalt"
    /// User-defined start and end date (GitHub #53); the dates live in `AppModel`.
    case custom = "Benutzerdefiniert"
    var id: String { rawValue }

    /// Fixed periods for the picker; `.custom` is chosen through the date dialog.
    static var presets: [Period] { allCases.filter { $0 != .custom } }

    var title: String {
        switch self {
        case .all: return String(localized: "Gesamter Zeitraum")
        case .thisMonth: return String(localized: "Dieser Monat")
        case .lastMonth: return String(localized: "Letzter Monat")
        case .last3Months: return String(localized: "Letzte 3 Monate")
        case .last12Months: return String(localized: "Letzte 12 Monate")
        case .thisQuarter: return String(localized: "Dieses Quartal")
        case .thisYear: return String(localized: "Dieses Jahr")
        case .lastYear: return String(localized: "Letztes Jahr")
        case .sinceSalary: return String(localized: "Seit letztem Gehalt")
        case .custom: return String(localized: "Benutzerdefiniert")
        }
    }

    var range: (String?, String?) { range(now: Date()) }

    /// ISO start/end (inclusive, `nil` = open) relative to `now`.
    func range(now: Date) -> (String?, String?) {
        let cal = Calendar(identifier: .gregorian)
        let startOfMonth = cal.date(from: cal.dateComponents([.year, .month], from: now))!
        let startOfYear = cal.date(from: cal.dateComponents([.year], from: now))!
        switch self {
        // `.custom` dates live in `AppModel.periodRange`.
        case .all, .custom: return (nil, nil)
        // The salary start date lives in `AppModel.periodRange`; calendar month as fallback.
        case .thisMonth, .sinceSalary: return (DateText.iso(startOfMonth), nil)
        case .lastMonth:
            let start = cal.date(byAdding: .month, value: -1, to: startOfMonth)!
            let end = cal.date(byAdding: .day, value: -1, to: startOfMonth)!
            return (DateText.iso(start), DateText.iso(end))
        case .last3Months: return (DateText.iso(cal.date(byAdding: .month, value: -2, to: startOfMonth)!), nil)
        case .last12Months: return (DateText.iso(cal.date(byAdding: .month, value: -11, to: startOfMonth)!), nil)
        case .thisQuarter:
            let c = cal.dateComponents([.year, .month], from: now)
            let qStart = cal.date(from: DateComponents(year: c.year, month: ((c.month ?? 1) - 1) / 3 * 3 + 1, day: 1))!
            return (DateText.iso(qStart), nil)
        case .thisYear: return (DateText.iso(startOfYear), nil)
        case .lastYear:
            let start = cal.date(byAdding: .year, value: -1, to: startOfYear)!
            let end = cal.date(byAdding: .day, value: -1, to: startOfYear)!
            return (DateText.iso(start), DateText.iso(end))
        }
    }
}

extension BookingDirection: @retroactive Identifiable {
    public var id: Self { self }

    static let ordered: [BookingDirection] = [.all, .income, .expense]

    var title: String {
        switch self {
        case .all: return String(localized: "Einnahmen und Ausgaben")
        case .income: return String(localized: "Nur Einnahmen")
        case .expense: return String(localized: "Nur Ausgaben")
        }
    }
}

enum StatusFilter: String, CaseIterable, Identifiable {
    case all = "Alle"
    case new = "Neu"
    case checked = "Geprüft"
    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return String(localized: "Alle")
        case .new: return String(localized: "Neu")
        case .checked: return String(localized: "Geprüft")
        }
    }
}

extension BookingScope {
    var title: String {
        switch self {
        case .all: return String(localized: "Alle Buchungen")
        case .everyday: return String(localized: "Alltag & Konsum")
        case .investments: return String(localized: "Sparpläne & Perks")
        }
    }
    /// Short labels for narrow (compact) screens.
    var shortTitle: String {
        switch self {
        case .all: return String(localized: "Alle")
        case .everyday: return String(localized: "Alltag")
        case .investments: return String(localized: "Sparpläne")
        }
    }
    static let ordered: [BookingScope] = [.all, .everyday, .investments]
}

struct DayJump: Equatable {
    let day: String
    let token = UUID()
}

struct DayGroup: Identifiable {
    let date: String
    let bookings: [Booking]
    var id: String { date }
    var sum: Int64 { bookings.reduce(0) { $0 + $1.amountCents } }
}

/// One lazily built element of the booking stream: a day header or a single
/// booking row. The list is flat, so its estimated height is based on rows of
/// nearly equal height instead of day cards of 1 to 30 bookings – the scroll
/// bar no longer jumps whenever a long or short day is built.
enum StreamItem: Identifiable {
    case header(DayGroup)
    case row(Booking, isLast: Bool)

    enum Key: Hashable {
        case day(String)
        case booking(Booking.ID)
    }

    var id: Key {
        switch self {
        case .header(let day): .day(day.id)
        case .row(let b, _): .booking(b.id)
        }
    }

    static func flatten(_ days: [DayGroup]) -> [StreamItem] {
        var out: [StreamItem] = []
        out.reserveCapacity(days.reduce(days.count) { $0 + $1.bookings.count })
        for day in days {
            out.append(.header(day))
            for (i, b) in day.bookings.enumerated() {
                out.append(.row(b, isLast: i == day.bookings.count - 1))
            }
        }
        return out
    }
}

struct ActiveChallenge: Identifiable {
    let id = UUID()
    let connection: BankConnection
    var challenge: Challenge
    var waiting: Bool
    var busy: Bool = false
    var error: String?
}

/// Missing FinTS product ID / API keys for a bank: shown as a setup form.
struct SetupRequest: Identifiable {
    let id = UUID()
    let connection: BankConnection
    let keys: [String]
}

/// Form state of "Neue Überweisung" (kept while the TAN sheet is shown).
/// CSV handed to `.fileExporter`.
struct CSVFileDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.commaSeparatedText] }
    var data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

struct TransferDraft {
    var accountID: Int64?
    var name = ""
    var iban = ""
    var bic = ""
    var amountText = ""
    var purpose = ""
    var pin = ""
    /// SEPA Instant (Echtzeitüberweisung). Off by default. If the bank does
    /// not offer or rejects it, nothing is sent automatically: the sheet
    /// offers to send it as a regular transfer (new approval).
    var instant = false
    /// Terminüberweisung: when on, `executionDate` is sent (FinTS only).
    var scheduled = false
    var executionDate = Calendar.current.date(byAdding: .day, value: 1, to: Date()) ?? Date()

    /// `YYYY-MM-DD` for the core, or nil when not scheduled.
    var executionDateISO: String? {
        guard scheduled && !instant else { return nil }
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: executionDate)
    }
}

/// A transfer whose result is unclear: it may have been executed.
struct TransferUnknownNotice: Identifiable {
    let id = UUID()
    let reason: String
    let endToEndId: String
}

/// Name check (Verification of Payee) result awaiting the user's decision.
struct PendingPayeeCheck: Identifiable {
    let id = UUID()
    let connection: BankConnection
    let check: PayeeCheck
    var busy = false
}

struct PendingLogin: Identifiable {
    let id = UUID()
    let connection: BankConnection
}

struct CategoryNode: Identifiable, Hashable {
    let category: GiroCrabCore.Category
    var children: [CategoryNode]?
    var id: Int64 { category.id }
}

extension Array where Element == CategoryNode {
    /// Depth-first lookup of a node anywhere in the tree.
    func node(_ id: Int64) -> CategoryNode? {
        for n in self {
            if n.id == id { return n }
            if let hit = n.children?.node(id) { return hit }
        }
        return nil
    }
}

/// Human-readable (localised) text for core errors (instead of
/// "GiroCrabCore.CoreError.MissingSetting(key: …)").
func describe(_ error: Error) -> String {
    guard let e = error as? CoreError else { return error.localizedDescription }
    switch e {
    case .Database(let m): return String(localized: "Datenbankfehler: \(m)")
    case .WrongKey: return String(localized: "Der Tresor konnte nicht entsperrt werden (falscher Schlüssel oder beschädigte Datei).")
    case .NotFound(let what): return String(localized: "Nicht gefunden: \(what)")
    case .InvalidInput(let m): return String(localized: "Ungültige Eingabe: \(m)")
    case .MissingSetting(let key):
        let label = settingLabels[key] ?? key
        return String(localized: "Für diese Bank fehlt noch eine Einstellung: \(label). Bitte in den Einstellungen unter „Banken“ beim Bearbeiten dieser Bank eintragen.")
    case .Network(let m): return String(localized: "Netzwerkfehler: \(m)")
    case .Bank(let code, let m): return String(localized: "Die Bank hat abgelehnt (\(code)): \(m)")
    case .Authentication(let m): return String(localized: "Anmeldung fehlgeschlagen: \(m)")
    case .Protocol(let m): return String(localized: "Protokollfehler: \(m)")
    case .SessionClosed: return String(localized: "Dieser Abruf ist nicht mehr aktiv.")
    case .BrowserLoginRequired(let m): return m
    case .BankingBlocked(let m): return m
    case .SchemaTooNew: return String(localized: "Dieser Tresor wurde mit einer neueren GiroCrab-Version gespeichert. Bitte die App aktualisieren.")
    }
}

let settingLabels: [String: String] = [
    "fints.product_id": String(localized: "FinTS-Produktregistrierung"),
    "fints.url.52090000": String(localized: "FinTS-Server Volksbank Kassel Göttingen"),
    "fints.url.50050201": String(localized: "FinTS-Server 1822direkt"),
    "comdirect.client_id": String(localized: "comdirect Client-ID"),
    "comdirect.client_secret": String(localized: "comdirect Client-Secret"),
    "paypal.client_id": String(localized: "PayPal Client-ID"),
    "paypal.client_secret": String(localized: "PayPal Secret"),
    "enablebanking.application_id": String(localized: "Enable Banking Application-ID"),
    "enablebanking.private_key_pem": String(localized: "Enable Banking Private Key"),
    "enablebanking.redirect_url": String(localized: "Enable Banking Redirect-URL"),
]

func formatIBAN(_ iban: String) -> String {
    var out = ""
    for (i, ch) in iban.enumerated() {
        if i > 0 && i % 4 == 0 { out.append(" ") }
        out.append(ch)
    }
    return out
}

/// How a booking row marks its account: short name and colour.
struct AccountMarker {
    let label: String
    let color: Color
    /// The account's SF Symbol, tinted with `color` (#170).
    let symbol: String
}


/// Startup timing (unified log, Debug builds only): milliseconds since process start.
enum StartupClock {
    @inline(__always)
    static func mark(_ label: String) {
        #if DEBUG
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        sysctl(&mib, 4, &info, &size, nil, 0)
        let start = Double(info.kp_proc.p_un.__p_starttime.tv_sec) + Double(info.kp_proc.p_un.__p_starttime.tv_usec) / 1e6
        let ms = Int((Date().timeIntervalSince1970 - start) * 1000)
        NSLog("[startup] %6d ms  %@", ms, label)
        #endif
    }
}

extension BookingFilter {
    /// The list query sums exactly the selection's bookings (all of them, not a
    /// narrowed subset), so its `flow` can stand in for `selectionFlow`.
    var listHasFlow: Bool {
        scope == .all && direction == .all && checked == nil && search == nil && categoryId == nil && !uncategorizedOnly
    }
}

extension Result where Failure == Error {
    /// Async counterpart of `Result(catching:)`.
    init(catchingAsync body: () async throws -> Success) async {
        do { self = .success(try await body()) } catch { self = .failure(error) }
    }
}
