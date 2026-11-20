import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtWebEngine

// RootView.swift › .sheet(item: $model.webLoginConnection): WebLoginSheet (Trade Republic,
// ConnectionsUI.swift), AmazonWebSheet, FnzWebSheet, QuirionWebSheet, SchwaebischHallWebSheet, EquatePlusWebSheet or PortalWebSheet
// (Amex / Amazon Visa). The browser part is PortalBrowser (PortalUI.swift); the flow of
// each login lives in its *WebLoginFlow.qml (texts, buttons, what is read from the page).
// Main.qml loads this file on demand, so the app starts without QtWebEngine.
//
// Browser data: Swift uses WKWebsiteDataStore.default(), whose cookies survive restarts.
// Here the profile is off the record (nothing on disk); the portal's cookies are kept
// encrypted in the vault per login instead (src/web_login.rs): saved after a successful
// import and whenever the sheet closes, put back before the first page loads. The Trade
// Republic session itself is the connection's session (`set_connection_session`).
Item {
    id: host
    // follow the cookie store from the start: "Sitzung löschen" must see every cookie
    Component.onCompleted: Qt.callLater(() => { try { AppModel.core("web_attach") } catch (e) {} })

    WebEngineProfile {
        id: webProfile
        // cpp/webcookies.cpp finds the cookie store by this name
        objectName: "girocrabWebProfile"
        offTheRecord: true
        // Plain Chrome user agent: picky sign-ins (Amazon) refuse the "QtWebEngine/…" token
        // (Swift sets a Safari user agent for Amazon for the same reason).
        Component.onCompleted: httpUserAgent = httpUserAgent.replace(/ QtWebEngine\/\S+/, "")
    }

    Sheet {
        id: sheet
        // the host item has no size: centre over the window like every other sheet
        parent: Overlay.overlay
        readonly property var connection: AppModel.webLoginConnection
        readonly property string kind: connection ? connection.connector : ""
        readonly property var flow: flowLoader.status === Loader.Ready ? flowLoader.item : null
        visible: connection !== null
        sheetWidth: 900
        padding: 0
        closePolicy: Popup.NoAutoClose
        readonly property real webHeight: Math.max(260, Math.min(620, maxBodyHeight - 150))

        // MARK: browser state (PortalBrowser)
        property var view: null
        property string currentURL: ""
        property bool loading: false
        /// Host (or scheme) of the last main-frame navigation refused as not https.
        property string blockedHost: ""
        /// Trade Republic: last automatic session hand-over per login (ms), see TrWebLoginFlow.
        property var lastAutoApply: ({})
        readonly property int mainWorld: WebEngineScript.MainWorld
        readonly property int appWorld: WebEngineScript.ApplicationWorld

        function urlParts(u) { try { return new URL(u) } catch (e) { return null } }
        /// The page shown is the portal itself (https, one of its domains).
        readonly property bool onPortal: {
            const u = urlParts(currentURL)
            return !!u && u.protocol === "https:" && !!flow && AppModel.domainAllows(flow.domains, u.hostname)
        }
        /// Login / 2FA / consent pages: never auto-import while the user is on one of these.
        readonly property bool onLoginPage: {
            const u = urlParts(currentURL)
            if (!u || currentURL === "about:blank") return true
            const s = (u.hostname + u.pathname + "?" + u.search.replace(/^\?/, "")).toLowerCase()
            const hints = ["login", "logon", "signin", "sign-in", "sign_in", "authenticate", "auth/", "2fa", "mfa", "otp", "sso", "verify", "challenge",
                           "password", "passwort", "anmeld", "acceso", "tan", "freigabe", "consent"]
            return hints.some(h => s.indexOf(h) >= 0)
        }

        function load(url) { if (view) view.url = url }
        function evaluate(script, world, done) {
            if (!view) { if (done) done(undefined); return }
            view.runJavaScript(script, world, r => { if (done) done(r) })
        }

        // `callAsyncJavaScript`: the body runs as an async function with `args` as its
        // parameters (values passed as JSON literals, never spliced into code). The result
        // is polled; a reload in between rejects with null.
        property var jobs: ({})
        property int jobSeq: 0
        function runAsync(body, args, world) {
            return new Promise((resolve, reject) => {
                if (!view) { reject(null); return }
                const id = ++jobSeq
                const names = Object.keys(args || {})
                const src = "(function(){var J=window.__gcJobs||(window.__gcJobs={});J[" + id + "]={done:false};"
                    + "(async function(" + names.join(",") + "){\n" + body + "\n})(" + names.map(n => JSON.stringify(args[n])).join(",") + ")"
                    + ".then(function(r){J[" + id + "]={done:true,ok:r};},function(e){J[" + id + "]={done:true,err:String(e&&e.message||e)};});return true;})()"
                jobs[id] = { world: world, resolve: resolve, reject: reject, polling: false }
                view.runJavaScript(src, world, started => {
                    if (started !== true && jobs[id]) { delete jobs[id]; reject(null) }
                })
                jobTimer.start()
            })
        }
        function pollJobs() {
            const ids = Object.keys(jobs)
            if (!ids.length || !view) { jobTimer.stop(); return }
            for (const id of ids) {
                const j = jobs[id]
                if (j.polling) continue
                j.polling = true
                view.runJavaScript("(function(){var J=window.__gcJobs,j=J&&J[" + id + "];if(!j)return null;if(!j.done)return 0;delete J[" + id + "];return j;})()", j.world, r => {
                    j.polling = false
                    if (r === 0 || !jobs[id]) return
                    delete jobs[id]
                    if (r === null || r === undefined) j.reject(null)
                    else if (r.err !== undefined) j.reject(r.err)
                    else j.resolve(r.ok)
                })
            }
        }
        function rejectJobs() {
            const all = jobs
            jobs = ({})
            for (const id in all) all[id].reject(null)
        }
        Timer { id: jobTimer; interval: 200; repeat: true; onTriggered: sheet.pollJobs() }

        /// PortalBrowser.hook: mirrors the portal's own JSON fetch/XHR responses; they are
        /// collected in the page and drained into `flow.captured` (main frame of the portal only).
        readonly property string captureHook: `(function(){
  if (window.__gcHooked) return; window.__gcHooked = true;
  var post = function(url, body) { try {
    if (!body || body.length > 3000000) return;
    var t = body.trim().charAt(0); if (t !== '{' && t !== '[') return;
    var q = window.__gcBodies || (window.__gcBodies = []);
    q.push({url: String(url), body: body}); if (q.length > 400) q.splice(0, q.length - 400);
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
})();`
        function drainCaptures(done) {
            evaluate("(function(){var q=window.__gcBodies||[];window.__gcBodies=[];return JSON.stringify(q);})()", mainWorld, r => {
                if (onPortal && flow) {
                    let list = []
                    try { list = JSON.parse(r || "[]") } catch (e) {}
                    for (const c of list) if (c && typeof c.body === "string") flow.captured(c.body, String(c.url || ""))
                }
                if (done) done()
            })
        }

        function addScript(v, name, source, world) {
            const s = WebEngine.script()
            s.name = name
            s.sourceCode = source
            s.injectionPoint = WebEngineScript.DocumentCreation
            s.worldId = world
            s.runsOnSubFrames = false
            v.userScripts.insert(s)
        }
        function startView(v) {
            view = v
            if (flow.captureTraffic) addScript(v, "girocrab-capture", captureHook, WebEngineScript.MainWorld)
            if (flow.userScript) addScript(v, "girocrab-portal", flow.userScript, WebEngineScript.MainWorld)
            v.url = flow.startURL
        }

        // A new connection (or none): fresh flow and fresh page; the profile (cookies) stays.
        onConnectionChanged: {
            // the login that was shown: keep its session in the vault
            if (lastConnection) AppModel.saveWebCookies(lastConnection)
            lastConnection = connection
            cookiesRestored = false
            rejectJobs()
            flowLoader.source = ""
            view = null
            currentURL = ""; loading = false; blockedHost = ""
            if (connection) Qt.callLater(startFlow)
        }
        property var lastConnection: null
        /// The stored cookies are back in the profile: only then the first page loads.
        property bool cookiesRestored: false
        Timer { id: restoreSettle; interval: 300; onTriggered: sheet.cookiesRestored = true }
        function startFlow() {
            if (!connection) return
            AppModel.restoreWebCookies(connection)
            restoreSettle.restart()
            const files = { TradeRepublic: "TrWebLoginFlow.qml", AmazonOrders: "AmazonWebLoginFlow.qml", Fnz: "FnzWebLoginFlow.qml", Quirion: "QuirionWebLoginFlow.qml",
                            SchwaebischHall: "SchwaebischHallWebLoginFlow.qml", EquatePlus: "EquatePlusWebLoginFlow.qml" }
            flowLoader.setSource(files[kind] || "PortalWebLoginFlow.qml", { browser: sheet, connection: connection })
        }
        Component.onCompleted: if (connection) { lastConnection = connection; Qt.callLater(startFlow) }

        Shortcut { sequence: "Esc"; enabled: sheet.visible && sheet.flow !== null; onActivated: sheet.flow.cancel() }

        ColumnLayout {
            width: sheet.availableWidth
            spacing: 0
            RowLayout {
                Layout.fillWidth: true
                Layout.margins: 14
                spacing: 10
                SymbolTile { symbol: sheet.flow ? sheet.flow.symbol : "safari"; size: 28; tint: Theme.accent }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 1
                    Text { text: sheet.flow ? sheet.flow.title : ""; font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text
                           elide: Text.ElideRight; Layout.fillWidth: true }
                    // plain text: it can carry portal-supplied values (EquatePlus document types)
                    Text { text: sheet.flow ? sheet.flow.info : ""; textFormat: Text.PlainText; font.pixelSize: Theme.caption; wrapMode: Text.WordWrap; Layout.fillWidth: true
                           color: sheet.flow && sheet.flow.infoWarning ? Theme.warning : Theme.secondary }
                }
                SyncBatchProgressLabel { visible: !!sheet.flow && sheet.flow.showBatchProgress }
                RowLayout {
                    visible: !!sheet.flow && sheet.flow.badgeText !== ""
                    spacing: 4
                    SFIcon { visible: !!sheet.flow && sheet.flow.badgeSymbol !== ""; symbol: sheet.flow ? sheet.flow.badgeSymbol : ""; size: 12
                             color: sheet.flow ? sheet.flow.badgeColor : Theme.secondary }
                    Text { text: sheet.flow ? sheet.flow.badgeText : ""; font.pixelSize: Theme.caption; font.weight: Font.Medium
                           color: sheet.flow ? sheet.flow.badgeColor : Theme.secondary }
                }
                BusyIndicator { implicitWidth: 18; implicitHeight: 18; visible: running
                                running: sheet.visible && (sheet.loading || (!!sheet.flow && sheet.flow.busy)) }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: sheet.webHeight
                color: "white"
                Loader {
                    id: viewLoader
                    anchors.fill: parent
                    active: sheet.visible && sheet.cookiesRestored && !!sheet.flow && sheet.flow.readyToLoad
                    onActiveChanged: if (!active) { sheet.rejectJobs(); sheet.view = null }
                    sourceComponent: WebEngineView {
                        id: webView
                        profile: webProfile
                        Component.onCompleted: sheet.startView(webView)
                        Component.onDestruction: if (sheet.view === webView) sheet.view = null
                        onUrlChanged: sheet.currentURL = url.toString()
                        onLoadingChanged: info => {
                            sheet.loading = webView.loading
                            // a job's result counts only from the document that started it
                            if (info.status === WebEngineView.LoadStartedStatus) sheet.rejectJobs()
                            if (info.status === WebEngineView.LoadSucceededStatus && sheet.flow) sheet.flow.pageLoaded(info.url.toString())
                        }
                        // PortalNavigationGuard: any https page in the main frame (bank SSO may
                        // redirect to other domains), never http, file, javascript, data or app schemes.
                        onNavigationRequested: request => {
                            if (!request.isMainFrame || !sheet.flow || !sheet.flow.navigationGuard) return
                            const u = request.url.toString()
                            if (u === "about:blank" || u.toLowerCase().startsWith("https:")) { sheet.blockedHost = ""; return }
                            const p = sheet.urlParts(u)
                            sheet.blockedHost = p && p.hostname ? p.hostname : (p ? p.protocol.replace(":", "") : "?")
                            request.reject()
                        }
                        // pop-ups and target=_blank: same view (AmazonUIDelegate)
                        onNewWindowRequested: request => { webView.url = request.requestedUrl }
                    }
                }
                // FnzUI.swift: AccountSelectionPanel over the page while a fetch asks (#346)
                Loader {
                    anchors.fill: parent
                    active: !!sheet.flow && sheet.flow.choosingAccounts
                    sourceComponent: Rectangle {
                        color: Theme.card
                        AccountSelectionPanel {
                            anchors.fill: parent
                            connection: sheet.flow ? sheet.flow.connection : null
                            onDone: if (sheet.flow) sheet.flow.endAccountChoice()
                        }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
            Loader {
                id: flowLoader
                Layout.fillWidth: true
                Layout.margins: 12
            }
        }
    }
}
