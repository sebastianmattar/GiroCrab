import SwiftUI
import WebKit
import PDFKit
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
        if !isDemo, (c.connector == .amazonOrders || c.connector == .fnz || c.connector == .quirion) { return true }
        guard !isDemo, c.connector == .americanExpress || c.connector == .amazonVisa else { return false }
        return !Self.cardUsesEnableBanking(
            optIn: settingEntry(Self.enableBankingOptInKey(c.connector))?.value,
            hasApp: settingEntry("enablebanking.application_id")?.hasValue ?? false,
            hasKey: settingEntry("enablebanking.private_key_pem")?.hasValue ?? false)
    }

    static func enableBankingOptInKey(_ kind: ConnectorKind) -> String {
        kind == .amazonVisa ? "enablebanking.use.amazonvisa" : "enablebanking.use.amex"
    }

    static func cardUsesEnableBanking(optIn: String?, hasApp: Bool, hasKey: Bool) -> Bool {
        optIn?.trimmingCharacters(in: .whitespaces) == "1" && hasApp && hasKey
    }

    /// Returns true when transactions were recognised (even if all were already known).
    @discardableResult
    func importPortalCapture(_ c: BankConnection, bodies: [String], domRowsJSON: String) -> Bool {
        guard let vault else { return false }
        do {
            let s = try vault.importPortalCapture(connectionId: c.id, jsonBodies: bodies, domRowsJson: domRowsJSON)
            toast = String(localized: "\(c.displayName): \(s.transactionsImported) neue Buchungen, \(s.transactionsCategorized) kategorisiert")
                + Self.skippedArchivedNote(s)
            reloadAll()
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

    func importStatement(_ c: BankConnection, url: URL) {
        guard let vault else { return }
        let ok = url.startAccessingSecurityScopedResource()
        defer { if ok { url.stopAccessingSecurityScopedResource() } }
        do {
            let data = try Data(contentsOf: url)
            var hint = ""
            if data.starts(with: Array("%PDF".utf8)) { hint = PDFDocument(data: data)?.string ?? "" }
            let s = try vault.importStatement(connectionId: c.id, fileName: url.lastPathComponent, bytes: data, pdfTextHint: hint)
            toast = String(localized: "\(c.displayName): \(s.transactionsImported) neue Buchungen aus \(url.lastPathComponent)")
            reloadAll()
        } catch let e as CoreError {
            if case .InvalidInput = e {
                showError(String(localized: "Keine Umsätze erkannt"), String(localized: "In \(url.lastPathComponent) wurden keine Umsätze erkannt. Erwartet: CSV mit Datum, Beschreibung, Betrag oder ein Kontoauszug als PDF mit Textebene."))
            } else {
                showError(String(localized: "Kontoauszug-Import fehlgeschlagen"), describe(e))
            }
        } catch {
            showError(String(localized: "Kontoauszug-Import fehlgeschlagen"), describe(error))
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
        case .quirion: return URL(string: "https://banking.quirion.de/login/")
        case .fnz: return URL(string: "https://portal.fnz.de/")
        case .amazonOrders: return URL(string: "https://www.amazon.de/gp/css/order-history")
        default: return nil
        }
    }
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
        case .quirion: return ["banking.quirion.de"]
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

    init(_ hosts: [String]) {
        var all = Set<String>()
        for h in hosts.map({ $0.lowercased() }) where !h.isEmpty {
            all.insert(h)
            all.insert(Self.baseDomain(h))
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
        Task { @MainActor in self.target?.captured(body, url: url) }
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
      var post = function(url, body) { try {
        if (!body || body.length > 3000000) return;
        var t = body.trim().charAt(0); if (t !== '{' && t !== '[') return;
        window.webkit.messageHandlers.girocrab.postMessage({url: String(url), body: body});
      } catch (e) {} };
      var of = window.fetch;
      if (of) { window.fetch = function() {
        var a = arguments, p = of.apply(this, a);
        p.then(function(r) { try { var u = (a[0] && a[0].url) || a[0]; r.clone().text().then(function(b) { post(u, b); }); } catch (e) {} }).catch(function(){});
        return p; }; }
      var ox = XMLHttpRequest.prototype.open;
      XMLHttpRequest.prototype.open = function(m, u) {
        this.__gcu = u;
        this.addEventListener('load', function() { try { if (this.responseType === '' || this.responseType === 'text') post(this.__gcu, this.responseText); } catch (e) {} });
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
    init(url: URL, domains: PortalDomains, captureTraffic: Bool = true, userScript: String? = nil) {
        self.domains = domains
        navigationGuard = PortalNavigationGuard()
        let cfg = WKWebViewConfiguration()
        cfg.websiteDataStore = .default()
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

    func captured(_ body: String, url: String) {
        let path = Self.maskedPath(url)
        seenPaths.append(path)
        if seenPaths.count > 200 { seenPaths.removeFirst(seenPaths.count - 200) }
        let lower = url.lowercased()
        guard Self.relevantURLWords.contains(where: lower.contains),
              !Self.privateURLWords.contains(where: lower.contains) else { return }
        bodies.append(body)
        // room for a long history paged in with "mehr anzeigen" (≈20 bookings per page)
        if bodies.count > 300 { bodies.removeFirst(bodies.count - 300) }
        lastCapture = Date()
    }

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
    /// The booking list was opened after sign-in (only once, so the user may browse freely).
    @State private var openedTransactions = false
    /// Labels logged once at the end of the list (to adapt to portal changes).
    @State private var loggedLabels = false
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
        if importedCount > 0 && olderOffered {
            return String(localized: "\(importedCount) Umsätze übernommen. Für ältere Umsätze im Portal „Umsätze anzeigen“ wählen und den SMS-Code bestätigen – die Übernahme läuft danach automatisch weiter.")
        }
        if importedCount > 0 { return String(localized: "\(importedCount) Umsätze übernommen – weitere werden geladen …") }
        if recognized == 0 { return String(localized: "Angemeldet – öffnen Sie die Umsatzübersicht, damit das Portal die Buchungen lädt.") }
        return String(localized: "\(recognized) Umsätze erkannt – Übernahme startet automatisch, sobald die Seite fertig geladen ist.")
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                SymbolTile(symbol: "safari", tint: connection.connector.tint, size: 28)
                VStack(alignment: .leading, spacing: 1) {
                    Text("\(connection.displayName) – Direkt-Login").appFont(.headline)
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
            Divider()
            HStack {
                Button(importedCount > 0 ? "Fertig" : "Abbrechen", role: .cancel) { close() }
                    .keyboardShortcut(.cancelAction)
                Button { importing = true } label: { Label("CSV- oder PDF-Kontoauszug importieren …", systemImage: "doc.badge.plus") }
                // Developer tool, hidden from everyday use (--developer). Also offered on
                // the login page: the dump records `on_login_page` itself.
                if model.options.developer && recognized == 0 {
                    Button("Portal-Struktur prüfen") { Task { await dumpSchema() } }
                        .help("Speichert nur Feldnamen und Typen (keine Werte) nach /tmp/girocrab-portal-schema.json")
                }
                Spacer()
                Button("Umsätze übernehmen (\(recognized) erkannt)") { Task { await attempt() } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || recognized == 0)
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
            // Count recognised bookings live; auto-import only when >= 1 booking is
            // recognised, the user is past login/2FA and the count has been stable for 4 s.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                guard !busy else { continue }
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
                if n > importedCount, !away, !pager.more, Date().timeIntervalSince(lastChange) > 4 {
                    // First sync with older history on offer: store what is there, but keep
                    // the sheet open so the user can unlock the rest (SMS code).
                    let keepOpen = pager.older && importedCount == 0
                        && !UserDefaults.standard.bool(forKey: historyOfferedKey)
                    if await attempt(closing: !keepOpen) {
                        if !keepOpen { return }
                        importedCount = n
                    }
                }
            }
        }
        .onDisappear { browser.teardown() }
    }

    @discardableResult
    private func attempt(closing: Bool = true) async -> Bool {
        busy = true
        defer { busy = false }
        let rows = await browser.domRowsJSON()
        let ok = model.importPortalCapture(connection, bodies: browser.bodies, domRowsJSON: rows)
        if ok, closing { close() }
        return ok
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
        if importedCount > 0 { UserDefaults.standard.set(true, forKey: historyOfferedKey) }
        model.webLoginConnection = nil
        dismiss()
    }
}
