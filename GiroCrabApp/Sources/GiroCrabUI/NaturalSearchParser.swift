import Foundation
import GiroCrabCore
#if canImport(FoundationModels)
import FoundationModels
#endif

/// What the on-device model extracted from a search phrase. Plain value type so `NaturalSearch.apply`
/// is testable without a model.
struct ParsedSearchQuery: Equatable, Sendable {
    /// Remaining free text (merchant, purpose); empty when the phrase had nothing else.
    var text: String = ""
    /// ISO days `YYYY-MM-DD`; nil = open on that side.
    var fromDate: String?
    var toDate: String?
    var categoryName: String?
    /// Absolute amounts in euros; nil = open.
    var minAmount: Double?
    var maxAmount: Double?
}

/// The filter state a parsed query may change (a value copy of the matching `AppModel` properties).
struct NaturalSearchState: Equatable {
    var searchText: String
    var period: Period
    var customFrom: String
    var customTo: String
    var categoryID: Int64?
    var amountFilter: AmountFilter
}

/// A natural-language reading of the typed phrase, kept next to it: the search field keeps what the
/// user typed and the list searches `keyword`. Writing the keyword back into the field fought the
/// focused macOS search field (AppKit pushed the typed phrase back, the list stayed empty), and on
/// every platform it hid what was typed. Typing on or clearing the field drops it and restores the
/// filters it set (`AppModel.revertNaturalSearch`).
struct NaturalSearchOverride: Equatable {
    /// The typed phrase this reading belongs to.
    var query: String
    /// What the list searches instead of `query`; empty = only the filters below.
    var keyword: String
    /// Filters before and after the reading; only those still as `applied` are restored.
    var before: NaturalSearchState
    var beforeSelection: SidebarItem?
    var applied: NaturalSearchState
    var appliedSelection: SidebarItem?
}

/// Optional on-device natural-language search (Apple FoundationModels, Einstellungen › Allgemein).
/// Privacy: only the typed search text, today's date and the category names reach the model, never
/// bookings, balances or IBANs; the model runs on the device.
enum NaturalSearch {
    static let defaultsKey = "search.naturalLanguage"

    static var isAvailable: Bool {
        #if canImport(FoundationModels)
        if #available(macOS 26, iOS 26, *) {
            return SystemLanguageModel.default.availability == .available
        }
        #endif
        return false
    }

    /// Opt-in, off by default.
    static var isEnabled: Bool { UserDefaults.standard.bool(forKey: defaultsKey) && isAvailable }

    /// A phrase worth refining: three or more words, or two when one is a filler word
    /// ("zeige amazon", "amazon buchungen").
    static func looksNatural(_ query: String) -> Bool {
        let words = tokens(query)
        if words.count >= 3 { return true }
        return words.count == 2 && words.contains { fillerWords.contains($0.lowercased()) }
    }

    private static let fillerWords: Set<String> = [
        "zeige", "zeig", "mir", "alle", "sämtliche", "saemtliche",
        "buchungen", "buchung", "umsätze", "umsaetze", "umsatz",
        "zahlungen", "zahlung", "transaktionen", "transaktion",
        "ausgaben", "einnahmen", "von", "bei", "fuer", "für", "mit", "nach", "suche",
        "show", "me", "all", "transactions", "transaction", "payments", "bookings", "find", "search", "for", "from"
    ]

    /// Words of a phrase without separators and surrounding punctuation or quotes.
    private static func tokens(_ raw: String) -> [String] {
        raw.split(whereSeparator: { $0.isWhitespace || $0 == "," || $0 == ":" || $0 == ";" })
            .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".!?\"'„“”‚‘’»«")) }
            .filter { !$0.isEmpty }
    }

    /// Strips conversational wrapper words (e.g. "zeige alle amazon" -> "amazon").
    /// A booking compound with a plain booking noun ("mietzahlungen", "amazonbestellungen") shrinks to its
    /// stem like in `literalSearchTerm`. With `dropDateWords` (a date filter is set) month and period
    /// words and years go as well: the period filter already says them.
    static func stripFillerWords(_ raw: String, dropDateWords: Bool = false) -> String {
        tokens(raw).filter { word in
            let lower = word.lowercased()
            if fillerWords.contains(lower) { return false }
            return !(dropDateWords && (dateWords.contains(lower) || isYear(lower)))
        }.map { word -> String in
            if let split = splitBookingCompound(word), split.strong { return split.stem }
            return stemPlural(word)
        }.joined(separator: " ")
    }

    private static func isYear(_ word: String) -> Bool {
        word.count == 4 && (word.hasPrefix("19") || word.hasPrefix("20")) && word.allSatisfy(\.isNumber)
    }

    /// German plurals the substring search would miss ("mieten" vs "Miete", "kredite" vs "Kredit"):
    /// the word itself first, then shorter stems, longest first. Only letters-only words of at least
    /// 6 characters are shortened, so "miete" and "kredit" stay as they are and IBANs, amounts and
    /// years are never touched. Every form is a prefix of the word, so searching for it finds at
    /// least what the word found. Umlaut plurals ("verträge" → "Vertrag") only meet their singular
    /// where the comparison ignores diacritics (category matching), not in the core's LIKE.
    static func wordForms(_ word: String) -> [String] {
        var forms = [word]
        guard word.count >= 6, word.allSatisfy(\.isLetter) else { return forms }
        let lower = word.lowercased()
        if lower.hasSuffix("en") {
            forms.append(String(word.dropLast()))   // mieten → miete
            forms.append(String(word.dropLast(2)))  // versicherungen → versicherung
        } else if lower.hasSuffix("e") || lower.hasSuffix("s") {
            forms.append(String(word.dropLast()))   // kredite → kredit, depots → depot
        }
        return forms
    }

    /// The shortest form of `word`: what the substring search looks for.
    static func stemPlural(_ word: String) -> String { wordForms(word).last ?? word }

    /// Whether two words are the same noun up to a German plural ending ("mieten" / "Miete").
    static func sameWord(_ a: String, _ b: String) -> Bool {
        let opts: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
        let bForms = wordForms(b)
        return wordForms(a).contains { x in bForms.contains { x.compare($0, options: opts) == .orderedSame } }
    }

    /// `phrase` with every word cut back to its plural stem ("alle kredite" → "alle kredit"). The core
    /// searches each whitespace-separated word on its own, so each stem is still a substring of what
    /// the word matched.
    static func stemWords(_ phrase: String) -> String {
        phrase.split(whereSeparator: \.isWhitespace).map { stemPlural(String($0)) }.joined(separator: " ")
    }

    /// What the core searches for when the user typed `text` into the search field (literal path).
    /// A booking compound with a plain booking noun ("amazonbestellungen") searches its stem ("amazon").
    static func literalSearchTerm(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return text }
        return trimmed.split(whereSeparator: \.isWhitespace).map { raw -> String in
            let word = String(raw)
            if let split = splitBookingCompound(word), split.strong { return split.stem }
            return stemPlural(word)
        }.joined(separator: " ")
    }

    /// Lowercased, umlauts spelled out (ä→ae, ß→ss), other diacritics dropped.
    static func fold(_ s: String) -> String {
        s.lowercased()
            .replacingOccurrences(of: "ä", with: "ae").replacingOccurrences(of: "ö", with: "oe")
            .replacingOccurrences(of: "ü", with: "ue").replacingOccurrences(of: "ß", with: "ss")
            .folding(options: .diacriticInsensitive, locale: nil)
    }

    /// Booking nouns that only say "a booking of …": the stem may replace the word in the text search.
    private static let strongBookingSuffixes = [
        "transaktionen", "transaktion", "bestellungen", "bestellung",
        "zahlungen", "zahlung", "buchungen", "buchung", "umsaetze", "umsatz",
    ].sorted { $0.count > $1.count }
    /// Nouns with meaning of their own (Nebenkosten ≠ neben): their stem only serves to match a category.
    private static let weakBookingSuffixes = [
        "rechnungen", "rechnung", "einnahmen", "einnahme", "gebuehren", "gebuehr",
        "beitraege", "beitrag", "ausgaben", "ausgabe", "kosten", "raten", "rate", "abos", "abo",
    ].sorted { $0.count > $1.count }
    /// Fugen-s only after endings where German regularly inserts it (versicherungs-, gesundheits-).
    private static let fugenSEndings = ["ungs", "heits", "keits", "schafts", "ions", "taets"]

    /// `mietzahlungen` → ("miet", strong); `versicherungsbeitraege` → ("versicherung", weak); nil without
    /// a booking suffix or when the stem would be shorter than 3 letters.
    static func splitBookingCompound(_ word: String) -> (stem: String, strong: Bool)? {
        let w = fold(word)
        for (list, strong) in [(strongBookingSuffixes, true), (weakBookingSuffixes, false)] {
            for suffix in list where w.hasSuffix(suffix) && w != suffix {
                var stem = String(w.dropLast(suffix.count))
                if fugenSEndings.contains(where: { stem.hasSuffix($0) }) { stem.removeLast() }
                guard stem.count >= 3, stem.allSatisfy(\.isLetter) else { continue }
                return (stem, strong)
            }
        }
        return nil
    }

    /// Forms a category name may be met as: "Miete" → [miete, miet]; "Kredite" → [kredite, kredit].
    static func categoryForms(_ name: String) -> Set<String> {
        let n = fold(name)
        var forms = Set(wordForms(n)).union([n])
        for ending in ["en", "n", "e", "s"] where n.hasSuffix(ending) && n.count - ending.count >= 4 {
            forms.insert(String(n.dropLast(ending.count)))
        }
        return forms
    }

    /// Word forms of what the user typed, including the stem of a booking compound.
    static func queryForms(_ word: String) -> Set<String> {
        let w = fold(word)
        var forms = Set(wordForms(w)).union([w])
        if let split = splitBookingCompound(w) {
            forms.formUnion(wordForms(split.stem))
            forms.insert(split.stem)
        }
        return forms
    }

    /// Content words of a category name: "Kredite & Darlehen" → ["kredite", "darlehen"].
    static func categoryNameWords(_ name: String) -> [String] {
        fold(name).split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init)
            .filter { $0.count >= 3 && !fillerWords.contains($0) }
    }

    /// True when `word` (or the stem of a strong booking compound: "mietzahlungen" → "miet") is one whole
    /// word of the category name up to a plural ending: "kredite" names "Kredite & Darlehen" but not
    /// "Kreditkartenabrechnung".
    static func namesCategoryWord(_ word: String, _ categoryName: String) -> Bool {
        let nameWords = categoryNameWords(categoryName)
        if nameWords.contains(where: { sameWord(word, $0) }) { return true }
        if let split = splitBookingCompound(word), split.strong {
            return nameWords.contains { sameWord(split.stem, $0) || categoryForms($0).contains(split.stem) }
        }
        return false
    }

    /// The one category whose name contains `word` as a whole word; with several, the one whose name
    /// starts with it.
    static func wordLevelCategory(_ word: String, in categories: [GiroCrabCore.Category]) -> Int64? {
        let hits = categories.filter { namesCategoryWord(word, $0.name) }
        if hits.count == 1 { return hits[0].id }
        let leading = hits.filter { categoryNameWords($0.name).first.map { first in sameWord(word, first) } ?? false }
        return leading.count == 1 ? leading[0].id : nil
    }

    /// True when `word` names the category: same form, or a stem of at least 4 letters that the
    /// name extends by at most 2 letters ("miet" → "Miete", but not "Mieteinnahmen").
    static func wordMatchesCategory(_ word: String, _ category: String) -> Bool {
        if namesCategoryWord(word, category) { return true }
        let q = queryForms(word), c = categoryForms(category)
        if !q.isDisjoint(with: c) { return true }
        let name = fold(category)
        return q.contains { $0.count >= 4 && name.hasPrefix($0) && name.count - $0.count <= 2 }
    }

    /// Words that point at a period, an amount or a direction: such a phrase needs the model.
    private static let structuredWords: Set<String> = [
        "heute", "gestern", "vorgestern", "letzten", "letzte", "letzter", "letztes", "vorigen", "vorige",
        "diesen", "diese", "dieser", "dieses", "seit", "bis", "zwischen", "über", "ueber", "unter", "ab",
        "mehr", "weniger", "als", "euro", "eur", "€",
        "tag", "tage", "tagen", "woche", "wochen", "monat", "monate", "monaten", "monats",
        "quartal", "jahr", "jahre", "jahren", "jahres",
        "januar", "februar", "märz", "maerz", "april", "mai", "juni", "juli", "august",
        "september", "oktober", "november", "dezember",
        "today", "yesterday", "last", "this", "since", "until", "between", "over", "under", "above", "below",
        "more", "less", "than", "day", "days", "week", "weeks", "month", "months", "year", "years", "quarter",
        "january", "february", "march", "may", "june", "july", "october", "december"
    ]

    /// Words that name a time span; a query without them (and without a date number) never filters by date.
    /// "ab"/"bis" are left out on purpose: they also introduce amounts ("ab 50 euro").
    static let dateWords: Set<String> = [
        "heute", "gestern", "vorgestern", "morgen", "woche", "wochen", "monat", "monate", "monaten", "monats",
        "jahr", "jahre", "jahren", "jahres", "quartal", "letzte", "letzten", "letzter", "letztes",
        "vorige", "vorigen", "vorjahr", "vormonat", "diese", "diesen", "dieser", "dieses", "seit", "zwischen",
        "wochenende", "tag", "tage", "tagen",
        "januar", "jan", "februar", "feb", "märz", "maerz", "mär", "mrz", "april", "apr", "mai", "juni", "jun",
        "juli", "jul", "august", "aug", "september", "sep", "sept", "oktober", "okt", "november", "nov",
        "dezember", "dez",
        "today", "yesterday", "week", "weeks", "month", "months", "year", "years", "quarter", "last", "since",
        "this", "until", "between", "day", "days",
        "january", "february", "march", "may", "june", "july", "october", "december"
    ]

    /// True when the query names a period: a date word, a year (19xx/20xx) or a day.month[.year] / ISO date.
    static func mentionsDate(_ query: String) -> Bool {
        let lower = query.lowercased()
        let words = lower.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init)
        if words.contains(where: dateWords.contains) { return true }
        if words.contains(where: { $0.count == 4 && ($0.hasPrefix("19") || $0.hasPrefix("20")) && $0.allSatisfy(\.isNumber) }) {
            return true
        }
        return lower.range(of: #"\b\d{1,2}\.\d{1,2}\.?(\d{2,4})?\b|\b\d{4}-\d{2}(-\d{2})?\b"#,
                           options: .regularExpression) != nil
    }

    /// Drops invented dates: a query without a period never filters by date.
    static func withoutUnnamedDates(_ parsed: ParsedSearchQuery, for query: String) -> ParsedSearchQuery {
        guard parsed.fromDate != nil || parsed.toDate != nil, !mentionsDate(query) else { return parsed }
        var cleaned = parsed
        cleaned.fromDate = nil
        cleaned.toDate = nil
        return cleaned
    }

    /// A pure keyword search wrapped in filler words ("zeige alle amazon", "alle amazon buchungen"):
    /// the keyword, without asking the model. Nil when the phrase also names a period, an amount,
    /// a number or one of the categories, or when nothing but filler words is left.
    static func keywordOnly(_ query: String, categoryNames: [String]) -> String? {
        let words = tokens(query)
        let kept = words.filter { !fillerWords.contains($0.lowercased()) }
        guard !kept.isEmpty, kept.count < words.count else { return nil }
        for word in kept {
            if structuredWords.contains(word.lowercased()) { return nil }
            if word.contains(where: \.isNumber) || word.contains("€") { return nil }
            if categoryNames.contains(where: { sameWord($0, word) || namesCategoryWord(word, $0) }) { return nil }
        }
        return stemWords(kept.joined(separator: " "))
    }

    /// Resolves "mieten", "zeige alle mietzahlungen", "kredite" to a category filter without the model:
    /// only when every word left after the filler words (at most two) names the same category.
    static func categoryOnly(_ query: String, categories: [GiroCrabCore.Category]) -> ParsedSearchQuery? {
        let words = tokens(query).filter { !fillerWords.contains($0.lowercased()) }
        guard !words.isEmpty, words.count <= 2 else { return nil }
        var ids = Set<Int64>()
        for word in words {
            guard !structuredWords.contains(word.lowercased()), !word.contains(where: \.isNumber),
                  let id = matchCategory(word, in: categories),
                  let name = categories.first(where: { $0.id == id })?.name,
                  wordMatchesCategory(word, name) else { return nil }
            ids.insert(id)
        }
        guard ids.count == 1, let id = ids.first,
              let name = categories.first(where: { $0.id == id })?.name else { return nil }
        return ParsedSearchQuery(text: "", categoryName: name)
    }

    /// Maps a parsed query onto the filters. Unknown categories, invalid dates and negative or
    /// non-finite amounts are ignored; with nothing usable the state stays untouched.
    static func apply(parsed: ParsedSearchQuery, to state: inout NaturalSearchState,
                      categories: [GiroCrabCore.Category], today: Date) {
        var next = state
        var found = false

        let from = parsed.fromDate.flatMap(DateText.parseISODay)
        let to = parsed.toDate.flatMap(DateText.parseISODay)
        if from != nil || to != nil {
            var a = from ?? to!, b = to ?? from!
            if a > b { swap(&a, &b) }
            next.period = .custom
            next.customFrom = DateText.iso(a)
            next.customTo = DateText.iso(b)
            found = true
        }

        var matchedCategoryName: String?
        if let name = parsed.categoryName, let id = matchCategory(name, in: categories) {
            next.categoryID = id
            matchedCategoryName = categories.first { $0.id == id }?.name
            found = true
        }

        let lo = cents(parsed.minAmount), hi = cents(parsed.maxAmount)
        if lo != nil || hi != nil {
            next.amountFilter = AmountFilter(minCents: lo, maxCents: hi).normalized
            found = true
        }

        let currentTrimmed = state.searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        // The model named no category (or none that matched), yet a word of the phrase names exactly
        // one ("zeige alle mietzahlungen november" → Miete): select it instead of searching its text.
        var consumed = Set<String>()
        if matchedCategoryName == nil {
            let source = parsed.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? currentTrimmed : parsed.text
            var hits: [Int64: [String]] = [:]
            for word in tokens(source) {
                let lower = word.lowercased()
                guard !fillerWords.contains(lower), !dateWords.contains(lower), !structuredWords.contains(lower),
                      !word.contains(where: \.isNumber),
                      let id = matchCategory(word, in: categories),
                      let name = categories.first(where: { $0.id == id })?.name,
                      wordMatchesCategory(word, name) else { continue }
                hits[id, default: []].append(lower)
            }
            if hits.count == 1, let (id, words) = hits.first {
                next.categoryID = id
                matchedCategoryName = categories.first { $0.id == id }?.name
                consumed = Set(words)
                found = true
            }
        }

        let remaining = tokens(parsed.text).filter { !consumed.contains($0.lowercased()) }.joined(separator: " ")
        var text = stripFillerWords(remaining, dropDateWords: from != nil || to != nil)
        // The model found neither a filter nor a keyword (empty text): the typed phrase without
        // its filler words is still the best keyword ("zeige alle amazon" -> "amazon").
        if text.isEmpty && !found { text = stripFillerWords(currentTrimmed) }
        // The category already says it: as an extra keyword it would only cut away bookings whose
        // purpose or counterparty lacks the word ("zeige alle miete" with category Miete).
        if let category = matchedCategoryName {
            // Drop every word that only names the category ("miet", "mietzahlungen"): as a substring it
            // would filter out bookings of that category whose text lacks it ("Nebenkosten Abschlag").
            text = text.split(separator: " ")
                .filter { !wordMatchesCategory(String($0), category) }
                .joined(separator: " ")
        }
        let textChanged = !text.isEmpty && text.caseInsensitiveCompare(currentTrimmed) != .orderedSame
        guard found || textChanged else { return }
        next.searchText = text
        state = next
    }

    private static func cents(_ euros: Double?) -> Int64? {
        guard let euros, euros.isFinite, euros >= 0, euros < 1e9 else { return nil }
        return Int64((euros * 100).rounded())
    }

    /// Exact (case-insensitive) name first, then a unique partial match.
    static func matchCategory(_ name: String, in categories: [GiroCrabCore.Category]) -> Int64? {
        let q = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return nil }
        let opts: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
        let forms = wordForms(q)
        // An exact hit on any form first: "mieten" is "Miete", not "Mieteinnahmen".
        for form in forms {
            if let hit = categories.first(where: { $0.name.compare(form, options: opts) == .orderedSame }) { return hit.id }
        }
        // A whole word of a longer name ("kredite" → "Kredite & Darlehen", never "Kreditkartenabrechnung").
        if let id = wordLevelCategory(q, in: categories) { return id }
        // Then a unique plural / compound match ("miet", "mietzahlungen" → "Miete").
        let named = categories.filter { wordMatchesCategory(q, $0.name) }
        if named.count == 1 { return named[0].id }
        for form in forms {
            let partial = categories.filter { $0.name.range(of: form, options: opts) != nil }
            if partial.count == 1 { return partial[0].id }
            if partial.count > 1 { return nil } // ambiguous; a shorter stem only matches more
        }
        return nil
    }

    /// The filters for a phrase: a pure keyword search directly, everything else from the
    /// on-device model. Without a model answer (unavailable, error, timeout) the phrase itself,
    /// which `apply` then strips of its filler words.
    static func understand(_ query: String, categoryNames: [String], today: String) async -> ParsedSearchQuery {
        // (The category-only reading needs the categories themselves and runs before this in `scheduleSearch`.)
        if let keyword = keywordOnly(query, categoryNames: categoryNames) { return ParsedSearchQuery(text: keyword) }
        let parsed = await parse(query, categoryNames: categoryNames, today: today) ?? ParsedSearchQuery(text: query)
        return withoutUnnamedDates(parsed, for: query)
    }

    /// Asks the on-device model; nil on any failure or after 8 s.
    static func parse(_ query: String, categoryNames: [String], today: String) async -> ParsedSearchQuery? {
        #if canImport(FoundationModels)
        guard #available(macOS 26, iOS 26, *), isAvailable else { return nil }
        return await withTaskGroup(of: ParsedSearchQuery?.self) { group in
            group.addTask { await generate(query, categoryNames: categoryNames, today: today) }
            group.addTask { try? await Task.sleep(nanoseconds: 8_000_000_000); return nil }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first
        }
        #else
        return nil
        #endif
    }
}

#if canImport(FoundationModels)
@available(macOS 26, iOS 26, *)
@Generable
struct GeneratedSearchQuery {
    @Guide(description: "Remaining core search keyword (merchant, purpose) stripped of filler words like alle, buchungen, umsaetze, show, transactions, and without period, category or amount words; empty if none")
    var text: String
    @Guide(description: "Start of the period as YYYY-MM-DD, only if the phrase names a period")
    var fromDate: String?
    @Guide(description: "End of the period as YYYY-MM-DD, only if the phrase names a period")
    var toDate: String?
    @Guide(description: "Exactly one name from the given category list, only if the phrase names it")
    var categoryName: String?
    @Guide(description: "Lower amount limit in euros (positive number), e.g. for 'over 50'")
    var minAmount: Double?
    @Guide(description: "Upper amount limit in euros (positive number), e.g. for 'under 20'")
    var maxAmount: Double?
}

@available(macOS 26, iOS 26, *)
private func generate(_ query: String, categoryNames: [String], today: String) async -> ParsedSearchQuery? {
    // Only these three inputs leave the app's own data: search text, today's date, category names.
    let session = LanguageModelSession(instructions: """
        You turn a bookkeeping search phrase into filters. Today is \(today). \
        Categories: \(categoryNames.joined(separator: ", ")). \
        Resolve relative periods (last month, this year) to concrete dates. Never invent values. \
        A merchant, shop or person name (e.g. a store or online shop) always goes into text, \
        never into categoryName unless the phrase names that category itself.
        """)
    guard let out = try? await session.respond(to: query, generating: GeneratedSearchQuery.self).content
    else { return nil }
    return ParsedSearchQuery(text: out.text, fromDate: out.fromDate, toDate: out.toDate,
                             categoryName: out.categoryName, minAmount: out.minAmount, maxAmount: out.maxAmount)
}
#endif
