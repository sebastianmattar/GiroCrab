import Foundation
import GiroCrabCore

// MARK: - Print report (GitHub #53, part 2)
//
// Pure model + HTML builder for the booking printout. The sheet collects the
// options and the selected bookings; `PrintReportBuilder.html` renders them.
// HTML is printed through WebKit (macOS) / UIKit print formatters (iOS), which
// paginate and repeat table headers on every page.

/// Which bookings to print by direction of the amount.
enum PrintDirection: String, CaseIterable, Identifiable, Codable {
    case all, income, expenses
    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return String(localized: "Einnahmen und Ausgaben")
        case .income: return String(localized: "Nur Einnahmen")
        case .expenses: return String(localized: "Nur Ausgaben")
        }
    }

    func includes(_ amountCents: Int64) -> Bool {
        switch self {
        case .all: return true
        case .income: return amountCents > 0
        case .expenses: return amountCents < 0
        }
    }
}

/// Where the GiroCrab logo appears on the printout.
enum PrintLogo: String, CaseIterable, Identifiable, Codable {
    case off, header, watermark
    var id: String { rawValue }

    var title: String {
        switch self {
        case .off: return String(localized: "Kein Logo")
        case .header: return String(localized: "In der Kopfzeile")
        case .watermark: return String(localized: "Als Wasserzeichen")
        }
    }
}

/// User choices of the print dialog; remembered across launches.
struct PrintOptions: Codable, Equatable {
    var showCategory = true
    var showNote = true
    var showTotals = true
    var groupByAccount = false
    var showCurrentBalance = true
    var showDailyBalance = false
    var firstLineOnly = false
    var direction: PrintDirection = .all
    var newestFirst = true
    var logo: PrintLogo = .header
    /// Kontoauszug-Export: German dates/amounts, IBAN in the header (not persisted for print).
    var statementStyle = false

    private static let key = "print.options"

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = PrintOptions.defaults
        showCategory = try c.decodeIfPresent(Bool.self, forKey: .showCategory) ?? d.showCategory
        showNote = try c.decodeIfPresent(Bool.self, forKey: .showNote) ?? d.showNote
        showTotals = try c.decodeIfPresent(Bool.self, forKey: .showTotals) ?? d.showTotals
        groupByAccount = try c.decodeIfPresent(Bool.self, forKey: .groupByAccount) ?? d.groupByAccount
        showCurrentBalance = try c.decodeIfPresent(Bool.self, forKey: .showCurrentBalance) ?? d.showCurrentBalance
        showDailyBalance = try c.decodeIfPresent(Bool.self, forKey: .showDailyBalance) ?? d.showDailyBalance
        firstLineOnly = try c.decodeIfPresent(Bool.self, forKey: .firstLineOnly) ?? d.firstLineOnly
        direction = try c.decodeIfPresent(PrintDirection.self, forKey: .direction) ?? d.direction
        newestFirst = try c.decodeIfPresent(Bool.self, forKey: .newestFirst) ?? d.newestFirst
        logo = try c.decodeIfPresent(PrintLogo.self, forKey: .logo) ?? d.logo
        // statementStyle is export-only; never restore from print prefs
        statementStyle = false
    }

    private enum CodingKeys: String, CodingKey {
        case showCategory, showNote, showTotals, groupByAccount, showCurrentBalance
        case showDailyBalance, firstLineOnly, direction, newestFirst, logo
    }

    private static let defaults = PrintOptions()

    static func load(_ defaults: UserDefaults = AppDefaults.store) -> PrintOptions {
        guard let data = defaults.data(forKey: key),
              let o = try? JSONDecoder().decode(PrintOptions.self, from: data) else { return PrintOptions() }
        return o
    }

    func save(_ defaults: UserDefaults = AppDefaults.store) {
        if let data = try? JSONEncoder().encode(self) { defaults.set(data, forKey: Self.key) }
    }
}

/// One booking as the printout needs it (decoupled from the UniFFI record).
struct PrintRow: Identifiable, Equatable {
    let id: Int64
    let accountId: Int64
    let accountName: String
    let date: String
    let amountCents: Int64
    let currency: String
    let counterparty: String
    let purpose: String
    let category: String
    let note: String
    /// CSV only: counterparty IBAN and tag names.
    let counterpartyIban: String
    let tagNames: [String]

    init(id: Int64, accountId: Int64, accountName: String, date: String, amountCents: Int64,
         currency: String = "EUR", counterparty: String, purpose: String, category: String = "", note: String = "",
         counterpartyIban: String = "", tagNames: [String] = []) {
        self.id = id; self.accountId = accountId; self.accountName = accountName; self.date = date
        self.amountCents = amountCents; self.currency = currency; self.counterparty = counterparty
        self.purpose = purpose; self.category = category; self.note = note
        self.counterpartyIban = counterpartyIban; self.tagNames = tagNames
    }

    init(_ b: Booking) {
        let name = b.merchant.name.isEmpty ? b.counterparty : b.merchant.name
        self.init(id: b.id, accountId: b.accountId, accountName: b.accountName, date: b.bookingDate,
                  amountCents: b.amountCents, currency: b.currency, counterparty: name,
                  purpose: b.cleanPurpose.isEmpty ? b.purpose : b.cleanPurpose,
                  category: b.categoryPath, note: b.note, counterpartyIban: b.counterpartyIban,
                  tagNames: b.tags.map(\.name))
    }
}

/// Account header data (name, owner, IBAN, current balance).
struct PrintAccount: Equatable {
    let id: Int64
    let name: String
    let iban: String
    let balanceCents: Int64
    let balanceDate: String
    let currency: String
    var owner: String = ""
}

struct PrintReport {
    /// Sidebar selection, e.g. "Girokonto" or "Übersicht".
    var title: String
    /// Period label, e.g. "Letzter Monat" or "15.03.2025 – 30.06.2025".
    var periodLabel: String
    /// Selected bookings (any order; the builder filters and sorts).
    var rows: [PrintRow]
    var accounts: [PrintAccount] = []
    /// End-of-day balances per account id; key 0 = all printed accounts combined.
    var dailyBalances: [Int64: [String: Int64]] = [:]
    /// Balance after each booking (key = booking id) for the CSV „Saldo“ column (#577):
    /// of its account, or the sum of the exported accounts.
    var runningBalances: [Int64: Int64] = [:]
    var generatedAt = Date()
    /// App icon as `data:image/png;base64,…`; nil = no logo available on this platform.
    var logoDataURI: String?
    /// Active list filters to name in the header (e.g. "Nur Ausgaben").
    var filterNotes: [String] = []
}

enum PrintReportBuilder {
    /// Rows after the direction filter, in print order.
    static func rows(_ rows: [PrintRow], options: PrintOptions) -> [PrintRow] {
        rows.filter { options.direction.includes($0.amountCents) }
            .sorted { a, b in
                if a.date != b.date { return options.newestFirst ? a.date > b.date : a.date < b.date }
                return options.newestFirst ? a.id > b.id : a.id < b.id
            }
    }

    struct Totals: Equatable {
        var incomeCents: Int64 = 0
        var expenseCents: Int64 = 0
        var netCents: Int64 { incomeCents + expenseCents }
    }

    static func totals(_ rows: [PrintRow]) -> Totals {
        var t = Totals()
        for r in rows {
            if r.amountCents > 0 { t.incomeCents += r.amountCents } else { t.expenseCents += r.amountCents }
        }
        return t
    }

    /// "Letzter Monat · Nur Ausgaben · 25 Buchungen": a filtered printout says what it leaves out.
    static func metaLine(_ report: PrintReport, options: PrintOptions, count: Int) -> String {
        var parts = [report.periodLabel]
        var notes = report.filterNotes
        if options.direction != .all { notes.append(options.direction.title) }
        for n in notes where !parts.contains(n) { parts.append(n) }
        parts.append(String(localized: "\(count) Buchungen"))
        return parts.joined(separator: " · ")
    }

    /// "Erstellt am 11.11.2026 um 21:45 Uhr" – Umsatzliste omits the time.
    static func createdLine(_ date: Date, statementStyle: Bool = false) -> String {
        if statementStyle {
            return String(localized: "Erstellt am \(statementDate(DateText.iso(date)))")
        }
        let day = DateText.medium(DateText.iso(date))
        let time = date.formatted(date: .omitted, time: .shortened)
        return String(localized: "Erstellt am \(day) um \(time) Uhr")
    }

    /// Fixed German day for export PDFs (independent of system locale).
    static func statementDate(_ iso: String) -> String {
        guard let d = DateText.date(iso) else { return iso }
        let f = DateFormatter()
        f.locale = Locale(identifier: "de_DE")
        f.dateFormat = "dd.MM.yyyy"
        return f.string(from: d)
    }

    static func statementMoney(_ cents: Int64, currency code: String) -> String {
        let f = NumberFormatter()
        f.numberStyle = .currency
        f.locale = Locale(identifier: "de_DE")
        f.currencyCode = code
        let s = f.string(from: (Decimal(cents) / 100) as NSDecimalNumber) ?? Money.format(cents, currency: code)
        return s.replacingOccurrences(of: "-", with: "−")
    }

    /// Opening balance at the start of the first booking day (from end-of-day history).
    static func openingBalance(rows: [PrintRow], endOfDay: [String: Int64]) -> (date: String, cents: Int64)? {
        guard let first = rows.map(\.date).filter({ !$0.isEmpty }).min() else { return nil }
        let sumFirst = rows.filter { $0.date == first }.reduce(Int64(0)) { $0 + $1.amountCents }
        if let eod = endOfDay[first] { return (first, eod - sumFirst) }
        if let d = DateText.date(first),
           let prev = Calendar(identifier: .gregorian).date(byAdding: .day, value: -1, to: d) {
            let prevISO = DateText.iso(prev)
            if let eod = endOfDay[prevISO] { return (first, eod) }
        }
        return (first, 0)
    }

    /// Closing balance = end-of-day on the last booking day (or opening + sum).
    static func closingBalance(rows: [PrintRow], endOfDay: [String: Int64]) -> (date: String, cents: Int64)? {
        guard let last = rows.map(\.date).filter({ !$0.isEmpty }).max() else { return nil }
        if let eod = endOfDay[last] { return (last, eod) }
        if let open = openingBalance(rows: rows, endOfDay: endOfDay) {
            return (last, open.cents + rows.reduce(Int64(0)) { $0 + $1.amountCents })
        }
        return nil
    }

    /// Payment reference as printed; empty when it only repeats the payee.
    static func displayPurpose(_ r: PrintRow, firstLineOnly: Bool) -> String {
        let p = (firstLineOnly ? firstLine(r.purpose) : r.purpose).trimmingCharacters(in: .whitespacesAndNewlines)
        let party = r.counterparty.trimmingCharacters(in: .whitespacesAndNewlines)
        return p.compare(party, options: [.caseInsensitive, .diacriticInsensitive]) == .orderedSame ? "" : p
    }

    /// First line of the payment reference ("Verwendungszweck").
    static func firstLine(_ s: String) -> String {
        s.split(whereSeparator: \.isNewline).first.map { String($0).trimmingCharacters(in: .whitespaces) } ?? ""
    }

    /// Paper size in millimetres (portrait); the paginator lays out pages of this size.
    struct Paper: Equatable {
        var widthMM: Double
        var heightMM: Double
        static let a4 = Paper(widthMM: 210, heightMM: 297)

        /// "A4 · Hochformat", or the size in mm for other papers.
        var label: String {
            let short = min(widthMM, heightMM), long = max(widthMM, heightMM)
            let known: [(String, Double, Double)] = [("A4", 210, 297), ("A5", 148, 210), ("A3", 297, 420),
                                                     ("US Letter", 215.9, 279.4), ("US Legal", 215.9, 355.6)]
            let name = known.first { abs($0.1 - short) < 1.5 && abs($0.2 - long) < 1.5 }?.0
                ?? "\(Int(short.rounded())) × \(Int(long.rounded())) mm"
            let orientation = widthMM > heightMM ? String(localized: "Querformat") : String(localized: "Hochformat")
            return "\(name) · \(orientation)"
        }
    }

    static func html(_ report: PrintReport, options: PrintOptions, paper: Paper = .a4) -> String {
        let printed = rows(report.rows, options: options)
        let groups: [(account: Int64?, rows: [PrintRow])]
        if options.groupByAccount {
            var order: [Int64] = []
            var byAccount: [Int64: [PrintRow]] = [:]
            for r in printed {
                if byAccount[r.accountId] == nil { order.append(r.accountId) }
                byAccount[r.accountId, default: []].append(r)
            }
            // stable, readable order: by account name
            order.sort { (byAccount[$0]?.first?.accountName ?? "") < (byAccount[$1]?.first?.accountName ?? "") }
            groups = order.map { ($0, byAccount[$0] ?? []) }
        } else {
            groups = [(nil, printed)]
        }
        let showAccountColumn = !options.groupByAccount && Set(printed.map(\.accountId)).count > 1
        let currency = printed.first?.currency ?? "EUR"

        let bodyClass = options.statementStyle ? "statement" : ""
        var h = """
        <!doctype html><html lang="\(esc(Locale.current.language.languageCode?.identifier ?? "de"))"><head><meta charset="utf-8">
        <title>\(esc(report.title))</title><style>\(css)</style></head><body class="\(bodyClass)">
        <div id="flow">
        """
        let logo = report.logoDataURI.map(esc)
        if options.logo == .watermark, let logo {
            h += "<img class=\"watermark\" src=\"\(logo)\" alt=\"\">"
        }
        h += "<header>"
        if options.logo == .header, let logo {
            h += "<img class=\"logo\" src=\"\(logo)\" alt=\"GiroCrab\">"
        }
        if options.statementStyle {
            h += statementHeader(report, printed: printed, options: options)
        } else {
            h += """
            <h1>\(esc(report.title))</h1>
            <p class="meta">\(esc(metaLine(report, options: options, count: printed.count)))</p>
            <p class="meta">\(esc(createdLine(report.generatedAt)))</p>
            """
            h += accountSummary(report, groups: groups, options: options)
        }
        h += "</header>"

        if printed.isEmpty {
            h += "<p class=\"empty\">\(esc(String(localized: "Keine Buchungen ausgewählt.")))</p>"
        }
        for g in groups where !g.rows.isEmpty {
            let account = g.account.flatMap { id in report.accounts.first { $0.id == id } }
            h += "<section>"
            if options.groupByAccount {
                let name = account?.name ?? g.rows.first?.accountName ?? ""
                // Single-account Umsatzliste already titles the account in h1.
                if !(options.statementStyle && groups.count == 1) {
                    h += "<h2>\(esc(name))"
                    if let iban = account?.iban, !iban.isEmpty {
                        let shown = options.statementStyle ? formatIBAN(iban) : iban
                        h += " <span class=\"iban\">\(esc(shown))</span>"
                    }
                    h += "</h2>"
                    if options.statementStyle {
                        h += periodBalanceBlock(
                            rows: g.rows,
                            endOfDay: report.dailyBalances[g.account ?? account?.id ?? 0] ?? [:],
                            currency: g.rows.first?.currency ?? currency
                        )
                    }
                }
            }
            let balances = options.showDailyBalance ? report.dailyBalances[g.account ?? 0] : nil
            h += table(g.rows, options: options, showAccount: showAccountColumn, balances: balances,
                       combinedBalance: showAccountColumn)
            if options.showTotals && !options.statementStyle && options.groupByAccount && groups.count > 1 {
                h += totalsBlock(totals(g.rows), currency: g.rows.first?.currency ?? currency,
                                 label: String(localized: "Summe Konto"))
            }
            h += "</section>"
        }
        if options.showTotals && !options.statementStyle && !printed.isEmpty {
            h += totalsBlock(totals(printed), currency: currency, label: String(localized: "Summe"))
        }
        h += "</div>"
        h += "<script>\(paginator(report: report, options: options, paper: paper))</script>"
        h += "</body></html>"
        return h
    }

    /// Account name, owner, IBAN, date range, opening/closing for the Umsatzliste.
    private static func statementHeader(_ report: PrintReport, printed: [PrintRow], options: PrintOptions) -> String {
        let ids = Set(printed.map(\.accountId))
        let accounts = report.accounts.filter { ids.contains($0.id) }.sorted { $0.name < $1.name }
        var h = "<h1>\(esc(report.title))</h1>"
        if accounts.count == 1, let a = accounts.first {
            if !a.owner.isEmpty {
                h += "<p class=\"owner\">\(esc(a.owner))</p>"
            }
            if !a.iban.isEmpty {
                h += "<p class=\"iban-line\">\(esc(formatIBAN(a.iban)))</p>"
            }
        }
        var meta = report.periodLabel
        if options.direction != .all { meta += " · \(options.direction.title)" }
        for n in report.filterNotes where !meta.contains(n) { meta += " · \(n)" }
        meta += " · " + String(localized: "\(printed.count) Umsätze")
        h += "<p class=\"meta\">\(esc(meta))</p>"
        if accounts.count == 1, let a = accounts.first {
            let eod = report.dailyBalances[a.id] ?? report.dailyBalances[0] ?? [:]
            h += periodBalanceBlock(rows: printed, endOfDay: eod, currency: a.currency)
        } else if accounts.count > 1, !options.groupByAccount {
            let eod = report.dailyBalances[0] ?? [:]
            h += periodBalanceBlock(rows: printed, endOfDay: eod, currency: printed.first?.currency ?? "EUR")
        }
        return h
    }

    /// Anfangssaldo | Endsaldo side by side; labels, dates and amounts share one baseline each.
    private static func periodBalanceBlock(rows: [PrintRow], endOfDay: [String: Int64], currency: String) -> String {
        guard let open = openingBalance(rows: rows, endOfDay: endOfDay),
              let close = closingBalance(rows: rows, endOfDay: endOfDay) else { return "" }
        return """
        <div class="period-balances">
          <div class="bal">
            <div class="bal-label">\(esc(String(localized: "Anfangssaldo")))</div>
            <div class="bal-date">\(esc(statementDate(open.date)))</div>
            <div class="bal-amount">\(esc(statementMoney(open.cents, currency: currency)))</div>
          </div>
          <div class="bal">
            <div class="bal-label">\(esc(String(localized: "Endsaldo")))</div>
            <div class="bal-date">\(esc(statementDate(close.date)))</div>
            <div class="bal-amount">\(esc(statementMoney(close.cents, currency: currency)))</div>
          </div>
        </div>
        """
    }

    /// Lays the flowing report out into fixed pages (WebKit neither repeats table
    /// headers nor keeps table rows together when printing). Without script the
    /// plain flowing document still prints.
    private static func paginator(report: PrintReport, options: PrintOptions, paper: Paper) -> String {
        let footerLeft = options.statementStyle
            ? createdLine(report.generatedAt, statementStyle: true)
            : "\(report.title) · \(report.periodLabel)"
        let footerMid = options.statementStyle ? "\(report.title) · \(report.periodLabel)" : ""
        let pageOf = String(localized: "Seite \("{p}") von \("{n}")")
        let bodyClass = options.statementStyle ? "paged statement" : "paged"
        return """
        (function(){
        var W=\(paper.widthMM), H=\(paper.heightMM), FOOTER=\(jsString(footerLeft)), FOOTERMID=\(jsString(footerMid)), BODYCLASS=\(jsString(bodyClass)), PAGEOF=\(jsString(pageOf));
        var flow=document.getElementById('flow'); if(!flow) return;
        var st=document.createElement('style');
        st.textContent='@page{size:'+W+'mm '+H+'mm;margin:0}.page{width:'+W+'mm;height:'+(H-1)+'mm}';
        document.head.appendChild(st);
        var blocks=[];
        Array.prototype.slice.call(flow.children).forEach(function(el){
          var t=el.tagName==='SECTION'?el.querySelector('table.bookings'):null;
          if(t){
            var h2=el.querySelector('h2');
            var bal=el.querySelector('.period-balances');
            blocks.push({h2:h2,bal:bal,table:t,rows:Array.prototype.slice.call(t.tBodies[0].rows),
              after:Array.prototype.slice.call(el.children).filter(function(c){return c!==h2&&c!==bal&&c!==t;})});
          } else blocks.push({el:el});
        });
        var wm=flow.querySelector('img.watermark');
        if(wm){wm.parentNode.removeChild(wm);blocks=blocks.filter(function(b){return b.el!==wm;});}
        flow.parentNode.removeChild(flow);
        document.body.className=BODYCLASS;
        var pages=[],body;
        function newPage(){var p=document.createElement('div');p.className='page';if(wm)p.appendChild(wm.cloneNode(false));body=document.createElement('div');body.className='page-body';p.appendChild(body);var f=document.createElement('div');f.className='page-footer';p.appendChild(f);document.body.appendChild(p);pages.push(p);}
        function fits(){return body.scrollHeight<=body.clientHeight+1;}
        function place(el){body.appendChild(el);if(!fits()&&body.childNodes.length>1){body.removeChild(el);newPage();body.appendChild(el);}}
        function emptyTable(t){var c=t.cloneNode(false);if(t.tHead)c.appendChild(t.tHead.cloneNode(true));c.appendChild(document.createElement('tbody'));return c;}
        newPage();
        blocks.forEach(function(b){
          if(b.el){place(b.el);return;}
          var t=emptyTable(b.table),lead=[];
          if(b.h2){body.appendChild(b.h2);lead.push(b.h2);}
          if(b.bal){body.appendChild(b.bal);lead.push(b.bal);}
          body.appendChild(t);lead.push(t);
          if(b.rows.length)t.tBodies[0].appendChild(b.rows[0]);
          if(!fits()&&body.childNodes.length>lead.length){lead.forEach(function(n){body.removeChild(n);});newPage();lead.forEach(function(n){body.appendChild(n);});}
          for(var i=1;i<b.rows.length;i++){
            var r=b.rows[i];t.tBodies[0].appendChild(r);
            if(!fits()){t.tBodies[0].removeChild(r);newPage();t=emptyTable(b.table);body.appendChild(t);t.tBodies[0].appendChild(r);}
          }
          b.after.forEach(place);
        });
        pages.forEach(function(p,i){
          var f=p.lastChild;
          var l=document.createElement('span');
          var m=document.createElement('span');
          var r=document.createElement('span');
          l.textContent=FOOTER;
          m.textContent=FOOTERMID;
          r.textContent=PAGEOF.replace('{p}',i+1).replace('{n}',pages.length);
          f.appendChild(l); if(FOOTERMID) f.appendChild(m); f.appendChild(r);
        });
        })();
        """
    }

    /// JSON string literal that is safe inside a <script> element.
    static func jsString(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s])) ?? Data("[\"\"]".utf8)
        let array = String(decoding: data, as: UTF8.self)
        return String(array.dropFirst().dropLast()).replacingOccurrences(of: "</", with: "<\\/")
    }

    // MARK: - Pieces

    private static func accountSummary(_ report: PrintReport, groups: [(account: Int64?, rows: [PrintRow])],
                                       options: PrintOptions) -> String {
        guard options.showCurrentBalance else { return "" }
        let ids = Set(report.rows.map(\.accountId))
        let accounts = report.accounts.filter { ids.contains($0.id) }.sorted { $0.name < $1.name }
        guard !accounts.isEmpty else { return "" }
        var h = "<ul class=\"balances\">"
        for a in accounts {
            h += "<li><span class=\"name\">\(esc(a.name))</span>"
            h += " <span class=\"value\">\(esc(Money.format(a.balanceCents, currency: a.currency)))</span>"
            h += " <span class=\"asof\">\(esc(String(localized: "Kontostand am \(DateText.medium(a.balanceDate))")))</span></li>"
        }
        h += "</ul>"
        return h
    }

    private static func table(_ rows: [PrintRow], options: PrintOptions, showAccount: Bool, balances: [String: Int64]?,
                              combinedBalance: Bool = false) -> String {
        func day(_ iso: String) -> String {
            options.statementStyle ? statementDate(iso) : DateText.medium(iso)
        }
        func money(_ cents: Int64, currency: String) -> String {
            options.statementStyle ? statementMoney(cents, currency: currency) : Money.format(cents, currency: currency)
        }
        var h = "<table class=\"bookings\"><thead><tr>"
        h += "<th class=\"date\">\(esc(String(localized: "Datum")))</th>"
        if showAccount { h += "<th>\(esc(String(localized: "Konto")))</th>" }
        h += "<th>\(esc(String(localized: "Empfänger / Verwendungszweck")))</th>"
        if options.showCategory { h += "<th>\(esc(String(localized: "Kategorie")))</th>" }
        h += "<th class=\"amount\">\(esc(String(localized: "Betrag")))</th>"
        if balances != nil {
            let label = combinedBalance ? String(localized: "Tagessaldo (alle Konten)") : String(localized: "Tagessaldo")
            h += "<th class=\"amount\">\(esc(label))</th>"
        }
        h += "</tr></thead><tbody>"
        for (i, r) in rows.enumerated() {
            h += "<tr>"
            h += "<td class=\"date\">\(esc(day(r.date)))</td>"
            if showAccount { h += "<td>\(esc(r.accountName))</td>" }
            let purpose = displayPurpose(r, firstLineOnly: options.firstLineOnly)
            h += "<td><div class=\"party\">\(esc(r.counterparty))</div>"
            if !purpose.isEmpty { h += "<div class=\"purpose\">\(esc(purpose).replacingOccurrences(of: "\n", with: "<br>"))</div>" }
            if options.showNote && !r.note.isEmpty { h += "<div class=\"note\">\(esc(r.note))</div>" }
            h += "</td>"
            if options.showCategory { h += "<td class=\"category\">\(esc(r.category))</td>" }
            h += "<td class=\"amount\">\(esc(money(r.amountCents, currency: r.currency)))</td>"
            if let balances {
                // End-of-day balance on the day's row closest to the end of that day.
                let next = i + 1 < rows.count ? rows[i + 1].date : nil
                let prev = i > 0 ? rows[i - 1].date : nil
                let edge = options.newestFirst ? prev != r.date : next != r.date
                let value = edge ? balances[r.date].map { money($0, currency: r.currency) } ?? "" : ""
                h += "<td class=\"amount balance\">\(esc(value))</td>"
            }
            h += "</tr>"
        }
        h += "</tbody></table>"
        return h
    }

    private static func totalsBlock(_ t: Totals, currency: String, label: String) -> String {
        """
        <table class="totals"><tbody>
        <tr><td>\(esc(String(localized: "Einnahmen")))</td><td class="amount">\(esc(Money.format(t.incomeCents, currency: currency)))</td></tr>
        <tr><td>\(esc(String(localized: "Ausgaben")))</td><td class="amount">\(esc(Money.format(t.expenseCents, currency: currency)))</td></tr>
        <tr class="net"><td>\(esc(label))</td><td class="amount">\(esc(Money.format(t.netCents, currency: currency)))</td></tr>
        </tbody></table>
        """
    }

    /// Escapes for HTML and XML text/attributes. Drops what XML 1.0 forbids (control
    /// characters below U+0020 except tab, LF, CR; U+FFFE/U+FFFF), so bank texts with
    /// stray control bytes still give a well-formed CAMT.053 file.
    static func esc(_ s: String) -> String {
        var out = String.UnicodeScalarView()
        for u in s.unicodeScalars {
            switch u {
            case "&": out.append(contentsOf: "&amp;".unicodeScalars)
            case "<": out.append(contentsOf: "&lt;".unicodeScalars)
            case ">": out.append(contentsOf: "&gt;".unicodeScalars)
            case "\"": out.append(contentsOf: "&quot;".unicodeScalars)
            case "'": out.append(contentsOf: "&#39;".unicodeScalars)
            case "\t", "\n", "\r": out.append(u)
            default:
                if u.value < 0x20 || u.value == 0xFFFE || u.value == 0xFFFF { continue }
                out.append(u)
            }
        }
        return String(out)
    }

    private static let css = """
    @page { margin: 16mm 14mm; }
    body { font: 9.5pt -apple-system, "Helvetica Neue", Helvetica, sans-serif; color: #111; margin: 0; }
    body.statement { font: 9.5pt "Avenir Next", "Helvetica Neue", Helvetica, sans-serif; color: #1a1a1a; }
    h1 { font-size: 15pt; margin: 0 0 2pt; }
    body.statement h1 { font: 600 16pt "Avenir Next", "Helvetica Neue", Helvetica, sans-serif;
                        letter-spacing: -0.01em; margin: 0 0 1pt; }
    h2 { font-size: 11pt; margin: 14pt 0 4pt; }
    body.statement h2 { font: 600 11pt "Avenir Next", "Helvetica Neue", Helvetica, sans-serif; margin: 12pt 0 4pt; }
    .meta { color: #555; margin: 0 0 2pt; }
    body.statement .meta { color: #555; font-size: 9pt; margin: 0 0 6pt; }
    body.statement .owner { margin: 0 0 1pt; color: #333; font-size: 10pt; font-weight: 500; }
    body.statement .iban-line { margin: 0 0 4pt; color: #444; font-size: 9.5pt;
                                font-variant-numeric: tabular-nums; letter-spacing: 0.02em; }
    header { position: relative; margin-bottom: 8pt; }
    body.statement header { margin-bottom: 10pt; padding-bottom: 6pt; border-bottom: 1pt solid #222; }
    header .logo { position: absolute; top: 0; right: 0; width: 15mm; height: 15mm; }
    img.watermark { position: fixed; left: 50%; top: 50%; width: 110mm; height: 110mm;
                    transform: translate(-50%, -50%); opacity: 0.07; z-index: -1; pointer-events: none; }
    .page { position: relative; }
    .page > img.watermark { position: absolute; }
    .page-body, .page-footer { position: relative; z-index: 1; }
    .iban { font-weight: normal; color: #555; font-size: 9pt; }
    table { width: 100%; border-collapse: collapse; }
    thead { display: table-header-group; }
    tr { page-break-inside: avoid; break-inside: avoid; }
    th { text-align: left; font-weight: 600; border-bottom: 1pt solid #333; padding: 3pt 4pt; }
    body.statement th { font-weight: 600; font-size: 8pt; text-transform: uppercase;
                        letter-spacing: 0.04em; color: #333; border-bottom: 1pt solid #222; padding: 4pt 3pt; }
    td { vertical-align: top; padding: 3pt 4pt; border-bottom: 0.5pt solid #ddd; }
    body.statement td { padding: 4pt 3pt; border-bottom: 0.4pt solid #e2e2e2; }
    .date { white-space: nowrap; width: 1%; }
    body.statement .date { font-variant-numeric: tabular-nums; color: #333; }
    .amount { text-align: right; white-space: nowrap; font-variant-numeric: tabular-nums; width: 1%; }
    body.statement .amount { font-feature-settings: "tnum"; }
    .party { font-weight: 500; }
    body.statement .party { font-weight: 600; }
    .purpose, .category { color: #444; }
    body.statement .purpose, body.statement .category { color: #555; font-size: 8.5pt; }
    .note { color: #444; font-style: italic; }
    .balance { color: #444; }
    .balances { list-style: none; margin: 0 0 6pt; padding: 0; }
    .balances li { margin: 1pt 0; }
    .balances .name { font-weight: 500; }
    .balances .value { font-variant-numeric: tabular-nums; margin-left: 6pt; }
    .balances .asof { color: #555; margin-left: 6pt; }
    .period-balances { display: flex; gap: 8mm; margin: 2pt 0 8pt; }
    .period-balances .bal { flex: 1; min-width: 0; }
    .period-balances .bal-label { font-size: 8pt; text-transform: uppercase; letter-spacing: 0.04em;
                                  color: #555; margin: 0 0 2pt; }
    .period-balances .bal-date { font-size: 9pt; color: #444; font-variant-numeric: tabular-nums;
                                 margin: 0 0 2pt; min-height: 1.2em; }
    .period-balances .bal-amount { font-size: 12pt; font-weight: 600; font-variant-numeric: tabular-nums;
                                   font-feature-settings: "tnum"; }
    .totals { width: auto; margin: 10pt 0 0 auto; page-break-inside: avoid; break-inside: avoid; }
    .totals td { border: none; padding: 1pt 0 1pt 16pt; }
    .totals .net td { border-top: 1pt solid #333; font-weight: 600; }
    .empty { color: #555; }
    body.paged { margin: 0; }
    .page { box-sizing: border-box; padding: 14mm 14mm 8mm; display: flex; flex-direction: column;
            overflow: hidden; page-break-after: always; break-after: page; }
    .page:last-child { page-break-after: auto; break-after: auto; }
    .page-body { flex: 1; min-height: 0; overflow: hidden; }
    .page-body > table.bookings + table.bookings { margin-top: 0; }
    .page-footer { flex: none; min-height: 6mm; display: flex; justify-content: space-between; align-items: flex-end;
                   gap: 4mm; font-size: 7.5pt; color: #777; }
    body.statement .page-footer { font-size: 7.5pt; color: #888; }
    body.statement .page-footer span:nth-child(2) { flex: 1; text-align: center; }
    """
}
