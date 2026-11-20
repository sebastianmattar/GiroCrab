import CryptoKit
import Foundation
import Observation
import Security
import GiroCrabCore

// MARK: - Security lock for bank connections ("Sicherheitssperre")
//
// The publisher can publish a signed `banking-policy.json` with a minimum
// version per distribution (Developer ID, Mac App Store, iOS) and channel.
// An older build may then no longer contact banks: no syncs, no transfers, no
// new or changed bank logins. Reading, searching, exporting stay possible.
// The file can only *restrict* existing functions; it never unlocks or adds
// anything and contains no code (App Review Guideline 2.5.2 / 2.4.5 (iv)).
//
// See docs/banking-policy.md for the file format, the release tooling and the
// emergency procedure.

/// Where this build comes from. Set via the Info.plist key `GiroCrabDistribution`.
public enum BankingDistribution: String, Sendable, CaseIterable {
    case developerID = "developer-id"
    case macAppStore = "mac-app-store"
    case iOS = "ios"

    public static var current: BankingDistribution {
        if let raw = Bundle.main.object(forInfoDictionaryKey: "GiroCrabDistribution") as? String,
           let d = BankingDistribution(rawValue: raw) {
            return d
        }
        #if os(macOS)
        return .developerID
        #else
        return .iOS
        #endif
    }
}

// MARK: - File format (schema 1)

public struct BankingPolicyDocument: Equatable, Sendable {
    public struct Rule: Equatable, Sendable {
        /// Known distributions of this rule (unknown values are ignored).
        public var distributions: Set<BankingDistribution>
        /// nil = all channels; otherwise the known channels of this rule.
        public var channels: Set<AppUpdateChannel>?
        public var minimumVersion: String
        /// nil = all connectors; otherwise connector IDs (prefix rule, see `BankingPolicy.connectorID(_:matches:)`).
        public var connectors: [String]?
        public var advisory: URL?
        public var updateAvailable: Bool
    }

    public static let supportedSchema = 1

    public var schema: Int
    public var issued: String?
    public var sequence: UInt64
    public var rules: [Rule]

    private struct Raw: Decodable {
        struct RawRule: Decodable {
            var distribution: [String]
            var channels: [String]?
            var minimumVersion: String
            var connectors: [String]?
            var advisory: String?
            var updateAvailable: Bool?
        }
        var schema: Int
        var issued: String?
        var sequence: UInt64
        var rules: [RawRule]
    }

    public enum ParseError: Error, Equatable {
        case malformed
        case unknownSchema(Int)
    }

    /// Parses the exact bytes that were signed. Unknown values in
    /// `distribution`/`channels` are ignored (a newer publisher may know more
    /// distributions); a rule with no known distribution left applies nowhere.
    public static func parse(_ data: Data) throws -> BankingPolicyDocument {
        guard let raw = try? JSONDecoder().decode(Raw.self, from: data) else { throw ParseError.malformed }
        guard raw.schema == supportedSchema else { throw ParseError.unknownSchema(raw.schema) }
        let rules = raw.rules.map { r in
            Rule(
                distributions: Set(r.distribution.compactMap(BankingDistribution.init(rawValue:))),
                channels: r.channels.map { Set($0.compactMap(AppUpdateChannel.init(rawValue:))) },
                minimumVersion: r.minimumVersion.trimmingCharacters(in: .whitespaces),
                connectors: r.connectors?.map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty },
                advisory: r.advisory.flatMap(URL.init(string:)).flatMap { $0.scheme == "https" ? $0 : nil },
                updateAvailable: r.updateAvailable ?? true
            )
        }
        return BankingPolicyDocument(schema: raw.schema, issued: raw.issued, sequence: raw.sequence, rules: rules)
    }
}

// MARK: - Result for this build

/// Why bank contact is not allowed.
public struct BankingGate: Equatable, Sendable {
    public enum Reason: Equatable, Sendable {
        /// This version is older than the policy's minimum version.
        case outdated
        /// No policy was ever verified online on this device and the check failed now.
        case checkUnavailable
    }

    public var reason: Reason
    /// True if every connector is blocked; else `connectorIDs` lists the blocked ones.
    public var all: Bool
    public var connectorIDs: [String]
    public var minimumVersion: String?
    public var advisory: URL?
    public var updateAvailable: Bool

    /// Banner headline.
    public var title: String {
        switch reason {
        case .checkUnavailable: return String(localized: "Sicherheitsprüfung nicht möglich")
        case .outdated: return String(localized: "Bankverbindungen gesperrt")
        }
    }

    /// Full sentence(s) for alerts, help texts and core errors.
    public var message: String {
        switch reason {
        case .checkUnavailable: return title + ". " + detail
        case .outdated: return detail
        }
    }

    /// Explanation below the banner headline.
    public var detail: String {
        switch reason {
        case .checkUnavailable:
            return String(localized: "GiroCrab konnte nicht prüfen, ob diese Version noch sicher mit Banken kommunizieren darf. Bitte später erneut versuchen oder in Firewall bzw. Little Snitch Verbindungen zu raw.githubusercontent.com erlauben.")
        case .outdated:
            var text: String
            if all {
                text = String(localized: "Bankverbindungen sind gesperrt, weil diese Version eine Sicherheitslücke oder einen schweren Fehler im Bankzugang enthält. Ihre Daten bleiben lesbar.")
            } else {
                let names = ListFormatter.localizedString(byJoining: connectorIDs.map(BankingGate.displayName(forConnectorID:)))
                text = String(localized: "Verbindungen über \(names) sind gesperrt, weil diese Version eine Sicherheitslücke oder einen schweren Fehler im Bankzugang enthält. Ihre Daten bleiben lesbar.")
            }
            text += " "
            text += updateAvailable
                ? String(localized: "Bitte aktualisieren Sie GiroCrab.")
                : String(localized: "Ein Update ist in Vorbereitung.")
            return text
        }
    }

    /// "fints" → "FinTS", "fints:50050201" → "FinTS (BLZ 50050201)", …
    static func displayName(forConnectorID id: String) -> String {
        let parts = id.split(separator: ":", maxSplits: 1).map(String.init)
        let base: String
        switch parts.first ?? id {
        case "fints": base = "FinTS"
        case "comdirect": base = "comdirect"
        case "paypal": base = "PayPal"
        case "traderepublic": base = "Trade Republic"
        case "enablebanking": base = "Enable Banking"
        case "bunq": base = "bunq"
        case "scalable": base = "Scalable Capital"
        case "amazon-orders": base = "Amazon"
        case "fnz": base = "FNZ Bank"
        case "ibkr": base = "Interactive Brokers"
        case "equateplus": base = "EquatePlus"
        case "quirion": base = "quirion"
        case "easybank": base = "easybank"
        case "fin4u": base = "Alte Leipziger"
        case "volkswagenbank": base = "Volkswagen Bank"
        case "uberspace": base = "Uberspace"
        case "schwaebischhall": base = "Schwäbisch Hall"
        case "ffb": base = "FFB"
        case "fordmoney": base = "Ford Money"
        case "opeldirektbank": base = "Opel Direktbank"
        case "hanseatic": base = "Hanseatic Bank"
        case "bhw": base = "BHW Bausparkasse"
        case "finpension": base = "Finpension"
        case "kontist": base = "Kontist"
        default: base = parts.first ?? id
        }
        guard parts.count == 2 else { return base }
        if base == "FinTS" { return "\(base) (BLZ \(parts[1]))" }
        return "\(base) (\(parts[1]))"
    }
}

// MARK: - Pure evaluation (unit-tested)

public enum BankingPolicyEvaluation {
    /// Ed25519 over the exact bytes; any of `keys` may have signed.
    public static func verify(json: Data, signature: Data, keys: [Curve25519.Signing.PublicKey]) -> Bool {
        keys.contains { $0.isValidSignature(signature, for: json) }
    }

    /// The block for this build, or nil. Several matching rules: the strictest
    /// wins (union of blocked connectors, "update in preparation" if any rule says so).
    public static func gate(
        for document: BankingPolicyDocument,
        distribution: BankingDistribution,
        bundleVersion: String
    ) -> BankingGate? {
        let channel = AppUpdateChannel.inferred(fromBundleVersion: bundleVersion)
        let blocking = document.rules.filter { rule in
            rule.distributions.contains(distribution)
                && (rule.channels?.contains(channel) ?? true)
                && !rule.minimumVersion.isEmpty
                && BankingVersion.compare(bundleVersion, rule.minimumVersion) == .orderedAscending
        }
        guard !blocking.isEmpty else { return nil }
        let all = blocking.contains { $0.connectors == nil || $0.connectors?.isEmpty == true }
        var ids: [String] = []
        if !all {
            for id in blocking.flatMap({ $0.connectors ?? [] }) where !ids.contains(id) { ids.append(id) }
        }
        let strictestVersion = blocking.map(\.minimumVersion).max { BankingVersion.compare($0, $1) == .orderedAscending }
        return BankingGate(
            reason: .outdated,
            all: all,
            connectorIDs: ids,
            minimumVersion: strictestVersion,
            advisory: blocking.compactMap(\.advisory).first,
            updateAvailable: blocking.allSatisfy(\.updateAvailable)
        )
    }

    /// Same rule as the core (`banking_gate::connector_id_matches`).
    public static func connectorID(_ id: String, matches key: String) -> Bool {
        let id = id.trimmingCharacters(in: .whitespaces)
        guard !id.isEmpty else { return false }
        return key == id || key.hasPrefix(id + ":")
    }
}

// MARK: - Storage and network (protocols for tests)

/// The last accepted policy plus whether a policy was ever verified online
/// on this device ("ohne Prüfung kein Bankkontakt").
public struct StoredBankingPolicy: Codable, Equatable, Sendable {
    public var json: Data
    public var signature: Data
    public var verifiedOnline: Bool
}

public protocol BankingPolicyStore: Sendable {
    func load() -> StoredBankingPolicy?
    func save(_ policy: StoredBankingPolicy) throws
}

public protocol BankingPolicyFetcher: Sendable {
    /// Returns the JSON bytes and the detached signature.
    func fetch() async throws -> (json: Data, signature: Data)
}

/// Non-synchronised keychain item: survives `brew uninstall --zap`, deleting
/// Application Support and reinstalling (anti-rollback of `sequence`).
public struct KeychainBankingPolicyStore: BankingPolicyStore {
    let service = "de.girocrab.banking-policy"
    let account = "policy"

    public init() {}

    private func baseQuery(dataProtection: Bool) -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false,
        ]
        #if os(macOS)
        if dataProtection { q[kSecUseDataProtectionKeychain as String] = true }
        #endif
        return q
    }

    public func load() -> StoredBankingPolicy? {
        for dp in [true, false] {
            var q = baseQuery(dataProtection: dp)
            q[kSecReturnData as String] = true
            q[kSecMatchLimit as String] = kSecMatchLimitOne
            var out: CFTypeRef?
            if SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data {
                return try? JSONDecoder().decode(StoredBankingPolicy.self, from: data)
            }
        }
        return nil
    }

    public func save(_ policy: StoredBankingPolicy) throws {
        let data = try JSONEncoder().encode(policy)
        // Data-protection keychain first; unsigned/ad-hoc macOS builds lack the
        // entitlement (errSecMissingEntitlement) and use the login keychain,
        // like VaultKeyManager.
        var lastStatus: OSStatus = errSecSuccess
        for dp in [true, false] {
            let q = baseQuery(dataProtection: dp)
            let attrs: [String: Any] = [
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]
            var status = SecItemUpdate(q as CFDictionary, [kSecValueData as String: data] as CFDictionary)
            if status == errSecItemNotFound {
                status = SecItemAdd(q.merging(attrs) { $1 } as CFDictionary, nil)
            }
            if status == errSecSuccess { return }
            lastStatus = status
            if status != errSecMissingEntitlement { break }
        }
        throw NSError(domain: NSOSStatusErrorDomain, code: Int(lastStatus))
    }
}

#if DEBUG
/// Debug builds with a test key only (see `makeDefault`).
final class MemoryBankingPolicyStore: BankingPolicyStore, @unchecked Sendable {
    private var value: StoredBankingPolicy?
    func load() -> StoredBankingPolicy? { value }
    func save(_ policy: StoredBankingPolicy) throws { value = policy }
}

/// Debug builds: a file instead of the keychain item; survives restarts
/// like the keychain item (see `makeDefault`).
struct FileBankingPolicyStore: BankingPolicyStore {
    let url: URL
    func load() -> StoredBankingPolicy? {
        (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode(StoredBankingPolicy.self, from: $0) }
    }
    func save(_ policy: StoredBankingPolicy) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(policy).write(to: url, options: .atomic)
    }
}
#endif

/// HTTPS download without cookies, cache or user data; at most 64 KB, 10 s.
public struct URLSessionBankingPolicyFetcher: BankingPolicyFetcher {
    public static let maxBytes = 64 * 1024
    let url: URL

    public init(url: URL) { self.url = url }

    public func fetch() async throws -> (json: Data, signature: Data) {
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieStorage = nil
        config.httpShouldSetCookies = false
        config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        config.timeoutIntervalForRequest = 10
        config.timeoutIntervalForResource = 10
        let session = URLSession(configuration: config)
        defer { session.finishTasksAndInvalidate() }
        async let json = get(url, session)
        async let sig = get(url.appendingPathExtension("sig"), session)
        return try await (json, sig)
    }

    private func get(_ url: URL, _ session: URLSession) async throws -> Data {
        var request = URLRequest(url: url)
        request.setValue(nil, forHTTPHeaderField: "User-Agent")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw URLError(.badServerResponse)
        }
        guard data.count <= Self.maxBytes else { throw URLError(.dataLengthExceedsMaximum) }
        return data
    }
}

// MARK: - Controller

@MainActor
@Observable
public final class BankingPolicy {
    /// Policy public keys (Ed25519, raw 32 bytes, base64). An array so the key
    /// can be rotated (docs/banking-policy.md). Entries that are not valid keys
    /// – like the placeholder – are ignored; without a valid key the whole
    /// feature is off: no request, no block.
    /// `scripts/banking-policy.sh keygen` / `rotate-key` edit this list.
    nonisolated public static let publicKeys: [String] = [
        "sy8puSCv1QXXl0hU5cYaPaaShIltXDIXiKLRXLuNW+A=", // banking-policy-public-key
    ]

    /// Fixed address in the repository (`policy/` on `main`), independent of
    /// releases: a release without the file can never make it disappear, and
    /// every change is a reviewable commit. The signature is what protects it.
    nonisolated public static let defaultURL = URL(string: "https://raw.githubusercontent.com/GiroCrab/GiroCrab/main/policy/banking-policy.json")!

    /// A successful check is "fresh" for this long (also the minimum interval between downloads).
    nonisolated public static let freshness: TimeInterval = 15 * 60

    /// How long a release build may talk to banks on the strength of the policy
    /// bundled at build time alone, i.e. without any successful online check
    /// on this device (firewalls such as Little Snitch that only allow the bank
    /// servers). Counted from `GiroCrabBuildDate` in Info.plist.
    nonisolated public static let bundledGracePeriod: TimeInterval = 90 * 24 * 60 * 60

    /// Info.plist key with the build time (ISO 8601, UTC), written by
    /// package-macos-app.sh. Part of the signed bundle.
    nonisolated public static let buildDateInfoKey = "GiroCrabBuildDate"

    public static let shared = BankingPolicy.makeDefault()

    // Inputs
    @ObservationIgnored let keys: [Curve25519.Signing.PublicKey]
    @ObservationIgnored let store: BankingPolicyStore
    @ObservationIgnored let fetcher: BankingPolicyFetcher?
    @ObservationIgnored let bundled: (json: Data, signature: Data)?
    @ObservationIgnored let distribution: BankingDistribution
    @ObservationIgnored let bundleVersion: String
    @ObservationIgnored let buildDate: Date?
    /// The policy bundled with this build, if its signature is valid.
    @ObservationIgnored private var bundledDocument: BankingPolicyDocument?
    @ObservationIgnored let now: () -> Date
    @ObservationIgnored private var inFlight: Task<Void, Never>?

    // State
    /// False while no valid public key is compiled in (placeholder).
    public let isEnabled: Bool
    public private(set) var document: BankingPolicyDocument?
    public private(set) var verifiedOnline = false
    public private(set) var lastSuccessfulCheck: Date?
    public private(set) var lastFailure: String?
    /// The block for this build (nil = banking allowed).
    public private(set) var gate: BankingGate?

    public init(
        publicKeys: [String],
        store: BankingPolicyStore,
        fetcher: BankingPolicyFetcher?,
        bundled: (json: Data, signature: Data)?,
        distribution: BankingDistribution,
        bundleVersion: String,
        buildDate: Date? = nil,
        now: @escaping () -> Date = Date.init
    ) {
        let keys = publicKeys.compactMap { Data(base64Encoded: $0) }
            .compactMap { try? Curve25519.Signing.PublicKey(rawRepresentation: $0) }
        self.keys = keys
        self.isEnabled = !keys.isEmpty
        self.store = store
        self.fetcher = fetcher
        self.bundled = bundled
        self.distribution = distribution
        self.bundleVersion = bundleVersion
        self.buildDate = buildDate
        self.now = now
        loadLocal()
    }

    static func makeDefault() -> BankingPolicy {
        var keys = publicKeys
        var url = defaultURL
        var version = BankingVersion.policyVersion(
            build: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String,
            marketing: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String,
            core: coreVersion()
        )
        var store: BankingPolicyStore = KeychainBankingPolicyStore()
        var buildDate = (Bundle.main.object(forInfoDictionaryKey: buildDateInfoKey) as? String)
            .flatMap { ISO8601DateFormatter().date(from: $0) }
        #if DEBUG
        // Test hooks for throwaway keys and a local server. Not compiled into
        // release builds, so nobody can redirect or disable the check there.
        let env = ProcessInfo.processInfo.environment
        // Debug builds never touch the keychain item. They are not signed like
        // the release app, so they lack the data-protection keychain and fall
        // back to the login keychain, where the item belongs to the signed app:
        // macOS then asks for the password on every launch of a new build (and
        // for every `AppModel` in unit tests). Tests keep the policy in memory,
        // other debug runs in their own file.
        store = isRunningTests(env) ? MemoryBankingPolicyStore() : FileBankingPolicyStore(url: debugStoreURL)
        if let k = env["GIROCRAB_BANKING_POLICY_KEY"], !k.isEmpty {
            keys = [k]
            // Test keys never touch the real keychain item: a file (to test
            // restarts) or memory.
            if let path = env["GIROCRAB_BANKING_POLICY_STORE"], !path.isEmpty {
                store = FileBankingPolicyStore(url: URL(fileURLWithPath: path))
            } else {
                store = MemoryBankingPolicyStore()
            }
        }
        if let u = env["GIROCRAB_BANKING_POLICY_URL"].flatMap(URL.init(string:)) { url = u }
        if let v = env["GIROCRAB_BANKING_POLICY_VERSION"], !v.isEmpty { version = v }
        if let d = env["GIROCRAB_BANKING_POLICY_BUILD_DATE"] { buildDate = ISO8601DateFormatter().date(from: d) }
        #endif
        let bundledJSON = Bundle.main.url(forResource: "banking-policy", withExtension: "json")
            .flatMap { try? Data(contentsOf: $0) }
        let bundledSig = Bundle.main.url(forResource: "banking-policy.json", withExtension: "sig")
            .flatMap { try? Data(contentsOf: $0) }
        let bundled = bundledJSON.flatMap { j in bundledSig.map { (json: j, signature: $0) } }
        return BankingPolicy(
            publicKeys: keys,
            store: store,
            fetcher: URLSessionBankingPolicyFetcher(url: url),
            bundled: bundled,
            distribution: .current,
            bundleVersion: version,
            buildDate: buildDate
        )
    }

    #if DEBUG
    /// Where debug builds keep the policy instead of the keychain item.
    static var debugStoreURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("GiroCrab", isDirectory: true)
            .appendingPathComponent("banking-policy-debug.json")
    }

    static func isRunningTests(_ env: [String: String]) -> Bool {
        NSClassFromString("XCTestCase") != nil || env["XCTestConfigurationFilePath"] != nil
    }
    #endif

    /// True when the debug overrides are in use (shown in settings).
    public static var usesDebugOverrides: Bool {
        #if DEBUG
        let env = ProcessInfo.processInfo.environment
        return env["GIROCRAB_BANKING_POLICY_KEY"] != nil || env["GIROCRAB_BANKING_POLICY_URL"] != nil
            || env["GIROCRAB_BANKING_POLICY_VERSION"] != nil || env["GIROCRAB_BANKING_POLICY_BUILD_DATE"] != nil
        #else
        return false
        #endif
    }

    /// Block for one connection key (`BankConnection.connector`), or nil.
    public func gate(for connectorKey: String) -> BankingGate? {
        guard let gate else { return nil }
        if gate.all { return gate }
        return gate.connectorIDs.contains { BankingPolicyEvaluation.connectorID($0, matches: connectorKey) } ? gate : nil
    }

    /// Any block at all (for the banner).
    public var isBlocking: Bool { gate != nil }

    /// Until when this build may contact banks without any successful online
    /// check on this device: build date + `bundledGracePeriod`, provided the
    /// bundle carries a validly signed policy (notarize.sh always adds one).
    /// nil = no such allowance (no build date, no bundled policy, feature off).
    public var bundledPolicyValidUntil: Date? {
        guard isEnabled, bundledDocument != nil, let buildDate else { return nil }
        return buildDate.addingTimeInterval(Self.bundledGracePeriod)
    }

    /// Banking runs on the bundled policy alone right now (never verified online).
    public var reliesOnBundledPolicy: Bool {
        guard !verifiedOnline, let until = bundledPolicyValidUntil else { return false }
        return now() < until
    }

    // MARK: Loading

    private func verifiedDocument(json: Data, signature: Data) -> BankingPolicyDocument? {
        guard BankingPolicyEvaluation.verify(json: json, signature: signature, keys: keys) else { return nil }
        return try? BankingPolicyDocument.parse(json)
    }

    /// Stored (keychain) and bundled policy; the higher `sequence` wins.
    private func loadLocal() {
        guard isEnabled else { apply(); return }
        var stored = store.load()
        var storedDoc = stored.flatMap { verifiedDocument(json: $0.json, signature: $0.signature) }
        if storedDoc == nil { stored = stored.map { StoredBankingPolicy(json: Data(), signature: Data(), verifiedOnline: $0.verifiedOnline) } }
        let wasOnline = stored?.verifiedOnline ?? false
        bundledDocument = bundled.flatMap { verifiedDocument(json: $0.json, signature: $0.signature) }
        if let bundledDoc = bundledDocument, let bundled,
           bundledDoc.sequence > (storedDoc?.sequence ?? 0) || storedDoc == nil {
            storedDoc = bundledDoc
            stored = StoredBankingPolicy(json: bundled.json, signature: bundled.signature, verifiedOnline: wasOnline)
            try? store.save(stored!)
        }
        document = storedDoc
        verifiedOnline = wasOnline
        apply()
    }

    // MARK: Checking

    /// Downloads and verifies the policy if the last successful check is older
    /// than `freshness` (or `force`). Concurrent callers share one download.
    public func refresh(force: Bool = false) async {
        guard isEnabled, let fetcher else { return }
        if !force, let last = lastSuccessfulCheck, now().timeIntervalSince(last) < Self.freshness { return }
        if let inFlight { await inFlight.value; return }
        let task = Task { @MainActor in
            do {
                let (json, sig) = try await fetcher.fetch()
                self.accept(json: json, signature: sig)
            } catch {
                self.lastFailure = error.localizedDescription
                self.apply()
            }
        }
        inFlight = task
        await task.value
        inFlight = nil
    }

    /// Takes a downloaded file if signature, schema and `sequence` are valid.
    /// Anything else leaves the stored state untouched (no grace period).
    func accept(json: Data, signature: Data) {
        guard BankingPolicyEvaluation.verify(json: json, signature: signature, keys: keys) else {
            lastFailure = String(localized: "Ungültige Signatur")
            apply()
            return
        }
        let doc: BankingPolicyDocument
        do { doc = try BankingPolicyDocument.parse(json) } catch {
            lastFailure = String(localized: "Unbekanntes Format")
            apply()
            return
        }
        if let current = document, doc.sequence < current.sequence {
            lastFailure = String(localized: "Veraltete Richtlinie (sequence \(doc.sequence) < \(current.sequence))")
            apply()
            return
        }
        try? store.save(StoredBankingPolicy(json: json, signature: signature, verifiedOnline: true))
        document = doc
        verifiedOnline = true
        lastSuccessfulCheck = now()
        lastFailure = nil
        apply()
    }

    /// Recomputes `gate` and hands it to the core (second line of defence).
    private func apply() {
        let newGate: BankingGate?
        if !isEnabled {
            newGate = nil
        } else if !verifiedOnline {
            // Never verified online on this device (fresh install, GitHub
            // blocked by a firewall, or the keychain item was lost): only the
            // bundled floor is known. It is trusted for `bundledGracePeriod`
            // after the build date – at release time it was current. Older
            // builds must not talk to a bank unchecked.
            let bundledGate = document.flatMap {
                BankingPolicyEvaluation.gate(for: $0, distribution: distribution, bundleVersion: bundleVersion)
            }
            let unchecked = BankingGate(
                reason: .checkUnavailable, all: true, connectorIDs: [],
                minimumVersion: nil, advisory: nil, updateAvailable: true
            )
            if let bundledGate {
                newGate = bundledGate
            } else if reliesOnBundledPolicy {
                newGate = nil
            } else if lastFailure == nil {
                // Not tried yet: no banner, but the core stays closed until the
                // first check (every bank entry point awaits `refresh()` first).
                if gate != nil { gate = nil }
                setBankingBlock(all: true, connectorIds: [], message: unchecked.message)
                return
            } else {
                newGate = unchecked
            }
        } else {
            newGate = document.flatMap {
                BankingPolicyEvaluation.gate(for: $0, distribution: distribution, bundleVersion: bundleVersion)
            }
        }
        if gate != newGate { gate = newGate }
        if let g = newGate {
            setBankingBlock(all: g.all, connectorIds: g.connectorIDs, message: g.message)
        } else {
            clearBankingBlock()
        }
    }
}
