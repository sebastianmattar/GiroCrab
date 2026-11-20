import SwiftUI
import WebKit
import CryptoKit
import UniformTypeIdentifiers
import GiroCrabCore

// MARK: - Model glue (American Express / Amazon Visa: keyless import)

extension AppModel {
    /// Amex / Amazon Visa use the embedded web portal unless the user switched
    /// that card to Enable Banking (PSD2) under "Erweitert" *and* set the
    /// credentials. The credentials alone don't count: they are shared with
    /// PSD2 banks such as C24 and must not move the cards off the web login.
    func usesPortal(_ c: BankConnection) -> Bool {
        if Self.isMagicDemoLogin(c.loginName) { return false }
        if !isDemo, [.advanzia, .amazonOrders, .fnz, .quirion, .schwaebischHall, .equatePlus].contains(c.connector) { return true }
        guard !isDemo, c.connector == .americanExpress || c.connector == .amazonVisa else { return false }
        // the card's own application (#824) or the default one
        let app = connectionEnableBankingApp(c.id)
        return !Self.cardUsesEnableBanking(
            optIn: settingEntry(Self.enableBankingOptInKey(c.connector))?.value,
            hasApp: !(app?.applicationId.isEmpty ?? true),
            hasKey: app?.hasKey ?? false)
    }

    /// Connections fed by an export file the user picks (no login, no sync).
    func usesFileImport(_ c: BankConnection) -> Bool {
        c.connector == .finpension && !isDemo && !Self.isMagicDemoLogin(c.loginName)
    }

    func importFinpension(_ c: BankConnection, url: URL) {
        guard let pin = pinVault() else { return }
        let ok = url.startAccessingSecurityScopedResource()
        defer { if ok { url.stopAccessingSecurityScopedResource() } }
        let fileName = url.lastPathComponent
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            showError(String(localized: "Finpension-Import fehlgeschlagen"), describe(error))
            return
        }
        let id = c.id
        Task {
            do {
                let s = try await pin.run { try $0.importFinpensionExport(connectionId: id, bytes: data) }
                guard isCurrent(pin) else { return }
                toast = String(localized: "\(c.displayName): \(s.transactionsImported) neue Buchungen aus \(fileName)")
                reloadAll()
            } catch where !isCurrent(pin) {
                // Locked or switched vault meanwhile: no error over another vault.
            } catch CoreError.InvalidInput {
                showError(String(localized: "Nichts erkannt"), String(localized: "In \(fileName) wurden keine Finpension-Positionen oder -Buchungen erkannt. Erwartet: CSV oder JSON-Export mit ISIN, Anzahl, Kurs, Marktwert bzw. Datum, Beschreibung, Betrag."))
            } catch {
                showError(String(localized: "Finpension-Import fehlgeschlagen"), describe(error))
            }
        }
    }

    static func enableBankingOptInKey(_ kind: ConnectorKind) -> String {
        kind == .amazonVisa ? "enablebanking.use.amazonvisa" : "enablebanking.use.amex"
    }

    static func cardUsesEnableBanking(optIn: String?, hasApp: Bool, hasKey: Bool) -> Bool {
        optIn?.trimmingCharacters(in: .whitespaces) == "1" && hasApp && hasKey
    }

    /// Returns true when transactions were recognised (even if all were already known).
    @discardableResult
    func importPortalCapture(_ c: BankConnection, bodies: [String], cardKeys: [String] = [], cards: [PortalCard] = [],
                             domRowsJSON: String) -> Bool {
        guard let vault else { return false }
        do {
            let s = try vault.importPortalCaptureCards(connectionId: c.id, jsonBodies: bodies, cardKeys: cardKeys,
                                                       cards: cards, domRowsJson: domRowsJSON)
            toast = String(localized: "\(c.displayName): \(s.transactionsImported) neue Buchungen, \(s.transactionsCategorized) kategorisiert")
                + Self.skippedArchivedNote(s)
            reloadAll()
            // several cards behind one login (#243): new ones wait for the choice
            checkPendingAccountSelection()
            return true
        } catch {
            AppModel.debugLog("portal import failed: \(describe(error))")
            return false
        }
    }

    /// Number of bookings currently recognised in the captured JSON / visible table (nothing stored).
    /// Runs off the main thread: the browser polls this every 1.5 s while the user types.
    func previewPortalCapture(_ c: BankConnection, bodies: [String], domRowsJSON: String) async -> Int {
        let id = c.id
        return Int((try? await vaultOffMain { try $0.previewPortalCapture(connectionId: id, jsonBodies: bodies, domRowsJson: domRowsJSON) }) ?? 0)
    }

    /// Pending charges the portal no longer lists as open whose posting was not found yet (#629).
    func portalOpenHolds(_ c: BankConnection, bodies: [String]) async -> [PortalHold] {
        let id = c.id
        return (try? await vaultOffMain { try $0.portalOpenHolds(connectionId: id, jsonBodies: bodies) }) ?? []
    }

    /// Deletes pending charges the user gave up on (after the warning in the portal sheet).
    func discardPortalHolds(_ c: BankConnection, ids: [Int64]) async {
        let id = c.id
        do {
            _ = try await requireVaultOffMain { try $0.discardPortalHolds(connectionId: id, ids: ids) }
            reloadAll()
        } catch {
            showError(String(localized: "Vormerkungen konnten nicht verworfen werden"), describe(error))
        }
    }

    func importStatement(_ c: BankConnection, url: URL) {
        guard let pin = pinVault() else { return }
        let ok = url.startAccessingSecurityScopedResource()
        defer { if ok { url.stopAccessingSecurityScopedResource() } }
        let fileName = url.lastPathComponent
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            showError(String(localized: "Kontoauszug-Import fehlgeschlagen"), describe(error))
            return
        }
        let id = c.id
        Task {
            do {
                let isPDF = data.starts(with: Array("%PDF".utf8))
                let hint = isPDF ? await ReceiptText.extract(data: data, fileName: fileName) ?? "" : ""
                // Parsing a large PDF statement takes seconds.
                let s = try await pin.run {
                    try $0.importStatement(connectionId: id, fileName: fileName, bytes: data, pdfTextHint: hint)
                }
                guard isCurrent(pin) else { return }
                toast = String(localized: "\(c.displayName): \(s.transactionsImported) neue Buchungen aus \(fileName)")
                reloadAll()
            } catch where !isCurrent(pin) {
                // Locked or switched vault meanwhile: no error over another vault.
            } catch CoreError.InvalidInput {
                showError(String(localized: "Keine Umsätze erkannt"), String(localized: "In \(fileName) wurden keine Umsätze erkannt. Erwartet: CSV mit Datum, Beschreibung, Betrag oder ein Kontoauszug als PDF mit Textebene."))
            } catch {
                showError(String(localized: "Kontoauszug-Import fehlgeschlagen"), describe(error))
            }
        }
    }
}

extension ConnectorKind {
    var portalURL: URL? {
        switch self {
        case .americanExpress: return URL(string: "https://global.americanexpress.com/activity/recent")
        // Amazon Visa customer portal (Open Bank / Zinia); www.openbank.de is
        // Open Bank's general site without the card login.
        case .amazonVisa: return URL(string: "https://kunden.openbankpay.com/amazon/login")
        case .equatePlus: return URL(string: "https://www.equateplus.com/EquatePlusParticipant2/?login")
        case .quirion: return URL(string: "https://banking.quirion.de/login/")
        case .schwaebischHall: return SchwaebischHallScripts.loginURL
        case .fnz: return URL(string: "https://portal.fnz.de/")
        case .amazonOrders: return URL(string: "https://www.amazon.de/gp/css/order-history")
        default: return nil
        }
    }
    /// The portal shows one billing period at a time; older ones are picked in the portal
    /// itself while the sheet stays open (#629).
    var portalHasBillingPeriods: Bool { self == .americanExpress }
    /// Page with the booking list, opened once right after sign-in when the portal
    /// lands elsewhere (Open Bank starts on its overview, which loads no bookings).
    var portalTransactionsURL: URL? {
        switch self {
        case .amazonVisa: return URL(string: "https://kunden.openbankpay.com/amazon/transactions")
        default: return nil
        }
    }
    var portalHosts: [String] {
        switch self {
        case .americanExpress: return ["americanexpress.com", "aexp.com"]
        case .amazonVisa: return ["openbankpay.com", "openbank.de", "openbank.com", "zinia.de", "zinia.com"]
        case .amazonOrders: return ["amazon.de", "amazon.com"]
        case .fnz: return ["portal.fnz.de"]
        case .equatePlus: return ["www.equateplus.com", "www.emea.equateplus.com", "www.na.equateplus.com"]
        case .quirion: return ["banking.quirion.de"]
        case .schwaebischHall: return ["www.schwaebisch-hall.de", "login.schwaebisch-hall.de"]
        default: return []
        }
    }
}

/// Removes the persisted web-portal cookies/session for this connector.
@MainActor
func clearPortalSession(_ kind: ConnectorKind) async {
    let store = WKWebsiteDataStore.default()
    let records = await store.dataRecords(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes())
    let hosts = kind.portalHosts
    await store.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), for: records.filter { r in hosts.contains { r.displayName.contains($0) } })
}

// MARK: - Embedded browser

/// Domains a portal browser may show and read: the portal's hosts and the
/// base domain of its start page, matched on label boundaries
/// (`kunden.openbankpay.com` matches `openbankpay.com`, `openbankpay.com.evil.io` does not).
struct PortalDomains: Sendable {
    let domains: [String]

    init(_ hosts: [String], includeBaseDomains: Bool = true) {
        var all = Set<String>()
        for h in hosts.map({ $0.lowercased() }) where !h.isEmpty {
            all.insert(h)
            if includeBaseDomains { all.insert(Self.baseDomain(h)) }
        }
        domains = all.sorted()
    }

    init(kind: ConnectorKind) {
        self.init(kind.portalHosts + [kind.portalURL?.host].compactMap { $0 })
    }

    /// Last two labels ("portal.fnz.de" -> "fnz.de"); enough for the portals' TLDs.
    static func baseDomain(_ host: String) -> String {
        host.split(separator: ".").suffix(2).joined(separator: ".")
    }

    func allows(_ host: String?) -> Bool {
        guard var h = host?.lowercased(), !h.isEmpty else { return false }
        if h.hasSuffix(".") { h.removeLast() }
        return domains.contains { h == $0 || h.hasSuffix("." + $0) }
    }
}

private final class WeakHandler: NSObject, WKScriptMessageHandler {
    weak var target: PortalBrowser?
    let domains: PortalDomains
    init(_ t: PortalBrowser, domains: PortalDomains) { target = t; self.domains = domains }
    func userContentController(_ c: WKUserContentController, didReceive m: WKScriptMessage) {
        // Only the portal page itself: no third-party iframe and no page the
        // user navigated to may hand "bookings" to the importer.
        guard m.frameInfo.isMainFrame,
              m.frameInfo.securityOrigin.protocol == "https",
              domains.allows(m.frameInfo.securityOrigin.host) else { return }
        guard let d = m.body as? [String: Any], let body = d["body"] as? String else { return }
        let url = d["url"] as? String ?? ""
        let card = d["card"] as? String ?? ""
        Task { @MainActor in self.target?.captured(body, url: url, card: card) }
    }
}

/// Main frame: any https page (bank SSO / OAuth logins may redirect to other
/// domains), never http, file, javascript, data or app schemes. Reading data
/// stays strict: the message handler and the DOM import accept only the
/// portal's own domains (`PortalDomains`, `PortalBrowser.onPortal`).
@MainActor
private final class PortalNavigationGuard: NSObject, WKNavigationDelegate {
    weak var browser: PortalBrowser?

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        guard navigationAction.targetFrame?.isMainFrame ?? true else { decisionHandler(.allow); return }
        let url = navigationAction.request.url
        if url?.absoluteString == "about:blank" || url?.scheme?.lowercased() == "https" {
            browser?.blockedHost = nil
            decisionHandler(.allow)
            return
        }
        let what = url?.host ?? url?.scheme ?? "?"
        browser?.blockedHost = what
        AppModel.debugLog("portal: blocked non-https main-frame navigation (\(url?.scheme ?? "?"))")
        decisionHandler(.cancel)
    }
}

/// Persistent (default) WKWebsiteDataStore: cookies survive restarts, so later
/// syncs reuse the session. A user script mirrors the portal's own JSON
/// fetch/XHR responses to the app.
@MainActor
@Observable
final class PortalBrowser {
    let webView: WKWebView
    private(set) var bodies: [String] = []
    /// Card of each body (`cardKey`, "" = unknown), parallel to `bodies`.
    private(set) var bodyCards: [String] = []
    /// Labels of the card keys seen so far (Amex card switcher, #243).
    private(set) var cards: [PortalCard] = []
    /// The card the portal is being switched to, and the card shown before the switch.
    @ObservationIgnored private var expectedCard: (name: String, number: String, previous: String?)?
    /// Card of the latest booking-list response: the card the portal shows. Side
    /// requests (balances, eligibility) may name other cards of the login too.
    private(set) var listCard: String?
    private(set) var lastCapture: Date?
    private(set) var loading = true
    private(set) var currentURL: URL?
    private var observation: NSKeyValueObservation?
    private var urlObservation: NSKeyValueObservation?
    let domains: PortalDomains
    @ObservationIgnored private let navigationGuard: PortalNavigationGuard
    /// Host (or scheme) of the last main-frame navigation refused as not https.
    fileprivate(set) var blockedHost: String?

    /// The page shown is the portal itself (https, one of its domains): only
    /// then are its tables read or its bookings imported.
    var onPortal: Bool {
        guard let u = currentURL else { return false }
        return u.scheme?.lowercased() == "https" && domains.allows(u.host)
    }

    /// Login / 2FA / consent pages: never auto-import while the user is on one of these.
    var onLoginPage: Bool {
        guard let u = currentURL else { return true }
        let s = (u.host ?? "") + u.path + "?" + (u.query ?? "")
        let hints = ["login", "logon", "signin", "sign-in", "sign_in", "authenticate", "auth/", "2fa", "mfa", "otp", "sso", "verify", "challenge",
                     "password", "passwort", "anmeld", "acceso", "tan", "freigabe", "consent"]
        return hints.contains { s.lowercased().contains($0) } || u.absoluteString == "about:blank"
    }

    static let hook = """
    (function(){
      if (window.__gcHooked) return; window.__gcHooked = true;
      var post = function(url, body, card) { try {
        if (!body || body.length > 3000000) return;
        var t = body.trim().charAt(0); if (t !== '{' && t !== '[') return;
        window.webkit.messageHandlers.girocrab.postMessage({url: String(url), body: body, card: card || ''});
      } catch (e) {} };
      // Which card a request is for (Amex: an account token header, #243). The app
      // keeps only a hash of it to group the responses per card.
      var isCard = function(k) { return /account.?(token|key)/i.test(String(k)); };
      var cardOf = function(h) { var c = ''; try {
        var f = function(k, v) { if (!c && v && isCard(k)) c = String(v); };
        if (!h || typeof h !== 'object') return '';
        if (typeof Headers !== 'undefined' && h instanceof Headers) h.forEach(function(v, k) { f(k, v); });
        else if (Array.isArray(h)) h.forEach(function(p) { f(p[0], p[1]); });
        else Object.keys(h).forEach(function(k) { f(k, h[k]); });
      } catch (e) {} return c; };
      var of = window.fetch;
      if (of) { window.fetch = function() {
        var a = arguments, p = of.apply(this, a);
        var c = cardOf(a[1] && a[1].headers) || cardOf(a[0] && a[0].headers);
        p.then(function(r) { try { var u = (a[0] && a[0].url) || a[0]; r.clone().text().then(function(b) { post(u, b, c); }); } catch (e) {} }).catch(function(){});
        return p; }; }
      var ox = XMLHttpRequest.prototype.open, oh = XMLHttpRequest.prototype.setRequestHeader;
      XMLHttpRequest.prototype.setRequestHeader = function(k, v) {
        try { if (isCard(k)) this.__gcc = String(v); } catch (e) {}
        return oh.apply(this, arguments); };
      XMLHttpRequest.prototype.open = function(m, u) {
        this.__gcu = u;
        this.addEventListener('load', function() { try { if (this.responseType === '' || this.responseType === 'text') post(this.__gcu, this.responseText, this.__gcc); } catch (e) {} });
        return ox.apply(this, arguments); };
    })();
    """

    static let domRows = """
    (function(){ var rows = [];
      document.querySelectorAll('tr,[role="row"]').forEach(function(r) { var cells = [];
        r.querySelectorAll('td,th,[role="cell"],[role="gridcell"]').forEach(function(c) {
          var t = (c.innerText || '').trim().replace(/\\s+/g, ' '); if (t) cells.push(t); });
        if (cells.length >= 2) rows.push(cells); });
      return JSON.stringify(rows); })()
    """

    /// `captureTraffic: false` leaves the page untouched (no injected fetch/XHR hook) and
    /// identifies as desktop Safari – used for Amazon, whose sign-in is picky about both.
    init(url: URL, domains: PortalDomains, captureTraffic: Bool = true, userScript: String? = nil,
         websiteDataStore: WKWebsiteDataStore? = nil) {
        self.domains = domains
        navigationGuard = PortalNavigationGuard()
        let cfg = WKWebViewConfiguration()
        cfg.websiteDataStore = websiteDataStore ?? .default()
        let ucc = WKUserContentController()
        if captureTraffic {
            ucc.addUserScript(WKUserScript(source: Self.hook, injectionTime: .atDocumentStart, forMainFrameOnly: false))
        }
        if let userScript {
            ucc.addUserScript(WKUserScript(source: userScript, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        }
        cfg.userContentController = ucc
        webView = WKWebView(frame: .zero, configuration: cfg)
        if !captureTraffic {
            webView.customUserAgent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
        }
        ucc.add(WeakHandler(self, domains: domains), name: "girocrab")
        navigationGuard.browser = self
        webView.navigationDelegate = navigationGuard
        observation = webView.observe(\.isLoading) { [weak self] w, _ in
            Task { @MainActor in self?.loading = w.isLoading }
        }
        urlObservation = webView.observe(\.url) { [weak self] w, _ in
            Task { @MainActor in self?.currentURL = w.url }
        }
        webView.load(URLRequest(url: url))
    }

    /// URL paths that may carry bookings/balances; other JSON (config, i18n, tracking)
    /// is only kept as a path for the structure dump.
    static let relevantURLWords = ["transaction", "activity", "balance", "account", "statement", "card", "umsa", "booking",
                                   "posting", "movement", "movimiento", "operation", "history", "contract", "product", "position"]

    /// Customer profile / login endpoints: never handed to the parser or the structure dump.
    static let privateURLWords = ["customer", "profile", "personal", "authentication", "authorize", "login", "kyc"]

    /// Paths (digits masked) of every JSON response seen, for "Portal-Struktur prüfen".
    private(set) var seenPaths: [String] = []

    /// Short hash of the portal's card token: groups responses per card, the token itself is not kept.
    static func cardKey(_ token: String) -> String {
        guard !token.isEmpty else { return "" }
        return SHA256.hash(data: Data(token.utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    func captured(_ body: String, url: String, card: String = "") {
        let path = Self.maskedPath(url)
        seenPaths.append(path)
        if seenPaths.count > 200 { seenPaths.removeFirst(seenPaths.count - 200) }
        let lower = url.lowercased()
        guard Self.relevantURLWords.contains(where: lower.contains),
              !Self.privateURLWords.contains(where: lower.contains) else { return }
        let key = Self.cardKey(card)
        bodies.append(body)
        bodyCards.append(key)
        // room for a long history paged in with "mehr anzeigen" (≈20 bookings per page)
        if bodies.count > 300 {
            bodies.removeFirst(bodies.count - 300)
            bodyCards.removeFirst(bodyCards.count - 300)
        }
        if !key.isEmpty, Self.isBookingList(url) {
            listCard = key
            if let e = expectedCard, key != e.previous {
                expectedCard = nil
                if !cards.contains(where: { $0.key == key }) {
                    cards.append(PortalCard(key: key, name: e.name, number: e.number))
                }
            }
        }
        lastCapture = Date()
    }

    /// Card keys seen so far.
    var cardKeys: Set<String> { Set(bodyCards.filter { !$0.isEmpty }) }

    /// The portal's booking list (not balances or summaries).
    static func isBookingList(_ url: String) -> Bool {
        let path = (URL(string: url)?.path ?? url).lowercased()
        return path.hasSuffix("/transactions")
    }

    /// Names the card the booking list shows now.
    func labelCurrentCard(name: String, number: String) {
        guard let key = listCard, !cards.contains(where: { $0.key == key }) else { return }
        cards.append(PortalCard(key: key, name: name, number: number))
    }

    /// Entries of the Amex card switcher ("American Express® Card••••12345").
    struct CardList: Decodable {
        struct Entry: Decodable { var name: String; var number: String }
        /// Last digits of the card shown now.
        var current = ""
        var cards: [Entry] = []
    }

    /// Opens the Amex card switcher, reads its entries and closes it again.
    func amexCards() async -> CardList {
        let js = Self.amexSwitcherJS + """
        if (!sw) return JSON.stringify({current: '', cards: []});
        var current = txt(sw).replace(/[^0-9]/g, '');
        sw.click(); await new Promise(function(r) { setTimeout(r, 800); });
        var seen = {}, cards = [];
        entries().forEach(function(m) { if (!seen[m[2]]) { seen[m[2]] = 1; cards.push({name: m[1].trim(), number: m[2]}); } });
        sw.click();
        return JSON.stringify({current: current, cards: cards});
        """
        guard let s = try? await webView.callAsyncJavaScript(js, contentWorld: .page) as? String,
              let list = try? JSONDecoder().decode(CardList.self, from: Data(s.utf8)) else { return CardList() }
        return list
    }

    /// Types user name and password into the visible login form (empty fields only).
    /// Runs in an isolated script world, so the page's scripts never get the values
    /// as arguments; does not submit.
    func fillLogin(user: String, password: String) async -> Bool {
        guard onPortal else { return false }
        let js = """
        var vis = function(e) { var r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
        var pw = Array.prototype.find.call(document.querySelectorAll('input[type=password]'), vis);
        if (!pw) return false;
        var texts = Array.prototype.filter.call(document.querySelectorAll('input'), function(i) {
          return vis(i) && /^(text|email|tel)?$/i.test(i.getAttribute('type') || ''); });
        var named = texts.find(function(i) {
          return /user|login|benutzer/i.test((i.id || '') + ' ' + (i.name || '') + ' ' + (i.getAttribute('autocomplete') || '')); });
        var before = texts.filter(function(i) { return pw.compareDocumentPosition(i) & Node.DOCUMENT_POSITION_PRECEDING; });
        var u = named || before[before.length - 1];
        if (!u) return false;
        var set = function(el, v) {
          if (el.value) return;
          Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
          el.dispatchEvent(new Event('input', {bubbles: true}));
          el.dispatchEvent(new Event('change', {bubbles: true}));
        };
        set(u, user); set(pw, password);
        return true;
        """
        return (try? await webView.callAsyncJavaScript(js, arguments: ["user": user, "password": password],
                                                        contentWorld: .defaultClient) as? Bool) ?? false
    }

    /// Switches the Amex portal to the card with these last digits; its responses get this label.
    @discardableResult
    func selectAmexCard(name: String, number: String) async -> Bool {
        expectedCard = (name, number, listCard)
        let js = Self.amexSwitcherJS + """
        if (!sw) return false;
        sw.click(); await new Promise(function(r) { setTimeout(r, 800); });
        var hit = entries().find(function(m) { return m[2] === number; });
        if (!hit) { sw.click(); return false; }
        hit[0].click();
        return true;
        """
        let ok = (try? await webView.callAsyncJavaScript(js, arguments: ["number": number], contentWorld: .page) as? Bool) ?? false
        if !ok { expectedCard = nil }
        return ok
    }

    /// Shared part of the switcher scripts: `sw` is the switcher button ("-12345"),
    /// `entries()` lists `[button, name, digits]` of the open menu.
    private static let amexSwitcherJS = """
    var vis = function(e) { var r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
    var txt = function(e) { return (e.innerText || '').trim().replace(/\\s+/g, ' '); };
    var buttons = function() { return Array.prototype.filter.call(document.querySelectorAll('button'), vis); };
    var sw = buttons().find(function(b) { return /^-[0-9]{4,5}$/.test(txt(b)); });
    var entries = function() { return buttons().map(function(b) {
      var m = /^(.+?)\\s*•+\\s*([0-9]{4,5})$/.exec(txt(b)); return m ? [b, m[1], m[2]] : null; }).filter(Boolean); };
    """

    /// Host + path without query, digits masked – no ids or card numbers.
    static func maskedPath(_ url: String) -> String {
        let u = URL(string: url)
        let s = u.map { ($0.host ?? "") + $0.path } ?? String(url.prefix(120))
        return String(s.map { $0.isASCII && $0.isNumber ? "9" : $0 })
    }

    /// Keys-only DOM structure: smallest elements whose text holds a date and an amount,
    /// described by tag/class and a masked text pattern (letters → a, digits → 9).
    static let domShape = """
    (function(){
      var dre = /\\b\\d{1,2}[.\\/]\\d{1,2}[.\\/]\\d{2,4}\\b|\\b\\d{4}-\\d{2}-\\d{2}\\b|\\b\\d{1,2}\\.? ?(jan|feb|mär|mar|apr|mai|may|jun|jul|aug|sep|okt|oct|nov|dez|dec)/i;
      var are = /-?\\d{1,3}([.,\\u00a0 ]\\d{3})*[.,]\\d{2}/;
      var mask = function(t){ return t.replace(/[0-9]/g,'9').replace(/[^\\s9.,:\\/€+\\-]/g,'a').replace(/a+/g,'a'); };
      var desc = function(e){ var c = (typeof e.className === 'string' ? e.className : '').trim().split(/\\s+/).slice(0,3).join('.');
        return e.tagName.toLowerCase() + (c ? '.' + c : '') + (e.getAttribute('role') ? '[role=' + e.getAttribute('role') + ']' : ''); };
      var out = [], amountOnly = 0;
      document.querySelectorAll('body *').forEach(function(e){
        var t = (e.innerText || ''); if (!t || t.length > 400) return;
        if (!are.test(t)) return;
        if (!dre.test(t)) { if (!Array.prototype.some.call(e.children, function(c){ return are.test(c.innerText||''); })) amountOnly++; return; }
        if (Array.prototype.some.call(e.children, function(c){ var x = c.innerText||''; return are.test(x) && dre.test(x); })) return;
        var p = e.parentElement, path = [];
        for (var i = 0; i < 4 && p; i++, p = p.parentElement) path.push(desc(p));
        out.push({el: desc(e), parents: path, siblings: e.parentElement ? e.parentElement.children.length : 0,
                  lines: t.split('\\n').map(function(l){ return mask(l.trim()); }).filter(function(l){ return l; }).slice(0,8)});
      });
      var shadow = 0; document.querySelectorAll('*').forEach(function(e){ if (e.shadowRoot) shadow++; });
      var frames = Array.prototype.map.call(document.querySelectorAll('iframe'), function(f){ try { return new URL(f.src, location.href).host; } catch (e) { return '?'; } });
      return JSON.stringify({path: location.pathname.replace(/[0-9]/g,'9'), date_amount_elements: out.slice(0,40), date_amount_count: out.length,
        amount_only_leaf_count: amountOnly, shadow_roots: shadow, iframes: frames,
        tables: document.querySelectorAll('table').length, aria_rows: document.querySelectorAll('[role=row]').length});
    })()
    """

    func domShapeJSON() async -> String {
        (try? await webView.evaluateJavaScript(Self.domShape) as? String) ?? "{}"
    }

    /// Paging buttons of the Amazon Visa (Open Bank) transaction list: "mehr anzeigen"
    /// loads the next page (clicked when `clickMore`), "Umsätze anzeigen" unlocks
    /// history older than ~3 months behind an SMS code (left to the user).
    static func pagerScript(clickMore: Bool) -> String {
        """
        (function(){
          var vis = function(e){ var r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
          var label = function(b){ return (b.innerText || b.getAttribute('aria-label') || '').trim().replace(/\\s+/g, ' '); };
          var buttons = Array.prototype.filter.call(document.querySelectorAll('button,a,[role=button]'),
            function(b){ var t = label(b); return vis(b) && !b.disabled && t && t.length <= 40; });
          var more = buttons.find(function(b){ return /^(mehr|weitere) (anzeigen|laden)$/i.test(label(b)); });
          var older = buttons.find(function(b){ return /umsätze anzeigen|ältere umsätze|alle umsätze/i.test(label(b)); });
          if (more && \(clickMore)) more.click();
          var labels = buttons.map(function(b){ return label(b).replace(/[0-9]/g, '9'); })
            .filter(function(t, i, a){ return a.indexOf(t) === i; }).slice(0, 25);
          return JSON.stringify({more: !!more, older: !!older, labels: labels});
        })()
        """
    }

    struct Pager: Decodable {
        var more = false, older = false
        /// Visible button labels (digits masked) – UI texts only, for the debug log.
        var labels: [String] = []
    }

    func pager(clickMore: Bool) async -> Pager {
        guard let s = try? await webView.evaluateJavaScript(Self.pagerScript(clickMore: clickMore)) as? String,
              let p = try? JSONDecoder().decode(Pager.self, from: Data(s.utf8)) else { return Pager() }
        return p
    }

    func domRowsJSON() async -> String {
        guard onPortal else { return "[]" }
        return (try? await webView.evaluateJavaScript(Self.domRows) as? String) ?? "[]"
    }

    func teardown() {
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.configuration.userContentController.removeScriptMessageHandler(forName: "girocrab")
        observation = nil
        urlObservation = nil
    }
}

#if os(macOS)
struct PortalWebView: NSViewRepresentable {
    let browser: PortalBrowser
    func makeNSView(context: Context) -> WKWebView { browser.webView }
    func updateNSView(_ nsView: WKWebView, context: Context) {}
}
#else
struct PortalWebView: UIViewRepresentable {
    let browser: PortalBrowser
    func makeUIView(context: Context) -> WKWebView { browser.webView }
    func updateUIView(_ uiView: WKWebView, context: Context) {}
}
#endif

// MARK: - Sheet

struct PortalWebSheet: View {
    @Environment(\.reportRedaction) private var redaction
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection
    @State private var browser: PortalBrowser
    @State private var busy = false
    @State private var importing = false
    @State private var recognized = 0
    @State private var lastChange = Date()
    @State private var lastSeenCount = 0
    /// Bookings stored so far while the sheet stays open for older history.
    @State private var importedCount = 0
    /// The portal offers older history ("Umsätze anzeigen" + SMS code).
    @State private var olderOffered = false
    /// The user browses older billing periods (#629): no automatic import, the blue button
    /// imports everything recognised and closes. Bookings are only stored on closing.
    @State private var browsingHistory = false
    /// Pending charges no longer open whose posting is not among the captured pages (#629).
    @State private var openHolds: [PortalHold] = []
    @State private var confirmDiscard = false
    /// Stored pending charges the portal no longer lists, found before importing: they are
    /// matched on the blue button, so the automatic import (which closes) stays off.
    @State private var holdsToCheck = 0
    /// The booking list was opened after sign-in (only once, so the user may browse freely).
    @State private var openedTransactions = false
    /// Labels logged once at the end of the list (to adapt to portal changes).
    @State private var loggedLabels = false
    /// Amex: cards of the switcher still to open, so every card's bookings are captured
    /// (#243); nil until the switcher was read.
    @State private var cardQueue: [PortalBrowser.CardList.Entry]?
    /// Amex: the card the portal is switching to right now.
    @State private var loadingCard: String?
    /// Amex: when the card round began waiting, for its time limit (#1010).
    @State private var cardsSince: Date?
    /// Amex: the stored login was filled in (or there is none): not tried again.
    @State private var loginFilled = false

    /// Amex: not every card of the switcher has been opened yet. Importing now would
    /// take only the cards loaded so far and close the sheet, so it waits (#243).
    private var cardsPending: Bool {
        connection.connector == .americanExpress && (cardQueue.map { !$0.isEmpty } ?? true || loadingCard != nil)
    }
    /// Amex: the card round still running after this long gives up, so the page is not
    /// hidden for good and the cards loaded so far can be imported (#1010).
    static let cardRoundTimeout: TimeInterval = 90
    static func cardRoundTimedOut(since: Date?, now: Date = Date()) -> Bool {
        since.map { now.timeIntervalSince($0) > cardRoundTimeout } ?? false
    }

    /// Amex: whether to switch to the next card now. Also while older billing periods are
    /// browsed: switching cards neither imports nor closes the sheet (#1010).
    static func switchesCard(isAmex: Bool, recognized: Int, away: Bool, quietFor: TimeInterval) -> Bool {
        isAmex && recognized > 0 && !away && quietFor > 4
    }
    /// The older history (SMS code) is offered once per connection; later syncs close on their own.
    private var historyOfferedKey: String { "portal.historyOffered.\(connection.id)" }

    init(connection: BankConnection) {
        self.connection = connection
        _browser = State(initialValue: PortalBrowser(url: connection.connector.portalURL ?? URL(string: "about:blank")!,
                                                     domains: PortalDomains(kind: connection.connector)))
    }

    private var info: String {
        if let blocked = browser.blockedHost {
            return String(localized: "Unsichere Seite blockiert (\(blocked)) – nur HTTPS-Seiten werden geöffnet.")
        }
        if browser.onLoginPage { return String(localized: "Melden Sie sich wie gewohnt an (Benutzername, Passwort, 2FA) und öffnen Sie die Umsatzübersicht.") }
        if !openHolds.isEmpty && importedCount > 0 {
            return String(localized: "\(importedCount) Umsätze übernommen. \(openHolds.count) vorgemerkte Umsätze sind nicht mehr offen, ihre Buchung wurde aber noch nicht gefunden. Öffnen Sie im Portal den vorherigen Abrechnungszeitraum.")
        }
        if !openHolds.isEmpty {
            return String(localized: "\(openHolds.count) vorgemerkte Umsätze sind nicht mehr offen, ihre Buchung wurde aber noch nicht gefunden. Öffnen Sie im Portal den vorherigen Abrechnungszeitraum.")
        }
        if holdsToCheck > 0 {
            return String(localized: "\(holdsToCheck) vorgemerkte Umsätze sind nicht mehr offen. „Umsätze übernehmen“ gleicht sie mit den Buchungen ab – liegt eine Buchung in einem früheren Abrechnungszeitraum, öffnen Sie diesen vorher im Portal.")
        }
        if browsingHistory {
            return String(localized: "Öffnen Sie im Portal frühere Abrechnungszeiträume. „Umsätze übernehmen“ übernimmt alle erkannten Umsätze und schließt das Fenster.")
        }
        if importedCount > 0 && olderOffered {
            return String(localized: "\(importedCount) Umsätze übernommen. Für ältere Umsätze im Portal „Umsätze anzeigen“ wählen und den SMS-Code bestätigen – die Übernahme läuft danach automatisch weiter.")
        }
        if let card = loadingCard { return String(localized: "Umsätze der Karte „\(card)“ werden geladen …") }
        if importedCount > 0 { return String(localized: "\(importedCount) Umsätze übernommen – weitere werden geladen …") }
        if recognized == 0 { return String(localized: "Angemeldet – öffnen Sie die Umsatzübersicht, damit das Portal die Buchungen lädt.") }
        return String(localized: "\(recognized) Umsätze erkannt – Übernahme startet automatisch, sobald die Seite fertig geladen ist.")
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                SymbolTile(symbol: "safari", tint: connection.connector.tint, size: 28)
                VStack(alignment: .leading, spacing: 1) {
                    Text("\(redaction.masking(connection.displayName)) – Direkt-Login").appFont(.headline)
                    Text(info).appFont(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
                Label("Erkannte Umsätze: \(recognized)", systemImage: recognized > 0 ? "checkmark.circle.fill" : "circle.dashed")
                    .appFont(.caption, weight: .medium)
                    .foregroundStyle(recognized > 0 ? Theme.inflow : Theme.secondary)
                if busy || browser.loading { ProgressView().controlSize(.small) }
            }
            .padding(14)
            Divider()
            PortalWebView(browser: browser)
                .macFrame(minWidth: 760, minHeight: 520)
                .overlay {
                    // GiroCrab switches the cards itself: the page stays hidden meanwhile,
                    // so nobody clicks into it or imports a half-loaded card (#243).
                    if cardsPending, recognized > 0, !browser.onLoginPage {
                        VStack(spacing: 12) {
                            ProgressView()
                            Text("Karten werden geladen …").appFont(.headline)
                            if let card = loadingCard {
                                Text(card).appFont(.callout).foregroundStyle(Theme.secondary)
                            }
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Theme.canvas)
                    }
                }
            Divider()
            HStack {
                Button(importedCount > 0 ? "Fertig" : "Abbrechen", role: .cancel) { requestClose() }
                    .keyboardShortcut(.cancelAction)
                if connection.connector.portalHasBillingPeriods && !browsingHistory {
                    Button { browsingHistory = true } label: {
                        Label("Ältere Abrechnungen laden", systemImage: "clock.arrow.circlepath")
                    }
                    .help("Lässt dieses Fenster offen, damit Sie im Portal frühere Abrechnungszeiträume öffnen können.")
                }
                Button { importing = true } label: { Label("CSV- oder PDF-Kontoauszug importieren …", systemImage: "doc.badge.plus") }
                // Developer tool, hidden from everyday use (--developer). Also offered on
                // the login page: the dump records `on_login_page` itself.
                if model.options.developer && recognized == 0 {
                    Button("Portal-Struktur prüfen") { Task { await dumpSchema() } }
                        .help("Speichert nur Feldnamen und Typen (keine Werte) nach /tmp/girocrab-portal-schema.json")
                }
                Spacer()
                Button("Umsätze übernehmen (\(recognized) erkannt)") { Task { await attempt(fromButton: true) } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || recognized == 0 || cardsPending)
            }
            .padding(12)
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.commaSeparatedText, .plainText, .pdf, .data]) { result in
            if case .success(let url) = result {
                model.importStatement(connection, url: url)
                close()
            }
        }
        .task {
            // Billing-period portals: the first sync of a connection offers the older periods.
            if connection.connector.portalHasBillingPeriods && !AppDefaults.store.bool(forKey: historyOfferedKey) {
                browsingHistory = true
            }
            // Count recognised bookings live; auto-import only when >= 1 booking is
            // recognised, the user is past login/2FA and the count has been stable for 4 s.
            // Never while browsing older periods: the automatic import closes the sheet.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                guard !busy else { continue }
                if connection.connector == .americanExpress, !loginFilled, browser.onLoginPage, browser.onPortal,
                   !browser.loading {
                    loginFilled = await fillStoredLogin()
                }
                let away = browser.onLoginPage || !browser.onPortal
                let rows = away ? "[]" : await browser.domRowsJSON()
                let n = away ? 0 : await model.previewPortalCapture(connection, bodies: browser.bodies, domRowsJSON: rows)
                if n != lastSeenCount || n != recognized { lastSeenCount = n; lastChange = Date() }
                recognized = n
                if !openedTransactions, !browser.onLoginPage, !browser.loading,
                   let list = connection.connector.portalTransactionsURL, let here = browser.currentURL,
                   here.host == list.host {
                    openedTransactions = true
                    if here.path != list.path { browser.webView.load(URLRequest(url: list)) }
                }
                // Amazon Visa lists ~20 bookings per page: keep paging until the end.
                var pager = PortalBrowser.Pager()
                if connection.connector == .amazonVisa, !away {
                    pager = await browser.pager(clickMore: !browser.loading)
                    olderOffered = pager.older
                    if pager.more { lastChange = Date() }
                    if !pager.more, n > 0, !loggedLabels {
                        loggedLabels = true
                        AppModel.debugLog("portal pager: older=\(pager.older) buttons=\(pager.labels.joined(separator: " | "))")
                    }
                }
                // Amex: open every card of the switcher once before importing (#243).
                if cardsPending, n > 0, !away {
                    if cardsSince == nil { cardsSince = Date() }
                    if Self.cardRoundTimedOut(since: cardsSince) {
                        AppModel.debugLog("portal cards: gave up, \(cardQueue?.count ?? -1) left")
                        cardQueue = []
                        loadingCard = nil
                        model.toast = String(localized: "Nicht alle Karten ließen sich öffnen – übernommen werden die bisher geladenen Karten.")
                    }
                }
                if Self.switchesCard(isAmex: connection.connector == .americanExpress, recognized: n, away: away,
                                     quietFor: Date().timeIntervalSince(lastChange)) {
                    if await openNextCard() { lastChange = Date(); continue }
                }
                if !browsingHistory, n > importedCount, !away, !pager.more, Date().timeIntervalSince(lastChange) > 4 {
                    // The automatic import closes the sheet, so it never runs while stored pending
                    // charges are no longer listed: they are matched (or discarded) via the button.
                    let unlisted = await model.portalOpenHolds(connection, bodies: browser.bodies)
                    if !unlisted.isEmpty {
                        holdsToCheck = unlisted.count
                        browsingHistory = true
                        continue
                    }
                    // First sync with older history on offer: store what is there, but keep
                    // the sheet open so the user can unlock the rest (SMS code).
                    let firstSync = importedCount == 0 && !AppDefaults.store.bool(forKey: historyOfferedKey)
                    let keepOpen = pager.older && firstSync
                    if await attempt(closing: !keepOpen) {
                        if !keepOpen && openHolds.isEmpty { return }
                        importedCount = n
                    }
                }
            }
        }
        .interactiveDismissDisabled(!openHolds.isEmpty)
        .alert("Vorgemerkte Umsätze nicht gefunden", isPresented: $confirmDiscard) {
            Button("Weiter suchen", role: .cancel) {}
            // Holds stay pending and are checked again on the next sync.
            Button("Vorerst behalten") { close() }
            Button("Verwerfen und schließen", role: .destructive) {
                Task {
                    await model.discardPortalHolds(connection, ids: openHolds.map(\.id))
                    close()
                }
            }
        } message: {
            Text(discardMessage)
        }
        .onDisappear { browser.teardown() }
    }

    /// Imports what is recognised and closes. Unresolved pending charges (#629) keep the
    /// sheet open in history mode: from the button with the discard warning right away,
    /// from the automatic import with the hint only.
    @discardableResult
    private func attempt(closing: Bool = true, fromButton: Bool = false) async -> Bool {
        busy = true
        defer { busy = false }
        let rows = await browser.domRowsJSON()
        let ok = model.importPortalCapture(connection, bodies: browser.bodies, cardKeys: browser.bodyCards,
                                           cards: browser.cards, domRowsJSON: rows)
        guard ok else { return false }
        importedCount = max(importedCount, recognized)
        holdsToCheck = 0
        openHolds = await model.portalOpenHolds(connection, bodies: browser.bodies)
        if !openHolds.isEmpty {
            // its posting may be in an older billing period
            browsingHistory = true
            if fromButton { confirmDiscard = true }
            return true
        }
        if closing { close() }
        return true
    }

    /// Closing with unresolved pending charges asks first: they would be discarded.
    private func requestClose() {
        if openHolds.isEmpty { close() } else { confirmDiscard = true }
    }

    /// Alert text (alerts are never part of a bug report picture).
    private var discardMessage: String {
        let lines = openHolds.map { h in
            "\(DateText.medium(h.bookingDate)) · \(h.counterparty) · \(Money.format(h.amountCents))"
        }
        return String(localized: "Zu diesen Vormerkungen wurde weder ein offener noch ein gebuchter Umsatz gefunden:")
            + "\n\n" + lines.joined(separator: "\n") + "\n\n"
            + String(localized: "„Vorerst behalten“ lässt sie als Vormerkung stehen; beim nächsten Abruf wird erneut gesucht.")
    }

    /// Fills the stored user name and password into the portal's login form. True once
    /// done, or when nothing is stored; false while the form is not there yet.
    private func fillStoredLogin() async -> Bool {
        let id = connection.id
        let stored = try? await model.vaultOffMain {
            (try $0.getSetting(key: AppModel.portalLoginKey(id)) ?? "", try $0.getSetting(key: "pin.conn.\(id)") ?? "")
        }
        guard let (user, password) = stored, !user.isEmpty, !password.isEmpty else { return true }
        let filled = await browser.fillLogin(user: user, password: password)
        if filled { AppModel.debugLog("portal: stored login filled in") }
        return filled
    }

    /// Amex: switches the portal to the next card not seen yet. Returns true while
    /// switching (the caller waits for its bookings), false once every card was opened.
    private func openNextCard() async -> Bool {
        if cardQueue == nil {
            let list = await browser.amexCards()
            var queue = list.cards
            if let current = list.cards.first(where: { $0.number == list.current }) {
                browser.labelCurrentCard(name: current.name, number: current.number)
                queue.removeAll { $0.number == current.number }
            }
            // A single card: nothing to switch to. Without a card key on the booking list
            // the cards could not be told apart, and switching would mix their bookings.
            cardQueue = list.cards.count > 1 && browser.listCard != nil ? queue : []
            AppModel.debugLog("portal cards: \(list.cards.count)")
        }
        guard var queue = cardQueue, !queue.isEmpty else {
            loadingCard = nil
            return false
        }
        let next = queue.removeFirst()
        cardQueue = queue
        loadingCard = next.name
        await browser.selectAmexCard(name: next.name, number: next.number)
        return true
    }

    /// Keys-only structure dump (all strings/numbers masked) for debugging portal changes.
    private func dumpSchema() async {
        let rows = await browser.domRowsJSON()
        let core = portalSchemaDump(jsonBodies: browser.bodies, domRowsJson: rows)
        let shape = await browser.domShapeJSON()
        let paths = (try? JSONSerialization.data(withJSONObject: browser.seenPaths)).flatMap { String(data: $0, encoding: .utf8) } ?? "[]"
        let dump = "{\n\"core\": \(core),\n\"json_paths\": \(paths),\n\"dom_shape\": \(shape),\n\"on_login_page\": \(browser.onLoginPage)\n}"
        do {
            try dump.write(toFile: "/tmp/girocrab-portal-schema.json", atomically: true, encoding: .utf8)
            model.toast = String(localized: "Struktur gespeichert: /tmp/girocrab-portal-schema.json (\(browser.bodies.count) Antworten, ohne Werte)")
        } catch {
            model.errorMessage = describe(error)
        }
    }

    private func close() {
        if importedCount > 0 { AppDefaults.store.set(true, forKey: historyOfferedKey) }
        model.webLoginConnection = nil
        dismiss()
    }
}
