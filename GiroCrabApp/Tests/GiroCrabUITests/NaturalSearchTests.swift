// test: serial-defaults (reads UserDefaults.standard: the natural-search toggle)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

final class NaturalSearchTests: XCTestCase {
    private let cats: [GiroCrabCore.Category] = [
        GiroCrabCore.Category(id: 1, parentId: nil, name: "Lebensmittel", sfSymbol: "tag", colorHex: "", sortOrder: 0, depth: 0,
                              path: "Lebensmittel", ownTotalCents: 0, totalCents: 0, transactionCount: 0, budgetCents: nil),
        GiroCrabCore.Category(id: 2, parentId: nil, name: "Freizeit", sfSymbol: "tag", colorHex: "", sortOrder: 0, depth: 0,
                              path: "Freizeit", ownTotalCents: 0, totalCents: 0, transactionCount: 0, budgetCents: nil),
    ]
    private func state(_ text: String = "orig") -> NaturalSearchState {
        NaturalSearchState(searchText: text, period: .all, customFrom: "2020-01-01", customTo: "2020-01-31",
                           categoryID: nil, amountFilter: AmountFilter())
    }
    private let today = Date()

    func testDateRangeBecomesCustomPeriod() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: " rewe ", fromDate: "2026-10-01", toDate: "2026-10-31"),
                            to: &s, categories: cats, today: today)
        XCTAssertEqual(s.period, .custom)
        XCTAssertEqual(s.customFrom, "2026-10-01")
        XCTAssertEqual(s.customTo, "2026-10-31")
        XCTAssertEqual(s.searchText, "rewe")
    }

    func testReversedDatesAreSwapped() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(fromDate: "2026-10-31", toDate: "2026-10-01"),
                            to: &s, categories: cats, today: today)
        XCTAssertEqual(s.customFrom, "2026-10-01")
        XCTAssertEqual(s.customTo, "2026-10-31")
    }

    func testOneSidedDateUsesItForBoth() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(fromDate: "2026-05-04"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.customFrom, "2026-05-04")
        XCTAssertEqual(s.customTo, "2026-05-04")
    }

    func testAmountMapsToCentsAndIgnoresNegative() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(minAmount: 50, maxAmount: 99.99), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.amountFilter, AmountFilter(minCents: 5000, maxCents: 9999))
        var t = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(minAmount: -5), to: &t, categories: cats, today: today)
        XCTAssertEqual(t, state(), "nothing usable leaves the state untouched")
    }

    func testReversedAmountsAreNormalized() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(minAmount: 200, maxAmount: 50), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.amountFilter, AmountFilter(minCents: 5000, maxCents: 20000))
    }

    func testCategoryMatching() {
        XCTAssertEqual(NaturalSearch.matchCategory("lebensmittel", in: cats), 1)
        XCTAssertEqual(NaturalSearch.matchCategory("Freiz", in: cats), 2)
        XCTAssertNil(NaturalSearch.matchCategory("Reisen", in: cats))
        XCTAssertNil(NaturalSearch.matchCategory("  ", in: cats))
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(categoryName: "Lebensmittel"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.categoryID, 1)
        XCTAssertEqual(s.searchText, "")
    }

    func testInvalidDateAndUnknownCategoryChangeNothing() {
        var s = state()
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "", fromDate: "morgen", categoryName: "Nope"),
                            to: &s, categories: cats, today: today)
        XCTAssertEqual(s, state())
    }

    func testLooksNaturalNeedsThreeWordsOrAFillerWord() {
        XCTAssertFalse(NaturalSearch.looksNatural("rewe"))
        XCTAssertFalse(NaturalSearch.looksNatural("rewe berlin"))
        XCTAssertTrue(NaturalSearch.looksNatural("Lebensmittel letzten Monat"))
        XCTAssertTrue(NaturalSearch.looksNatural("zeige amazon"))
        XCTAssertTrue(NaturalSearch.looksNatural("amazon Buchungen"))
    }

    func testKeywordOnlyExtractsTheKeywordWithoutModel() {
        let names = cats.map(\.name)
        XCTAssertEqual(NaturalSearch.keywordOnly("zeige alle amazon", categoryNames: names), "amazon")
        XCTAssertEqual(NaturalSearch.keywordOnly("alle amazon buchungen", categoryNames: names), "amazon")
        XCTAssertEqual(NaturalSearch.keywordOnly("Zeige mir alle Umsätze von Amazon!", categoryNames: names), "Amazon")
        XCTAssertEqual(NaturalSearch.keywordOnly("show all „amazon“ transactions", categoryNames: names), "amazon")
        XCTAssertEqual(NaturalSearch.keywordOnly("zeige alle kredite", categoryNames: []), "kredit")
        XCTAssertEqual(NaturalSearch.keywordOnly("zeige alle kredit", categoryNames: []), "kredit")
        XCTAssertEqual(NaturalSearch.keywordOnly("zeige alle mieten", categoryNames: []), "miet")
    }

    func testWordFormsCoverGermanPlurals() {
        XCTAssertEqual(NaturalSearch.wordForms("mieten"), ["mieten", "miete", "miet"])
        XCTAssertEqual(NaturalSearch.wordForms("miete"), ["miete"], "5 characters: unchanged")
        XCTAssertEqual(NaturalSearch.wordForms("kredite"), ["kredite", "kredit"])
        XCTAssertEqual(NaturalSearch.wordForms("kredit"), ["kredit"])
        XCTAssertEqual(NaturalSearch.wordForms("versicherungen").last, "versicherung")
        XCTAssertEqual(NaturalSearch.stemPlural("rechnungen"), "rechnung")
        XCTAssertEqual(NaturalSearch.stemPlural("abonnements"), "abonnement")
        XCTAssertEqual(NaturalSearch.stemPlural("amazon"), "amazon")
        XCTAssertEqual(NaturalSearch.stemPlural("DE89370400440532013000"), "DE89370400440532013000")
    }

    func testStemIsAlwaysASubstringOfTheWord() {
        for w in ["mieten", "Kredite", "Versicherungen", "gebühren", "depots", "Ostsee"] {
            XCTAssertTrue(w.lowercased().contains(NaturalSearch.stemPlural(w).lowercased()), w)
        }
    }

    func testLiteralSearchTermStemsEveryWord() {
        XCTAssertEqual(NaturalSearch.literalSearchTerm("kredite"), "kredit")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("Mieten"), "Miet", "case kept")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("miete"), "miete")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("kredite sparkasse"), "kredit sparkass")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("miete 2024"), "miete 2024")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("  "), "  ")
    }

    func testPluralMatchesSingularCategory() {
        func cat(_ id: Int64, _ name: String) -> GiroCrabCore.Category {
            var c = cats[0]
            c.id = id
            c.name = name
            return c
        }
        let list = [cat(1, "Miete"), cat(2, "Mieteinnahmen"), cat(3, "Kredit"), cat(4, "Versicherungen")]
        XCTAssertEqual(NaturalSearch.matchCategory("mieten", in: list), 1, "exact form beats partial")
        XCTAssertEqual(NaturalSearch.matchCategory("miete", in: list), 1)
        XCTAssertEqual(NaturalSearch.matchCategory("kredite", in: list), 3)
        XCTAssertEqual(NaturalSearch.matchCategory("kredit", in: list), 3)
        XCTAssertEqual(NaturalSearch.matchCategory("versicherung", in: list), 4)
    }

    func testWordOfALongerCategoryNameBeatsSimilarNames() {
        func cat(_ id: Int64, _ name: String) -> GiroCrabCore.Category {
            var c = cats[0]
            c.id = id
            c.name = name
            return c
        }
        let demo = [cat(1, "Kredite & Darlehen"), cat(2, "Darlehensraten"), cat(3, "Kreditkartenabrechnung")]
        XCTAssertEqual(NaturalSearch.matchCategory("kredite", in: demo), 1)
        XCTAssertEqual(NaturalSearch.matchCategory("kredit", in: demo), 1)
        XCTAssertEqual(NaturalSearch.matchCategory("darlehen", in: demo), 1)
        XCTAssertTrue(NaturalSearch.wordMatchesCategory("kredite", "Kredite & Darlehen"))
        XCTAssertFalse(NaturalSearch.wordMatchesCategory("kredite", "Kreditkartenabrechnung"))
        XCTAssertNotEqual(NaturalSearch.matchCategory("kreditraten", in: demo), 3)
        for q in ["zeige alle kredite", "kredite"] {
            XCTAssertEqual(NaturalSearch.categoryOnly(q, categories: demo)?.categoryName, "Kredite & Darlehen", q)
        }
        let own = [cat(1, "Kredite"), cat(2, "Kreditraten"), cat(3, "Kreditkartenabrechnung")]
        XCTAssertEqual(NaturalSearch.matchCategory("kredite", in: own), 1)
        XCTAssertEqual(NaturalSearch.matchCategory("kreditraten", in: own), 2)
    }

    func testStripFillerWordsReducesCompoundsAndDateWords() {
        XCTAssertEqual(NaturalSearch.stripFillerWords("amazonbestellungen"), "amazon")
        XCTAssertEqual(NaturalSearch.stripFillerWords("mietzahlungen november", dropDateWords: true), "miet")
        XCTAssertEqual(NaturalSearch.stripFillerWords("rewe november 2026", dropDateWords: true), "rewe")
        XCTAssertEqual(NaturalSearch.stripFillerWords("rewe november"), "rewe november")
    }

    /// The model left `categoryName` empty and kept the rent word in the text.
    func testApplyDerivesCategoryFromTextWhenModelOmitsIt() {
        func cat(_ id: Int64, _ name: String) -> GiroCrabCore.Category {
            var c = cats[0]
            c.id = id
            c.name = name
            return c
        }
        let list = [cat(1, "Miete"), cat(2, "Mieteinnahmen"), cat(3, "Lebensmittel")]
        for text in ["mietzahlungen", "mietzahlungen november", ""] {
            var s = state("zeige alle mietzahlungen november")
            NaturalSearch.apply(parsed: ParsedSearchQuery(text: text, fromDate: "2026-11-01", toDate: "2026-11-30"),
                                to: &s, categories: list, today: today)
            XCTAssertEqual(s.categoryID, 1, text)
            XCTAssertEqual(s.period, .custom, text)
            XCTAssertEqual(s.customFrom, "2026-11-01", text)
            XCTAssertEqual(s.customTo, "2026-11-30", text)
            XCTAssertEqual(s.searchText, "", text)
        }
        var s = state("gutscheine")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "gutscheine"), to: &s, categories: list, today: today)
        XCTAssertNil(s.categoryID)
        XCTAssertEqual(s.searchText, "gutschein")
    }

    func testSplitsBookingCompounds() {
        func split(_ w: String) -> String? { NaturalSearch.splitBookingCompound(w).map { "\($0.stem)|\($0.strong)" } }
        XCTAssertEqual(split("mietzahlungen"), "miet|true")
        XCTAssertEqual(split("mietbuchungen"), "miet|true")
        XCTAssertEqual(split("amazonbestellungen"), "amazon|true")
        XCTAssertEqual(split("kreditraten"), "kredit|false")
        XCTAssertEqual(split("versicherungsbeitraege"), "versicherung|false")
        XCTAssertNil(split("zahlungen"), "only the suffix")
        XCTAssertNil(split("abrate"), "stem shorter than 3")
    }

    func testLiteralTermKeepsMeaningfulNouns() {
        XCTAssertEqual(NaturalSearch.literalSearchTerm("amazonbestellungen"), "amazon")
        XCTAssertEqual(NaturalSearch.literalSearchTerm("nebenkosten"), "nebenkost")
    }

    func testShortStemsMatchCategory() {
        XCTAssertTrue(NaturalSearch.wordMatchesCategory("miet", "Miete"))
        XCTAssertTrue(NaturalSearch.wordMatchesCategory("mietzahlungen", "Miete"))
        XCTAssertFalse(NaturalSearch.wordMatchesCategory("miet", "Mieteinnahmen"))
        XCTAssertTrue(NaturalSearch.wordMatchesCategory("kreditraten", "Kredit"))
    }

    func testCategoryOnlyResolvesWithoutModel() {
        func cat(_ id: Int64, _ name: String) -> GiroCrabCore.Category {
            var c = cats[0]
            c.id = id
            c.name = name
            return c
        }
        let list = [cat(1, "Miete"), cat(2, "Mieteinnahmen"), cat(3, "Kredit"), cat(4, "Nebenkosten")]
        for q in ["zeige alle mieten", "mieten", "zeige alle mietzahlungen", "mietzahlungen", "mietbuchungen"] {
            XCTAssertEqual(NaturalSearch.categoryOnly(q, categories: list)?.categoryName, "Miete", q)
        }
        for q in ["zeige alle kredite", "kredite", "kreditraten"] {
            XCTAssertEqual(NaturalSearch.categoryOnly(q, categories: list)?.categoryName, "Kredit", q)
        }
        XCTAssertNil(NaturalSearch.categoryOnly("miete rewe", categories: list))
    }

    func testKeywordOnlyLeavesStructuredPhrasesToTheModel() {
        let names = cats.map(\.name)
        XCTAssertNil(NaturalSearch.keywordOnly("amazon", categoryNames: names), "no filler word: literal search")
        XCTAssertNil(NaturalSearch.keywordOnly("zeige alle buchungen", categoryNames: names), "nothing left")
        XCTAssertNil(NaturalSearch.keywordOnly("alle amazon letzten Monat", categoryNames: names))
        XCTAssertNil(NaturalSearch.keywordOnly("alle amazon über 50", categoryNames: names))
        XCTAssertNil(NaturalSearch.keywordOnly("alle buchungen 2025", categoryNames: names))
        XCTAssertNil(NaturalSearch.keywordOnly("zeige alle lebensmittel", categoryNames: names), "a category")
        XCTAssertNil(NaturalSearch.keywordOnly("zeige alle mieten", categoryNames: ["Miete"]))
        XCTAssertNil(NaturalSearch.keywordOnly("zeige alle kredite", categoryNames: ["Kredit"]))
    }

    func testStripFillerWordsDropsPunctuation() {
        XCTAssertEqual(NaturalSearch.stripFillerWords("zeige alle amazon?"), "amazon")
        XCTAssertEqual(NaturalSearch.stripFillerWords("Buchungen: amazon, rewe."), "amazon rewe")
    }

    /// The model echoes the typed phrase unchanged.
    func testUnchangedSentenceFromModelIsStripped() {
        var s = state("zeige alle amazon")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "zeige alle amazon"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.searchText, "amazon")
    }

    /// The model returns an empty text and no filter: the typed phrase without filler words.
    func testEmptyModelTextFallsBackToTypedPhrase() {
        var s = state("alle amazon buchungen")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "  "), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.searchText, "amazon")
    }

    /// An empty text next to a recognised filter means the phrase was only that filter.
    func testEmptyModelTextWithFilterClearsSearch() {
        var s = state("Lebensmittel letzten Monat")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "", fromDate: "2026-10-01", toDate: "2026-10-31",
                                                      categoryName: "Lebensmittel"),
                            to: &s, categories: cats, today: today)
        XCTAssertEqual(s.searchText, "")
        XCTAssertEqual(s.categoryID, 1)
    }

    /// No model answer (unavailable, error, timeout): `understand` hands back the phrase itself.
    func testNoModelAnswerStillStripsFillerWords() async {
        let parsed = await NaturalSearch.understand("zeige alle amazon", categoryNames: [], today: "2026-11-18")
        XCTAssertEqual(parsed, ParsedSearchQuery(text: "amazon"))
        var s = state("bitte zeige amazon")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "bitte zeige amazon"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.searchText, "bitte amazon")
    }

    func testOnlyFillerWordsChangeNothing() {
        var s = state("zeige alle buchungen")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "zeige alle buchungen"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s, state("zeige alle buchungen"))
    }

    func testDisabledByDefault() {
        XCTAssertFalse(UserDefaults.standard.bool(forKey: NaturalSearch.defaultsKey))
        XCTAssertFalse(NaturalSearch.isEnabled)
    }

    // MARK: - No period named, no date filter

    func testDropsInventedDatesWithoutTimeWords() {
        let invented = ParsedSearchQuery(text: "miete", fromDate: "2026-11-18", toDate: "2026-11-18")
        let cleaned = NaturalSearch.withoutUnnamedDates(invented, for: "zeige alle miete")
        XCTAssertNil(cleaned.fromDate)
        XCTAssertNil(cleaned.toDate)
        XCTAssertEqual(cleaned.text, "miete")
        let amazon = ParsedSearchQuery(text: "amazon", fromDate: "2026-11-18", toDate: "2026-11-18")
        XCTAssertNil(NaturalSearch.withoutUnnamedDates(amazon, for: "zeige alle amazon buchungen").fromDate)
    }

    func testAmountNumbersAreNoDate() {
        XCTAssertFalse(NaturalSearch.mentionsDate("amazon über 50 euro"))
        XCTAssertFalse(NaturalSearch.mentionsDate("zeige alle miete"))
        XCTAssertFalse(NaturalSearch.mentionsDate("alle amazon ab 20 euro"))
    }

    func testNamedPeriodsKeepTheirDates() {
        for query in ["Miete im November", "miete letzten monat", "amazon 2025", "miete seit 01.10.", "miete 2026-10"] {
            XCTAssertTrue(NaturalSearch.mentionsDate(query), query)
        }
        let parsed = ParsedSearchQuery(text: "miete", fromDate: "2026-11-01", toDate: "2026-11-30")
        XCTAssertEqual(NaturalSearch.withoutUnnamedDates(parsed, for: "Miete im November").fromDate, "2026-11-01")
    }

    /// A reading without dates leaves the period alone.
    func testApplyWithoutDatesKeepsPeriod() {
        var s = state("zeige alle miete")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "miete"), to: &s, categories: cats, today: today)
        XCTAssertEqual(s.searchText, "miete")
        XCTAssertEqual(s.period, .all)
        XCTAssertEqual(s.customFrom, "2020-01-01")
        XCTAssertEqual(s.customTo, "2020-01-31")
    }

    /// Category and the same word as keyword would AND each other: the category alone is enough.
    func testKeywordEqualToCategoryNameIsDropped() {
        let withRent = cats + [GiroCrabCore.Category(id: 3, parentId: nil, name: "Miete", sfSymbol: "tag", colorHex: "",
                                                    sortOrder: 0, depth: 0, path: "Miete", ownTotalCents: 0,
                                                    totalCents: 0, transactionCount: 0, budgetCents: nil)]
        var s = state("zeige alle miete")
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "miete", categoryName: "Miete"),
                            to: &s, categories: withRent, today: today)
        XCTAssertEqual(s.categoryID, 3)
        XCTAssertEqual(s.searchText, "")
        XCTAssertEqual(s.period, .all)
    }

    func testStripsFillerWordsEvenWithoutStructuredFilters() {
        var state = NaturalSearchState(
            searchText: "alle amazon buchungen",
            period: .all,
            customFrom: "",
            customTo: "",
            categoryID: nil,
            amountFilter: AmountFilter()
        )
        NaturalSearch.apply(
            parsed: ParsedSearchQuery(text: "amazon"),
            to: &state,
            categories: [],
            today: Date()
        )
        XCTAssertEqual(state.searchText, "amazon")
    }
}

/// The natural reading reaches the booking list next to the typed text (which stays untouched:
/// rewriting the focused macOS search field lost against AppKit) and ends cleanly.
@MainActor
final class NaturalSearchModelTests: XCTestCase {
    private func demoModel(naturalSearch: Bool = true) -> AppModel {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed", "--simulate-system-language", "de"], environment: ["GIROCRAB_SIMULATE_SYSTEM_LANGUAGE": "de"]))
        XCTAssertEqual(model.vaultState, .open)
        model.naturalSearchEnabled = { naturalSearch }
        if model.categories.isEmpty { model.reloadCategories() }
        return model
    }

    /// Waits until the list shows what the vault returns for the model's current filter.
    private func waitForList(_ model: AppModel, file: StaticString = #filePath, line: UInt = #line) async throws -> [Int64] {
        var expected: [Int64] = []
        for _ in 0..<300 {
            expected = try XCTUnwrap(model.vault).listTransactions(filter: model.currentFilter).transactions.map(\.id)
            if model.bookings.map(\.id) == expected { return expected }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("The list was not reloaded for the current filter", file: file, line: line)
        return expected
    }

    private func waitForReading(_ model: AppModel) async throws {
        for _ in 0..<300 where model.naturalSearch == nil { try await Task.sleep(for: .milliseconds(10)) }
    }

    /// The whole path as typed: "zeige alle amazon" searches "amazon", the field keeps the phrase.
    func testTypedKeywordPhraseFindsAmazonBookings() async throws {
        let model = demoModel()
        model.searchText = "zeige alle amazon"
        try await waitForReading(model)
        XCTAssertEqual(model.searchText, "zeige alle amazon", "the field is never rewritten")
        XCTAssertEqual(model.naturalSearch?.keyword, "amazon")
        XCTAssertEqual(model.effectiveSearchText, "amazon")
        XCTAssertEqual(model.currentFilter.search, "amazon")
        let shown = try await waitForList(model)
        XCTAssertFalse(shown.isEmpty, "the demo vault has Amazon bookings")
    }

    func testClearingTheFieldEndsTheReading() async throws {
        let model = demoModel()
        let unfiltered = try XCTUnwrap(model.vault).listTransactions(filter: model.currentFilter).transactions.map(\.id)
        model.searchText = "zeige alle amazon"
        try await waitForReading(model)
        XCTAssertNotNil(model.naturalSearch)

        model.searchText = ""
        XCTAssertNil(model.naturalSearch)
        XCTAssertEqual(model.effectiveSearchText, "")
        XCTAssertNil(model.currentFilter.search)
        let shown = try await waitForList(model)
        XCTAssertEqual(shown, unfiltered)
    }

    func testTypingOnDropsTheReadingAtOnce() async throws {
        let model = demoModel()
        model.searchText = "zeige alle amazon"
        try await waitForReading(model)
        model.searchText = "zeige alle amazon marketplace"
        XCTAssertNil(model.naturalSearch, "the old reading must not outlive the phrase it belongs to")
        XCTAssertEqual(model.currentFilter.search, "zeige alle amazon marketplac", "plural stems: a superset of the typed words")
    }

    /// The literal path: "mieten" / "kredite" search the stem, so the singular is found too.
    func testPluralLiteralSearchFindsSingularBookings() async throws {
        let model = demoModel(naturalSearch: false)
        let vault = try XCTUnwrap(model.vault)
        func ids(_ text: String) throws -> Set<Int64> {
            model.searchText = text
            return Set(try vault.listTransactions(filter: model.currentFilter).transactions.map(\.id))
        }
        for (singular, plural) in [("miete", "mieten"), ("kredit", "kredite")] {
            let one = try ids(singular)
            let many = try ids(plural)
            XCTAssertTrue(one.isSubset(of: many), "\(plural) finds at least what \(singular) finds")
            XCTAssertEqual(model.currentFilter.search, NaturalSearch.stemPlural(plural))
        }
        XCTAssertFalse(try ids("miete").isEmpty, "the demo vault has rent bookings")
        XCTAssertFalse(try ids("mieten").isEmpty)
    }

    /// "zeige alle gutscheine" names no category: it takes the keyword path (no model) and searches the stem.
    func testNaturalPhraseWithPluralSearchesTheStem() async throws {
        let model = demoModel()
        model.searchText = "zeige alle gutscheine"
        try await waitForReading(model)
        XCTAssertEqual(model.naturalSearch?.keyword, "gutschein")
        XCTAssertEqual(model.currentFilter.search, "gutschein")
        XCTAssertEqual(model.searchText, "zeige alle gutscheine")
        _ = try await waitForList(model)
    }

    /// Plurals and booking compounds naming the category "Miete" select it without a text filter.
    func testRentWordsSelectTheCategory() async throws {
        for query in ["zeige alle mieten", "mieten", "zeige alle mietzahlungen", "mietzahlungen"] {
            let model = demoModel()
            model.reloadCategories()
            let rent = try XCTUnwrap(model.categories.first { $0.name == "Miete" }?.id)
            model.searchText = query
            try await waitForReading(model)
            XCTAssertNotNil(model.naturalSearch, query)
            XCTAssertEqual(model.sidebarSelection, .category(rent), query)
            XCTAssertNil(model.currentFilter.search, "no leftover text: \(query)")
            let shown = try await waitForList(model)
            XCTAssertFalse(shown.isEmpty, query)
        }
    }

    /// "kredite" is a word of the demo category "Kredite & Darlehen": it selects that category and
    /// never shows the credit card statements ("Kreditkartenabrechnung").
    func testCreditWordsSelectTheCreditCategory() async throws {
        for query in ["zeige alle kredite", "kredite"] {
            let model = demoModel()
            model.reloadCategories()
            let credit = try XCTUnwrap(model.categories.first { $0.name == "Kredite & Darlehen" }?.id)
            model.searchText = query
            try await waitForReading(model)
            XCTAssertEqual(model.sidebarSelection, .category(credit), query)
            XCTAssertNil(model.currentFilter.search, "no leftover text: \(query)")
            _ = try await waitForList(model)
            XCTAssertFalse(model.bookings.contains { $0.categoryName == "Kreditkartenabrechnung" }, query)
        }
    }

    /// The model left the category empty and the rent word in the text: the period is November and
    /// the category "Miete", nothing stays behind as text.
    func testRentWordWithPeriodSelectsCategoryAndPeriod() {
        let model = demoModel()
        model.reloadCategories()
        let rent = model.categories.first { $0.name == "Miete" }?.id
        XCTAssertNotNil(rent)
        var s = NaturalSearchState(searchText: "zeige alle mietzahlungen november", period: .all, customFrom: "",
                                   customTo: "", categoryID: nil, amountFilter: AmountFilter())
        NaturalSearch.apply(parsed: ParsedSearchQuery(text: "mietzahlungen", fromDate: "2026-11-01", toDate: "2026-11-30"),
                            to: &s, categories: model.categories, today: Date())
        XCTAssertEqual(s.categoryID, rent)
        XCTAssertEqual(s.period, .custom)
        XCTAssertEqual(s.customFrom, "2026-11-01")
        XCTAssertEqual(s.customTo, "2026-11-30")
        XCTAssertEqual(s.searchText, "")
    }

    func testDisabledSearchesLiterally() async throws {
        let model = demoModel(naturalSearch: false)
        model.searchText = "zeige alle amazon"
        _ = try await waitForList(model)
        XCTAssertNil(model.naturalSearch)
        XCTAssertEqual(model.currentFilter.search, "zeige alle amazon")
    }

    /// Filters a reading set go back when the field is cleared; one the user changed since stays.
    func testResetRestoresTheFiltersOfTheReading() {
        let model = demoModel(naturalSearch: false)
        model.searchText = "amazon über 10 euro"
        XCTAssertTrue(model.applyParsedSearch(ParsedSearchQuery(text: "amazon", minAmount: 10)))
        XCTAssertEqual(model.amountFilter.minCents, 1000)
        XCTAssertEqual(model.searchText, "amazon über 10 euro")
        XCTAssertEqual(model.effectiveSearchText, "amazon")
        model.searchText = ""
        XCTAssertFalse(model.amountFilter.isActive)

        model.searchText = "amazon über 10 euro"
        model.applyParsedSearch(ParsedSearchQuery(text: "amazon", minAmount: 10))
        model.amountFilter = AmountFilter(minCents: 500, maxCents: nil)
        model.searchText = ""
        XCTAssertEqual(model.amountFilter.minCents, 500, "the user's own change is kept")
    }

    /// A model that invents today's date for a phrase without a period must not move the period.
    func testInventedDateDoesNotChangeThePeriod() throws {
        let model = demoModel(naturalSearch: false)
        let rent = try XCTUnwrap(model.categories.first { $0.name == "Miete" }?.id)
        let before = (model.period, model.customPeriodFrom, model.customPeriodTo)
        model.searchText = "zeige alle miete"
        model.applyParsedSearch(ParsedSearchQuery(text: "miete", fromDate: "2026-11-18", toDate: "2026-11-18"))
        XCTAssertEqual(model.period, before.0)
        XCTAssertEqual(model.customPeriodFrom, before.1)
        XCTAssertEqual(model.customPeriodTo, before.2)
        XCTAssertEqual(model.sidebarSelection, .category(rent))
        XCTAssertEqual(model.effectiveSearchText, "")
    }

    func testSearchLiterallyKeepsThePhrase() async throws {
        let model = demoModel()
        model.searchText = "zeige alle amazon"
        try await waitForReading(model)
        model.searchLiterally()
        XCTAssertNil(model.naturalSearch)
        _ = try await waitForList(model)
        XCTAssertNil(model.naturalSearch, "not read again for the same phrase")
        XCTAssertEqual(model.currentFilter.search, "zeige alle amazon")
    }
}
