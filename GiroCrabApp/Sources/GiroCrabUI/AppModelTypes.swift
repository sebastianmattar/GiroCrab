import Foundation
import SwiftUI
import UniformTypeIdentifiers
import GiroCrabCore

enum SidebarItem: Hashable {
    case allAccounts
    case group(Int64)
    /// Accounts of one bank ("Nach Bank"); empty = "Ohne Bank".
    case bank(String)
    /// User-defined group ("Nach Gruppe"); empty uuid = "Ohne Gruppe".
    case userGroup(String)
    case account(Int64)
    case category(Int64)
    case uncategorized
    /// "Belege & PostBox"
    case documents
    /// „Neue Umsätze“ (GitHub #352): unreviewed bookings of all accounts.
    case newBookings
    /// „Geplant“ (GitHub #240): future payments with an order at the bank.
    case planned
    /// „Lastschriften“ (GitHub #143): mandates and collections.
    case directDebits
    /// "Vermögen": cash and assets (depots) minus liabilities
    case wealth

    /// Whether this selection is an overall money context (#337): its KPI sums
    /// follow the effective total set (account switch, group, archiving). The
    /// booking list never narrows — a total context is not a proof context. A
    /// single account, a category and every proof view keep their sums.
    var usesTotalsContext: Bool {
        switch self {
        case .allAccounts, .group, .bank, .userGroup: true
        case .account, .category, .uncategorized, .documents, .newBookings, .planned, .directDebits, .wealth:
            false
        }
    }
}

/// What the main window's booking column shows for a sidebar selection
/// (`AppModel.mainColumn`). Accounts, groups and categories all show bookings
/// (or a depot / Sachwerte view, decided by `AccountColumn`).
enum MainColumn: Equatable {
    case bookings, documents, planned, directDebits, wealth

    init(_ item: SidebarItem?) {
        switch item {
        case .documents?: self = .documents
        case .planned?: self = .planned
        case .directDebits?: self = .directDebits
        case .wealth?: self = .wealth
        default: self = .bookings
        }
    }
}

/// Whether one sidebar row is the selected one (`AppModel.sidebarRowSelection(for:)`). A row
/// that compared `sidebarSelection` itself re-ran on every click, and every sidebar row has a
/// hosting view of its own; with one flag per row only the old and the new selection re-run.
@MainActor
@Observable
final class SidebarRowSelection {
    var isSelected: Bool

    init(isSelected: Bool) {
        self.isSelected = isSelected
    }
}

/// Document origin is independent of its booking link. Bank connectors and
/// statement imports store a source key; user uploads leave it empty.
enum DocumentsScope: String, CaseIterable, Identifiable {
    case all, receipts, postbox
    var id: Self { self }
    var title: String {
        switch self {
        case .all: return String(localized: "Alle Dokumente")
        case .receipts: return String(localized: "Belege")
        case .postbox: return String(localized: "PostBox")
        }
    }

    var heading: String {
        self == .all ? String(localized: "Belege & PostBox") : title
    }

    var description: String {
        switch self {
        case .all:
            return String(localized: "Eigene Belege und Bankdokumente – verschlüsselt im Tresor. Die Auswahl eines Kontos grenzt die Sammlung ein.")
        case .receipts:
            return String(localized: "Selbst hinzugefügte Rechnungen, Kassenbons und Fotos. PDF oder Foto hierher ziehen: GiroCrab sucht den passenden Umsatz.")
        case .postbox:
            return String(localized: "Von der Bank bereitgestellte Kontoauszüge, Wertpapierabrechnungen und Mitteilungen. Sie gehören zum Konto und können einem Umsatz zugeordnet sein.")
        }
    }

    func includes(_ document: DocumentInfo) -> Bool {
        switch self {
        case .all: return true
        case .receipts: return !document.isBankDocument
        case .postbox: return document.isBankDocument
        }
    }
}

extension DocumentInfo {
    var isBankDocument: Bool {
        // Old demo vaults used synthetic source keys for their uploaded receipts.
        !sourceUrl.isEmpty && !sourceUrl.hasPrefix("demo-doc-")
    }
    var originTitle: String {
        isBankDocument ? String(localized: "PostBox") : String(localized: "Beleg")
    }
}

/// Period of the booking list and its sums. The menu follows MoneyMoney
/// (GitHub #382): rolling windows, then the latest months, quarters and years
/// by name, counted back from today.
enum Period: Hashable, Identifiable {
    case all
    /// Today and the `n − 1` days before.
    case days(Int)
    /// Rolling year: from the same day a year ago (exclusive) to today.
    case months12
    /// Pay cycle: from the latest detected salary inflow (GitHub #37).
    case sinceSalary
    case month(Int, Int)
    case quarter(Int, Int)
    case year(Int)
    /// User-defined start and end date (GitHub #53); the dates live in `AppModel`.
    case custom
    var id: Self { self }

    private static var calendar: Calendar { Calendar(identifier: .gregorian) }

    /// The menu sections; `.custom` is chosen through the date dialog.
    static func menu(now: Date = Date()) -> [[Period]] {
        let c = calendar.dateComponents([.year, .month], from: now)
        let (y, m) = (c.year ?? 2000, c.month ?? 1)
        let months = (0..<3).map { back -> Period in
            let i = y * 12 + (m - 1) - back
            return .month(i / 12, i % 12 + 1)
        }
        let quarters = (0..<3).map { back -> Period in
            let i = y * 4 + (m - 1) / 3 - back
            return .quarter(i / 4, i % 4 + 1)
        }
        return [
            [.all],
            [.days(30), .days(90), .months12, .sinceSalary],
            months,
            quarters,
            (0..<3).map { .year(y - $0) },
        ]
    }

    /// The month before the current one (default of the date dialog, demo print).
    static func previousMonth(now: Date = Date()) -> Period { menu(now: now)[2][1] }
    /// The year before the current one.
    static func previousYear(now: Date = Date()) -> Period { menu(now: now)[4][1] }

    var title: String {
        switch self {
        case .all: return String(localized: "Gesamter Zeitraum")
        case .days(let n): return String(localized: "\(n) Tage")
        case .months12: return String(localized: "12 Monate")
        case .sinceSalary: return String(localized: "Seit letztem Gehalt")
        case .month(let y, let m):
            let date = Self.calendar.date(from: DateComponents(year: y, month: m, day: 1)) ?? Date()
            return date.formatted(.dateTime.month(.wide).year())
        case .quarter(let y, let q): return String(localized: "\(q). Quartal \(String(y))")
        case .year(let y): return String(localized: "Jahr \(String(y))")
        case .custom: return String(localized: "Benutzerdefiniert")
        }
    }

    var range: (String?, String?) { range(now: Date()) }

    /// ISO start/end (inclusive, `nil` = open) relative to `now`.
    func range(now: Date) -> (String?, String?) {
        let cal = Self.calendar
        let today = cal.startOfDay(for: now)
        // first day of a month and the day before the first of `months` later
        let span = { (y: Int, m: Int, months: Int) -> (String?, String?) in
            let start = cal.date(from: DateComponents(year: y, month: m, day: 1))!
            let end = cal.date(byAdding: DateComponents(month: months, day: -1), to: start)!
            return (DateText.iso(start), DateText.iso(end))
        }
        switch self {
        // `.custom` dates live in `PeriodSetting.range`.
        case .all, .custom: return (nil, nil)
        case .days(let n): return (DateText.iso(cal.date(byAdding: .day, value: -(n - 1), to: today)!), nil)
        case .months12:
            let start = cal.date(byAdding: DateComponents(year: -1, day: 1), to: today)!
            return (DateText.iso(start), nil)
        // The salary start date lives in `PeriodSetting.range`; calendar month as fallback.
        case .sinceSalary:
            return (DateText.iso(cal.date(from: cal.dateComponents([.year, .month], from: now))!), nil)
        case .month(let y, let m): return span(y, m, 1)
        case .quarter(let y, let q): return span(y, (q - 1) * 3 + 1, 3)
        case .year(let y): return span(y, 1, 12)
        }
    }
}

/// What the sidebar category sums were loaded for (#1413): period range and `categorySumFilter`.
struct CategorySumsRequest: Equatable {
    let from: String?
    let to: String?
    let accounts: [Int64]
    let checked: Bool?
    let direction: BookingDirection
    let amount: AmountFilter
}

/// A period with the dates of `.custom` (#1413): what `AppModel.shownPeriod` holds.
struct PeriodSetting: Equatable {
    let period: Period
    /// Start/end of `.custom` (ISO, inclusive); `nil` for every other period.
    let customFrom: String?
    let customTo: String?

    init(_ period: Period, customFrom: String? = nil, customTo: String? = nil) {
        self.period = period
        self.customFrom = period == .custom ? customFrom : nil
        self.customTo = period == .custom ? customTo : nil
    }

    /// ISO start/end (inclusive, `nil` = open); `.sinceSalary` starts at the cycle's salary.
    func range(salaryCycle: SalaryCycle?) -> (String?, String?) {
        if period == .sinceSalary, let c = salaryCycle { return (c.lastDate, nil) }
        if period == .custom { return (customFrom, customTo) }
        return period.range
    }

    /// The preset name or "15.03.2025 – 30.06.2025".
    var title: String {
        guard let customFrom, let customTo else { return period.title }
        return "\(DateText.medium(customFrom)) – \(DateText.medium(customTo))"
    }
}

extension Period {
    /// Relative, launch-independent form for per-category defaults (#425):
    /// "this year" stays "this year" next January. `nil` for `.custom`.
    func storageKey(now: Date = Date()) -> String? {
        let menu = Period.menu(now: now)
        switch self {
        case .all: return "all"
        case .days(let n): return "days:\(n)"
        case .months12: return "months12"
        case .sinceSalary: return "sinceSalary"
        case .custom: return nil
        case .month: return menu[2].firstIndex(of: self).map { "month:\($0)" }
        case .quarter: return menu[3].firstIndex(of: self).map { "quarter:\($0)" }
        case .year: return menu[4].firstIndex(of: self).map { "year:\($0)" }
        }
    }

    init?(storageKey: String, now: Date = Date()) {
        let menu = Period.menu(now: now)
        let parts = storageKey.split(separator: ":")
        let n = parts.count == 2 ? Int(parts[1]) : nil
        switch (parts.first.map(String.init), n) {
        case ("all", _): self = .all
        case ("months12", _): self = .months12
        case ("sinceSalary", _): self = .sinceSalary
        case ("days", let n?) where n > 0: self = .days(n)
        case ("month", let i?) where menu[2].indices.contains(i): self = menu[2][i]
        case ("quarter", let i?) where menu[3].indices.contains(i): self = menu[3][i]
        case ("year", let i?) where menu[4].indices.contains(i): self = menu[4][i]
        default: return nil
        }
    }

    /// `storageKey` plus `.custom`, whose dates are saved separately: the period restored at launch (#840).
    func lastChoiceKey(now: Date = Date()) -> String {
        self == .custom ? "custom" : storageKey(now: now) ?? "all"
    }

    init?(lastChoiceKey key: String, now: Date = Date()) {
        if key == "custom" { self = .custom } else { self.init(storageKey: key, now: now) }
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

/// The merchant a merchant-centred booking list is narrowed to (#495).
struct MerchantFocus: Equatable {
    let key: String
    let name: String
}

/// What `leaveMerchantFocus()` restores.
struct MerchantFocusReturn {
    let selection: SidebarItem?
    let scope: BookingScope
    let status: StatusFilter
    let direction: BookingDirection
    let search: String
    let tags: BookingTagFilter
}

/// Tag filter of the booking list (#228): bookings carrying every tag in `tagIDs`,
/// or (`untagged`) only bookings without any tag. Both at once never happens.
struct BookingTagFilter: Equatable {
    var tagIDs: Set<Int64> = []
    var untagged = false
    var isActive: Bool { untagged || !tagIDs.isEmpty }
}

struct DayJump: Equatable {
    let day: String
    let token = UUID()
}

/// A running balance (#577) as the list shows it, passed into rows as a value so
/// a new page re-renders only the rows whose balance changed. Formatted once.
struct RunningBalance: Equatable {
    let cents: Int64
    let text: String

    init(cents: Int64, currency: String) {
        self.cents = cents
        text = Money.format(cents, currency: currency)
    }
}

/// Running balances of the loaded list pages (#577), by booking and by day.
struct ListBalances {
    /// One account's own currency, else EUR (the core sums only EUR accounts).
    var currency = "EUR"
    private(set) var bookings: [Booking.ID: RunningBalance] = [:]
    private(set) var days: [String: RunningBalance] = [:]

    mutating func merge(_ page: BookingPage) {
        for b in page.runningBalances { bookings[b.bookingId] = RunningBalance(cents: b.balanceCents, currency: currency) }
        for d in page.dayBalances { days[d.date] = RunningBalance(cents: d.balanceCents, currency: currency) }
    }
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
///
/// Its identity is its position, the nth header or the nth row, not the day or the booking:
/// a new list (another category, period or filter) then updates the elements on screen in
/// place instead of tearing them down and building new ones, which made up most of a
/// selection change's main-thread time. Scroll targets go through `StreamSlots`.
/// Consequence for every view inside a stream element: `@State` that belongs to one booking or
/// day must be reset when that changes (`onChange(of: booking.id)`), or it shows on the next one
/// (see `DayCardRow`, `BookingRow`, `MerchantSignet`, `StreamDayTracker`).
enum StreamItem: Identifiable {
    case header(DayGroup, ordinal: Int)
    case row(Booking, isLast: Bool, ordinal: Int)

    var id: StreamSlot {
        switch self {
        case .header(_, let n): .header(n)
        case .row(_, _, let n): .row(n)
        }
    }

    static func flatten(_ days: [DayGroup]) -> (items: [StreamItem], slots: StreamSlots) {
        var out: [StreamItem] = []
        var slots = StreamSlots()
        out.reserveCapacity(days.reduce(days.count) { $0 + $1.bookings.count })
        var row = 0
        for (h, day) in days.enumerated() {
            out.append(.header(day, ordinal: h))
            slots.day[day.date] = .header(h)
            for (i, b) in day.bookings.enumerated() {
                out.append(.row(b, isLast: i == day.bookings.count - 1, ordinal: row))
                slots.booking[b.id] = .row(row)
                row += 1
            }
        }
        return (out, slots)
    }
}

/// Identity of a booking stream element: the nth day header or the nth booking row.
enum StreamSlot: Hashable {
    case header(Int)
    case row(Int)
}

/// Where a day's header and each booking sit in the stream: `ScrollViewProxy` targets.
struct StreamSlots {
    var day: [String: StreamSlot] = [:]
    var booking: [Booking.ID: StreamSlot] = [:]
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

/// What the transfer sheets do: one transfer, an Umbuchung between own
/// accounts, or a batch.
enum TransferKind: Hashable { case single, own, batch }

struct TransferDraft {
    var accountID: Int64?
    /// Umbuchung: name and IBAN come from an own account.
    var ownTransfer = false
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

    /// The fields a payment fills (paste, invoice, clipboard check #663).
    static let paymentFields: [WritableKeyPath<TransferDraft, String>] = [\.name, \.iban, \.bic, \.amountText, \.purpose]

    func isBlank(_ field: KeyPath<TransferDraft, String>) -> Bool {
        self[keyPath: field].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var executionMode: ExecutionMode {
        get { instant ? .instant : (scheduled ? .scheduled : .regular) }
        set {
            instant = newValue == .instant
            scheduled = newValue == .scheduled
        }
    }

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

/// How a transfer is executed; exactly one applies.
enum ExecutionMode: Hashable {
    case regular, scheduled, instant
}

/// A transfer whose result is unclear: it may have been executed.
struct TransferUnknownNotice: Identifiable {
    let id = UUID()
    let reason: String
    let endToEndId: String
}

/// A transfer cancellation the bank has not confirmed: the order (bunq draft)
/// may still be approvable. The session stays alive so the status can be re-checked.
struct UnconfirmedTransferCancel: Identifiable {
    let id = UUID()
    let session: TransferSession
    var outcome: CancelOutcome
    var busy = false

    var message: String {
        let head: String
        let reason: String
        switch outcome {
        case .cancelled:
            return ""
        case .cancellationPending(let r):
            head = String(localized: "Die Stornierung wurde von der Bank angenommen, aber noch nicht bestätigt.")
            reason = r
        case .cancellationUnknown(let r):
            head = String(localized: "Es ist unklar, ob die Überweisung storniert wurde (keine eindeutige Antwort der Bank).")
            reason = r
        case .cancellationFailed(let r):
            head = String(localized: "Die Überweisung konnte nicht storniert werden.")
            reason = r
        }
        let advice = String(localized: "Sie kann möglicherweise noch in der bunq-App freigegeben werden. Prüfen Sie den Status erneut oder lehnen Sie sie dort ab.")
        return "\(head)\n\n\(reason)\n\n\(advice)"
    }
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

/// Where a dragged category lands relative to the row it is dropped on (#977).
enum CategoryDropPlace {
    /// Right above the row, as its sibling.
    case before
    /// Right below the row, as its sibling.
    case after
    /// Into the row's category, as its last sub-category.
    case into
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
    "kontist.client_id": String(localized: "Kontist Client-ID"),
    "kontist.client_secret": String(localized: "Kontist Client-Secret"),
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
    /// narrowed subset) with the same calculation context, so its `flow` can
    /// stand in for `selectionFlow` / `selectionFlowForTotals` (#337: reuse
    /// only when the page was loaded with this very filter — `respectInTotal`
    /// included — and nothing narrows it).
    var listHasFlow: Bool {
        scope == .all && direction == .all && checked == nil && search == nil && categoryId == nil && !uncategorizedOnly
            && tagIds.isEmpty && !untaggedOnly
            && amountMinCents == nil && amountMaxCents == nil
    }
}

/// Amount range of the booking list (#357): absolute amounts in cents (the sign is the
/// direction filter's business). `nil` = open on that side.
struct AmountFilter: Equatable {
    var minCents: Int64?
    var maxCents: Int64?

    var isActive: Bool { minCents != nil || maxCents != nil }

    /// A range given the wrong way round is swapped.
    var normalized: AmountFilter {
        guard let lo = minCents, let hi = maxCents, lo > hi else { return self }
        return AmountFilter(minCents: hi, maxCents: lo)
    }

    /// Label of the header pill: „Betrag“, or the range („Betrag: 50,00 € – 200,00 €“, „Betrag: ≥ 100,00 €“).
    /// Contains amounts: show it with `.sensitive(.amounts)` while a range is active.
    var title: String {
        switch (minCents, maxCents) {
        case (nil, nil): String(localized: "Betrag")
        case (let lo?, let hi?) where lo == hi: String(localized: "Betrag: \(Money.format(lo))")
        case (let lo?, let hi?): String(localized: "Betrag: \(Money.format(lo)) – \(Money.format(hi))")
        case (let lo?, nil): String(localized: "Betrag: ≥ \(Money.format(lo))")
        case (nil, let hi?): String(localized: "Betrag: ≤ \(Money.format(hi))")
        }
    }
}

extension Result where Failure == Error {
    /// Async counterpart of `Result(catching:)`.
    init(catchingAsync body: () async throws -> Success) async {
        do { self = .success(try await body()) } catch { self = .failure(error) }
    }
}

/// Account kinds the user can pick (import, Konto-Einstellungen), on Mac and iPhone.
extension AccountKind {
    static let importChoices: [AccountKind] = [.checking, .savings, .creditCard, .paymentService, .brokerage, .cash, .loan]

    var label: String {
        switch self {
        case .checking: return String(localized: "Girokonto")
        case .savings: return String(localized: "Tagesgeld / Sparen")
        case .creditCard: return String(localized: "Kreditkarte")
        case .paymentService: return String(localized: "Zahlungsdienst")
        case .brokerage: return String(localized: "Depot")
        case .cash: return String(localized: "Bargeld")
        case .loan: return String(localized: "Darlehen")
        }
    }
}

/// Result of the connection test in Einstellungen › Analyse (`girocrab-mcp --check --json`).
enum LocalApiCheck: Equatable {
    /// The helper reached GiroCrab and the key was accepted.
    case works
    /// Reachable; no key was at hand to test (only the newly created one is).
    case reachableWithoutKey
    case notReachable
    case keyRejected
    case turnedOff
    case locked
    case busy
    /// The helper did not finish in time and was stopped.
    case noAnswer
    case failed(String)
}

/// The running socket and the vault it serves (`reconcileLocalApiServer`).
struct RunningLocalApi {
    let server: LocalApiServer
    weak var servedVault: Vault?
}

/// What Return in a tag field does with the typed text (#228).
enum TagEntryIntent: Equatable {
    case none
    /// A new tag with this (cleaned) name.
    case create(String)
    /// The existing tag, not yet on (every one of) the bookings.
    case add(Int64, String)
    /// The existing tag is already on every booking: Return does nothing.
    case already(String)
    /// A new name that looks like an existing tag („test 1“ / „test-1“, #228):
    /// Return still creates it; the hint names the look-alike.
    case similar(new: String, existingID: Int64, existing: String)
    /// Longer than the core accepts: Return does nothing.
    case tooLong
}

/// Progress of pasting the vault key of another device (iCloud restore, #646).
enum KeyAdoptionState: Equatable {
    case idle, working, done
    case failed(String)
}

/// What the tag sheet changed but has not stored yet (#228): names, colours,
/// order. Stored when the sheet is dismissed (`AppModel.applyTagDraft`), so the
/// sidebar and the booking rows behind it stay as they were until then.
struct TagDraft: Equatable {
    var renames: [Int64: String] = [:]
    var colors: [Int64: String] = [:]
    /// The arrangement (drag, „Sortieren“); nil: unchanged.
    var order: [Int64]?
    var isEmpty: Bool { renames.isEmpty && colors.isEmpty && order == nil }
}

/// What „Umbenennen“ in the tag window does with the typed text (#228).
enum TagRenameIntent: Equatable {
    case empty
    /// The same name as now: nothing to save.
    case unchanged
    case rename(String)
    /// Another tag has this name (case aside): not allowed, merge instead.
    case taken(String)
    /// Allowed, but looks like another tag („test 1“ / „test-1“).
    case similar(new: String, existing: String)
    case tooLong
}

/// Sum of the selected bookings in one currency (#878).
struct SelectionTotal: Equatable {
    let currency: String
    let cents: Int64
}
