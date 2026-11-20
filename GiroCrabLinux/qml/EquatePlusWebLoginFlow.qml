import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// EquatePlusUI.swift › EquatePlusWebSheet (#720): the user signs in to EquatePlus and confirms
// with EquateAccess (or the alternative offered). A script installed before the portal's first
// request (`hook`, main frame, document start, page world) keeps the session headers in the
// page's closure; only a whitelisted inventory snapshot and the library's PDFs leave the page
// (`import_equateplus_snapshot`, `pending_equateplus_documents`, `import_equateplus_document`).
// The scripts are the ones of EquatePlusUI.swift, copied by scripts/gen-equateplus-qml.py;
// scripts/test-equateplus-linux.cjs checks both are identical. Never edit the generated block by hand.
WebLoginFlow {
    id: flow
    title: connection ? qsTr("%1 – EquatePlus").arg(connection.display_name) : ""
    busy: running
    infoWarning: !running && (notice !== "" || browser.blockedHost !== "")
    info: {
        if (running) return progress !== "" ? progress : qsTr("Programme und Bestände abrufen …")
        if (notice !== "") return notice
        if (browser.blockedHost !== "")
            return qsTr("Unsichere Seite blockiert (%1) – nur HTTPS-Seiten werden geöffnet.").arg(browser.blockedHost)
        return ready ? qsTr("Angemeldet – verfügbare und gesperrte Bestände können abgerufen werden.")
                     : qsTr("Melden Sie sich bei EquatePlus an und bestätigen Sie mit EquateAccess oder der angebotenen Alternative.")
    }
    userScript: hook

    property bool ready: false
    property bool running: false
    property bool probing: false
    property string progress: ""
    property string notice: ""
    property int generation: 0
    readonly property var cancelled: ({ equatePlus: "cancelled" })
    /// EquatePlusDocumentFailures.limit: failures in a row that stop a document run.
    readonly property int failureLimit: 15

    // BEGIN generated: EquatePlusScripts (EquatePlusUI.swift)
    readonly property string hook: `(() => {
  const hosts = ['www.equateplus.com','www.emea.equateplus.com','www.na.equateplus.com'];
  const allowed = () => location.protocol === 'https:' && hosts.includes(location.hostname);
  if (!allowed() || window.__gcEquatePlus) return;
  const prefix = '/EquatePlusParticipant2/';
  const nativeFetch = window.fetch;
  let csrf = null, csrf2 = null, clientId = null, authenticated = false;
  let pendingProbe = null, nextProbe = 0;
  const trusted = raw => {
    try { const u = new URL(raw, location.href);
      return u.origin === location.origin && u.pathname.startsWith(prefix + 'services/');
    } catch { return false; }
  };
  const remember = (url, headers) => {
    if (!trusted(url)) return;
    const h = new Headers(headers || {}), u = new URL(url, location.href);
    csrf = h.get('csrfpId') || u.searchParams.get('csrfpId') || csrf;
    csrf2 = h.get('EQUATE-CSRF2-TOKEN-PARTICIPANT2') || csrf2;
    clientId = u.searchParams.get('_cId') || clientId;
  };
  const observeUser = (url, data) => {
    if (trusted(url) && new URL(url, location.href).pathname === prefix + 'services/user/get'
      && data?.participant?.userId !== undefined && data?.reportingCurrency?.code)
      authenticated = true;
  };
  window.fetch = function(input, init) {
    const url = input instanceof Request ? input.url : String(input);
    const headers = init?.headers || (input instanceof Request ? input.headers : {});
    const p = nativeFetch.apply(this, arguments);
    if (trusted(url)) p.then(async r => { if (r.ok) { remember(url, headers); observeUser(url,await r.clone().json()); } }).catch(() => {});
    return p;
  };
  const open = XMLHttpRequest.prototype.open, setHeader = XMLHttpRequest.prototype.setRequestHeader;
  const requests = new WeakMap();
  XMLHttpRequest.prototype.open = function(method, url) {
    const request = {url:String(url),headers:{}}; requests.set(this,request);
    this.addEventListener('load', () => {
      if (this.status >= 200 && this.status < 300 && trusted(request.url)) {
        remember(request.url,request.headers);
        try { observeUser(request.url, this.responseType === 'json' ? this.response : JSON.parse(this.responseText)); } catch {}
      }
    }, {once:true});
    return open.apply(this,arguments);
  };
  XMLHttpRequest.prototype.setRequestHeader = function(name,value) {
    const request = requests.get(this);
    if (request && trusted(request.url)) request.headers[name] = value;
    return setHeader.apply(this,arguments);
  };
  const seed = () => {
    const string = value => typeof value === 'string' && value ? value : null;
    csrf ||= new URL(location.href).searchParams.get('csrfpId')
      || string(document.querySelector('[name="csrfpId"]')?.value);
    csrf2 ||= string(window.equateCsrfToken2);
    // Some portal versions put initial tokens in inline bootstrap scripts.
    for (const script of document.scripts) {
      const s = script.textContent || '';
      csrf ||= s.match(/csrf(?:RegisterAjax|ModifyLinks)\\(\\s*["']csrfpId["']\\s*,\\s*["']([^"']+)["']/)?.[1];
      csrf2 ||= s.match(/["']equateCsrfToken2["']\\s*:\\s*["']([^"']+)["']/)?.[1];
    }
    csrf2 ||= document.querySelector('[name="EQUATE-CSRF2-TOKEN-PARTICIPANT2"]')?.value;
  };
  const canRequest = () => {
    seed();
    return allowed() && location.pathname.startsWith(prefix) && !document.querySelector('#loginForm') && !!csrf;
  };
  const ready = () => canRequest() && authenticated;
  const get = async (path, body) => {
    if (!canRequest()) throw new Error('EquatePlus: Bitte vollständig anmelden.');
    const url = new URL(prefix + 'services/' + path, location.origin);
    url.searchParams.set('csrfpId',csrf);
    if (clientId) url.searchParams.set('_cId',clientId);
    url.searchParams.set('_rId',crypto.randomUUID());
    const headers = {'Accept':'application/json','Content-Type':'application/json;charset=UTF-8',
      'X-Requested-With':'XMLHttpRequest','csrfpId':csrf};
    if (csrf2) headers['EQUATE-CSRF2-TOKEN-PARTICIPANT2'] = csrf2;
    const controller = new AbortController(), timeout = setTimeout(() => controller.abort(),30000);
    try {
      const response = await nativeFetch.call(window,url.href,{
        method:body === undefined ? 'GET' : 'POST',credentials:'same-origin',redirect:'error',
        headers,signal:controller.signal,...(body === undefined ? {} : {body:JSON.stringify(body)})});
      if (!response.ok) {
        if ([401,403].includes(response.status)) authenticated = false;
        const error = new Error('EquatePlus: Abruf fehlgeschlagen (HTTP ' + response.status + '). Bitte erneut anmelden.');
        error.status = response.status;
        throw error;
      }
      const data = await response.json();
      if (!data || typeof data !== 'object' || /Error$/.test(data.$type || ''))
        throw new Error('EquatePlus: Keine gültigen Bestandsdaten erhalten.');
      if (typeof data.csrfpId === 'string' && data.csrfpId) csrf = data.csrfpId;
      return data;
    } finally { clearTimeout(timeout); }
  };
  const keys = ['totals','type','quantity','value','vehicle','productType','archived','entries','name','displayValue','canTrade','marketPrice','amount','unit','code','currency',
    'VEHICLE','VEHICLE_DESCRIPTION','SECURITY','VEHICLE_NAME','QUANTITY','AVAIL_QTY','AVAILABLE_UNITS',
    'LOCKED_QTY','LOCKED_PERF_QTY','TOTAL_UNITS','NET_UNITS','UNITS','SELL_PURCHASE_PRICE','COST_BASIS','PURCHASE_PRICE'];
  const clean = value => {
    if (Array.isArray(value)) return value.map(clean);
    if (value && typeof value === 'object') return Object.fromEntries(keys.filter(k => value[k] !== undefined).map(k => [k,clean(value[k])]));
    return value;
  };
  // The portal can bootstrap its user in the HTML instead of issuing an
  // observable user/get request. Verify that session explicitly; passive
  // request observation alone would leave a successful login disabled.
  const probe = async () => {
    if (!canRequest()) return false;
    if (authenticated) return true;
    if (pendingProbe) return pendingProbe;
    if (Date.now() < nextProbe) return false;
    nextProbe = Date.now() + 5000;
    pendingProbe = (async () => {
      try {
        const user = await get('user/get');
        observeUser(prefix + 'services/user/get', user);
        return ready();
      } catch { return false; }
      finally { pendingProbe = null; }
    })();
    return pendingProbe;
  };
  // Optional PDF import (library of statements, confirmations, tax certificates). Only the
  // portal's own two download services are ever requested, and only by document id.
  const downloadPath = /^\\/services\\/(documents|statements)\\/download\\?documentId=([A-Za-z0-9_.~%-]{1,200})$/;
  const base64 = bytes => {
    let text = '';
    for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(text);
  };
  const stats = {attempts: 0};
  const download = async raw => {
    const match = typeof raw === 'string' ? raw.match(downloadPath) : null;
    if (!match) throw new Error('EquatePlus: Ungültige Dokumentadresse.');
    if (!canRequest()) throw new Error('EquatePlus: Bitte vollständig anmelden.');
    for (let attempt = 0; attempt < 8; attempt++) {
      const url = new URL(prefix + 'services/' + match[1] + '/download', location.origin);
      url.searchParams.set('documentId', decodeURIComponent(match[2]));
      url.searchParams.set('downloadType', 'attachment');
      url.searchParams.set('csrfpId', csrf);
      if (clientId) url.searchParams.set('_cId', clientId);
      url.searchParams.set('_rId', crypto.randomUUID());
      const headers = {'X-Requested-With':'XMLHttpRequest','csrfpId':csrf};
      if (csrf2) headers['EQUATE-CSRF2-TOKEN-PARTICIPANT2'] = csrf2;
      const controller = new AbortController(), timeout = setTimeout(() => controller.abort(), 60000);
      try {
        const response = await nativeFetch.call(window, url.href, {method:'GET', credentials:'same-origin', redirect:'error', headers, signal:controller.signal});
        if (!response.ok) {
          if ([401,403].includes(response.status)) authenticated = false;
          throw new Error('EquatePlus: Dokument nicht abrufbar (HTTP ' + response.status + ').');
        }
        const bytes = new Uint8Array(await response.arrayBuffer());
        if (bytes.length > 50 * 1024 * 1024) throw new Error('EquatePlus: Dokument zu groß.');
        // a PDF may have a few bytes in front of its header (the format allows 1024)
        const head = String.fromCharCode.apply(null, bytes.subarray(0, Math.min(1024, bytes.length)));
        if (head.includes('%PDF-')) { stats.attempts = attempt + 1; return base64(bytes); }
        // the portal answers with a short status while it prepares the file
        const type = (response.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
        if (!/json/.test(type))
          throw new Error('EquatePlus: Der Download ist kein PDF (' + (/^[a-z0-9.+\\/-]{1,60}$/.test(type) ? type : 'unbekannt') + ', ' + bytes.length + ' B).');
      } finally { clearTimeout(timeout); }
      await new Promise(resolve => setTimeout(resolve, 1500));
    }
    throw new Error('EquatePlus: Das Dokument wurde nicht rechtzeitig bereitgestellt.');
  };
  window.__gcEquatePlus = {
    ready, probe,
    documents: async () => {
      const data = await get('documents/library', {$type:'Object'});
      if (!Array.isArray(data.documents) || data.documents.length > 5000)
        throw new Error('EquatePlus: Keine gültige Dokumentenliste erhalten.');
      return JSON.stringify(data.documents.map(d => ({id:d.id, date:d.date, addedDate:d.addedDate, description:d.description,
        documentType:d.documentType, plan:d.plan, language:d.language?.code, url:d.url})));
    },
    pdf: download,
    lastAttempts: () => String(stats.attempts),
    collect: async () => {
      const user = await get('user/get');
      let summary;
      try { summary = await get('planSummary/get',{$type:'Object'}); }
      catch (error) {
        if (![400,405].includes(error.status)) throw error;
        summary = await get('planSummary/get');
      }
      if (!Array.isArray(summary.entries) || !summary.entries.length) summary = await get('planSummary/get');
      if (!Array.isArray(summary.entries) || !summary.entries.length || summary.entries.length > 500)
        throw new Error('EquatePlus: Keine vollständige Programmübersicht erhalten.');
      const plans = [], seen = new Set();
      for (const plan of summary.entries) {
        if (!['string','number'].includes(typeof plan.id) || !String(plan.id) || seen.has(String(plan.id)))
          throw new Error('EquatePlus: Programm ohne eindeutige Kennung.');
        seen.add(String(plan.id));
        const details = await get('planDetails/get',{$type:'EntityIdentifier',id:plan.id});
        if (!Array.isArray(details.entries)) throw new Error('EquatePlus: Unvollständige Programmdetails.');
        plans.push({id:plan.id,name:clean(plan.name),details:clean(details)});
      }
      const now = new Date();
      return JSON.stringify({version:1,date:new Date(now.getTime()-now.getTimezoneOffset()*60000).toISOString().slice(0,10),
        user:{participant:{userId:user.participant?.userId},reportingCurrency:{code:user.reportingCurrency?.code}},plans});
    }
  };
})();`
    readonly property string probe: `return (await window.__gcEquatePlus?.probe()) === true;`
    readonly property string guard: `if (location.protocol !== 'https:' || !['www.equateplus.com','www.emea.equateplus.com','www.na.equateplus.com'].includes(location.hostname)
    || !window.__gcEquatePlus?.ready()) throw new Error('EquatePlus: Bitte erneut anmelden.');
`
    readonly property string collect: `if (!window.__gcEquatePlus?.ready()) throw new Error('EquatePlus: Bitte erneut anmelden.'); return await window.__gcEquatePlus.collect();`
    // END generated

    /// EquatePlusWebSheet.task: the session is checked every second (the page script throttles
    /// its own server probe to one every 5 s and never probes on a login or 2FA form).
    Timer {
        interval: 1000
        repeat: true
        running: true
        onTriggered: {
            if (flow.probing) return
            flow.probing = true
            flow.browser.runAsync(flow.probe, {}, flow.browser.mainWorld).then(r => {
                flow.probing = false
                if (flow.ready !== (r === true)) flow.ready = r === true
            }, () => {
                flow.probing = false
                if (flow.ready) flow.ready = false
            })
        }
    }

    function close(success) {
        generation++
        if (success) AppModel.closeWebLogin()
        else AppModel.cancelWebLogin()
    }
    function cancel() { close(false) }

    /// runScript(_:arguments:): only on the portal's own hosts and with a valid session. The
    /// rejection is the page script's own message (never shown as such: it could carry URLs).
    function run(script, args, gen) {
        if (gen !== generation) return Promise.reject(cancelled)
        return browser.runAsync(script, args || {}, browser.mainWorld).then(r => {
            if (gen !== generation) throw cancelled
            if (typeof r !== "string") throw { message: "" }
            return r
        }, e => { throw gen !== generation ? cancelled : { message: e === null ? "" : String(e) } })
    }
    /// The status ("HTTP 401") of a page-script message, or "".
    function httpStatus(message) {
        const m = /HTTP [0-9]{3}/.exec(message || "")
        return m ? m[0] : ""
    }

    /// EquatePlusDocumentFailures.reason(for:): a short reason and whether the session is gone.
    function failureReason(message) {
        message = message || ""
        const status = httpStatus(message)
        if (status) return { reason: status, sessionLost: status.endsWith("401") || status.endsWith("403") }
        if (message.indexOf("anmelden") >= 0) return { reason: qsTr("nicht angemeldet"), sessionLost: true }
        if (message.indexOf("kein PDF") >= 0) {
            // "(text/html, 4568 B)" from the page script: file type and size only
            const d = /\((?:[a-z0-9.+\/-]{1,60}|unbekannt), [0-9]+ B\)/.exec(message)
            return { reason: qsTr("kein PDF") + (d ? " " + d[0] : ""), sessionLost: false }
        }
        if (message.indexOf("rechtzeitig") >= 0) return { reason: qsTr("Zeitüberschreitung"), sessionLost: false }
        if (message.indexOf("zu groß") >= 0) return { reason: qsTr("zu groß"), sessionLost: false }
        return { reason: qsTr("unlesbar"), sessionLost: false }
    }
    /// EquatePlusDocumentFailures.summary: the four most frequent reasons.
    function failureSummary(reasons) {
        return Object.keys(reasons)
            .sort((a, b) => reasons[a] !== reasons[b] ? reasons[b] - reasons[a] : (a < b ? -1 : a > b ? 1 : 0))
            .slice(0, 4).map(k => k + ": " + reasons[k]).join(", ")
    }

    /// „Programme und Bestände abrufen“
    function start() {
        running = true
        notice = ""
        progress = ""
        const gen = ++generation
        const id = connection.id
        // the app-side steps after an import hold AppModel directly: the sheet may release this
        // flow while the import runs, the stored data must still reach the views
        const app = AppModel
        const refresh = () => { try { app.core("checkpoint") } catch (e) {} app.reloadAll(); app.checkPendingAccountSelection() }
        let stage = "portal"
        run(guard + collect, {}, gen)
            .then(snapshot => {
                stage = "import"
                return AppModel.corePromise("import_equateplus_snapshot", { connection_id: id, snapshot_json: snapshot })
            })
            .then(s => {
                // the import is stored even when the window was closed meanwhile: reload anyway
                refresh()
                if (gen !== generation) return
                endRun()
                app.toast = qsTr("EquatePlus: %1 Programme aktualisiert.").arg(s.accounts_updated)
                close(true)
            }, e => {
                if (stage === "import") refresh()
                if (gen !== generation) return
                endRun()
                if (e !== cancelled) {
                    // never the raw error: it can include token-bearing URLs
                    const status = stage === "portal" ? httpStatus(e && e.message) : ""
                    notice = (stage === "import"
                        ? qsTr("EquatePlus: Die gelieferten Bestandsdaten konnten nicht verarbeitet werden. Die bisherigen Bestände bleiben erhalten.")
                        : qsTr("EquatePlus: Der Datenabruf ist fehlgeschlagen. Bitte erneut versuchen."))
                        + (status ? " (" + status + ")" : "")
                }
            })
    }

    /// „Dokumente laden …“: the PDFs of the portal's library, only those not stored yet.
    function startDocuments() {
        running = true
        notice = ""
        progress = qsTr("Dokumentenliste abrufen …")
        const gen = ++generation
        const id = connection.id
        let downloaded = 0, failed = 0, open = 0
        const reasons = {}
        run(guard + "return await window.__gcEquatePlus.documents();", {}, gen)
            .then(list => AppModel.corePromise("pending_equateplus_documents", { connection_id: id, documents_json: list }))
            .then(pending => {
                open = pending.length
                let consecutive = 0
                const next = i => {
                    if (gen !== generation) return Promise.reject(cancelled)
                    if (i >= pending.length) return null
                    progress = qsTr("PDF %1 von %2 …").arg(i + 1).arg(pending.length)
                    const metadata = pending[i]
                    let meta = null
                    try { meta = JSON.parse(metadata) } catch (e) {}
                    const step = meta && typeof meta.url === "string"
                        ? run(guard + "return await window.__gcEquatePlus.pdf(url);", { url: meta.url }, gen)
                            .then(b64 => AppModel.corePromise("import_equateplus_document", { connection_id: id, document_json: metadata, bytes_base64: b64 }))
                        : Promise.reject({ message: "" })
                    return step.then(stored => {
                        if (stored) downloaded++
                        consecutive = 0
                        return next(i + 1)
                    }, e => {
                        if (e === cancelled) throw e
                        failed++
                        // one document the portal cannot provide must not stop the others
                        const r = failureReason(e && e.message)
                        const type = meta && typeof meta.documentType === "string" && meta.documentType !== "" ? meta.documentType : "?"
                        const key = r.reason + " · " + type
                        reasons[key] = (reasons[key] || 0) + 1
                        if (r.sessionLost || ++consecutive >= failureLimit) { failed += pending.length - i - 1; return null }
                        return next(i + 1)
                    })
                }
                return next(0)
            })
            .then(() => {
                const message = open === 0 ? qsTr("EquatePlus: Keine neuen Dokumente.")
                    : qsTr("EquatePlus: %1 neue PDFs in „Belege & PostBox“.").arg(downloaded)
                AppModel.toast = message
                finish(false)
                if (failed === 0) { close(true); return }
                const summary = failureSummary(reasons)
                notice = message + " " + qsTr("%1 PDFs nicht abrufbar%2. Ein erneuter Abruf lädt nur fehlende Dokumente.")
                    .arg(failed).arg(summary === "" ? "" : " (" + summary + ")")
            }, e => {
                if (e !== cancelled) notice = qsTr("EquatePlus: Der Dokumentenabruf ist fehlgeschlagen. Bitte erneut versuchen.")
                finish(false)
            })
    }

    /// The flow's own state after a run (the app-side refresh is separate, see `start`).
    function endRun() {
        progress = ""
        running = false
    }
    function finish(holdings) {
        try { AppModel.core("checkpoint") } catch (e) {}
        endRun()
        if (!holdings) AppModel.documentsVersion++
        AppModel.reloadAll()
        if (holdings) AppModel.checkPendingAccountSelection()
    }

    ColumnLayout {
        Layout.fillWidth: true
        spacing: 8
        // where the optional PDFs go, before anyone looks for them in the accounts
        Text {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: qsTr("Dokumente werden unter „Belege & PostBox“ abgelegt (Suche: equateplus) und nicht automatisch Umsätzen oder Aktivitäten zugeordnet. Nur wenn der Programmname im Dokument zu einem Programm passt, steht es auch bei dessen Konto.")
            font.pixelSize: Theme.caption - 1; color: Theme.secondary
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Button { text: qsTr("Schließen"); onClicked: flow.close(false) }
            Item { Layout.fillWidth: true }
            Button {
                id: documentsButton
                text: qsTr("Dokumente laden …")
                enabled: flow.ready && !flow.running
                onClicked: flow.startDocuments()
                ToolTip.visible: hovered
                ToolTip.delay: 600
                ToolTip.text: qsTr("Lädt die PDFs der Bibliothek (Bestätigungen, Auszüge, Steuerbescheinigungen). Bereits gespeicherte Dokumente werden übersprungen.")
            }
            Button {
                text: qsTr("Programme und Bestände abrufen")
                highlighted: true
                enabled: flow.ready && !flow.running
                onClicked: flow.start()
            }
        }
    }
}
