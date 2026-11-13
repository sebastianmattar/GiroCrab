import SwiftUI
import WebKit
import GiroCrabCore

/// FNZ's own authenticated portal calls.
enum FnzScripts {
    static let probe = #"""
    (() => {
      if (location.hostname !== 'portal.fnz.de' || !/\/EOY\/EL\//.test(location.pathname)
        || !/#\/eo\//.test(location.hash)) return false;
      const text = document.body?.innerText || '';
      // The compact layout hides "Abmelden" inside the hamburger menu.
      // Account/depot detail links also prove the overview has finished loading.
      const signedIn = /Logout|Abmelden/i.test(text)
        || !!document.querySelector('a[href*="/services/logout"]')
        || !!document.querySelector('a[href*="#/eo/bestand/"]');
      return signedIn && /Gesamtbestand|Postkorb|Depot/i.test(text);
    })()
    """#

    static let overview = #"""
    const r = await fetch(location.origin + location.pathname + 'eox/rest/depot/getDepotOverviewDataForEndCustomer', {
      credentials: 'include', headers: {'Accept':'application/json', 'X-EOX-Session':'',
        'X-EOX-CLIENT-VERSION':'26.4.01', 'X-Context-ID':crypto.randomUUID()}
    });
    if (!r.ok) throw new Error('FNZ: Kontoübersicht nicht abrufbar (HTTP ' + r.status + '). Bitte erneut anmelden.');
    const data = await r.json();
    if (!Array.isArray(data.ownerGroups)) throw new Error('FNZ: Keine Kontoübersicht erkannt. Bitte erneut anmelden.');
    return JSON.stringify(data);
    """#

    static let documents = #"""
    const base = location.origin + location.pathname;
    const dateTo = new Date().toISOString().slice(0, 19);
    const all = [];
    for (const archived of [false, true]) {
      let from = 0;
      const pageIds = new Set();
      for (;;) {
        const body = {postboxCategoryAndStateFilter:{dateFrom:'2000-01-01T00:00:00', dateTo,
          category:null, onlyUnread:false, onlyFavoured:false, archived}, postboxDepotFilter:null,
          indexFrom:String(from), indexTo:String(from + 100)};
        const r = await fetch(base + 'eox/rest/postbox/getCustomerPostboxForCustomer', {
          method:'POST', credentials:'include', headers:{'content-type':'application/json',
            'X-EOX-Session':'', 'X-EOX-CLIENT-VERSION':'26.4.01', 'X-Context-ID':crypto.randomUUID()},
          body:JSON.stringify(body)});
        if (!r.ok) throw new Error('FNZ: Postkorb nicht abrufbar (HTTP ' + r.status + ').');
        const j = await r.json();
        if (!Array.isArray(j.documents)) throw new Error('FNZ: Ungültige Dokumentenliste. Bitte erneut anmelden.');
        const docs = j.documents;
        if (docs.some(d => !d || typeof d.id !== 'string' || !d.id)) throw new Error('FNZ: Dokument ohne ID.');
        if (docs.length && docs.every(d => pageIds.has(d.id))) throw new Error('FNZ: Wiederholte Postkorbseite.');
        for (const d of docs) pageIds.add(d.id);
        all.push(...docs);
        from += 100;
        if (docs.length < 100 || (Number.isInteger(j.totalMessages) && j.totalMessages >= 0 && from >= j.totalMessages)) break;
        if (from >= 100000) throw new Error('FNZ: Zu viele Postkorbseiten.');
      }
    }
    const seen = new Set();
    return JSON.stringify(all.filter(d => !seen.has(d.id) && seen.add(d.id)));
    """#

    /// `id` is passed as an argument, never interpolated into JavaScript.
    static let pdf = #"""
    const prefix = location.pathname.split('EOY')[0];
    const r = await fetch(location.origin + prefix + 'p/postkorb.aspx?docId=' + encodeURIComponent(id)
      + '&download=true', {credentials:'include'});
    const ct = (r.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
    if (!r.ok || ct !== 'application/pdf') throw new Error('FNZ: PDF nicht abrufbar. Bitte Sitzung prüfen.');
    const buf = new Uint8Array(await r.arrayBuffer());
    if (String.fromCharCode(...buf.subarray(0,5)) !== '%PDF-') throw new Error('FNZ: Ungültiger PDF-Download.');
    let bin = '';
    for (let i=0; i<buf.length; i+=0x8000) bin += String.fromCharCode.apply(null, buf.subarray(i,i+0x8000));
    return btoa(bin);
    """#
}

struct FnzWebSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection
    @State private var browser = PortalBrowser(url: URL(string: "https://portal.fnz.de/")!,
                                                domains: PortalDomains(kind: .fnz), captureTraffic: false)
    @State private var uiDelegate = AmazonUIDelegate()
    @State private var ready = false
    @State private var running = false
    @State private var includeDocuments = true
    @State private var progress = ""
    @State private var notice: String?
    @State private var task: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("\(connection.displayName) – FNZ Bank").font(.headline)
                    if running { Text(progress).font(.caption) }
                    else if let notice { Text(notice).font(.caption).foregroundStyle(.orange) }
                    else if let blocked = browser.blockedHost {
                        Text("Unsichere Seite blockiert (\(blocked)) – nur HTTPS-Seiten werden geöffnet.").font(.caption).foregroundStyle(.orange)
                    }
                    else if ready { Text("Angemeldet – Kontosalden, Depotwerte und Postkorb können abgerufen werden.").font(.caption) }
                    else { Text("Melden Sie sich bei FNZ an und bestätigen Sie in FNZsecure.").font(.caption) }
                }
                Spacer()
                if running || browser.loading { ProgressView().controlSize(.small) }
            }.padding(14)
            Divider()
            PortalWebView(browser: browser).macFrame(minWidth: 760, minHeight: 620)
            Divider()
            HStack {
                Button("Schließen", role: .cancel) { task?.cancel(); close() }
                    .keyboardShortcut(.cancelAction)
                Toggle("PDF-Postkorb inkl. Archiv", isOn: $includeDocuments).disabled(running)
                Spacer()
                Button("Salden und Dokumente abrufen") { start() }
                    .buttonStyle(.borderedProminent).disabled(!ready || running)
            }.padding(12)
        }
        .webPageDialogs(uiDelegate)
        .task {
            browser.webView.uiDelegate = uiDelegate
            while !Task.isCancelled {
                let ok = (try? await browser.webView.evaluateJavaScript(FnzScripts.probe)) as? Bool ?? false
                if ready != ok { ready = ok }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
        .onDisappear { task?.cancel(); browser.teardown() }
        .interactiveDismissDisabled(running)
    }

    private func close() {
        model.webLoginConnection = nil
        dismiss()
    }

    private func run(_ script: String, arguments: [String: Any] = [:]) async throws -> String {
        try Task.checkCancellation()
        let guardScript = #"""
        if (location.hostname !== 'portal.fnz.de' || !/\/EOY\/EL\//.test(location.pathname))
          throw new Error('FNZ: Bitte erneut anmelden.');
        """#
        let result = try await browser.webView.callAsyncJavaScript(guardScript + "\n" + script,
            arguments: arguments, in: nil, contentWorld: .page)
        try Task.checkCancellation()
        guard let result = result as? String else { throw FnzError.unreadable }
        return result
    }

    private func start() {
        running = true
        notice = nil
        task = Task {
            // Pinned: a lock or demo switch mid-run must not send later steps to another vault.
            guard let pin = model.pinVault() else { running = false; return }
            let connectionId = connection.id
            do {
                progress = String(localized: "Kontosalden und Depotwerte abrufen …")
                let overview = try await run(FnzScripts.overview)
                let summary = try await pin.run { try $0.importFnzOverview(connectionId: connectionId, overviewJson: overview) }
                var downloaded = 0
                var failed = 0
                if includeDocuments {
                    progress = String(localized: "Postkorb und Archiv auflisten …")
                    let list = try await run(FnzScripts.documents)
                    let pending = try await pin.run { try $0.pendingFnzDocuments(connectionId: connectionId, documentsJson: list) }
                    var consecutiveFailures = 0
                    for (i, metadata) in pending.enumerated() {
                        try Task.checkCancellation()
                        progress = String(localized: "PDF \(i + 1) von \(pending.count) …")
                        do {
                            guard let data = metadata.data(using: .utf8),
                                  let doc = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                                  let id = doc["id"] as? String else { throw FnzError.unreadable }
                            let b64 = try await run(FnzScripts.pdf, arguments: ["id": id])
                            guard let bytes = Data(base64Encoded: b64) else { throw FnzError.unreadable }
                            if try await pin.run({ try $0.importFnzDocument(connectionId: connectionId, documentJson: metadata, bytes: bytes) }) {
                                downloaded += 1
                            }
                            consecutiveFailures = 0
                        } catch is CancellationError { throw CancellationError() }
                        catch {
                            failed += 1
                            consecutiveFailures += 1
                            // A lost session must not cause hundreds of failed requests.
                            if consecutiveFailures >= 3 {
                                failed += pending.count - i - 1
                                break
                            }
                        }
                    }
                }
                let message = String(localized: "FNZ: \(summary.accountsUpdated) Konten/Depots aktualisiert, \(downloaded) neue PDFs.")
                model.toast = message
                if failed == 0 { close() }
                else { notice = message + " " + String(localized: "\(failed) PDFs nicht abrufbar. Erfolgreiche Downloads bleiben gespeichert; ein erneuter Abruf lädt nur fehlende Dokumente.") }
            } catch is CancellationError {
            } catch {
                notice = describe(error)
            }
            // PDFs are committed individually; flush them on success, cancellation and
            // partial failure before a vault file copy – in the vault they were written to.
            await pin.checkpoint()
            running = false
            model.documentsVersion += 1
            model.reloadAll()
        }
    }
}

private enum FnzError: LocalizedError {
    case unreadable
    var errorDescription: String? { String(localized: "FNZ: Die Portalantwort konnte nicht gelesen werden.") }
}
