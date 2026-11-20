import CryptoKit
import Foundation
import Observation

// MARK: - Project announcements ("Mitteilungen")
//
// News from the project that do not depend on a release: a signed
// `announcements.json` (same Ed25519 keys, same fetcher and same rules as
// `banking-policy.json`, see docs/announcements.md). The file contains text
// and https links only – no code, nothing that unlocks or adds functions.
//
// Designed along the Apple HIG so it does not nag:
// * never modal, never at launch: one quiet banner above the booking list,
// * at most ONE banner at a time, always dismissible, a dismissed
//   announcement never comes back,
// * everything stays readable later in Hilfe › Mitteilungen,
// * can be switched off in Settings (security locks are separate and not
//   affected: they are not announcements),
// * the check is a plain GET without identifiers, cookies or cache.

public struct Announcement: Equatable, Identifiable, Sendable {
    public enum Severity: String, Sendable { case info, important }

    public var id: String
    public var severity: Severity
    public var from: Date?
    public var until: Date?
    /// nil = all distributions.
    public var distributions: Set<BankingDistribution>?
    public var minVersion: String?
    public var maxVersion: String?
    /// Language code -> text. Falls back to "en", then to any text.
    public var title: [String: String]
    public var summary: [String: String]
    public var body: [String: String]
    public var link: URL?

    public func localized(_ field: [String: String], language: String) -> String {
        field[language] ?? field["en"] ?? field.sorted { $0.key < $1.key }.first?.value ?? ""
    }
}

public struct AnnouncementsDocument: Equatable, Sendable {
    public static let supportedSchema = 1

    public var schema: Int
    public var sequence: UInt64
    public var announcements: [Announcement]

    private struct Raw: Decodable {
        struct Item: Decodable {
            var id: String
            var severity: String?
            var from: String?
            var until: String?
            var distribution: [String]?
            var minVersion: String?
            var maxVersion: String?
            var title: [String: String]
            var summary: [String: String]?
            var body: [String: String]?
            var link: String?
        }
        var schema: Int
        var sequence: UInt64
        var announcements: [Item]
    }

    public enum ParseError: Error, Equatable {
        case malformed
        case unknownSchema(Int)
    }

    private static func day(_ s: String?, endOfDay: Bool) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.timeZone = TimeZone(identifier: "UTC")
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        guard let d = f.date(from: s) else { return nil }
        return endOfDay ? d.addingTimeInterval(24 * 3600 - 1) : d
    }

    /// Parses the exact signed bytes. Entries without id or title are dropped;
    /// unknown severities count as `info` (a newer publisher may know more),
    /// links other than https are ignored.
    public static func parse(_ data: Data) throws -> AnnouncementsDocument {
        guard let raw = try? JSONDecoder().decode(Raw.self, from: data) else { throw ParseError.malformed }
        guard raw.schema == supportedSchema else { throw ParseError.unknownSchema(raw.schema) }
        let items: [Announcement] = raw.announcements.compactMap { r in
            let id = r.id.trimmingCharacters(in: .whitespaces)
            guard !id.isEmpty, !r.title.isEmpty else { return nil }
            return Announcement(
                id: id,
                severity: r.severity.flatMap(Announcement.Severity.init(rawValue:)) ?? .info,
                from: day(r.from, endOfDay: false),
                until: day(r.until, endOfDay: true),
                distributions: r.distribution.map { Set($0.compactMap(BankingDistribution.init(rawValue:))) },
                minVersion: r.minVersion,
                maxVersion: r.maxVersion,
                title: r.title,
                summary: r.summary ?? [:],
                body: r.body ?? [:],
                link: r.link.flatMap(URL.init(string:)).flatMap { $0.scheme == "https" ? $0 : nil }
            )
        }
        return AnnouncementsDocument(schema: raw.schema, sequence: raw.sequence, announcements: items)
    }

    /// Announcements that apply to this build right now (newest first by file
    /// order; the publisher puts the most important one first).
    public func applicable(now: Date, distribution: BankingDistribution, version: String) -> [Announcement] {
        announcements.filter { a in
            if let from = a.from, now < from { return false }
            if let until = a.until, now > until { return false }
            if let d = a.distributions, !d.contains(distribution) { return false }
            if let min = a.minVersion, !version.isEmpty, BankingVersion.compare(version, min) == .orderedAscending { return false }
            if let max = a.maxVersion, !version.isEmpty, BankingVersion.compare(version, max) == .orderedDescending { return false }
            return true
        }
    }
}

// MARK: - Controller

@MainActor
@Observable
public final class AnnouncementCenter {
    public static let shared = AnnouncementCenter.makeDefault()

    /// Fixed address next to the banking policy, independent of releases.
    nonisolated public static let defaultURL = URL(string: "https://raw.githubusercontent.com/GiroCrab/GiroCrab/main/policy/announcements.json")!
    /// Between two downloads at least this long (the file rarely changes).
    nonisolated public static let freshness: TimeInterval = 6 * 60 * 60

    nonisolated static let enabledKey = "announcements.enabled"
    nonisolated static let dismissedKey = "announcements.dismissed"
    nonisolated static let storedJSONKey = "announcements.json"
    nonisolated static let storedSignatureKey = "announcements.signature"
    nonisolated static let lastCheckKey = "announcements.lastCheck"

    @ObservationIgnored private let keys: [Curve25519.Signing.PublicKey]
    @ObservationIgnored private let fetcher: BankingPolicyFetcher?
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let distribution: BankingDistribution
    @ObservationIgnored private let version: String
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private let language: () -> String
    @ObservationIgnored private var inFlight: Task<Void, Never>?

    public private(set) var document: AnnouncementsDocument?
    public private(set) var dismissed: Set<String>

    /// Settings › Allgemein. On by default: it is project news, not marketing,
    /// and the banner is quiet; one switch turns the whole thing (and the
    /// network request) off.
    public var isEnabled: Bool {
        didSet { defaults.set(isEnabled, forKey: Self.enabledKey) }
    }

    public init(
        publicKeys: [String],
        fetcher: BankingPolicyFetcher?,
        defaults: UserDefaults = AppDefaults.store,
        distribution: BankingDistribution = .current,
        version: String,
        now: @escaping () -> Date = Date.init,
        language: @escaping () -> String = { AnnouncementCenter.currentLanguage }
    ) {
        self.keys = publicKeys.compactMap { Data(base64Encoded: $0) }
            .compactMap { try? Curve25519.Signing.PublicKey(rawRepresentation: $0) }
        self.fetcher = fetcher
        self.defaults = defaults
        self.distribution = distribution
        self.version = version
        self.now = now
        self.language = language
        self.isEnabled = defaults.object(forKey: Self.enabledKey) as? Bool ?? true
        self.dismissed = Set(defaults.stringArray(forKey: Self.dismissedKey) ?? [])
        loadStored()
    }

    nonisolated public static var currentLanguage: String {
        Bundle.main.preferredLocalizations.first.map { String($0.prefix(2)) } ?? "en"
    }

    static func makeDefault() -> AnnouncementCenter {
        #if DEBUG
        let env = ProcessInfo.processInfo.environment
        var keys = BankingPolicy.publicKeys
        var url = defaultURL
        if let k = env["GIROCRAB_ANNOUNCEMENTS_KEY"], !k.isEmpty { keys = [k] }
        if let u = env["GIROCRAB_ANNOUNCEMENTS_URL"].flatMap(URL.init(string:)) { url = u }
        #else
        let keys = BankingPolicy.publicKeys
        let url = defaultURL
        #endif
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        return AnnouncementCenter(
            publicKeys: keys,
            fetcher: URLSessionBankingPolicyFetcher(url: url),
            version: version
        )
    }

    // MARK: Visible content

    /// Everything that applies to this build, dismissed or not (Hilfe › Mitteilungen).
    public var all: [Announcement] {
        guard isEnabled, let document else { return [] }
        return document.applicable(now: now(), distribution: distribution, version: version)
    }

    /// The one announcement for the banner: the first not yet dismissed.
    public var banner: Announcement? {
        all.first { !dismissed.contains($0.id) }
    }

    public func dismiss(_ a: Announcement) {
        dismissed.insert(a.id)
        defaults.set(Array(dismissed).sorted(), forKey: Self.dismissedKey)
    }

    public var languageCode: String { language() }

    // MARK: Loading

    private func loadStored() {
        guard let json = defaults.data(forKey: Self.storedJSONKey),
              let sig = defaults.data(forKey: Self.storedSignatureKey),
              BankingPolicyEvaluation.verify(json: json, signature: sig, keys: keys),
              let doc = try? AnnouncementsDocument.parse(json) else { return }
        document = doc
    }

    /// Checks the feed (no-op when off, without a valid key, or when the last
    /// check is fresh). Failures stay silent: announcements are optional and
    /// must never show an error.
    public func refresh(force: Bool = false) async {
        guard isEnabled, !keys.isEmpty, let fetcher else { return }
        if !force, let last = defaults.object(forKey: Self.lastCheckKey) as? Date,
           now().timeIntervalSince(last) < Self.freshness { return }
        if let inFlight { await inFlight.value; return }
        let task = Task { @MainActor in
            defer { inFlight = nil }
            guard let (json, signature) = try? await fetcher.fetch() else { return }
            defaults.set(now(), forKey: Self.lastCheckKey)
            accept(json: json, signature: signature)
        }
        inFlight = task
        await task.value
    }

    /// Verifies and adopts a downloaded file. A lower `sequence` than the
    /// stored one is a rollback and ignored.
    func accept(json: Data, signature: Data) {
        guard BankingPolicyEvaluation.verify(json: json, signature: signature, keys: keys),
              let doc = try? AnnouncementsDocument.parse(json) else { return }
        if let current = document, doc.sequence < current.sequence { return }
        document = doc
        defaults.set(json, forKey: Self.storedJSONKey)
        defaults.set(signature, forKey: Self.storedSignatureKey)
    }
}
