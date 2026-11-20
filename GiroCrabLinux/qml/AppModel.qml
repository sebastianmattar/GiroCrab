pragma Singleton
import QtQuick
import QtCore
import GiroCrab

// AppModel.swift: app state and core bridge. Names follow the Swift model so
// the views read the same (`model.title`, `model.headerBalance`, …).
// Sidebar selection is a string key: "all", "documents", "uncategorized",
// "group:<id>", "userGroup:<uuid>", "account:<id>", "category:<id>".
QtObject {
    id: model

    // MARK: core bridge
    function core(method, args) {
        const r = JSON.parse(Core.call(method, JSON.stringify(args || {})))
        if (r.err !== undefined) throw new Error(r.err)
        return r.ok
    }
    /// `core`, null when the call fails.
    function coreOrNull(method, args) {
        try { return core(method, args) } catch (e) { return null }
    }
    /// `offMain` / `vaultOffMain`: runs on a worker thread, `done(result)` on the GUI thread.
    property var pendingCalls: ({})
    property Connections coreReplies: Connections {
        target: Core
        function onFinished(ticket, result) {
            const cb = model.pendingCalls[ticket]
            delete model.pendingCalls[ticket]
            if (cb) cb(JSON.parse(result))
        }
    }
    function coreAsync(method, args, done, failed) {
        const t = Core.callAsync(method, JSON.stringify(args || {}))
        pendingCalls[t] = r => {
            if (r.err !== undefined) { if (failed) failed(r.err); else errorMessage = r.err }
            else if (done) done(r.ok)
        }
    }

    // MARK: background loads (`offMain` + generation check, CLAUDE.md › Architecture)
    /// Bumped whenever the vault on screen changes (open, demo ↔ own, lock): an answer
    /// for the previous vault is dropped.
    property int vaultGeneration: 0
    /// The core has a vault in front (`use_own` / `open_demo` done); `vaultState` turns
    /// "open" only once the accounts are there.
    property bool vaultLoaded: false
    /// channel → { gen, busy, again, request, apply, failed }
    property var loads: ({})
    /// Latest-wins load on a worker thread. `request()` returns `[[method, args], …]`
    /// (or `{ calls, ctx }`, or null to skip) and is evaluated when the load starts;
    /// `apply(results, ctx)` runs on the GUI thread, only for the newest request of the
    /// channel and the same vault. One load per channel runs at a time: asking again
    /// meanwhile stops the running batch at its next call (`supersede`) and starts once
    /// more with fresh arguments when it is back, so fast clicking queues nothing.
    /// The demo vault (small, launch options and screenshots expect its data at once)
    /// loads synchronously.
    function loadOffMain(channel, request, apply, failed) {
        if (!vaultLoaded) return
        const l = loads[channel] || (loads[channel] = { gen: 0, busy: false, again: false })
        l.gen++
        l.request = request; l.apply = apply; l.failed = failed || null
        if (demoActive) {
            const req = loadRequest(request)
            if (!req) return
            let results
            try { results = req.calls.map(c => core(c[0], c[1] || {})) } catch (e) {
                if (failed) failed(e.message); else errorMessage = parseCoreError(e.message).message
                return
            }
            apply(results, req.ctx)
            return
        }
        Core.call("supersede", JSON.stringify({ channel: channel, token: l.gen }))
        if (l.busy) { l.again = true; return }
        startLoad(channel, l)
    }
    function loadRequest(request) {
        const r = request()
        if (!r) return null
        return Array.isArray(r) ? { calls: r, ctx: null } : r
    }
    function startLoad(channel, l) {
        l.again = false
        if (!vaultLoaded) return
        const req = loadRequest(l.request)
        if (!req) return
        const gen = l.gen, vgen = vaultGeneration, apply = l.apply, failed = l.failed
        l.busy = true
        l.runningGen = gen
        const back = () => {
            l.busy = false
            if (!l.again) return false
            startLoad(channel, l)   // a newer request came in meanwhile
            return true
        }
        coreAsync("batch", { channel: channel, token: gen, calls: req.calls.map(c => ({ method: c[0], args: c[1] || {} })) }, r => {
            if (back() || r.cancelled || gen !== l.gen || vgen !== vaultGeneration || !vaultLoaded) return
            apply(r.results, req.ctx)
        }, err => {
            if (back() || gen !== l.gen || vgen !== vaultGeneration || !vaultLoaded) return
            if (failed) failed(err); else errorMessage = parseCoreError(err).message
        })
    }
    /// Drops the answer of a running load of `channel` (and stops its batch early).
    function cancelLoad(channel) {
        const l = loads[channel]
        if (!l) return
        l.gen++
        l.again = false
        if (l.busy) Core.call("supersede", JSON.stringify({ channel: channel, token: l.gen }))
    }
    /// A load of `channel` will still publish: running (not cancelled) or asked for again.
    function isLoadPending(channel) { const l = loads[channel]; return !!l && (l.again || (l.busy && l.runningGen === l.gen)) }

    /// UserDefaults → QSettings (~/.config/GiroCrab/GiroCrab.conf).
    property Settings defaults: Settings {
        /// `girocrab.hasRealData`: "" = never answered, "true" / "false".
        property string hasRealData: ""
        /// AccountSidebarMode choice; "" = none yet (then "byGroup" once groups exist).
        property string accountView: ""
        property bool showInspector: true
        property bool showStatistics: true
        // Also read/written by key (`value` / `setValue`): `bookings/autoReviewDelay`,
        // `period/customFrom`, `period/customTo` (the keys of the former separate Settings).
    }

    /// `run { … }` in the Swift model: errors end up in the alert.
    function run(fn) {
        try { return fn() } catch (e) { errorMessage = e.message; console.warn(e.message, e.stack); return undefined }
    }

    // MARK: vault state ("starting", "locked", "unlocking", "open", "failed")
    property string vaultState: "starting"
    property string failure: ""
    property bool demoActive: false
    readonly property bool isUnlocked: vaultState === "open"
    readonly property bool isDemo: demoActive
    /// `ownVault != nil`: the personal vault is unlocked (maybe in the background of the demo).
    property bool hasOwnVault: false
    property bool unlockCelebrating: false
    /// Demo only: the lock screen shown once so the protection can be tried out.
    property bool lockPreview: false
    /// Set when the user left the demo to the lock screen: no automatic prompt then.
    property bool demoDismissedLock: false
    /// The personal vault is what is shown (not the demo).
    readonly property bool holdsRealData: hasOwnVault && !demoActive
    property var vaultInfo: ({ path: "", size: -1, keychain_bypassed: false, demo_seed: false })
    property string errorMessage: ""
    property string errorTitle: ""
    /// AppModel+Transfer.swift reportUnrecordedOrder (#808): the bank executed the order, the vault
    /// could not record it (no optimistic booking, planned transfer or standing order; for a
    /// collection the mandates are not marked as collected).
    function reportUnrecordedOrder(error, debit) {
        const message = debit
            ? qsTr("Die Bank hat die Lastschriften ausgeführt, GiroCrab konnte sie aber nicht speichern. Die Mandate sind nicht als eingezogen markiert: Prüfen Sie sie vor dem nächsten Einzug, sonst wird doppelt eingezogen.")
            : qsTr("Die Bank hat den Auftrag ausgeführt, GiroCrab konnte ihn aber nicht speichern. Er erscheint mit dem nächsten Abruf. Senden Sie ihn nicht erneut.")
        errorTitle = qsTr("Ausgeführt, aber nicht gespeichert")
        errorMessage = message + "\n\n" + error
    }
    property string toast: ""
    property bool showAddBank: false
    property bool showMoneyMoneyImport: false

    // MARK: data
    property var groups: []          // [AccountGroup] (by kind)
    property var userGroups: []      // [UserGroup]
    property var accountsByID: ({})
    property var accountMarkers: ({})
    property var categories: []
    property var categoryTree: []
    property var bookings: []
    property var dayGroups: []
    property var streamItems: []     // flattened: {type:"header", day} | {type:"row", booking, isLast}
    // Model of the booking list: one empty row per stream item; the delegate reads
    // streamItems[index]. Only the count is synced, so paging appends rows instead of
    // resetting the view (which would jump back to the top).
    readonly property ListModel streamModel: ListModel {}
    property var page: null
    property var selectionFlow: null
    property var history: []
    /// AppModel.swift › categoryHistory: monthly sums of the selected category incl.
    /// subcategories (chart above its bookings, #502).
    property var categoryHistory: []
    property var connections: []
    property var recurring: []
    property var recurringMonth: null
    property var merchantStats: null
    /// The same history restricted to the booking's own account (#299): { account_id, stats }.
    property var merchantAccountStats: null
    property var budgets: []
    property var cashflow: []

    // MARK: UI state
    property string sidebarSelection: "all"
    // any other sidebar choice ends the merchant list (#495) before the list reloads
    onSidebarSelectionChanged: {
        MerchantFocus.sidebarChanged(); listLimit = defaultListLimit
        // AppModel.swift › sidebarSelection: a category with its own default period switches
        // to it; that load covers the selection too (#425).
        const saved = sidebarSelection.startsWith("category:") ? categoryPeriod(Number(sidebarSelection.slice(9))) : null
        if (saved !== null && saved !== period) { selectionNeeds.recurring = true; period = saved; return }
        reloadSelection(false)
    }
    property string scope: "All"               // BookingScope
    onScopeChanged: { listLimit = defaultListLimit; if (scope === "Investments") reloadDashboard(); reloadBookings() }
    property string period: "all"
    onPeriodChanged: { listLimit = defaultListLimit; reloadSelection(true) }
    property string customPeriodFrom: ""
    property string customPeriodTo: ""
    property string statusFilter: "all"        // all | new | checked
    onStatusFilterChanged: { listLimit = defaultListLimit; reloadBookings() }
    property string directionFilter: "All"     // BookingDirection
    onDirectionFilterChanged: { listLimit = defaultListLimit; reloadBookings() }
    property string searchText: ""
    property Timer searchTimer: Timer { interval: 250; onTriggered: model.reloadBookings() }
    onSearchTextChanged: searchTimer.restart()
    /// AccountSidebarMode (byGroup | byBank | byKind): the explicit choice, else "Nach Gruppe" once a group exists.
    /// Set it with `setAccountView` (AccountGroupsModel.swift › accountView).
    readonly property string accountView: defaults.accountView !== "" ? defaults.accountView : (hasUserGroups ? "byGroup" : "byKind")
    function setAccountView(v) { defaults.accountView = v }
    // sidebar state, kept like the Swift UserDefaults (see MARK: account groups)
    property var collapsedGroups: sidebarDefaults.loadSet("collapsedKindGroups")
    onCollapsedGroupsChanged: sidebarDefaults.setValue("collapsedKindGroups", JSON.stringify(Object.keys(collapsedGroups).map(Number)))
    property var collapsedUserGroups: sidebarDefaults.loadSet("collapsedUserGroups")
    onCollapsedUserGroupsChanged: sidebarDefaults.setValue("collapsedUserGroups", JSON.stringify(Object.keys(collapsedUserGroups)))
    property var collapsedBankGroups: sidebarDefaults.loadSet("collapsedBankGroups")
    onCollapsedBankGroupsChanged: sidebarDefaults.setValue("collapsedBankGroups", JSON.stringify(Object.keys(collapsedBankGroups)))
    /// AllAccountsRoot.swift › `sidebar.allAccountsCollapsed`
    property bool allAccountsCollapsed: String(sidebarDefaults.value("allAccountsCollapsed", false)) === "true"
    onAllAccountsCollapsedChanged: sidebarDefaults.setValue("allAccountsCollapsed", allAccountsCollapsed)
    /// SidebarView.swift › CategoriesHeader: `sidebar.categoriesCollapsed`
    property bool categoriesCollapsed: String(sidebarDefaults.value("categoriesCollapsed", false)) === "true"
    onCategoriesCollapsedChanged: sidebarDefaults.setValue("categoriesCollapsed", categoriesCollapsed)
    /// ViewLayout.swift › `@AppStorage("showBudgets")`: Darstellung › Budgets anzeigen
    property bool showBudgets: uiDefaults.showBudgets
    onShowBudgetsChanged: uiDefaults.showBudgets = showBudgets
    /// ViewLayout.swift › `@AppStorage("compactHeader")`: Darstellung › Kopfbereich minimieren (#358)
    property bool compactHeader: uiDefaults.compactHeader
    onCompactHeaderChanged: uiDefaults.compactHeader = compactHeader
    /// Sidebar account search (AppModel.swift › sidebarAccountSearch, not persisted).
    property string sidebarAccountSearch: ""
    property var expandedCategories: ({})
    property bool showArchivedAccounts: String(sidebarDefaults.value("showArchived", true)) !== "false"
    onShowArchivedAccountsChanged: sidebarDefaults.setValue("showArchived", showArchivedAccounts)
    property bool showInspector: defaults.showInspector
    onShowInspectorChanged: defaults.showInspector = showInspector
    property bool showStatistics: defaults.showStatistics
    onShowStatisticsChanged: defaults.showStatistics = showStatistics

    property var selectedBookingID: null
    property var selectedBookingIDs: ({})
    onSelectedBookingIDChanged: {
        if (stickyBooking !== null && stickyBooking.id !== selectedBookingID) stickyBooking = null
        if (lingeringBookingID !== null && lingeringBookingID !== selectedBookingID) {
            const gone = lingeringBookingID
            lingeringBookingID = null
            bookings = bookings.filter(b => b.id !== gone)
            rebuildStream()
        }
        const ids = {}
        if (selectedBookingID !== null) ids[selectedBookingID] = true
        if (selectedBookingID === null || !selectedBookingIDs[selectedBookingID]) selectedBookingIDs = ids
        loadMerchantStats()
    }
    /// A booking that left the filtered list because the user just edited it
    /// (e.g. "Geprüft" under "Neu", "kein Gehalt" in the salary period). It stays
    /// selected and visible in the inspector until the user selects something else.
    property var stickyBooking: null
    /// The sticky booking was checked while selected in „Neue Umsätze“ or under "Neu": its row
    /// stays in place (shown as checked) until the user selects something else,
    /// instead of vanishing under the cursor (#470).
    property var lingeringBookingID: null
    /// Booking whose inspector note field should take focus (double-click / Return in the list).
    property var pendingNoteFocus: null
    /// Hands keyboard focus back to the booking list (Esc in the note field).
    signal focusList()
    /// The data behind the UI was replaced (demo ↔ own vault, unlock, backup restore): singletons
    /// holding per-vault state (undo history, merchant list, sidebar settings, standing orders) reset.
    signal vaultReplaced()
    /// Day card the stream should scroll to (click in the balance chart).
    signal scrollToDay(string day)
    /// The start-up preselection must not scroll the list (SwiftUI sets it before the view appears).
    property bool scrollOnSelect: true
    readonly property int defaultListLimit: 300
    property int listLimit: defaultListLimit

    // MARK: enums (titles as in the Swift extensions)
    // Period ids (AppModelTypes.swift › Period): "all", "days:<n>", "months12", "sinceSalary",
    // "month:<y>:<m>", "quarter:<y>:<q>", "year:<y>", "custom".
    /// The menu sections like MoneyMoney (GitHub #382): rolling windows, then the latest months,
    /// quarters and years by name, counted back from today; "custom" comes from the date dialog.
    function periodMenu(now) {
        now = now || new Date()
        const y = now.getFullYear(), m = now.getMonth() + 1
        const months = [0, 1, 2].map(back => { const i = y * 12 + (m - 1) - back; return "month:" + Math.floor(i / 12) + ":" + (i % 12 + 1) })
        const quarters = [0, 1, 2].map(back => { const i = y * 4 + Math.floor((m - 1) / 3) - back; return "quarter:" + Math.floor(i / 4) + ":" + (i % 4 + 1) })
        return [["all"], ["days:30", "days:90", "months12", "sinceSalary"], months, quarters, [0, 1, 2].map(b => "year:" + (y - b))]
    }
    /// The month before the current one (default of the date dialog, demo print).
    function previousMonthPeriod() { return periodMenu()[2][1] }
    /// The year before the current one.
    function previousYearPeriod() { return periodMenu()[4][1] }
    function periodTitleOf(p) {
        const a = p.split(":")
        switch (a[0]) {
        case "all": return qsTr("Gesamter Zeitraum")
        case "days": return qsTr("%1 Tage").arg(a[1])
        case "months12": return qsTr("12 Monate")
        case "sinceSalary": return qsTr("Seit letztem Gehalt")
        case "month": return new Date(Number(a[1]), Number(a[2]) - 1, 1).toLocaleDateString(Fmt.locale, "MMMM yyyy")
        case "quarter": return qsTr("%1. Quartal %2").arg(a[2]).arg(a[1])
        case "year": return qsTr("Jahr %1").arg(a[1])
        case "custom": return qsTr("Benutzerdefiniert")
        }
        return p
    }
    readonly property var scopes: [
        { id: "All", title: qsTr("Alle Buchungen"), shortTitle: qsTr("Alle") },
        { id: "Everyday", title: qsTr("Alltag & Konsum"), shortTitle: qsTr("Alltag") },
        { id: "Investments", title: qsTr("Sparpläne & Perks"), shortTitle: qsTr("Sparpläne") }]
    readonly property var statusFilters: [
        { id: "all", title: qsTr("Alle") }, { id: "new", title: qsTr("Neu") }, { id: "checked", title: qsTr("Geprüft") }]
    readonly property var directions: [
        { id: "All", title: qsTr("Einnahmen und Ausgaben") }, { id: "Income", title: qsTr("Nur Einnahmen") },
        { id: "Expense", title: qsTr("Nur Ausgaben") }]
    function titleOf(list, id) { for (const x of list) if (x.id === id) return x.title; return id }

    // MARK: launch options (LaunchOptions.swift: the same switches as the Mac app)
    readonly property var launchArgs: Qt.application.arguments
    /// The value after `flag`, lowercased like LaunchOptions.swift ("" = not given).
    function launchValue(flag) { const i = launchArgs.indexOf(flag); return i >= 0 && i + 1 < launchArgs.length ? launchArgs[i + 1].toLowerCase() : "" }
    /// `--demo-sync`: "Alle abrufen" right away (simulated PIN / TAN dialogs).
    readonly property bool optionDemoSync: launchArgs.indexOf("--demo-sync") >= 0
    /// `--demo-sheet <name>`, `--demo-settings <tab>[:edit|add|remove]` (opened by presentLaunchOptions).
    readonly property string demoSheet: launchValue("--demo-sheet")
    readonly property string demoSettings: launchValue("--demo-settings")
    /// `--demo-seed` or GIROCRAB_DEMO_SEED=1|true|yes; `--screenshot`, `--demo-sync`,
    /// `--demo-sheet` and `--demo-settings` imply it (reproducible pictures).
    readonly property bool optionDemoSeed: launchArgs.indexOf("--demo-seed") >= 0 || vaultInfo.demo_seed === true
        || screenshotMode || optionDemoSync || demoSheet !== "" || demoSettings !== ""

    // MARK: - Vault lifecycle

    /// Real data may exist in the (encrypted, hence unreadable while locked) vault.
    /// Without a stored answer, an existing vault file counts as real data.
    function vaultMayHoldRealData() {
        if (defaults.hasRealData !== "") return defaults.hasRealData === "true"
        return vaultInfo.size > 0
    }

    /// Called once by the root view: decides lock vs. demo.
    function bootstrap() {
        if (vaultState !== "starting") return
        if (appWasReset) toast = qsTr("GiroCrab wurde zurückgesetzt")
        withFreshBankingPolicy(null)
        vaultInfo = core("vault_info")
        if (optionDemoSeed) {
            openDemo()
            if (demoSheet === "account-selection" || demoSettings === "banks:select") {
                // a second login reaching accounts the first one delivers (#346)
                run(() => core("seed_demo_account_selection"))
                reloadAll()
            }
            applyLaunchSelection()
            // --demo-sheet / --demo-settings: presentLaunchOptions() once the vault is open
            return
        }
        if (vaultMayHoldRealData()) {
            vaultState = "locked"
            unlock()
        } else {
            openDemo()
        }
    }

    /// `openDemoVault()`: throw-away demo vault; the personal vault stays open in the background.
    function openDemo() {
        try {
            core("open_demo")
            vaultSwitched()
            lockPreview = false
            demoActive = true
            vaultState = "open"
            selectedBookingID = null
            sidebarSelection = "all"
            reloadAll()
            // Sachwerte: the demo's gold items load real prices from the API
            refreshAssetPrices(false)
            // macOS: preselect a consumption booking with a document, else the first one.
            let pick = bookings.find(b => b.flow === "Consumption" && b.document_count > 0)
                    || bookings.find(b => b.flow === "Consumption")
            scrollOnSelect = false
            selectedBookingID = pick ? pick.id : null
            scrollOnSelect = true
        } catch (e) {
            failure = e.message
            vaultState = "failed"
        }
    }

    /// Unlock: key from the Secret Service (the keyring may ask for its password),
    /// open the vault off the GUI thread. A new key only while no vault file exists.
    function unlock() {
        if (vaultState === "unlocking") return
        demoDismissedLock = false
        lockPreview = false
        vaultState = "unlocking"
        unlockPhase = "keyring"
        vaultInfo = core("vault_info")
        coreAsync("unlock_own", { allow_create: vaultInfo.size <= 0 }, r => {
            hasOwnVault = true
            vaultInfo = core("vault_info")
            // Let the crab's unlock hop play on the lock screen before the main view replaces it.
            unlockCelebrating = true
            celebrationTimer.next = () => {
                if (r.account_count === 0 && !r.has_connections) {
                    // Nothing synced yet: show demo bookings so the UI can be judged.
                    defaults.hasRealData = "false"
                    openDemo()
                    toast = qsTr("Ihr Tresor ist noch leer – Sie sehen Demo-Daten")
                } else {
                    // The key is there: "Buchungen werden geladen …" until the accounts are
                    // loaded (off the GUI thread); the rest fills in in the main view.
                    unlockPhase = "loading"
                    openVaultTimer.restart()
                }
            }
            celebrationTimer.restart()
        }, err => {
            if (!hasOwnVault && !vaultMayHoldRealData()) {
                // Cancelled before any real data exists: back to the demo, never a dead end.
                openDemo()
                toast = err
            } else {
                failure = err
                vaultState = "failed"
            }
        })
    }
    property Timer celebrationTimer: Timer {
        /// what unlock() does once the hop has played
        property var next: null
        interval: 320
        onTriggered: { model.unlockCelebrating = false; const f = next; next = null; if (f) f() }
    }
    /// "keyring" while the key comes from the Secret Service, "loading" while the vault loads.
    property string unlockPhase: "keyring"
    property Timer openVaultTimer: Timer { interval: 60; onTriggered: model.useVault(false) }

    /// `useVault(_:demo:)` for the personal vault.
    /// The main view shows once the accounts are loaded; list, chart and the rest follow.
    /// Coming from the demo, its data stays until then (the accounts load synchronously
    /// while `demoActive` still holds, so no demo id ever reaches the own vault).
    function useVault(demo) {
        if (!run(() => { core("use_own"); return true })) return
        vaultSwitched()
        vaultLoaded = false   // the selection reset below must not load yet
        selectedBookingID = null
        sidebarSelection = "all"
        vaultLoaded = true
        selectFirstBookingOnLoad = true
        reloadAll(() => {
            demoActive = demo
            if (!demo) defaults.hasRealData = "true"
            // nothing of the demo stays clickable under the own accounts: spinner until the list is there
            showSelectionPlaceholder()
            page = null; bookings = []; rebuildStream()
            categories = []; categoryTree = []; recurring = []; recurringHidden = []; recurringMonth = null
            selectionFlow = null; salaryCycle = null
            vaultState = "open"
            refreshAssetPrices(false)
        }, err => {
            vaultLoaded = false
            failure = parseCoreError(err).message
            vaultState = "failed"
        })
    }
    /// A different vault is in front (or none): answers still on their way belong to the old one.
    function vaultSwitched() {
        vaultGeneration++
        dropPendingPage()   // a "load more" page of the previous vault must not land in this list
        vaultLoaded = true
        afterAccountsQueue = []
        afterBookingsQueue = []
        selectionNeeds = ({ recurring: false, categories: false })
        selectFirstBookingOnLoad = false
        endSelectionLoad()
        merchantStatsTimer.stop()
        merchantStatsCache = ({})
        merchantStatsKey = ""
        vaultReplaced()
    }
    /// Switch to the demo data (own vault stays unlocked in the background).
    function showDemo() { if (!demoActive) openDemo() }
    /// Switch back to the personal vault (unlocks it first if needed).
    function showOwnVault() {
        if (hasOwnVault) useVault(false)
        else { vaultState = "locked"; unlock() }
    }
    function previewLock() {
        if (!demoActive || hasOwnVault || vaultMayHoldRealData()) return
        lockPreview = true
        demoDismissedLock = true
        vaultState = "locked"
    }
    function endLockPreview() {
        if (!lockPreview || vaultMayHoldRealData()) return
        lockPreview = false
        openDemo()
    }
    function lock() {
        if (!holdsRealData) return
        // an open tag sheet keeps what it changed (AppModel.swift › lock)
        if (TagManager.shown) TagManager.applyDraft()
        demoDismissedLock = true
        releaseVaultState()
        vaultState = "locked"
    }
    /// Ends every bank dialog, drops the vault and clears what it loaded (shared by `lock()` and the app reset).
    function releaseVaultState() {
        endBankDialogsForLock()
        run(() => core("lock"))   // also cancels every bank dialog in the core (src/sessions.rs)
        vaultSwitched()
        vaultLoaded = false
        hasOwnVault = false
        demoActive = false
        groups = []; userGroups = []; accountsByID = ({}); accountMarkers = ({}); bookings = []; categories = []; categoryTree = []
        connections = []; page = null; merchantStats = null; merchantAccountStats = null; streamItems = []; streamModel.clear(); dayGroups = []; history = []
        rules = []; settings = []; dashboard = null; budgets = []; recurring = []; recurringMonth = null; recurringHidden = []
        selectedBookingID = null; stickyBooking = null; salaryCycle = null
        ++loanGeneration; loans = []; loanLinks = ({}); loanEditor = null; openingBalanceRequest = null
        tangibleAccountIDs = ({}); assetItemsByAccount = ({}); selectedAssetItemID = null; selectedAssetHistory = []
        assetEditor = null; assetAction = null
    }

    // MARK: - App zurücksetzen (AppModel+Reset.swift, #241)
    /// „App zurücksetzen“ is running (guards against a second start).
    property bool isResettingApp: false
    /// `--app-was-reset`: the new instance started after „App zurücksetzen“.
    readonly property bool appWasReset: launchArgs.indexOf("--app-was-reset") >= 0
    /// Deletes vault, key and settings (core `reset_app`), then starts anew. The Mac app
    /// bootstraps again in place; here the app restarts, so no `Settings` object keeps
    /// an old value in memory. The new instance opens the demo and says so.
    function resetApp() {
        if (isResettingApp) return
        isResettingApp = true
        releaseVaultState()
        allAccountsCollapsed = false
        vaultState = "starting"
        try {
            core("reset_app")
        } catch (e) {
            isResettingApp = false
            toast = qsTr("Zurücksetzen fehlgeschlagen: %1").arg(e.message)
            vaultInfo = run(() => core("vault_info")) || vaultInfo
            if (vaultMayHoldRealData()) vaultState = "locked"
            else { vaultState = "starting"; bootstrap() }
            return
        }
        run(() => core("request_relaunch"))
        Qt.quit()
    }

    // MARK: loading
    /// Everything of the vault on screen. The accounts come first: `then` runs as soon as
    /// they are there (the main view can show), the rest fills in off the GUI thread.
    function reloadAll(then, failed) {
        reloadSettings()
        reloadAccounts(() => {
            if (then) then()
            // the list first: the other loads would queue before it at the vault mutex
            afterBookingsQueue.push(() => { reloadRules(); reloadPlanning(); reloadDashboard() })
            requestSelectionLoad({ recurring: true, categories: true }, true)
        }, failed)
    }
    /// While an accounts load runs, `fn` waits for it (group membership decides the account
    /// ids of a selection) and true comes back; one entry per `key`. False: nothing runs.
    property var afterAccountsQueue: []
    function whenAccountsLoaded(key, fn) {
        if (!isLoadPending("accounts")) return false
        if (!key || !afterAccountsQueue.some(e => e.key === key)) afterAccountsQueue.push({ key: key, fn: fn })
        return true
    }
    /// Groups, connections and Sachwerte in one background batch; `then` runs after they are published.
    function reloadAccounts(then, failed) {
        if (!vaultLoaded) return
        if (then) afterAccountsQueue.push({ key: null, fn: then })
        loadOffMain("accounts", () => [["list_account_groups"], ["list_user_groups"], ["list_bank_connections"],
                                       ["list_tangible_account_ids"], ["list_all_asset_items"]], r => {
            groups = r[0]
            userGroups = r[1]
            connections = r[2]
            if (sidebarSelection.startsWith("userGroup:") && !userGroups.some(g => "userGroup:" + g.uuid === sidebarSelection))
                sidebarSelection = "all"
            if (sidebarSelection.startsWith("bank:") && !bankGroup(sidebarSelection.slice(5)))
                sidebarSelection = "all"
            rebuildAccountIndex()
            applyAssets(r[3], r[4])
            const queue = afterAccountsQueue
            afterAccountsQueue = []
            for (const e of queue) e.fn()
        }, err => {
            afterAccountsQueue = []
            if (failed) failed(err); else errorMessage = parseCoreError(err).message
        })
    }
    function rebuildAccountIndex() {
        reloadLoans()
        const byID = {}, markers = {}, count = {}
        const accounts = allAccounts
        const base = a => a.name.replace(" (MoneyMoney)", "")
        for (const a of accounts) { byID[a.id] = a; const k = base(a).toLowerCase(); count[k] = (count[k] || 0) + 1 }
        for (const a of accounts) {
            let label = base(a)
            if (count[label.toLowerCase()] > 1) {
                const digits = (a.iban || a.account_number).replace(/\D/g, "").slice(-4)
                if (digits && label.indexOf(digits) < 0) label += " ··" + digits
            }
            markers[a.id] = { label: label, color: Theme.readable(a.color_hex), symbol: a.sf_symbol }
        }
        accountsByID = byID
        accountMarkers = markers
    }
    function reloadCategories() {
        if (!isUnlocked || selectionLoadTakes("categories")) return
        loadOffMain("categories", () => {
            const r = periodRange()
            return [["list_categories", { date_from: r[0], date_to: r[1] }]]
        }, r => applyCategories(r[0]))
    }
    function applyCategories(list) {
        categories = list
        categoryTree = buildTree(list)
    }
    /// Category tree from the flat list in one pass (grouped by parent first; the
    /// former per-node filter was quadratic and slow with large MoneyMoney imports).
    /// Order within a level stays the order of `flat`.
    function buildTree(flat) {
        const byParent = {}
        for (const c of flat) {
            const key = c.parent_id === null ? "root" : String(c.parent_id)
            ;(byParent[key] || (byParent[key] = [])).push(c)
        }
        function children(key) {
            return (byParent[key] || []).map(c => {
                const kids = children(String(c.id))
                return { category: c, children: kids.length ? kids : null }
            })
        }
        return children("root")
    }
    /// The booking list (and its sums) for the current filter, off the GUI thread; the
    /// previous list stays until the new one is there. `then` runs after it is published.
    function reloadBookings(then) {
        if (!isUnlocked) return
        if (then) afterBookingsQueue.push(then)
        if (selectionLoadTakes(null)) return
        if (whenAccountsLoaded("bookings", () => reloadBookings())) return
        reloadCategoryHistory()
        dropPendingPage()
        loadOffMain("bookings", () => {
            const f = currentFilter()
            const calls = [["list_transactions", { filter: f }]]
            const flowInPage = listHasFlow(f)
            if (!flowInPage) calls.push(selectionFlowCall(f))
            return { calls: calls, ctx: { flowInPage: flowInPage } }
        }, (r, ctx) => {
            publish(r[0])
            applySelectionFlow(r[0], ctx.flowInPage ? null : r[1])
            // Every edit, sync and import ends up here: cached merchant histories may be stale.
            merchantStatsCache = ({})
            afterListPublished(true)
        })
    }
    /// Run after the next published booking list (e.g. scroll to a day it had to load).
    property var afterBookingsQueue: []
    function afterListPublished(keepMerchantStats) {
        // the sticky booking is not in the list: re-read it so the inspector shows the edit
        if (stickyBooking !== null && !bookings.some(b => b.id === stickyBooking.id))
            stickyBooking = freshBooking(stickyBooking) || stickyBooking
        loadMerchantStats(keepMerchantStats)
        const queue = afterBookingsQueue
        afterBookingsQueue = []
        for (const fn of queue) fn()
    }
    /// Every full reload calls this first: a page still on its way belongs to the old list.
    function dropPendingPage() {
        listGeneration++
        isLoadingMoreBookings = false
    }
    /// The list page carries the selection's sums when no list-only filter narrows it.
    function listHasFlow(f) {
        return f.search === null && f.checked === null && f.direction === "All" && f.scope === "All"
            && f.category_id === null && !f.uncategorized_only && !(f.tag_ids || []).length && !f.untagged_only
    }
    function selectionFlowCall(f) {
        const r = periodRange()
        return ["selection_flow", { account_ids: f.account_ids, date_from: r[0], date_to: r[1] }]
    }
    function applySelectionFlow(page, s) {
        selectionFlow = s ? { consumption: s.consumption_cents, invested: s.invested_cents, income: s.income_cents }
                          : { consumption: page.flow.consumption_cents, invested: page.flow.invested_cents, income: page.flow.income_cents }
    }
    /// Shows a freshly loaded list page. A selected booking the page no longer
    /// contains falls back to the first row, unless it is sticky.
    function publish(p) {
        listGeneration++
        isLoadingMoreBookings = false
        page = p
        bookings = keepingLingeringRow(p.transactions)
        const sticky = stickyBooking !== null && stickyBooking.id === selectedBookingID
        if (selectedBookingID !== null && !sticky && !bookings.some(b => b.id === selectedBookingID))
            // „Neue Umsätze“: no booking selected unasked, auto review would check it
            selectedBookingID = bookings.length && sidebarSelection !== "newBookings" ? bookings[0].id : null
        pruneMultiSelection()
        rebuildStream()
    }
    /// The lingering booking (checked while selected) at its old place in a
    /// new page that no longer contains it: before the row that followed it.
    function keepingLingeringRow(rows) {
        const id = lingeringBookingID, kept = stickyBooking
        if (id === null || kept === null || kept.id !== id || rows.some(b => b.id === id)) return rows
        const old = bookings.findIndex(b => b.id === id)
        if (old < 0) return rows
        const ids = {}
        for (const b of rows) ids[b.id] = true
        const next = bookings.slice(old + 1).find(b => ids[b.id])
        const out = rows.slice()
        const at = next ? out.findIndex(b => b.id === next.id) : -1
        out.splice(at >= 0 ? at : out.length, 0, kept)
        return out
    }
    /// After a reload: the multiple selection keeps only bookings the list still
    /// shows (plus the inspector's own booking, which may be sticky).
    function pruneMultiSelection() {
        const keys = Object.keys(selectedBookingIDs)
        if (!keys.length) return
        const visible = {}
        for (const b of bookings) visible[b.id] = true
        const kept = {}
        let n = 0
        for (const k of keys) if (visible[k] || Number(k) === selectedBookingID) { kept[k] = true; n++ }
        if (n !== keys.length) selectedBookingIDs = kept
    }
    /// Re-reads one booking from the vault (independent of the list filter).
    function freshBooking(b) {
        if (!b || !isUnlocked) return null
        try {
            const f = { account_ids: [b.account_id], category_id: null, include_subcategories: true, uncategorized_only: false,
                        search: null, date_from: b.booking_date, date_to: b.booking_date, limit: 0, offset: 0,
                        scope: "All", merchant_key: null, direction: "All", checked: null, whole_days: false }
            return core("list_transactions", { filter: f }).transactions.find(x => x.id === b.id) || null
        } catch (e) { return null }
    }
    /// dayGroups / streamItems from `bookings` (`StreamItem.flatten`).
    function rebuildStream() {
        const days = []
        let current = null
        for (const b of bookings) {
            if (!current || current.date !== b.booking_date) {
                current = { date: b.booking_date, bookings: [], sum: 0 }
                days.push(current)
            }
            current.bookings.push(b)
            current.sum += b.amount_cents
        }
        dayGroups = days
        const items = []
        if (sidebarSelection === "newBookings") {
            // NewBookingsCards: one card per account instead of the day cards
            for (const sec of newBookingSections()) {
                items.push({ type: "account", section: sec, key: "a" + sec.accountID })
                sec.bookings.forEach((b, i) => items.push({ type: "row", booking: b, isLast: i === sec.bookings.length - 1, key: "b" + b.id }))
            }
        } else {
            for (const d of days) {
                items.push({ type: "header", day: d, key: "d" + d.date })
                d.bookings.forEach((b, i) => items.push({ type: "row", booking: b, isLast: i === d.bookings.length - 1, key: "b" + b.id }))
            }
        }
        streamItems = items
        const n = items.length, have = streamModel.count
        if (n < have) streamModel.remove(n, have - n)
        else if (n > have) streamModel.append(Array.from({ length: n - have }, () => ({ s: 0 })))
    }

    /// NewBookings.sections: the loaded bookings by account, known accounts in sidebar order,
    /// then unknown ones in order of appearance. Checked bookings are left out, except the
    /// one the user just checked (lingeringBookingID: it stays until the selection moves).
    function newBookingSections() {
        const keep = lingeringBookingID
        const list = bookings.filter(b => !b.checked || b.id === keep)
        const by = {}
        for (const b of list) (by[b.account_id] = by[b.account_id] || []).push(b)
        const out = []
        for (const a of allAccounts) if (by[a.id]) out.push(newBookingSection(a.id, a, a.name, by[a.id]))
        const seen = {}
        for (const b of list) if (!accountsByID[b.account_id] && !seen[b.account_id]) {
            seen[b.account_id] = true
            out.push(newBookingSection(b.account_id, null, b.account_name, by[b.account_id]))
        }
        return out
    }
    function newBookingSection(id, account, title, list) {
        return { accountID: id, account: account, title: title, bookings: list,
                 pendingCents: list.filter(b => b.is_pending).reduce((s, b) => s + b.amount_cents, 0) }
    }

    // MARK: paging (loadMoreBookings / loadAllBookings)
    property int listGeneration: 0
    property bool isLoadingMoreBookings: false
    // the previous selection's page may still show during a selection load
    readonly property bool hasMoreBookings: page !== null && !selectionLoadRunning && bookings.length < page.total_count
    readonly property int remainingBookingCount: page ? Math.max(0, page.total_count - bookings.length) : 0
    function loadMoreBookings(count) {
        if (!isUnlocked || !hasMoreBookings || isLoadingMoreBookings || selectionLoadRunning) return
        const f = currentFilter()
        f.offset = bookings.length
        f.limit = count > 0 ? count : defaultListLimit
        const generation = listGeneration
        isLoadingMoreBookings = true
        coreAsync("list_transactions", { filter: f }, next => {
            if (generation !== listGeneration) return
            isLoadingMoreBookings = false
            if (!next.transactions.length) return
            bookings = bookings.concat(next.transactions)
            listLimit = bookings.length
            rebuildStream()
        }, err => { isLoadingMoreBookings = false; errorMessage = err })
    }
    function loadAllBookings() {
        listLimit = Math.max(listLimit, page ? page.total_count : listLimit)
        reloadBookings()
    }

    function reloadHistory() {
        if (!isUnlocked || selectionLoadTakes(null)) return
        if (whenAccountsLoaded("history", () => reloadHistory())) return
        reloadCategoryHistory()
        loadOffMain("history", () => {
            const req = historyRequest()
            return { calls: [["balance_history", { account_ids: req.ids, days: req.days }]], ctx: periodRange() }
        }, (r, range) => { history = trimHistory(r[0], range) }, () => {})
    }
    /// AppModel+Loading.swift › categoryHistoryRequest: null unless a category is selected.
    function categoryHistoryCall() {
        const sel = sidebarSelection
        if (!sel.startsWith("category:")) return null
        const r = periodRange()
        return ["category_history_by_unit", { category_id: Number(sel.slice(9)), account_ids: categoryAccountIDs,
                                              date_from: r[0], date_to: r[1], key_length: 7 }]
    }
    /// AppModel+Loading.swift › reloadCategoryHistory (cheap SQL aggregate, still off the GUI
    /// thread). Callers check `selectionLoadTakes` first: a running selection load brings it.
    function reloadCategoryHistory() {
        if (categoryHistoryCall() === null) {
            cancelLoad("categoryHistory")
            if (categoryHistory.length) categoryHistory = []
            return
        }
        loadOffMain("categoryHistory", () => { const c = categoryHistoryCall(); return c ? [c] : null },
                    r => { categoryHistory = r[0] }, () => {})
    }
    function trimHistory(pts, r) {
        if (r[1]) pts = pts.filter(p => p.date <= r[1])
        if (!r[0]) {
            let first = -1
            for (let i = 1; i < pts.length; i++) if (pts[i].balance_cents !== pts[i - 1].balance_cents) { first = i; break }
            if (first > 0) pts = pts.slice(first - 1)
        }
        return pts
    }
    function historyRequest() {
        let ids = []
        const sel = sidebarSelection
        if (sel.startsWith("account:")) ids = [Number(sel.slice(8))]
        else if (sel.startsWith("group:")) {
            const g = groups.find(g => "group:" + g.id === sel)
            ids = g ? g.accounts.filter(a => a.archived_unix === null).map(a => a.id) : []
        } else if (sel.startsWith("bank:")) {
            const g = bankGroup(sel.slice(5))
            ids = g ? g.accounts.filter(a => a.archived_unix === null).map(a => a.id) : []
        } else if (sel.startsWith("userGroup:")) ids = userGroupBalanceAccountIDs(sel.slice(10))
        else ids = liquidAccounts.map(a => a.id)
        if (!ids.length) ids = [-1]
        const today = new Date(); today.setHours(0, 0, 0, 0)
        const from = periodRange()[0]
        const start = from ? Fmt.date(from) : new Date(today.getFullYear() - 15, today.getMonth(), today.getDate())
        const days = Math.max(2, Math.round((today - start) / 86400000) + 1)
        return { ids: ids, days: days }
    }
    /// Series, hidden series and this month's overview from one `recurring_snapshot` call.
    function refreshRecurring() {
        if (!isUnlocked) { recurring = []; recurringHidden = []; recurringMonth = null; return }
        if (selectionLoadTakes("recurring")) return
        if (whenAccountsLoaded("recurring", () => refreshRecurring())) return
        loadOffMain("recurring", () => [recurringCall()], r => applyRecurring(r[0]))
    }
    function recurringCall() {
        const now = new Date()
        return ["recurring_snapshot", { account_ids: selectionAccountIDs(), year: now.getFullYear(), month: now.getMonth() + 1 }]
    }
    function applyRecurring(s) {
        recurring = s.series
        recurringHidden = s.ignored
        recurringMonth = s.month
    }
    /// Pay cycle of the selection: `.sinceSalary` derives the period from it.
    property var salaryCycle: null
    function refreshSalaryCycle() {
        if (!isUnlocked) { salaryCycle = null; return }
        // a selection load reads the cycle first anyway
        if (selectionLoadTakes(null)) return
        if (whenAccountsLoaded("salary", () => refreshSalaryCycle())) return
        loadOffMain("salary", () => [["salary_cycle", { account_ids: selectionAccountIDs() }]], r => { salaryCycle = r[0] })
    }
    function reloadPlanning() {
        if (!isUnlocked) return
        loadOffMain("planning", () => {
            const now = new Date()
            return [["list_category_budget_progress", { year: now.getFullYear(), month: now.getMonth() + 1 }]]
        }, r => { budgets = r[0] })
    }
    /// Merchant history of the selected booking (Inspector › Händler-Historie). The core scans
    /// every booking for it, so it runs off the GUI thread and is cached per merchant; the
    /// short delay keeps arrow-key navigation from queueing one scan per row.
    /// `keepShown`: after a data change the shown history of the same merchant stays until
    /// the fresh one arrives (no flicker).
    property var merchantStatsCache: ({})
    property string merchantStatsKey: ""
    property Timer merchantStatsTimer: Timer { interval: 80; onTriggered: model.loadMerchantStatsNow() }
    function loadMerchantStats(keepShown) {
        // Not `selectedBooking`: inside onSelectedBookingIDChanged that binding may
        // not be re-evaluated yet and still answer the previous (or no) booking.
        const b = bookingByID(selectedBookingID)
        if (!b || !b.merchant.key || !isUnlocked) {
            merchantStatsTimer.stop(); cancelLoad("merchant")
            merchantStats = null; merchantAccountStats = null; merchantStatsKey = ""
            return
        }
        const key = b.merchant.key, own = b.account_id + "|" + key
        if (merchantStatsCache[key] && merchantStatsCache[own]) {
            merchantStatsTimer.stop(); cancelLoad("merchant")
            merchantStats = merchantStatsCache[key]; merchantStatsKey = key
            merchantAccountStats = { account_id: b.account_id, stats: merchantStatsCache[own] }
            return
        }
        if (!(keepShown && merchantStatsKey === key)) { merchantStats = null; merchantAccountStats = null; merchantStatsKey = "" }
        if (demoActive) loadMerchantStatsNow()
        else merchantStatsTimer.restart()
    }
    function loadMerchantStatsNow() {
        loadOffMain("merchant", () => {
            const b = bookingByID(selectedBookingID)
            if (!b || !b.merchant.key) return null
            return { calls: [["merchant_history", { merchant_key: b.merchant.key, account_id: b.account_id }]],
                     ctx: { key: b.merchant.key, accountId: b.account_id } }
        }, (r, ctx) => {
            merchantStatsCache[ctx.key] = r[0].all
            merchantStatsCache[ctx.accountId + "|" + ctx.key] = r[0].account
            const now = bookingByID(selectedBookingID)
            if (!now || now.merchant.key !== ctx.key) return
            merchantStats = r[0].all
            merchantStatsKey = ctx.key
            merchantAccountStats = now.account_id === ctx.accountId ? { account_id: ctx.accountId, stats: r[0].account } : null
        }, () => {})
    }

    // MARK: selection change (AppModel+Selection.swift › startSelectionLoad)
    /// A sidebar or period change: pay cycle first (`.sinceSalary` derives the period from it),
    /// then list, sums, chart and – depending on what changed – fixed costs or category sums,
    /// all off the GUI thread and published in one go. The previous data stays until then, at
    /// most `selectionPlaceholderTimer.interval`; after that list and chart show a spinner.
    function reloadSelection(periodChanged, quiet) {
        requestSelectionLoad(periodChanged ? { categories: true } : { recurring: true }, quiet)
    }
    /// What the next selection load brings besides list, sums and chart; a part stays
    /// requested until a load has published it.
    property var selectionNeeds: ({ recurring: false, categories: false })
    /// A selection load is running (list paging waits for it).
    property bool selectionLoadRunning: false
    /// It has run longer than the placeholder delay: list, chart and sums show the spinner / "–".
    property bool isLoadingSelection: false
    property Timer selectionPlaceholderTimer: Timer {
        interval: 200
        onTriggered: if (model.selectionLoadRunning) model.showSelectionPlaceholder()
    }
    /// Select the first booking once the next selection load is published (vault just opened).
    property bool selectFirstBookingOnLoad: false
    function requestSelectionLoad(parts, quiet) {
        if (!isUnlocked || !vaultLoaded) return
        if (parts.recurring) selectionNeeds.recurring = true
        if (parts.categories) selectionNeeds.categories = true
        if (whenAccountsLoaded("selection", () => requestSelectionLoad({}, quiet))) return
        // loads of a single part still running belong to the old selection: this one takes them over
        for (const ch of ["recurring", "categories"]) if (isLoadPending(ch)) { cancelLoad(ch); selectionNeeds[ch] = true }
        for (const ch of ["bookings", "history", "categoryHistory", "salary"]) cancelLoad(ch)
        dropPendingPage()
        selectionLoadRunning = true
        if (!quiet) {
            if (isLoadingSelection) showSelectionPlaceholder()
            else selectionPlaceholderTimer.restart()
        }
        if (period === "sinceSalary") {
            cancelLoad("selectionData")
            loadOffMain("selectionCycle", () => [["salary_cycle", { account_ids: selectionAccountIDs() }]], r => {
                salaryCycle = r[0]
                loadSelectionData(false)
            }, failSelectionLoad)
        } else {
            cancelLoad("selectionCycle")
            loadSelectionData(true)
        }
    }
    /// A running selection load brings list, sums and chart anyway (and `part`, now
    /// requested too): instead of a load of its own, it starts once more with the current
    /// state, so an older answer can never overwrite fresher data.
    function selectionLoadTakes(part) {
        if (!selectionLoadRunning) return false
        if (part) selectionNeeds[part] = true
        if (isLoadPending("selectionCycle")) return true   // the data load follows with the current state
        if (isLoadPending("selectionData")) { loadSelectionData(period !== "sinceSalary"); return true }
        return false
    }
    function loadSelectionData(withCycle) {
        loadOffMain("selectionData", () => {
            const f = currentFilter(), r = periodRange(), h = historyRequest()
            const parts = { recurring: selectionNeeds.recurring, categories: selectionNeeds.categories }
            const calls = []
            if (withCycle) calls.push(["salary_cycle", { account_ids: selectionAccountIDs() }])
            calls.push(["list_transactions", { filter: f }])
            const flowInPage = listHasFlow(f)
            if (!flowInPage) calls.push(selectionFlowCall(f))
            if (parts.recurring) calls.push(recurringCall())
            if (parts.categories) calls.push(["list_categories", { date_from: r[0], date_to: r[1] }])
            calls.push(["balance_history", { account_ids: h.ids, days: h.days }])
            const categoryChart = categoryHistoryCall()
            if (categoryChart) calls.push(categoryChart)
            return { calls: calls, ctx: { withCycle: withCycle, flowInPage: flowInPage, parts: parts, range: r,
                                          categoryChart: categoryChart !== null } }
        }, (res, ctx) => {
            let i = 0
            if (ctx.withCycle) salaryCycle = res[i++]
            const page = res[i++]
            const flow = ctx.flowInPage ? null : res[i++]
            const rec = ctx.parts.recurring ? res[i++] : null
            const cats = ctx.parts.categories ? res[i++] : null
            const pts = res[i++]
            const categorySums = ctx.categoryChart ? res[i++] : []
            endSelectionLoad()
            publish(page)
            applySelectionFlow(page, flow)
            if (rec) { applyRecurring(rec); selectionNeeds.recurring = false }
            if (cats) { applyCategories(cats); selectionNeeds.categories = false }
            history = trimHistory(pts, ctx.range)
            categoryHistory = categorySums
            if (selectFirstBookingOnLoad) {
                selectFirstBookingOnLoad = false
                if (selectedBookingID === null && bookings.length) selectedBookingID = bookings[0].id
            }
            afterListPublished(false)
        }, failSelectionLoad)
    }
    function failSelectionLoad(err) {
        endSelectionLoad()
        errorMessage = parseCoreError(err).message
        const queue = afterBookingsQueue
        afterBookingsQueue = []
        for (const fn of queue) fn()
    }
    /// The load is slow: no list, sums and chart of the previous selection under the new
    /// title. The list rows stay in the model, only hidden (BookingStreamView): building
    /// them anew after the load would block the GUI thread far longer than the load.
    function showSelectionPlaceholder() {
        isLoadingSelection = true
        history = []
        categoryHistory = []
    }
    function endSelectionLoad() {
        selectionPlaceholderTimer.stop()
        selectionLoadRunning = false
        isLoadingSelection = false
    }

    // MARK: filter
    function periodRange(p) {
        p = p || period
        const now = new Date()
        const I = Fmt.iso
        // first day of a month and the day before the first of `months` later
        const span = (y, m, months) => [I(new Date(y, m - 1, 1)), I(new Date(y, m - 1 + months, 0))]
        const a = p.split(":").map((x, i) => i ? Number(x) : x)
        switch (a[0]) {
        case "days": return [I(new Date(now.getFullYear(), now.getMonth(), now.getDate() - (a[1] - 1))), null]
        // rolling year: from the same day a year ago (exclusive) to today
        case "months12": return [I(new Date(now.getFullYear() - 1, now.getMonth(), now.getDate() + 1)), null]
        // calendar month as fallback without a detected salary
        case "sinceSalary": return salaryCycle ? [salaryCycle.last_date, null] : [I(new Date(now.getFullYear(), now.getMonth(), 1)), null]
        case "month": return span(a[1], a[2], 1)
        case "quarter": return span(a[1], (a[2] - 1) * 3 + 1, 3)
        case "year": return span(a[1], 1, 12)
        case "custom": return [customPeriodFrom || null, customPeriodTo || null]
        default: return [null, null]
        }
    }
    function selectionAccountIDs() {
        const sel = sidebarSelection
        if (sel.startsWith("account:")) return [Number(sel.slice(8))]
        if (sel.startsWith("group:")) { const g = groups.find(g => "group:" + g.id === sel); return g ? g.accounts.map(a => a.id) : [] }
        if (sel.startsWith("bank:")) { const g = bankGroup(sel.slice(5)); return g ? g.accounts.map(a => a.id) : [] }
        if (sel.startsWith("userGroup:")) return userGroupAccountIDs(sel.slice(10))
        return []
    }
    // MARK: category views narrowed to accounts (AppModel+Loading.swift, #406)
    /// AppModel.swift › categoryAccountFilter: accounts whose bookings the category views show
    /// („Ohne Kategorie“ and a category) as `{ id: true }`; empty = all. Kept while the app runs.
    property var categoryAccountFilter: ({})
    readonly property bool showsCategoryAccountFilter: sidebarSelection.startsWith("category:") || sidebarSelection === "uncategorized"
    /// The chosen accounts that still exist, in sidebar order; empty = all accounts.
    readonly property var categoryAccountIDs: Object.keys(categoryAccountFilter).length
        ? allAccounts.map(a => a.id).filter(id => categoryAccountFilter[id]) : []
    function setCategoryAccountFilter(ids) {
        const next = {}
        for (const id of ids) next[id] = true
        const same = Object.keys(next).length === Object.keys(categoryAccountFilter).length
            && Object.keys(next).every(k => categoryAccountFilter[k])
        if (same) return
        categoryAccountFilter = next
        if (!showsCategoryAccountFilter) return
        listLimit = defaultListLimit
        requestSelectionLoad({})
    }
    /// Adds or removes one account; choosing every account is the same as no filter.
    function toggleCategoryAccount(id) {
        const next = categoryAccountIDs.slice()
        const at = next.indexOf(id)
        if (at >= 0) next.splice(at, 1); else next.push(id)
        setCategoryAccountFilter(next.length === allAccounts.length ? [] : next)
    }
    /// Label of the account menu; never names an account (the header is part of bug report screenshots).
    readonly property string categoryAccountFilterTitle: categoryAccountIDs.length === 0
        ? qsTr("Konten: Alle") : qsTr("Konten: %1 von %2").arg(categoryAccountIDs.length).arg(allAccounts.length)

    // MARK: period per category (AppModel+Selection.swift, AppModelTypes.swift › Period.storageKey, #425)
    /// Relative, launch-independent form: "this year" stays "this year" next January. null for custom.
    function periodStorageKey(p, now) {
        const kind = p.split(":")[0]
        if (kind === "all" || kind === "months12" || kind === "sinceSalary" || kind === "days") return p
        const section = ({ month: 2, quarter: 3, year: 4 })[kind]
        if (section === undefined) return null
        const i = periodMenu(now)[section].indexOf(p)
        return i >= 0 ? kind + ":" + i : null
    }
    function periodFromStorageKey(key, now) {
        const parts = String(key).split(":")
        const n = parts.length === 2 ? Number(parts[1]) : NaN
        switch (parts[0]) {
        case "all": case "months12": case "sinceSalary": return parts.length === 1 ? parts[0] : null
        case "days": return Number.isInteger(n) && n > 0 ? "days:" + n : null
        }
        const section = ({ month: 2, quarter: 3, year: 4 })[parts[0]]
        if (section === undefined || !Number.isInteger(n)) return null
        const menu = periodMenu(now)[section]
        return n >= 0 && n < menu.length ? menu[n] : null
    }
    /// QSettings `period/byCategory` (UserDefaults `period.byCategory`): category id → storage key.
    function categoryPeriods() {
        try { return JSON.parse(defaults.value("period/byCategory", "{}")) || {} } catch (e) { return {} }
    }
    /// The period the user last chose for this category; null = follows the current one.
    function categoryPeriod(id) {
        const key = categoryPeriods()[String(id)]
        return key ? periodFromStorageKey(key) : null
    }
    function setCategoryPeriod(p, id) {
        const key = periodStorageKey(p)
        if (key === null) return
        const map = categoryPeriods()
        map[String(id)] = key
        defaults.setValue("period/byCategory", JSON.stringify(map))
    }
    /// AppModel.swift › interactivePeriod: the user's choice becomes the default of the selected category.
    function choosePeriod(p) {
        period = p
        if (sidebarSelection.startsWith("category:")) setCategoryPeriod(p, Number(sidebarSelection.slice(9)))
    }

    function currentFilter() {
        const r = periodRange()
        const sel = sidebarSelection
        // „Neue Umsätze“: everything new, whatever the list filters say elsewhere.
        if (sel === "newBookings")
            return { account_ids: [], category_id: null, include_subcategories: true, uncategorized_only: false,
                     search: null, date_from: null, date_to: null, limit: listLimit, offset: 0, scope: "All",
                     merchant_key: null, direction: "All", checked: false, whole_days: true }
        return TagFilter.applyTo({   // + the tag filter (#228)
            // AppModel+Loading.swift › currentFilter: the category views follow the account menu (#406)
            account_ids: showsCategoryAccountFilter ? categoryAccountIDs : selectionAccountIDs(),
            category_id: sel.startsWith("category:") ? Number(sel.slice(9)) : null,
            include_subcategories: true,
            uncategorized_only: sel === "uncategorized",
            search: searchText === "" ? null : searchText,
            date_from: r[0], date_to: r[1],
            limit: listLimit, offset: 0,
            scope: scope,
            merchant_key: MerchantFocus.key,   // the merchant list (#495)
            direction: directionFilter,
            checked: statusFilter === "all" ? null : statusFilter === "checked",
            whole_days: true
        })
    }

    // MARK: user groups (AccountGroupsModel.swift)
    function userGroupSubtree(uuid) {
        const i = userGroups.findIndex(g => g.uuid === uuid)
        if (uuid === "" || i < 0) return userGroups.filter(g => g.uuid === uuid)
        const out = [userGroups[i]]
        for (let j = i + 1; j < userGroups.length && userGroups[j].depth > userGroups[i].depth && !userGroups[j].is_ungrouped; j++)
            out.push(userGroups[j])
        return out
    }
    function userGroupAccountIDs(uuid) { return [].concat(...userGroupSubtree(uuid).map(g => g.accounts.map(a => a.id))) }
    function userGroupBalanceAccountIDs(uuid) {
        let skipBelow = null, ids = []
        userGroupSubtree(uuid).forEach((g, n) => {
            if (skipBelow !== null) { if (g.depth > skipBelow) return; skipBelow = null }
            if (n > 0 && !g.include_in_total) { skipBelow = g.depth; return }
            ids = ids.concat(g.accounts.filter(a => a.archived_unix === null).map(a => a.id))
        })
        return ids
    }
    function isUserGroupVisible(g) {
        let parent = g.parent_uuid
        while (parent) {
            if (collapsedUserGroups[parent]) return false
            const p = userGroups.find(x => x.uuid === parent)
            parent = p ? p.parent_uuid : null
        }
        return true
    }
    function toggleCollapsedUserGroup(uuid) {
        const c = Object.assign({}, collapsedUserGroups)
        if (c[uuid]) delete c[uuid]; else c[uuid] = true
        collapsedUserGroups = c
    }
    // MARK: sidebar search and „Nach Bank“ (AccountGroupsModel.swift)
    readonly property bool isSidebarAccountSearching: sidebarAccountSearch.trim() !== ""
    /// AllAccountsRoot.swift › showsAccountTree: the root is open, or the search runs.
    readonly property bool showsAccountTree: !allAccountsCollapsed || isSidebarAccountSearching
    /// Account.compactAccountIds: IBAN and account number without spaces.
    function compactAccountIds(a) {
        return [a.iban || "", a.account_number || ""].map(x => x.replace(/\s/g, "").toLowerCase()).filter(x => x !== "")
    }
    /// Account.matchesSidebarSearch: name, Kontoinhaber, bank, IBAN or account number.
    function matchesSidebarSearch(a, raw) {
        const q = raw.trim()
        if (q === "") return true
        const ids = compactAccountIds(a)
        const compactQ = q.replace(/\s/g, "").toLowerCase()
        if (compactQ.length >= 3 && ids.some(x => x.includes(compactQ))) return true
        const text = [a.name || "", a.owner || "", a.bank_name || ""].map(x => x.toLowerCase())
        return q.split(/\s+/).map(t => t.toLowerCase()).every(term =>
            text.some(x => x.includes(term)) || ids.some(x => x.includes(term)))
    }
    /// Not archived (unless shown) and matches the sidebar search.
    function isSidebarAccountListed(a) {
        return (showArchivedAccounts || a.archived_unix === null) && matchesSidebarSearch(a, sidebarAccountSearch)
    }
    /// Group title matches every whitespace-separated term.
    function sidebarTextMatches(text, query) {
        const q = query.trim()
        if (q === "") return true
        const hay = text.toLowerCase()
        return q.split(/\s+/).every(t => hay.includes(t.toLowerCase()))
    }
    /// Shown while searching: its name matches, or a listed account lives in it or a subgroup.
    function isUserGroupListed(g) {
        if (!isSidebarAccountSearching) return isUserGroupVisible(g) && !isUserGroupHiddenAsArchived(g)
        return userGroupSubtree(g.uuid).some(n => sidebarTextMatches(n.name, sidebarAccountSearch)
                                                  || n.accounts.some(a => isSidebarAccountListed(a)))
    }
    /// isSidebarSectionListed (#814): a kind or bank section is shown while searching only with a hit;
    /// otherwise it stays unless archived accounts are hidden and it holds nothing but archived
    /// accounts. A section without accounts stays.
    function isSidebarSectionListed(accounts) {
        const listed = accounts.some(a => isSidebarAccountListed(a))
        if (isSidebarAccountSearching) return listed
        return listed || !accounts.length || showArchivedAccounts
    }
    /// isUserGroupOnlyArchived (#814): archived accounts are hidden and this group (with its subgroups)
    /// holds accounts, all of them archived. A group without accounts stays.
    function isUserGroupOnlyArchived(g) {
        if (showArchivedAccounts) return false
        const accounts = [].concat(...userGroupSubtree(g.uuid).map(n => n.accounts))
        return accounts.length > 0 && accounts.every(a => a.archived_unix !== null)
    }
    /// The group or any group above it holds only archived accounts: a subgroup never stays behind
    /// under a hidden parent.
    function isUserGroupHiddenAsArchived(g) {
        for (let n = g; n; n = n.parent_uuid ? userGroups.find(x => x.uuid === n.parent_uuid) : null)
            if (isUserGroupOnlyArchived(n)) return true
        return false
    }
    function isKindGroupOpen(id) { return isSidebarAccountSearching || !collapsedGroups[id] }
    function isBankGroupOpen(key) { return isSidebarAccountSearching || !collapsedBankGroups[key] }
    function isUserGroupOpen(g) { return isSidebarAccountSearching || !collapsedUserGroups[g.uuid] }
    /// Bank key: `bank_name`, else the connection's name.
    function bankGroupKey(a) {
        const name = (a.bank_name || "").trim()
        if (name !== "") return name
        if (a.connection_id !== null) {
            const c = connections.find(c => c.id === a.connection_id)
            if (c) return (c.display_name || "").trim()
        }
        return ""
    }
    /// Accounts by bank, sorted by name („Ohne Bank“ last).
    readonly property var bankGroups: {
        const buckets = {}
        for (const a of allAccounts) (buckets[bankGroupKey(a)] = buckets[bankGroupKey(a)] || []).push(a)
        return Object.keys(buckets).sort((a, b) => {
            if ((a === "") !== (b === "")) return a === "" ? 1 : -1
            return a.localeCompare(b, undefined, { sensitivity: "accent" })
        }).map(key => {
            const accounts = buckets[key]
            const conn = accounts[0].connection_id !== null ? connections.find(c => c.id === accounts[0].connection_id) : null
            return { id: key, name: key === "" ? qsTr("Ohne Bank") : key,
                     sf_symbol: conn ? conn.sf_symbol : (accounts[0].sf_symbol || "building.columns"),
                     balance_cents: accounts.filter(a => a.currency === "EUR" && a.archived_unix === null).reduce((s, a) => s + a.balance_cents, 0),
                     accounts: accounts }
        })
    }
    function bankGroup(key) { return bankGroups.find(g => g.id === key) || null }
    /// AccountGroupsModel.swift › accountMenuSections: all accounts grouped and ordered like the
    /// sidebar shows them in the current view; subgroups carry their path ("Familie › Kinder")
    /// because a menu cannot indent. Empty sections are dropped.
    readonly property var accountMenuSections: {
        let sections
        if (accountView === "byKind") sections = groups.map(g => ({ id: "k" + g.id, title: g.name, accounts: g.accounts }))
        else if (accountView === "byBank") sections = bankGroups.map(g => ({ id: "b" + g.id, title: g.name, accounts: g.accounts }))
        else {
            let path = []
            sections = userGroups.map(g => {
                path = path.slice(0, g.depth).concat([g.name])
                return { id: "g" + g.uuid, title: path.join(" › "), accounts: g.accounts }
            })
        }
        return sections.filter(s => s.accounts.length > 0)
    }
    /// Combo box entries: "Alle Konten", then each section title and its accounts (`label(a)`).
    function accountMenuOptions(label, withoutAccount) {
        const out = [{ id: null, title: qsTr("Alle Konten") }]
        if (withoutAccount) out.push({ id: documentsWithoutAccount, title: qsTr("Ohne Konto") })
        for (const s of accountMenuSections) {
            out.push({ id: undefined, title: s.title, header: true })
            for (const a of s.accounts) out.push({ id: a.id, title: label(a) })
        }
        return out
    }
    function toggleCollapsedBank(key) {
        const c = Object.assign({}, collapsedBankGroups)
        if (c[key]) delete c[key]; else c[key] = true
        collapsedBankGroups = c
    }
    /// AllAccountsTotal.totals: accounts in the total, per currency; other currencies first, euro last.
    readonly property var allAccountsTotals: {
        const sums = {}
        for (const a of allAccounts) if (a.in_total) {
            const code = a.currency ? a.currency.toUpperCase() : "EUR"
            sums[code] = (sums[code] || 0) + a.balance_cents
        }
        // Linux: other currencies that sum to 0 are left out (several of them pushed the euro
        // total out of the narrow row); the euro total always stays.
        return Object.keys(sums).filter(c => c === "EUR" || sums[c] !== 0)
            .sort((a, b) => (a === "EUR") !== (b === "EUR") ? (a === "EUR" ? 1 : -1) : (a < b ? -1 : a > b ? 1 : 0))
            .map(c => ({ currency: c, cents: sums[c] }))
    }
    function toggleCollapsedGroup(id) {
        const c = Object.assign({}, collapsedGroups)
        if (c[id]) delete c[id]; else c[id] = true
        collapsedGroups = c
    }
    function toggleExpandedCategory(id) {
        const c = Object.assign({}, expandedCategories)
        if (c[id]) delete c[id]; else c[id] = true
        expandedCategories = c
    }

    // MARK: computed (AppModel.swift › MARK: derived values)
    readonly property var allAccounts: [].concat(...groups.map(g => g.accounts))
    readonly property var liquidAccounts: allAccounts.filter(a => a.in_total && ["Checking", "Savings", "PaymentService", "CreditCard", "Cash"].indexOf(a.kind) >= 0)
    readonly property int liquidBalance: liquidAccounts.reduce((s, a) => s + a.balance_cents, 0)
    readonly property int newCount: allAccounts.reduce((s, a) => s + a.new_count, 0)
    readonly property int uncategorizedCount: allAccounts.filter(a => a.kind !== "Loan").reduce((s, a) => s + a.uncategorized_count, 0)
    readonly property real consumptionThisMonth: selectionFlow ? selectionFlow.consumption : 0
    readonly property real investedThisMonth: selectionFlow ? selectionFlow.invested : 0
    readonly property real incomeThisMonth: selectionFlow ? selectionFlow.income : 0
    readonly property int bookingCountLabel: page ? page.total_count : bookings.length
    readonly property var selectedBooking: bookingByID(selectedBookingID)
    /// The listed booking with this id, or the sticky one (see stickyBooking).
    function bookingByID(id) {
        if (id === null || id === undefined) return null
        return bookings.find(b => b.id === id) || (stickyBooking !== null && stickyBooking.id === id ? stickyBooking : null)
    }
    readonly property var selectedAccount: sidebarSelection.startsWith("account:") ? (accountsByID[Number(sidebarSelection.slice(8))] || null) : null

    readonly property string title: {
        const sel = sidebarSelection
        if (sel === "all") return qsTr("Alle Konten")
        if (sel === "newBookings") return qsTr("Neue Umsätze")
        if (sel.startsWith("bank:")) { const g = bankGroup(sel.slice(5)); return g ? g.name : qsTr("Bank") }
        if (sel === "uncategorized") return SidebarModel.uncategorizedLabel
        if (sel === "documents") return qsTr("Belege & PostBox")
        if (sel === "wealth") return qsTr("Vermögen")
        if (sel.startsWith("group:")) { const g = groups.find(g => "group:" + g.id === sel); return g ? g.name : qsTr("Gruppe") }
        if (sel.startsWith("userGroup:")) { const g = userGroups.find(g => "userGroup:" + g.uuid === sel); return g ? g.name : qsTr("Gruppe") }
        if (sel.startsWith("account:")) { const a = accountsByID[Number(sel.slice(8))]; return a ? a.name : qsTr("Konto") }
        if (sel.startsWith("category:")) { const c = categories.find(c => "category:" + c.id === sel); return c ? c.path : qsTr("Kategorie") }
        return ""
    }
    readonly property var headerBalance: {
        const sel = sidebarSelection
        if (sel === "all") return { label: qsTr("Verfügbar (Giro, Tagesgeld, Karten)"), cents: liquidBalance }
        if (sel.startsWith("account:")) { const a = accountsByID[Number(sel.slice(8))]; return a ? { label: isTangible(a.id) ? qsTr("Wert heute") : qsTr("Kontostand"), cents: a.balance_cents } : null }
        if (sel.startsWith("group:")) { const g = groups.find(g => "group:" + g.id === sel); return g ? { label: qsTr("Summe"), cents: g.balance_cents } : null }
        if (sel.startsWith("bank:")) { const g = bankGroup(sel.slice(5)); return g ? { label: qsTr("Summe"), cents: g.balance_cents } : null }
        if (sel.startsWith("userGroup:")) { const g = userGroups.find(g => "userGroup:" + g.uuid === sel); return g ? { label: qsTr("Summe"), cents: g.balance_cents } : null }
        // The category total covers all accounts: no figure while accounts are filtered (#406).
        if (sel.startsWith("category:")) {
            if (categoryAccountIDs.length) return null
            const c = categories.find(c => "category:" + c.id === sel)
            return c ? { label: qsTr("Summe im Zeitraum"), cents: c.total_cents } : null
        }
        return null
    }
    readonly property string lastSyncText: {
        const latest = connections.reduce((m, c) => c.last_sync_unix !== null && c.last_sync_unix > m ? c.last_sync_unix : m, 0)
        return latest ? qsTr("Aktualisiert %1").arg(Fmt.relative(latest)) : qsTr("Noch nie aktualisiert")
    }
    readonly property string periodTitle: period === "custom"
        ? Fmt.medium(customPeriodFrom) + " – " + Fmt.medium(customPeriodTo) : periodTitleOf(period)
    /// Subtitle of the period-based KPI tiles.
    readonly property string periodSubtitle: period !== "sinceSalary" ? periodTitle
        : salaryCycle ? qsTr("Seit Gehalt am %1").arg(Fmt.medium(salaryCycle.last_date)) : qsTr("Monat (kein Gehalt)")
    readonly property string filterTitle: {
        if (directionFilter === "All") return qsTr("Status: %1").arg(titleOf(statusFilters, statusFilter))
        const kind = directionFilter === "Income" ? qsTr("Einnahmen") : qsTr("Ausgaben")
        return statusFilter === "all" ? kind : titleOf(statusFilters, statusFilter) + " · " + kind
    }
    readonly property bool showsAccountMarkers: {
        const sel = sidebarSelection
        if (sel.startsWith("account:")) return false
        if (sel.startsWith("group:")) { const g = groups.find(g => "group:" + g.id === sel); return g ? g.accounts.length > 1 : false }
        if (sel.startsWith("bank:")) { const g = bankGroup(sel.slice(5)); return g ? g.accounts.length > 1 : false }
        if (sel.startsWith("userGroup:")) return userGroupAccountIDs(sel.slice(10)).length > 1
        if (sel === "newBookings") return false // the card header names the account
        return Object.keys(accountMarkers).length > 1
    }
    function accountColor(id) { const m = accountMarkers[id]; return m ? m.color : Theme.tertiary }
    function accountSymbol(id) { const m = accountMarkers[id]; return m ? m.symbol : "building.columns" }
    function accountShortLabel(id) { const m = accountMarkers[id]; return m ? m.label : "" }
    function recurringSeries(b) {
        if (!b.recurrence_key) return null
        return recurring.find(s => s.base_key === b.recurrence_key
            && (!s.amount_specific || Math.abs(b.amount_cents - s.amount_cents) * 100 <= Math.abs(s.amount_cents) * 15)) || null
    }

    /// LaunchOptions --screenshot <path>: hides the demo banners (marketing shots).
    readonly property string screenshotPath: {
        const a = Qt.application.arguments, i = a.indexOf("--screenshot")
        return i >= 0 && i + 1 < a.length ? a[i + 1] : ""
    }
    readonly property bool screenshotMode: screenshotPath !== ""

    property TextEdit clipboardHelper: TextEdit { visible: false }
    function copyToClipboard(s) { clipboardHelper.text = s; clipboardHelper.selectAll(); clipboardHelper.copy() }
    /// Booking.clipboardLine (Formatting.swift): tab-separated for spreadsheets.
    function clipboardLine(b) {
        return [Fmt.medium(b.booking_date), b.counterparty || b.merchant_name, b.purpose, b.category_name, b.note, Fmt.clipboard(b.amount_cents)]
            .map(x => (x || "").replace(/[\t\n]/g, " ")).join("\t")
    }

    // MARK: selection / editing
    function selectBooking(id, toggle, extend, fromListClick) {
        const list = bookings.map(b => b.id)
        if (extend && selectedBookingID !== null) {
            const a = list.indexOf(selectedBookingID), b = list.indexOf(id)
            const ids = {}
            for (let i = Math.min(a, b); i <= Math.max(a, b); i++) ids[list[i]] = true
            selectedBookingIDs = ids
            selectedBookingID = id
        } else if (toggle) {
            const ids = Object.assign({}, selectedBookingIDs)
            if (ids[id]) { delete ids[id]; selectedBookingIDs = ids; if (selectedBookingID === id) selectedBookingID = list.find(x => ids[x]) ?? null }
            else { ids[id] = true; selectedBookingIDs = ids; selectedBookingID = id }
        } else {
            const ids = {}; ids[id] = true
            selectedBookingIDs = ids
            selectedBookingID = id
        }
        if (fromListClick && autoReviewDelay === "account" && !isMultiSelection && selectedBookingID === id) {
            const b = bookingByID(id)
            if (b && !b.checked) setChecked(true, b)
        }
    }
    /// Bearbeiten › Alles auswählen (Strg+A) in the booking list.
    function selectAllBookings() {
        if (!bookings.length || sidebarSelection === "documents") return
        const ids = {}
        for (const b of bookings) ids[b.id] = true
        selectedBookingIDs = ids
        if (selectedBookingID === null || !ids[selectedBookingID]) selectedBookingID = bookings[0].id
    }
    function moveSelection(delta) {
        if (!bookings.length) return
        const i = bookings.findIndex(b => b.id === selectedBookingID)
        const j = Math.max(0, Math.min(bookings.length - 1, i + delta))
        // a plain move replaces a multiple selection (also at the ends of the list)
        const ids = {}
        ids[bookings[j].id] = true
        selectedBookingIDs = ids
        selectedBookingID = bookings[j].id
    }
    /// Select the booking and put the cursor into its note field in the inspector.
    function editNote(b) {
        if (!b) return
        showInspector = true
        selectedBookingID = b.id
        pendingNoteFocus = b.id
    }
    /// Jump to `day` in the booking stream: that day, or the nearest earlier
    /// day with bookings (the balance of a quiet day comes from there).
    function jumpToDay(day) {
        const scroll = () => {
            const target = dayGroups.find(g => g.date <= day) || dayGroups[dayGroups.length - 1]
            if (target) scrollToDay(target.date)
        }
        // the list loads off the GUI thread: scroll once it reaches back to that day
        if (!loadBookingsThrough(day, scroll)) scroll()
    }
    /// Raises the list limit so the list reaches back to `day`; true when it reloads
    /// (`then` runs once the longer list is there).
    function loadBookingsThrough(day, then) {
        if (!isUnlocked || !page || bookings.length >= page.total_count) return false
        if (dayGroups.length && day >= dayGroups[dayGroups.length - 1].date) return false
        const f = currentFilter()
        if (f.date_from === null || f.date_from < day) f.date_from = day
        const newer = run(() => core("count_transactions", { filter: f }))
        if (newer === undefined) return false
        listLimit = Math.max(listLimit, newer + 200)
        reloadBookings(then)
        return true
    }
    function setChecked(on, b) {
        run(() => core("set_transaction_checked", { transaction_id: b.id, checked: on }))
        // With "Neu" / "Geprüft" or in „Neue Umsätze“ the booking leaves the list: keep it in the
        // inspector, and a just checked row in place until the selection moves (#470).
        if ((statusFilter !== "all" || sidebarSelection === "newBookings") && selectedBookingID === b.id) {
            stickyBooking = freshBooking(b)
            lingeringBookingID = on ? b.id : null
        }
        reloadBookings(); reloadAccounts()
    }
    /// The Buchung menu entry: nothing to do without new bookings, and not in
    /// Belege (there the list would silently mean every account).
    readonly property bool canMarkAllChecked: isUnlocked && newCount > 0 && sidebarSelection !== "documents"
    /// "Alle Buchungen als gelesen markieren": everything new in the list as filtered now, or –
    /// with `accountIDs`, from a sidebar entry – in those accounts regardless of
    /// period, search and scope. One SQL update off the GUI thread.
    function markAllChecked(accountIDs, quietWhenEmpty) {
        if (!isUnlocked) return
        if (!accountIDs && sidebarSelection === "documents") return
        if (accountIDs && !accountIDs.length) return
        const f = accountIDs
            ? { account_ids: accountIDs, include_subcategories: false, uncategorized_only: false,
                direction: "All", scope: "All" }
            : currentFilter()
        f.checked = false; f.limit = 0; f.offset = 0; f.whole_days = false
        const demo = demoActive
        coreAsync("set_checked_for_filter", { filter: f, checked: true }, ids => {
            if (!isUnlocked || demoActive !== demo) return
            if (!ids.length) {
                if (!quietWhenEmpty) toast = qsTr("Keine neuen Umsätze")
                return
            }
            UndoManager.register(qsTr("Alle Buchungen als gelesen markieren"), () => setCheckedIds(ids, false))
            toast = qsTr("%1 Umsätze als gelesen markiert").arg(ids.length)
            // the selected booking leaves a "Neu" list: keep it in the inspector (like setChecked)
            const b = selectedBooking
            if (statusFilter !== "all" && b && ids.indexOf(b.id) >= 0) stickyBooking = freshBooking(b)
            reloadBookings(); reloadAccounts()
        })
    }
    /// AppModel+Editing.swift setChecked(_:to:): undo / redo of markAllChecked for exactly the bookings it changed.
    function setCheckedIds(ids, checked) {
        if (!run(() => { core("set_checked_for_ids", { ids: ids, checked: checked }); return true })) return
        UndoManager.register(qsTr("Alle Buchungen als gelesen markieren"), () => setCheckedIds(ids, !checked))
        reloadBookings(); reloadAccounts()
    }
    function setCategory(b, categoryID) { setCategoryFor([b], categoryID) }
    function updateNote(b, note) {
        run(() => core("update_transaction_note", { transaction_id: b.id, note: note }))
        reloadBookings()
    }
    function updateText(b, counterparty, purpose) {
        run(() => core("update_transaction_text", { transaction_id: b.id, counterparty: counterparty, purpose: purpose }))
        reloadBookings()
    }
    function setSalaryFlag(b, flag) {
        run(() => core("set_booking_salary_flag", { booking_id: b.id, flag: flag }))
        // keep the edited booking selected even if it leaves the period
        if (selectedBookingID === b.id) stickyBooking = freshBooking(b)
        // „Seit Gehalt“ derives the period from the new cycle: everything that depends on it loads after it
        if (period === "sinceSalary") { reloadSelection(true, true); return }
        refreshSalaryCycle()
        reloadBookings()
    }
    /// `dates` (MM-DD) only for the interval "Custom".
    function markRecurring(b, interval, dates) {
        run(() => core("mark_booking_recurring", { booking_id: b.id, interval: interval, custom_dates: dates || [] }))
        refreshRecurring(); reloadBookings()
    }
    /// Back to automatic detection (also shows a hidden series again).
    function resetRecurring(seriesKey) { run(() => core("reset_recurring", { series_key: seriesKey })); refreshRecurring(); reloadBookings() }
    function ignoreRecurring(s) { run(() => core("ignore_recurring", { series_key: s.series_key })); refreshRecurring(); reloadBookings() }
    /// [CategoryNode].node(_:) – depth-first lookup.
    function categoryNode(id, nodes) {
        for (const n of (nodes || categoryTree)) {
            if (n.category.id === id) return n
            if (n.children) { const hit = categoryNode(id, n.children); if (hit) return hit }
        }
        return null
    }
    readonly property var selectedBookings: bookings.filter(b => !!selectedBookingIDs[b.id])
    readonly property bool isMultiSelection: Object.keys(selectedBookingIDs).length > 1
    readonly property var flowLabels: ({ Consumption: qsTr("Konsum"), Income: qsTr("Einnahme"), Investment: qsTr("Investment"),
                                         Perk: qsTr("Perk"), Transfer: qsTr("Umbuchung"), Loan: qsTr("Darlehen") })
    readonly property var intervalLabels: ({ Monthly: qsTr("Monatlich"), Quarterly: qsTr("Vierteljährlich"), HalfYearly: qsTr("Halbjährlich"),
                                             Yearly: qsTr("Jährlich"), Custom: qsTr("Individuell") })

    // MARK: - Settings (vault/settings.rs)
    property var settings: []
    function reloadSettings() { run(() => { settings = core("list_settings"); fintsProductIDSource = core("fints_product_id_source") }) }
    property string fintsProductIDSource: ""
    function settingEntry(key) { return settings.find(e => e.key === key) || null }
    function saveSetting(key, value) { run(() => core("set_setting", { key: key, value: value })); reloadSettings() }

    // MARK: - Bank catalog
    readonly property var bankCatalog: core("bank_catalog")
    function descriptor(kind) { return bankCatalog.find(d => d.kind === kind) || null }
    function isMagicDemoLogin(login) { return (login || "").trim().toLowerCase() === "demo" }
    /// Settings a connector still needs (`AppModel.missingSettings(for:)`).
    function missingSettings(c) {
        if (isMagicDemoLogin(c.login_name)) return []
        const d = descriptor(c.connector)
        if (!d) return []
        return userRequiredKeys(d.required_settings).filter(key => { const e = settingEntry(key); return !(e && e.has_value) })
    }
    /// Settings the core lists as required that are only an optional override for the user.
    /// A custom FinTS product ID is one: GiroCrab ships its own, and a value entered once is a
    /// global setting that covers every bank (AppModel+Connections.swift, #842/#865).
    readonly property var optionalSettingKeys: ["fints.product_id"]
    /// The ONLY place that turns `required_settings` into what the user must provide.
    /// Add-bank sheet, setup and the sidebar use it.
    function userRequiredKeys(requiredSettings) { return (requiredSettings || []).filter(k => optionalSettingKeys.indexOf(k) < 0) }
    /// The core's `MissingSetting` (parsed error) for a key that is only an optional override.
    function isMissingProductID(e) {
        if (e.kind !== "MissingSetting") return false
        const m = /key: "([^"]+)"/.exec(e.debug || "")
        return !!m && optionalSettingKeys.indexOf(m[1]) >= 0
    }
    /// The core could not resolve a FinTS registration (no embedded ID and no own one).
    /// Never a setup form: with an embedded ID the mode returns to `builtin` and the caller
    /// retries once; otherwise the user is offered the own-ID alternative in an alert.
    /// Returns true when a retry should follow.
    function recoverFromMissingProductID() {
        if (!fintsBuiltinAvailable || fintsProductIDMode === "builtin") return false
        // only a stored switch is worth a retry; a failed write stays visible as the error (null)
        try { core("set_fints_product_id_mode", { mode: "builtin" }) }
        catch (e) { errorMessage = e.message; return null }
        reloadSettings()
        reloadProductIDState()
        return true
    }
    function connectionStatus(id) { return run(() => core("connection_status", { connection_id: id })) || null }
    /// Amex / Amazon Visa / FNZ / Quirion / Schwäbisch Hall / EquatePlus / Amazon orders: embedded web-portal login.
    function usesPortal(c) { return usesPortalLogin(c) }   // MARK: Web logins

    // MARK: - Adding banks
    /// Adds a login at any FinTS 3.0 bank once the banking policy is fresh and allows it.
    function addFinTsConnection(name, blz, url, login, tanMedium) {
        withFreshBankingPolicy(() => {
            if (!isMagicDemoLogin(login) && !bankingAllowsAdding(bankingGateForKey("fints:" + (blz || "").trim()))) return
            addFinTsConnectionChecked(name, blz, url, login, tanMedium)
        })
    }
    function addConnection(kind, login) {
        withFreshBankingPolicy(() => {
            if (!isMagicDemoLogin(login) && !bankingAllowsAdding(bankingGateForNew(kind))) return
            addConnectionChecked(kind, login)
        })
    }
    function addEnableBankingConnection(aspsp, country, name, label) {
        withFreshBankingPolicy(() => {
            if (!isMagicDemoLogin(label) && !bankingAllowsAdding(bankingGateForNew("EnableBanking"))) return
            addEnableBankingConnectionChecked(aspsp, country, name, label)
        })
    }
    function addFinTsConnectionChecked(name, blz, url, login, tanMedium) {
        if (demoActive && hasOwnVault) useVault(false)
        const id = run(() => core("add_fints_connection", { blz: blz, url: url, display_name: name, login_name: login, tan_medium: tanMedium || "" }))
        afterAdd(id)
    }
    function addConnectionChecked(kind, login) {
        if (demoActive && hasOwnVault) useVault(false)
        const id = run(() => core("add_bank_connection", { connector: kind, display_name: "", login_name: login }))
        afterAdd(id)
    }
    function addEnableBankingConnectionChecked(aspsp, country, name, label) {
        if (demoActive && hasOwnVault) useVault(false)
        const id = run(() => core("add_enable_banking_connection", { aspsp: aspsp, country: country, display_name: name, login_name: label }))
        afterAdd(id)
    }
    /// Fixed catalog entry "Demo-Bank (Test & Vorschau)".
    function addDemoBank() { addFinTsConnection("Demo-Bank", "99999999", "https://demo.invalid/fints", "demo") }
    function afterAdd(id) {
        reloadSettings()
        reloadAccounts(() => {
            const conn = connections.find(c => c.id === id)
            if (conn) { toast = qsTr("%1 hinzugefügt").arg(conn.display_name); requestSync(conn) }
        })
    }
    function removeConnection(id) {
        // Its browser session goes too: cookies in the profile and in the vault (MARK: Web logins)
        if (webLoginConnection && webLoginConnection.id === id) webLoginConnection = null
        if ((connections.find(c => c.id === id) || {}).connector === "TradeRepublic") clearTradeRepublicWebData(false, id)
        forgetWebCookies(id)
        run(() => core("delete_bank_connection", { connection_id: id })); reloadAll()
    }
    /// comdirect: switch the login between FinTS and comdirect's own API (keeps accounts, categories, notes).
    function switchComdirectAccess(id) { run(() => core("switch_comdirect_access", { connection_id: id })); reloadAll() }
    function updateConnection(id, name, login) { run(() => core("update_bank_connection", { connection_id: id, display_name: name, login_name: login })); reloadAccounts() }
    function setConnectionPin(id, pin) { run(() => core("set_connection_pin", { connection_id: id, pin: pin })) }
    /// Enable Banking application of one login (#824): its own, else the default one.
    function connectionEnableBankingApp(id) { try { return core("connection_enable_banking_app", { connection_id: id }) || null } catch (e) { return null } }
    function connectionEnableBankingKey(id) { try { return core("connection_enable_banking_key", { connection_id: id }) || "" } catch (e) { return "" } }
    /// Stores the application of this login only; the default and all other logins keep theirs.
    function setConnectionEnableBankingApp(id, applicationID, pem) {
        const changed = run(() => core("set_connection_enable_banking_app", { connection_id: id, application_id: applicationID, private_key_pem: pem }))
        if (changed) toast = qsTr("Anwendung für diesen Bankzugang gespeichert. Beim nächsten Abruf ist eine neue Zustimmung nötig.")
    }

    // MARK: - Sync (AppModel.swift › requestSync … finishSync)
    property var syncingConnectionID: null
    /// The bank dialog in progress (session handle of src/sessions.rs).
    property var activeSession: null
    /// { connection, challenge, waiting, busy, error }
    property var activeChallenge: null
    property var pendingLogin: null          // BankConnection asking for the PIN
    property var setupRequest: null          // { connection, keys }
    property var webLoginConnection: null    // portal login (QtWebEngine)
    /// The running sync was started by the schedule (AutoSync.qml): no login prompts, no error dialogs.
    property var quietSyncConnectionID: null
    property var syncQueue: []
    property int syncBatchTotal: 0
    property var syncBatchSkipped: []
    property int syncGeneration: 0
    readonly property var syncBatchProgress: syncBatchTotal > 1 ? { current: syncBatchTotal - syncQueue.length, total: syncBatchTotal } : null
    readonly property bool canSkipInSyncBatch: syncQueue.length > 0
    property Timer pollTimer: Timer { property var connection; onTriggered: model.pollSession(connection) }

    function requestSync(connection) {
        withFreshBankingPolicy(() => {
            const gate = bankingGate(connection)
            if (gate) {
                statementsAccountID = null
                postboxConnectionID = null
                if (syncBatchTotal > 1 && !gate.all) {
                    syncBatchSkipped = syncBatchSkipped.concat([qsTr("%1 (Sicherheitssperre)").arg(connection.display_name)])
                    syncNextInQueue()
                } else {
                    resetSyncBatch()
                    showBankingGate(gate)
                }
                return
            }
            requestSyncChecked(connection)
        })
    }
    function requestSyncChecked(connection) {
        if (usesPortal(connection)) { webLoginConnection = connection; return }
        const magicDemo = isMagicDemoLogin(connection.login_name)
        if (!isDemo && !magicDemo && connection.connector === "TradeRepublic") {
            const st = connectionStatus(connection.id)
            if (!(st && st.has_session)) { webLoginConnection = connection; return }
        }
        if (!isDemo) {
            const st = connectionStatus(connection.id)
            const missing = missingSettings(connection)
            if (st && (st.has_pin || (["TradeRepublic", "Kontist"].indexOf(connection.connector) >= 0 && st.has_session)) && missing.length === 0) {
                startSync(connection, ""); return
            }
            if (missing.length) { setupRequest = { connection: connection, keys: missing }; return }
        }
        const d = descriptor(connection.connector)
        const needsSecret = !(d && d.secret_label.startsWith("("))
        if (isDemo || !needsSecret) startSync(connection, isDemo ? "demo" : "")
        else pendingLogin = connection
    }
    function requestSyncAll() {
        requestSyncBatch(connections.filter(c => !usesPortal(c) && !c.all_accounts_archived))
    }
    function requestSyncBatch(batch) {
        if (syncingConnectionID !== null || !batch.length) return
        refreshAssetPrices(false)
        withFreshBankingPolicy(() => {
            // a sync may have started while the policy was checked
            if (syncingConnectionID !== null) return
            // Logins the banking policy blocks are skipped and named at the end.
            const blocked = batch.filter(c => bankingGate(c) !== null)
            const allowed = batch.filter(c => bankingGate(c) === null)
            if (!allowed.length) {
                if (blocked.length) showBankingGate(bankingGate(blocked[0]))
                return
            }
            syncQueue = allowed.slice(1)
            syncBatchTotal = allowed.length
            syncBatchSkipped = blocked.map(c => qsTr("%1 (Sicherheitssperre)").arg(c.display_name))
            requestSyncChecked(allowed[0])
        })
    }
    function resetSyncBatch() { syncQueue = []; syncBatchTotal = 0; syncBatchSkipped = [] }
    function syncNextInQueue() {
        if (syncQueue.length) {
            const next = syncQueue[0]
            syncQueue = syncQueue.slice(1)
            requestSync(next)   // re-checks the banking policy for every login
            return
        }
        if (syncBatchSkipped.length) {
            const note = qsTr("Übersprungen: %1").arg(syncBatchSkipped.join(", "))
            toast = toast ? toast + " · " + note : note
        }
        resetSyncBatch()
    }
    function startSync(connection, secret) {
        if (usesPortal(connection)) { webLoginConnection = connection; return }
        // a manual sync never overwrites the running automatic session of another login
        if (quietSyncConnectionID !== null && quietSyncConnectionID !== connection.id) return
        // a manual sync of the same login while its automatic one runs: cancel that one, this one is not quiet
        if (quietSyncConnectionID === connection.id && syncingConnectionID === connection.id) {
            quietSyncConnectionID = null
            const oldSession = activeSession
            activeSession = null
            if (oldSession !== null) coreAsync("session_cancel", { session: oldSession }, null, () => {})
        }
        pendingLogin = null
        syncingConnectionID = connection.id
        const gen = ++syncGeneration
        // Statements only for an account of exactly this login: a stale value
        // would hand this login's PIN to another bank.
        if (statementsAccountID !== null && (!accountsByID[statementsAccountID] || accountsByID[statementsAccountID].connection_id !== connection.id)
            || quietSyncConnectionID === connection.id)
            statementsAccountID = null
        const statementsFor = statementsAccountID
        // "PostBox jetzt abrufen" (#838) only for exactly this login, never in an automatic sync
        if (postboxConnectionID !== connection.id || quietSyncConnectionID === connection.id) postboxConnectionID = null
        const postboxNow = postboxConnectionID !== null
        const quiet = quietSyncConnectionID === connection.id
        // Kept for the "GiroCrab-Registrierung" retry of a rejected product ID, so the
        // login is not asked again; finishSync takes it and clears it.
        const keepSecret = () => { failedSyncSecret = secret }
        const startSession = id => {
            activeSession = id
            coreAsync("session_start", { session: id }, step => {
                if (activeSession !== id || gen !== syncGeneration) return   // skipped / cancelled meanwhile
                handleStep(step, connection)
            }, err => { if (activeSession === id) { keepSecret(); finishSync(err, connection) } })
        }
        if (quiet) {
            // no new approval in the background (e.g. the comdirect PostBox, #838); prepared
            // off the GUI thread like the rest of the automatic fetch
            coreAsync("begin_quiet_sync", { connection_id: connection.id, secret: secret, demo: isDemo }, id => {
                if (gen !== syncGeneration) { coreAsync("session_cancel", { session: id }, null, () => {}); return }
                startSession(id)
            }, err => { if (gen === syncGeneration) finishSync(err, connection) })
            return
        }
        let id
        try {
            id = statementsFor !== null
                ? core("fetch_account_statements", { account_id: statementsFor, secret: secret, demo: isDemo })
                : postboxNow
                ? core("fetch_comdirect_postbox", { connection_id: connection.id, secret: secret, demo: isDemo })
                : core("begin_sync", { connection_id: connection.id, secret: secret, demo: isDemo })
        } catch (e) {
            keepSecret()
            finishSync(e.message, connection)
            return
        }
        startSession(id)
    }
    function handleStep(step, connection) {
        // A scheduled sync never asks for a login: the bank wants a new
        // consent, so stop quietly – the connection view shows it.
        if (quietSyncConnectionID === connection.id && (step.NeedsChallenge || step.StillWaiting)) {
            logSync(connection.id, "Cancelled", qsTr("Automatischer Abruf übersprungen: Die Bank verlangt eine neue Freigabe."), 0, "Automatic")
            quietSyncConnectionID = null
            const s = activeSession
            activeSession = null
            // The next login may be the same bank: go on only once this bank dialog has ended
            // (as skipCurrentSync); a newer sync meanwhile owns the state.
            const gen = ++syncGeneration
            const next = () => {
                if (gen !== syncGeneration) return
                syncingConnectionID = null
                syncNextInQueue()   // never leave a queued login waiting behind a skipped automatic fetch
            }
            if (s !== null) coreAsync("session_cancel", { session: s }, next, next)
            else next()
            return
        }
        if (step.NeedsChallenge) {
            const c = step.NeedsChallenge.challenge
            activeChallenge = { connection: connection, challenge: c, waiting: false, busy: false, error: "" }
            if (c.kind === "DecoupledPush" || c.kind === "Progress") schedulePoll(connection, c.poll_interval_seconds)
        } else if (step.StillWaiting) {
            const c = step.StillWaiting.challenge
            activeChallenge = activeChallenge === null
                ? { connection: connection, challenge: c, waiting: true, busy: false, error: "" }
                : Object.assign({}, activeChallenge, { challenge: c, waiting: true, busy: false })
            schedulePoll(connection, c.poll_interval_seconds)
        } else if (step.Finished) {
            const s = step.Finished.summary
            logSync(connection.id, "Ok", "", s.transactions_imported, syncLogSource(connection.id))
            activeChallenge = null
            syncingConnectionID = null
            activeSession = null
            if (statementsAccountID !== null) {
                const n = Math.max(0, accountDocuments(statementsAccountID).length - statementsCountBefore)
                statementsAccountID = null
                postboxConnectionID = null
                documentsVersion++
                toast = qsTr("%1: %2 neue Kontoauszüge").arg(connection.display_name).arg(n)
            } else if (postboxConnectionID === connection.id) {
                postboxConnectionID = null
                documentsVersion++
                toast = syncNotice(connection, s) + " · " + postboxNote(s)
            } else {
                // scheduled syncs only speak up when something changed
                if (quietSyncConnectionID !== connection.id || s.transactions_imported > 0 || s.new_accounts.length || s.revoked_accounts.length) {
                    toast = syncNotice(connection, s)
                    if (s.postbox_documents > 0) toast += " · " + postboxNote(s)
                }
                if (s.postbox_documents > 0) documentsVersion++
            }
            quietSyncConnectionID = null
            reloadAll()
            checkPendingAccountSelection()
            syncNextInQueue()
        }
    }
    /// Login with accounts that another login already delivers: asks which to fetch (#346).
    property var accountSelectionConnection: null
    /// Logins whose choice was put off with „Später“ in this session (not asked again).
    property var postponedAccountSelection: ({})
    /// Opens the account choice for the first login whose new accounts overlap with
    /// another login (#346). No-op while a choice is already shown.
    function checkPendingAccountSelection() {
        if (accountSelectionConnection !== null) return
        coreAsync("pending_account_selection_connections", {}, ids => {
            const id = ids.find(i => !postponedAccountSelection[i])
            if (id === undefined || accountSelectionConnection !== null) return
            accountSelectionConnection = connections.find(c => c.id === id) || null
        }, () => {})
    }
    function syncNotice(connection, s) {
        const parts = [qsTr("%1: %2 neue Buchungen, %3 kategorisiert").arg(connection.display_name).arg(s.transactions_imported).arg(s.transactions_categorized)]
        if (s.new_accounts.length) parts.push(qsTr("Neue Konten: %1").arg(s.new_accounts.join(", ")))
        if (s.revoked_accounts.length) parts.push(qsTr("Kein Zugriff mehr: %1").arg(s.revoked_accounts.join(", ")))
        let text = parts.join(" · ")
        if (s.transactions_skipped_archived) text += " · " + qsTr("%1 Buchungen archivierter Konten übersprungen").arg(s.transactions_skipped_archived)
        return text
    }
    function schedulePoll(connection, interval) {
        pollTimer.stop()
        pollTimer.connection = connection
        pollTimer.interval = Math.max(interval || 1, 1) * 1000
        pollTimer.start()
    }
    function pollSession(connection) {
        if (activeTransfer !== null) { pollTransfer(connection); return }
        const id = activeSession
        if (id === null) return
        coreAsync("session_poll", { session: id }, step => { if (activeSession === id) handleStep(step, connection) },
                  err => { if (activeSession === id) finishSync(err, connection) })
    }
    function submitCode(code) {
        if (activeTransfer !== null && activeChallenge) { submitTransferCode(code); return }
        const id = activeSession, ch = activeChallenge
        if (id === null || !ch) return
        activeChallenge = Object.assign({}, ch, { busy: true, error: "" })
        coreAsync("session_submit", { session: id, code: code }, step => { if (activeSession === id) handleStep(step, ch.connection) },
                  err => { if (activeSession === id) finishSync(err, ch.connection) })
    }
    /// Cancel button of the TAN sheet: in a batch drop this login and go on.
    function skipCurrentSync() {
        if (syncBatchTotal <= 1 || activeTransfer !== null) { cancelSync(); return }
        const current = connections.find(c => c.id === syncingConnectionID)
        const name = activeChallenge ? activeChallenge.connection.display_name : (current ? current.display_name : "")
        if (name) syncBatchSkipped = syncBatchSkipped.concat([name])
        pollTimer.stop()
        const s = activeSession
        activeSession = null; activeChallenge = null
        statementsAccountID = null
        postboxConnectionID = null
        const gen = ++syncGeneration
        // End the bank dialog first (the next login may be the same bank); syncingConnectionID
        // stays set until then. "Alle abbrechen", a lock or a new sync meanwhile own the state.
        const next = () => {
            if (gen !== syncGeneration) return
            syncingConnectionID = null
            syncNextInQueue()
        }
        if (s === null) { next(); return }
        coreAsync("session_cancel", { session: s }, next, next)
    }
    function cancelAllSync() { setupRequest = null; webLoginConnection = null; cancelSync() }
    function cancelSync() {
        if (activeTransfer !== null) { cancelTransfer(); return }
        pollTimer.stop()
        const s = activeSession
        if (s !== null && syncingConnectionID !== null)
            logSync(syncingConnectionID, "Cancelled", qsTr("Vom Benutzer abgebrochen"), 0, syncLogSource())
        activeSession = null; activeChallenge = null; pendingLogin = null; syncingConnectionID = null
        quietSyncConnectionID = null   // a cancelled automatic fetch must not make the next sync quiet
        statementsAccountID = null
        postboxConnectionID = null
        syncGeneration++
        resetSyncBatch()
        if (s !== null) coreAsync("session_cancel", { session: s }, null, () => {})
    }
    function cancelPendingLogin() { const c = pendingLogin; pendingLogin = null; skipPrompt(c) }
    function cancelSetupRequest() { const r = setupRequest; setupRequest = null; skipPrompt(r ? r.connection : null) }
    function cancelWebLogin() { const c = webLoginConnection; webLoginConnection = null; skipPrompt(c) }
    function skipPrompt(connection) {
        // A cancelled "Kontoauszüge abrufen" must never carry over to the next sync:
        // it would send that login's PIN for another bank's account.
        statementsAccountID = null
        postboxConnectionID = null
        if (syncBatchTotal <= 1) { resetSyncBatch(); return }
        if (connection) syncBatchSkipped = syncBatchSkipped.concat([connection.display_name])
        syncNextInQueue()
    }
    /// "Speichern & abrufen" in the setup sheet.
    function completeSetup(request, values) {
        for (const k in values) if ((values[k] || "").trim() !== "") run(() => core("set_setting", { key: k, value: values[k] }))
        reloadSettings()
        setupRequest = null
        const c = connections.find(x => x.id === request.connection.id) || request.connection
        requestSync(c)
    }
    /// Errors of a step: `\u0001{json}` keeps the CoreError kind (see dispatch.rs).
    function parseCoreError(err) {
        if (typeof err === "string" && err.charCodeAt(0) === 1) { try { return JSON.parse(err.slice(1)) } catch (e) {} }
        return { kind: "", message: String(err) }
    }
    property var errorRetry: null
    /// Extra actions of the error alert ([{ title, action }]) after the bank rejected the
    /// FinTS registration in use: replace the own one, or switch to the other one.
    property var errorAlternatives: []
    /// PIN of the sync that just failed (memory only, cleared in finishSync).
    property string failedSyncSecret: ""
    function finishSync(err, connection) {
        const e = parseCoreError(err)
        logSync(connection.id, "Error", e.message, 0, syncLogSource(connection.id))   // source before statementsAccountID is cleared
        const retrySecret = failedSyncSecret
        failedSyncSecret = ""
        // a „Kontoauszüge abrufen“ that failed stays one in the registration retry below
        const retryStatements = statementsAccountID
        if (quietSyncConnectionID === connection.id) {
            // scheduled sync: a notice instead of dialogs / login prompts
            quietSyncConnectionID = null
            pollTimer.stop()
            activeSession = null; activeChallenge = null; syncingConnectionID = null
            const name = connection.display_name || ""
            toast = qsTr("%1: automatischer Abruf fehlgeschlagen – %2").arg(name).arg(e.message)
            statementsAccountID = null
            postboxConnectionID = null
            syncNextInQueue()   // the queue goes on (or is reset) like after a finished sync
            return
        }
        statementsAccountID = null
        postboxConnectionID = null
        pollTimer.stop()
        activeSession = null; activeChallenge = null; syncingConnectionID = null
        const missingID = isMissingProductID(e)
        // Fall back to GiroCrab's registration (once) and retry with the same login; the batch stays
        const recovered = missingID ? recoverFromMissingProductID() : false
        if (recovered === true) {
            const c = connections.find(x => x.id === connection.id)
            if (c) { statementsAccountID = retryStatements; startSync(c, retrySecret) }
            else syncNextInQueue()
            return
        }
        // the switch could not be stored: its error stays the message, no retry
        if (recovered === null) {
            resetSyncBatch()
            errorTitle = qsTr("Abruf von „%1“ fehlgeschlagen").arg(connection.display_name)
            return
        }
        resetSyncBatch()
        if (e.kind === "BrowserLoginRequired") { toast = e.message; webLoginConnection = connection; return }
        if (missingID) {
            // Never a setup form for the optional FinTS registration: it would be an empty,
            // seemingly mandatory field. Offer the own ID instead.
            errorTitle = qsTr("Abruf von „%1“ fehlgeschlagen").arg(connection.display_name)
            errorRetry = null
            errorAlternatives = [{ title: qsTr("Eigene Registrierung eintragen …"), action: enterOwnProductID }]
            errorMessage = qsTr("Diese Version enthält keine FinTS-Registrierung von GiroCrab. Tragen Sie Ihre eigene Registrierung ein.")
            return
        }
        if (e.kind === "MissingSetting") {
            const keys = missingSettings(connection)
            const m = /key: "([^"]+)"/.exec(e.debug || "")
            if (m && keys.indexOf(m[1]) < 0) keys.unshift(m[1])
            setupRequest = { connection: connection, keys: keys }
            return
        }
        // Kontist on its stored refresh token alone (no password given, none stored) whose
        // token expired: ask for the password instead of showing an error.
        if (e.kind === "Authentication" && retrySecret === "" && connection.connector === "Kontist") {
            const st = connectionStatus(connection.id)
            if (!(st && st.has_pin)) { toast = e.message; pendingLogin = connection; return }
        }
        errorTitle = qsTr("Abruf von „%1“ fehlgeschlagen").arg(connection.display_name)
        errorRetry = () => { const c = connections.find(x => x.id === connection.id); if (c) requestSync(c) }
        errorAlternatives = productIDAlternatives(connection, e, retrySecret)
        errorMessage = e.message
    }
    /// The bank refused the FinTS product registration (9078, or 9010 as some banks answer).
    /// The core reports the FIRST code of the reply, so both are also looked for in the message.
    function isProductIDRejection(e) { return e.kind === "Bank" && /9010|9078/.test((e.debug || "") + " " + (e.message || "")) }
    /// "Mit … erneut versuchen" after a bank rejected the FinTS registration in use.
    function productIDAlternatives(conn, e, secret) {
        const d = descriptor(conn.connector)
        if (!isProductIDRejection(e) || !d || d.required_settings.indexOf("fints.product_id") < 0) return []
        const own = settingEntry("fints.product_id")
        const hasOwn = !!(own && own.has_value)
        const out = []
        if (fintsProductIDSource === "builtin") {
            // GiroCrab's registration was refused: the own one goes into Settings, then the account is fetched again by hand.
            out.push({ title: hasOwn ? qsTr("Eigene Registrierung ersetzen …") : qsTr("Eigene Registrierung eintragen …"), action: enterOwnProductID })
        } else if (fintsProductIDSource === "own") {
            out.push({ title: qsTr("Eigene Registrierung ersetzen …"), action: enterOwnProductID })
            if (fintsBuiltinAvailable) {
                out.push({ title: qsTr("Mit GiroCrab-Registrierung erneut versuchen"), action: () => {
                    setFintsProductIDMode("builtin")
                    const c = connections.find(x => x.id === conn.id)
                    if (!c) return
                    // Same login and PIN as the failed attempt: only the registration changes.
                    if (secret !== "" && bankingGate(c) === null) startSync(c, secret)
                    else requestSync(c)
                } })
            }
        }
        return out
    }
    /// After a rejected registration: switch to the own one, open Settings › Banken (FinTS
    /// registration) and put the cursor into its field (no extra field in the alert).
    function enterOwnProductID() {
        // Mode `own` only once an ID is stored (OwnProductIDField.commit), or an existing one is replaced:
        // an empty field must not leave the user in a mode without a registration.
        const own = settingEntry("fints.product_id")
        if (own && own.has_value) setFintsProductIDMode("own")
        showOwnProductID = true
        focusOwnProductID = true
        openSettings("banks")
    }
    /// OwnProductIDField takes the focus once (and clears this) when it appears.
    property bool focusOwnProductID: false
    /// The bank rejected the registration in use: the own-ID field shows up even in builds
    /// with a built-in one, so that one can be replaced.
    property bool showOwnProductID: false

    /// Enable Banking redirect (ConsentRedirect.defaultURL unless configured).
    function consentRedirectURL() {
        const e = settingEntry("enablebanking.redirect_url")
        const v = e ? (e.value || "").trim() : ""
        return v || "https://localhost/girocrab/callback"
    }

    // MARK: - Depot, loans, investment dashboard
    property var dashboard: null
    /// The investment dashboard reads every booking: off the GUI thread.
    function reloadDashboard() {
        if (!isUnlocked) return
        loadOffMain("dashboard", () => [["investment_dashboard", { months: 6 }]], r => { dashboard = r[0] })
    }
    function holdings(accountID) { return run(() => core("list_holdings", { account_id: accountID })) || [] }
    function setAccountInitialBalance(accountID, cents) {
        run(() => core("set_account_initial_balance", { account_id: accountID, cents: cents }))
        reloadAccounts()
    }
    function isDepot(a) { return a !== null && (a.kind === "Brokerage" || a.name.toLowerCase().indexOf("depot") >= 0) }

    // MARK: - Vermögen (WealthView.swift) and Sachwerte (AppModel+Assets.swift)
    /// WealthScope: segmented control of the wealth view, in the toolbar instead of the booking scope.
    property string wealthScope: "all"
    readonly property var wealthScopes: [
        { id: "all", title: qsTr("Alles") }, { id: "cash", title: qsTr("Cash") },
        { id: "securities", title: qsTr("Wertpapiere") }, { id: "tangibles", title: qsTr("Sachwerte") }
    ]
    /// AssetStore: ids of the Sachwerte accounts (as a set) and their items per account id.
    property var tangibleAccountIDs: ({})
    property var assetItemsByAccount: ({})
    /// AppModel.reloadAssets: one core pass for all accounts (prices and points are loaded once),
    /// Loaded with the accounts (`reloadAccounts`): `list_tangible_account_ids`, `list_all_asset_items`.
    function applyAssets(tangibleIDs, allItems) {
        const ids = {}, items = {}
        for (const id of tangibleIDs) { ids[id] = true; items[id] = [] }
        for (const it of allItems) (items[it.account_id] || (items[it.account_id] = [])).push(it)
        tangibleAccountIDs = ids
        assetItemsByAccount = items
        reloadSelectedAssetHistory()
    }
    function isTangible(accountID) { return !!tangibleAccountIDs[accountID] }
    function assetItems(accountID) { return assetItemsByAccount[accountID] || [] }
    function assetItem(itemID) {
        for (const id in assetItemsByAccount) { const it = assetItemsByAccount[id].find(x => x.id === itemID); if (it) return it }
        return null
    }
    /// AssetEditorRequest { accountId, itemId (null = new item) } on screen, or null.
    property var assetEditor: null
    /// AssetActionRequest { itemId, kind: "values" | "quantities" | "sell" } on screen, or null.
    property var assetAction: null
    property bool assetRefreshing: false
    /// The item whose own value chart the header shows, and that chart.
    property var selectedAssetItemID: null
    property var selectedAssetHistory: []
    /// Click on an item: its own value chart in the header; again = back to the account.
    function selectAssetItem(id) {
        selectedAssetItemID = selectedAssetItemID === id ? null : id
        reloadSelectedAssetHistory()
    }
    /// Back to the whole account (no-op without a selection).
    function deselectAssetItem() {
        if (selectedAssetItemID === null) return
        selectedAssetItemID = null
        reloadSelectedAssetHistory()
    }
    /// The selected item, if it belongs to `accountID`.
    function selectedAssetItem(accountID) {
        const it = selectedAssetItemID !== null ? assetItem(selectedAssetItemID) : null
        return it && it.account_id === accountID ? it : null
    }
    function reloadSelectedAssetHistory() {
        if (selectedAssetItemID === null || !isUnlocked || assetItem(selectedAssetItemID) === null) {
            selectedAssetItemID = null
            selectedAssetHistory = []
            return
        }
        const id = selectedAssetItemID
        run(() => { selectedAssetHistory = core("asset_item_history", { item_id: id }) })
    }
    /// DepotPositionsView: another account ends the item selection.
    property Connections assetSelectionReset: Connections {
        target: model
        function onSidebarSelectionChanged() { if (model.selectedAssetItemID !== null) model.selectAssetItem(null) }
    }
    function openAssetAction(kind, itemID) { assetAction = { itemId: itemID, kind: kind } }
    function openAssetEditor(accountID, itemID) { assetEditor = { accountId: accountID, itemId: itemID === undefined ? null : itemID } }
    /// „Neues Sachwerte-Konto …“: creates the account and selects it; its empty view
    /// offers „Sachwert hinzufügen …“.
    function createTangibleAccount() {
        const id = run(() => core("create_tangible_account", { name: "" }))
        if (id === undefined) return
        reloadAccounts()
        sidebarSelection = "account:" + id
    }
    /// Runs a Sachwerte change (throws on a core error) and refreshes everything that shows its value.
    function changeAssets(method, args) {
        core(method, args)
        reloadAccounts(); reloadHistory()
    }
    /// Price (EUR per troy ounce) of `symbol` on `date` or the last day before.
    function assetPrice(symbol, date) {
        try { const p = core("asset_price_on", { symbol: symbol, date: date }); return p ? p.value_cents : null } catch (e) { return null }
    }
    /// Value of one piece of an item on `date` (gold: price × fine weight).
    function assetUnitValue(itemID, date) {
        try { const v = core("asset_unit_value_on", { item_id: itemID, date: date }); return v === undefined ? null : v } catch (e) { return null }
    }
    /// Fetches missing gold prices (no request without automatic items), off the GUI thread.
    /// `userInitiated` shows the outcome; the automatic runs stay quiet.
    function refreshAssetPrices(userInitiated) {
        if (!isUnlocked || assetRefreshing) return
        assetRefreshing = true
        coreAsync("refresh_asset_prices", { force: !!userInitiated }, r => {
            assetRefreshing = false
            if (r.requested) { reloadAccounts(); reloadHistory() }
            if (!userInitiated) return
            if (r.error !== null) toast = qsTr("Goldpreis konnte nicht geladen werden: %1").arg(r.error)
            else if (r.requested) toast = qsTr("Goldpreis aktualisiert")
        }, err => {
            assetRefreshing = false
            if (userInitiated) toast = parseCoreError(err).message
        })
    }

    // MARK: - Manual loans (ManualLoansUI.swift) and Startsaldo (OpeningBalanceUI.swift)
    // A manual loan has no account at the bank: the core keeps a Stand (balance at the
    // end of a date), an optional interest rate and a search rule; bookings are linked
    // by the rule or by hand. Assigning, saving and creating are undoable (UndoManager).

    /// ManualLoanStore: [ManualLoan] and transaction id -> LoanLink, refreshed with the accounts.
    property var loans: []
    property var loanLinks: ({})
    /// LoanEditorRequest { mode: "create" | "edit" | "anchor", accountId, draft } or null.
    property var loanEditor: null
    /// OpeningBalanceRequest { accountId } or null.
    property var openingBalanceRequest: null
    property int loanGeneration: 0
    function indexLoanLinks(links) {
        const m = {}
        for (const l of links) if (m[l.transaction_id] === undefined) m[l.transaction_id] = l
        return m
    }
    /// Off the GUI thread (both calls walk every loan's bookings); the last call wins.
    function reloadLoans() {
        if (!isUnlocked) return
        const gen = ++loanGeneration
        coreAsync("list_manual_loans", {}, l => {
            if (gen !== loanGeneration) return
            coreAsync("list_loan_links", {}, links => {
                if (gen !== loanGeneration) return
                loans = l
                loanLinks = indexLoanLinks(links)
            })
        })
    }
    /// Synchronous reload after a loan edit; cancels a running background reload.
    function reloadLoansNow() {
        ++loanGeneration
        if (!isUnlocked) return
        run(() => {
            loans = core("list_manual_loans")
            loanLinks = indexLoanLinks(core("list_loan_links"))
        })
    }
    function manualLoan(accountId) { return loans.find(l => l.account_id === accountId) || null }
    /// Manual loans that bookings can be assigned to (not archived).
    readonly property var assignableLoans: loans.filter(l => { const a = accountsByID[l.account_id]; return a ? a.archived_unix === null : false })
    function loanLink(b) { return b ? (loanLinks[b.id] || null) : null }
    /// "Darlehen zuordnen" applies: no loan booking, and an outflow or already linked (#429).
    function loanAssignable(b) { return !!b && b.account_kind !== "Loan" && (b.amount_cents < 0 || loanLink(b) !== null) }
    /// Links (or with `null` unlinks) a booking; undoable via Bearbeiten › Rückgängig.
    function assignLoan(loanId, b) {
        const link = loanLink(b)
        const previous = link ? link.loan_account_id : null
        if (previous === loanId) return
        if (run(() => { core("set_booking_loan", { transaction_id: b.id, loan_account_id: loanId }); return true }) === undefined) return
        UndoManager.register(qsTr("Darlehenszuordnung"), () => assignLoan(previous, b))
        const name = b.merchant.name === "" ? b.counterparty : b.merchant.name
        const loan = loanId !== null ? manualLoan(loanId) : null
        toast = loan ? qsTr("„%1“ dem Darlehen „%2“ zugeordnet").arg(name).arg(loan.setup.name)
                     : qsTr("Zuordnung von „%1“ aufgehoben").arg(name)
        reloadAccounts(); reloadLoansNow(); reloadBookings()
    }
    /// Creates or changes a manual loan ("Stand neu setzen" too); returns the account id.
    /// Throws on failure. Undoable via Bearbeiten › Rückgängig.
    function saveLoan(setup, accountId) {
        let id = accountId
        if (accountId !== null) {
            const known = manualLoan(accountId)
            const old = known ? known.setup : null
            core("update_manual_loan", { account_id: accountId, setup: setup })
            if (old) UndoManager.register(qsTr("Darlehen ändern"), () => run(() => saveLoan(old, accountId)))
        } else {
            id = core("create_manual_loan", { setup: setup })
            UndoManager.register(qsTr("Darlehen anlegen"), () => removeCreatedLoan(id, setup))
        }
        reloadAccounts(); reloadLoansNow(); reloadBookings(); reloadHistory()
        return id
    }
    /// Undo of "Neues Darlehen": removes the loan again (redo creates it anew).
    function removeCreatedLoan(id, setup) {
        if (run(() => {
            core("set_account_archived", { account_id: id, archived: true })
            core("delete_account", { account_id: id })
            return true
        }) === undefined) return
        if (sidebarSelection === "account:" + id) sidebarSelection = "all"
        UndoManager.register(qsTr("Darlehen anlegen"), () => {
            const created = run(() => saveLoan(setup, null))
            if (created !== undefined) sidebarSelection = "account:" + created
        })
        reloadAccounts(); reloadLoansNow(); reloadBookings(); reloadHistory()
    }
    function openLoanEditor(mode, accountId, draft) {
        loanEditor = { mode: mode, accountId: accountId === undefined ? null : accountId, draft: draft || null }
    }
    /// "Neues Darlehen aus Buchung …": name and rule taken from the counterparty.
    function openLoanEditorFromBooking(b) {
        const name = b.counterparty === "" ? b.merchant.name : b.counterparty
        const quoted = name.replace(/"/g, "")
        const d = Fmt.date(b.booking_date)
        if (d) d.setDate(d.getDate() - 1)
        openLoanEditor("create", null, {
            name: name, original_cents: null,
            anchor_date: Fmt.iso(d || new Date()),
            anchor_balance_cents: 0, interest_rate_ppm: null,
            match_expr: quoted === "" ? "" : "name:\"" + quoted + "\""
        })
    }
    /// ManualLoanAccountMenu: "Darlehen bearbeiten …" / "Stand neu setzen …" apply.
    function isEditableManualLoan(a) { return !!a && a.kind === "Loan" && manualLoan(a.id) !== null && a.archived_unix === null }

    /// OpeningBalanceMenuItem: "Startsaldo …" applies to computed balances only.
    function canSetOpeningBalance(a) { return !!a && a.balance_derived && a.archived_unix === null }
    function openOpeningBalance(a) { openingBalanceRequest = { accountId: a.id } }
    /// Sets (or clears with `null`) the Startsaldo; `date` "" = all bookings. Throws on failure.
    /// Undoable via Bearbeiten › Rückgängig. `known` ({ cents, date }): the value stored now,
    /// passed by undo/redo because the account list reloads off the GUI thread.
    function setOpeningBalance(accountId, cents, date, known) {
        const a = accountsByID[accountId] || null
        const old = known || (a ? { cents: a.opening_balance_cents, date: a.opening_balance_date } : null)
        core("set_account_opening_balance", { account_id: accountId, cents: cents, date: date })
        if (old) UndoManager.register(cents === null ? qsTr("Startsaldo entfernen") : qsTr("Startsaldo ändern"),
                                      () => run(() => setOpeningBalance(accountId, old.cents, old.date, { cents: cents, date: date })))
        reloadAccounts(); reloadHistory()
    }
    /// What a Startsaldo at `date` would count, off the GUI thread; nothing is stored.
    function previewOpeningBalance(accountId, date, done, failed) {
        coreAsync("preview_account_opening_balance", { account_id: accountId, date: date }, done, failed)
    }

    // MARK: - Documents (DocumentsUI.swift / AccountDocumentsUI.swift)
    /// Bumped whenever documents change, so lists re-read them.
    property int documentsVersion: 0
    property var documentsAccountFilter: null
    /// `documentsAccountFilter` value for documents without an account (e.g. PostBox, #838).
    readonly property int documentsWithoutAccount: -1
    property var statementsAccountID: null
    /// comdirect FinTS login whose PostBox is fetched now ("PostBox jetzt abrufen", #838).
    property var postboxConnectionID: null
    property int statementsCountBefore: 0
    property var accountDocumentsFor: null      // AccountDocumentsSheet
    function openAccountDocuments(a) { accountDocumentsFor = a }
    /// The booking row counts its documents: no core call (which would wait for a running
    /// background load at the vault mutex) for the many bookings without one.
    function documents(b) {
        void documentsVersion
        if (!b || !isUnlocked || b.document_count === 0) return []
        return run(() => core("list_documents_for_booking", { booking_id: b.id })) || []
    }
    function allDocuments(search, unlinkedOnly, accountId) {
        void documentsVersion
        if (!isUnlocked) return []
        const withoutAccount = accountId === documentsWithoutAccount
        const docs = run(() => core("list_all_documents", { filter: { account_id: withoutAccount ? null : accountId, doc_type: null, search: search || null, unlinked_only: unlinkedOnly } })) || []
        return withoutAccount ? docs.filter(d => d.account_id === null || d.account_id === undefined) : docs
    }
    function accountDocuments(accountID) { return allDocuments("", false, accountID) }
    function documentsChanged() { documentsVersion++; reloadBookings() }
    /// QuickLook: a private copy opened in the default viewer.
    function quickLook(d) {
        const path = run(() => core("document_temp_file", { doc_id: d.id, mime_type: d.mime_type }))
        if (path) Qt.openUrlExternally("file://" + path)
    }
    /// Space in the list / Buchung › Beleg in Quick Look anzeigen: the booking's first document.
    /// `false` when it has none.
    function quickLookFirstDocument(b) {
        const d = documents(b)[0]
        if (!d) return false
        quickLook(d)
        return true
    }
    function documentFileURL(d) {
        const path = run(() => core("document_temp_file", { doc_id: d.id, mime_type: d.mime_type }))
        return path ? "file://" + path : ""
    }
    /// DocumentPreviewSheet "Sichern …": writes the document to `path`.
    function exportDocument(d, path) {
        try {
            core("export_document", { doc_id: d.id, dest_path: path })
        } catch (e) {
            errorTitle = qsTr("Dokument konnte nicht gesichert werden")
            errorMessage = e.message
        }
    }
    /// „Im Finder anzeigen“ (#622) → „Im Dateimanager anzeigen“: the private copy, selected in
    /// the desktop's file manager (org.freedesktop.FileManager1); without one, its folder.
    function showInFileManager(d) {
        const path = run(() => core("document_temp_file", { doc_id: d.id, mime_type: d.mime_type }))
        if (!path) return
        const folder = () => Qt.openUrlExternally("file://" + path.slice(0, path.lastIndexOf("/")))
        coreAsync("show_in_file_manager", { path: path }, shown => { if (!shown) folder() }, () => folder())
    }
    /// Moves a document to another account, or to none (#838).
    function setDocumentAccount(d, accountID) {
        run(() => core("set_document_account", { doc_id: d.id, account_id: accountID }))
        documentsChanged()
    }
    function linkDocument(d, bookingID) {
        const linked = run(() => { core("link_document", { doc_id: d.id, booking_id: bookingID }); return true })
        documentsChanged()
        if (linked && bookingID === null) toast = qsTr("Zuordnung aufgehoben")
    }
    function deleteDocument(d) { run(() => core("delete_document", { doc_id: d.id })); documentsChanged() }
    function exportAllDocuments(folderPath) {
        const n = run(() => core("export_all_documents", { dest_dir: folderPath }))
        if (n !== undefined) toast = qsTr("%1 Belege nach „%2/GiroCrab-Belege“ exportiert").arg(n).arg(folderPath.split("/").pop())
    }
    function localPath(url) { return decodeURIComponent(String(url).replace(/^file:\/\//, "")) }
    function attach(urls, b) {
        let n = 0
        for (const u of urls) if (run(() => core("attach_file_to_booking", { booking_id: b.id, path: localPath(u) })) !== undefined) n++
        if (n > 0) toast = qsTr("%1 Belege an „%2“ angehängt").arg(n).arg(b.merchant_name)
        documentsChanged()
    }
    /// Dropped anywhere: the core matches amount / date / merchant (PDF text, and for
    /// photos the tesseract OCR text, as hint). Off the GUI thread, one file after another.
    function dropAnywhere(urls) {
        const paths = urls.map(u => localPath(u))
        const linked = []
        let unmatched = 0
        const finish = () => {
            if (linked.length) toast = qsTr("Beleg zugeordnet: %1").arg(linked.join(", "))
            else if (unmatched > 0) toast = qsTr("Kein passender Umsatz gefunden – in „Belege & PostBox“ abgelegt")
            documentsChanged()
        }
        const next = i => {
            if (i >= paths.length) { finish(); return }
            coreAsync("auto_match_file", { path: paths[i] }, hit => {
                if (hit) linked.push(hit); else unmatched++
                next(i + 1)
            }, err => {
                errorTitle = qsTr("Beleg konnte nicht zugeordnet werden")
                errorMessage = parseCoreError(err).message
                next(i + 1)
            })
        }
        next(0)
    }
    function canFetchStatements(a) {
        return a && a.connector && (a.kind !== "Loan" || a.connector === "Bhw") && ["VolksbankKasselGoettingen", "Direkt1822", "FinTsGeneric", "Easybank", "FordMoney", "OpelDirektbank", "Bhw"].indexOf(a.connector) >= 0
    }
    function fetchStatements(a) {
        if (syncingConnectionID !== null) return
        const c = connections.find(x => x.id === a.connection_id)
        if (!c) return
        statementsAccountID = a.id
        statementsCountBefore = accountDocuments(a.id).length
        requestSync(c)
    }
    function autoStatements(a) { return run(() => core("auto_statements_enabled", { account_id: a.id })) || false }
    function setAutoStatements(a, on) { run(() => core("set_auto_statements", { account_id: a.id, enabled: on })) }
    /// comdirect via FinTS: also fetch the PostBox through comdirect's API (#838).
    function comdirectPostbox(id) { return run(() => core("comdirect_postbox_enabled", { connection_id: id })) || false }
    function setComdirectPostbox(id, on) { run(() => core("set_comdirect_postbox", { connection_id: id, enabled: on })) }
    /// "PostBox jetzt abrufen": syncs the login with the PostBox, however recent the last fetch.
    function fetchComdirectPostbox(c) {
        if (syncingConnectionID !== null) return
        postboxConnectionID = c.id
        requestSync(c)
    }
    /// Toast suffix of a sync with the comdirect PostBox.
    function postboxNote(s) {
        if (s.postbox_documents === null || s.postbox_documents === undefined)
            return qsTr("PostBox nicht abgerufen. Bitte Client-ID und Client-Secret der comdirect-API prüfen.")
        return s.postbox_documents === 0 ? qsTr("keine neuen PostBox-Dokumente")
            : qsTr("%1 neue PostBox-Dokumente").arg(s.postbox_documents)
    }
    function docSymbol(d) {
        switch (d.doc_type) {
        case "abrechnung": return "chart.line.uptrend.xyaxis.circle"
        case "kontoauszug": return "list.bullet.rectangle"
        case "postbox": return "tray.full"
        case "steuer": return "percent"
        default: return d.mime_type.startsWith("image") ? "photo" : "doc.text.fill"
        }
    }
    function byteCount(n) { return Fmt.byteCount(n) }

    // MARK: - Sidebar drag & drop (AccountGroupsModel.swift)
    readonly property var namedUserGroups: userGroups.filter(g => !g.is_ungrouped)
    function afterGroupChange() { reloadAccounts(); reloadHistory(); reloadBookings() }
    function moveAccount(accountID, uuid, index) {
        const target = uuid ? uuid : null
        if (run(() => { core("move_account_to_group", { account_id: accountID, group_uuid: target, index: index === undefined || index === null ? null : Math.max(0, index) }); return true }))
            afterGroupChange()
    }
    function setUserGroupParent(g, parent) {
        if (g.is_ungrouped) return
        if (run(() => { core("set_user_group_parent", { uuid: g.uuid, parent_uuid: parent }); return true })) {
            if (parent && collapsedUserGroups[parent]) toggleCollapsedUserGroup(parent)
            afterGroupChange()
        }
    }
    function reorderUserGroups(uuids) { run(() => core("reorder_user_groups", { uuids: uuids.filter(u => u) })); reloadAccounts() }
    function canDropGroup(uuid, target, into) {
        if (!uuid || userGroupSubtree(uuid).some(g => g.uuid === target.uuid)) return false
        return !(into && target.is_ungrouped)
    }
    /// A group dropped onto `target`: right above it as its sibling (`before`) or into it.
    function dropUserGroup(uuid, target, before) {
        const moving = userGroups.find(g => g.uuid === uuid)
        if (!uuid || uuid === target.uuid || !moving || userGroupSubtree(uuid).some(g => g.uuid === target.uuid)) return false
        const parent = target.is_ungrouped ? null : (before ? target.parent_uuid : target.uuid)
        if ((moving.parent_uuid || null) !== parent) setUserGroupParent(moving, parent)
        if (target.is_ungrouped || before) {
            const order = namedUserGroups.map(g => g.uuid).filter(u => u !== uuid)
            const at = target.is_ungrouped ? order.length : (order.indexOf(target.uuid) < 0 ? order.length : order.indexOf(target.uuid))
            order.splice(at, 0, uuid)
            reorderUserGroups(order)
        }
        return true
    }
    /// GroupDrag.insertionIndex
    function insertionIndex(row, below, from) {
        const at = below ? row + 1 : row
        return from !== null && from !== undefined && from >= 0 && from < at ? at - 1 : at
    }
    property var dragPayload: null
    property string dragLabel: ""
    property point dragScenePos: Qt.point(0, 0)
    property var dropLine: null
    /// The pointer was released: the sidebar's drag image delivers the drop, then clears the state.
    signal dragEnded()

    // MARK: - account groups (AccountGroupsModel.swift, SidebarView.swift, CommandPalette.swift)
    /// Sidebar state of AppModel.swift (UserDefaults keys `sidebar.*`).
    /// Read and written with value()/setValue(): bound properties of a Settings with a
    /// category got their declared default written back on exit.
    property Settings sidebarDefaults: Settings {
        category: "sidebar"
        /// stored JSON array → `{ key: true }` set
        function loadSet(key) {
            const out = {}
            try { for (const k of JSON.parse(value(key, "[]"))) out[k] = true } catch (e) {}
            return out
        }
    }
    /// `@AppStorage("showAccountAnalytics")` (BookingListView) and `@AppStorage("chartColorPalette")` (PlanningUI).
    property Settings uiDefaults: Settings {
        property bool showAccountAnalytics: false
        property string chartColorPalette: "girocrab"
        property bool showBudgets: true
        property bool compactHeader: false
    }
    readonly property bool hasUserGroups: userGroups.some(g => !g.is_ungrouped)
    readonly property bool hasArchivedAccounts: allAccounts.some(a => a.archived_unix !== null)
    /// GroupDialog: { kind: "create", movingAccount, parent } | { kind: "rename", group } | { kind: "renameAccount", account }
    /// | { kind: "delete", group } | { kind: "merge", from, into } – null = none.
    property var groupDialog: null
    /// SidebarDialog: { kind: "archiveAccount" | "deleteAccount", account } | { kind: "removeConnection", id } – null = none.
    property var sidebarDialog: null
    /// IconColorTarget of an account or group: { type: "account", id } | { type: "group", uuid } (categories: iconColorCategoryID).
    property var iconColorTarget: null
    property bool showGroupManager: false
    property bool showCommandPalette: false
    function openGroupManager() { showGroupManager = true }

    function userGroupByUUID(uuid) { return userGroups.find(g => g.uuid === uuid) || null }
    function account(id) { return accountsByID[id] || null }
    /// Groups `g` can be moved into: all but itself and its own subgroups.
    function parentCandidates(g) {
        const own = userGroupSubtree(g.uuid).map(x => x.uuid)
        return namedUserGroups.filter(x => own.indexOf(x.uuid) < 0)
    }
    /// A name in a MenuItem / Action text: a literal "&" must be "&&" (Qt reads "&" as a mnemonic).
    function menuText(s) { return (s || "").split("&").join("&&") }
    /// UserGroup.indentedName
    function indentedName(g) { return " ".repeat(g.depth) + g.name }
    /// Another group with this name (case-insensitive), if any.
    function userGroupNamed(name, exceptUUID) {
        const key = name.trim().toLowerCase()
        return namedUserGroups.find(g => g.uuid !== exceptUUID && g.name.toLowerCase() === key) || null
    }
    function uncollapseUserGroup(uuid) { if (collapsedUserGroups[uuid]) toggleCollapsedUserGroup(uuid) }

    /// Creates a group (and moves `accountID` into it). Returns the uuid.
    function createUserGroup(name, accountID, parent) {
        const uuid = run(() => {
            const u = core("create_user_group", { name: name, sf_symbol: "folder" })
            if (parent) { core("set_user_group_parent", { uuid: u, parent_uuid: parent }); uncollapseUserGroup(parent) }
            if (accountID !== null && accountID !== undefined) core("move_account_to_group", { account_id: accountID, group_uuid: u, index: null })
            return u
        })
        if (!uuid) return null
        setAccountView("byGroup")
        uncollapseUserGroup(uuid)
        afterGroupChange()
        return uuid
    }
    /// `changes`: { name, symbol, includeInTotal } (missing = unchanged).
    function updateUserGroup(g, changes) {
        if (g.is_ungrouped) return
        if (run(() => { core("update_user_group", { uuid: g.uuid, name: changes.name ?? g.name, sf_symbol: changes.symbol ?? g.sf_symbol,
                                                    include_in_total: changes.includeInTotal ?? g.include_in_total }); return true }))
            afterGroupChange()
    }
    function setUserGroupColor(g, hex) {
        if (g.is_ungrouped) return
        if (run(() => { core("set_user_group_color", { uuid: g.uuid, color_hex: hex }); return true })) afterGroupChange()
    }
    /// Renames a group; on a name clash nothing changes and the merge question is returned.
    function renameUserGroup(g, name) {
        const other = userGroupNamed(name, g.uuid)
        if (other) return { kind: "merge", from: g, into: other }
        updateUserGroup(g, { name: name })
        return null
    }
    function mergeUserGroups(from, into) {
        if (from.is_ungrouped || into.is_ungrouped) return
        if (!run(() => { core("merge_user_groups", { from_uuid: from.uuid, into_uuid: into.uuid }); return true })) return
        if (sidebarSelection === "userGroup:" + from.uuid) sidebarSelection = "userGroup:" + into.uuid
        if (collapsedUserGroups[from.uuid]) toggleCollapsedUserGroup(from.uuid)
        afterGroupChange()
        toast = qsTr("„%1“ mit „%2“ zusammengeführt").arg(from.name).arg(into.name)
    }
    function deleteUserGroup(g) {
        if (g.is_ungrouped) return
        if (!run(() => { core("delete_user_group", { uuid: g.uuid }); return true })) return
        if (sidebarSelection === "userGroup:" + g.uuid) sidebarSelection = "all"
        if (collapsedUserGroups[g.uuid]) toggleCollapsedUserGroup(g.uuid)
        afterGroupChange()
        toast = qsTr("Gruppe „%1“ gelöscht").arg(g.name)
    }
    function renameAccount(id, name) {
        if (run(() => { core("rename_account", { account_id: id, name: name }); return true })) { reloadAccounts(); reloadBookings() }
    }
    /// AccountGroupsModel.swift › setAccountOwner: empty clears it.
    function setAccountOwner(id, owner) {
        if (run(() => { core("set_account_owner", { account_id: id, owner: owner }); return true })) reloadAccounts()
    }
    /// AppModel.swift › setAccountNote: empty clears it; synced like the name (#980).
    function setAccountNote(id, note) {
        if (run(() => { core("set_account_note", { account_id: id, note: note }); return true })) reloadAccounts()
    }
    /// AccountGroupsModel.swift › setAccountKind ("Kontoart ändern …").
    function setAccountKind(id, kind) {
        if (run(() => { core("set_account_kind", { account_id: id, kind: kind }); return true })) { reloadAccounts(); reloadDashboard() }
    }
    /// AccountKind.importChoices with their labels.
    readonly property var accountKindChoices: [
        { id: "Checking", text: qsTr("Girokonto") }, { id: "Savings", text: qsTr("Tagesgeld / Sparen") },
        { id: "CreditCard", text: qsTr("Kreditkarte") }, { id: "PaymentService", text: qsTr("Zahlungsdienst") },
        { id: "Brokerage", text: qsTr("Depot") }, { id: "Cash", text: qsTr("Bargeld") }, { id: "Loan", text: qsTr("Darlehen") }
    ]
    function setAccountColor(id, hex) {
        if (run(() => { core("set_account_color", { account_id: id, color_hex: hex }); return true })) { reloadAccounts(); reloadBookings() }
    }
    function setAccountSymbol(id, symbol) {
        if (run(() => { core("set_account_symbol", { account_id: id, sf_symbol: symbol }); return true })) { reloadAccounts(); reloadBookings() }
    }
    function resetAccountSymbol(id) {
        if (run(() => { core("reset_account_symbol", { account_id: id }); return true })) { reloadAccounts(); reloadBookings() }
    }
    /// Archives ("Das Konto wurde aufgelöst") or reactivates an account.
    function setAccountArchived(a, archived) {
        run(() => {
            core("set_account_archived", { account_id: a.id, archived: archived })
            toast = archived ? qsTr("„%1“ archiviert").arg(a.name) : qsTr("„%1“ reaktiviert").arg(a.name)
        })
        reloadAll()
    }
    /// Deletes one archived account incl. bookings/documents/holdings.
    function deleteAccount(id) {
        run(() => core("delete_account", { account_id: id }))
        if (sidebarSelection === "account:" + id) sidebarSelection = "all"
        reloadAll()
    }
    /// The accounts of group `uuid` in a new order (group manager).
    function reorderAccounts(uuid, ids) {
        run(() => core("reorder_accounts_in_group", { group_uuid: uuid ? uuid : null, account_ids: ids }))
        reloadAccounts()
    }
    /// "Konten abrufen" of a group: the logins behind its accounts (subgroups included), each once.
    function groupSyncConnections(g) {
        const ids = {}
        for (const x of userGroupSubtree(g.uuid))
            for (const a of x.accounts) if (a.archived_unix === null && a.connection_id !== null) ids[a.connection_id] = true
        return connections.filter(c => ids[c.id] && !usesPortal(c))
    }
    function requestSyncGroup(g) { requestSyncBatch(groupSyncConnections(g)) }
    /// "Ohne Gruppe" for the group manager: always there, also when every account is in a group.
    readonly property var ungroupedForSettings: userGroups.find(g => g.is_ungrouped) || {
        uuid: "", name: qsTr("Ohne Gruppe"), sf_symbol: "tray", color_hex: "", sort_order: 2147483647, include_in_total: true,
        in_total: true, is_ungrouped: true, parent_uuid: null, depth: 0, balance_cents: 0, accounts: [] }
    /// Accounts that can be added to group `uuid`: those without a group first, then
    /// the members of every other group; archived accounts are left out.
    function addableAccounts(uuid) {
        const others = userGroups.filter(g => g.uuid !== uuid)
        return others.filter(g => g.is_ungrouped).concat(others.filter(g => !g.is_ungrouped)).map(g => {
            const active = g.accounts.filter(a => a.archived_unix === null)
            return active.length ? { id: g.uuid, title: g.is_ungrouped ? qsTr("Ohne Gruppe") : g.name, accounts: active } : null
        }).filter(x => x)
    }
    /// IBAN, else account number, else bank.
    function accountIdentity(a) {
        if (a.iban) return Fmt.iban(a.iban)
        if (a.account_number) return qsTr("Konto %1").arg(a.account_number)
        return a.bank_name
    }
    function accountDetail(a) {
        const id = accountIdentity(a)
        return a.archived_unix !== null ? qsTr("Archiviert · %1").arg(id) : id
    }
    /// "Keine Konten" / "1 Konto" / "3 Konten".
    function accountCountText(n) { return n === 0 ? qsTr("Keine Konten") : n === 1 ? qsTr("1 Konto") : qsTr("%1 Konten").arg(n) }
    /// The confirmation text of "Gruppe löschen …" (AccountGroupsUI.swift › deleteMessage).
    function deleteMessage(g) {
        const parent = g.parent_uuid ? userGroupByUUID(g.parent_uuid) : null
        const subgroups = userGroupSubtree(g.uuid).length - 1
        const target = parent ? parent.name : qsTr("Ohne Gruppe")
        const parts = []
        const n = g.accounts.length
        if (n === 0) { if (subgroups === 0) return qsTr("Die Gruppe ist leer.") }
        else if (n === 1) parts.push(qsTr("Das Konto wird nach „%1“ verschoben.").arg(target))
        else parts.push(qsTr("%1 Konten werden nach „%2“ verschoben.").arg(n).arg(target))
        if (subgroups > 0)
            parts.push(parent ? qsTr("Die Untergruppen bleiben erhalten und rücken nach „%1“.").arg(target)
                              : qsTr("Die Untergruppen bleiben erhalten und rücken auf die oberste Ebene."))
        parts.push(qsTr("Es werden keine Konten oder Buchungen gelöscht."))
        return parts.join(" ")
    }
    /// AccountCopyMenu: BIC from the Bundesbank BLZ list (German IBANs only).
    function bicForIban(iban) { try { return iban ? core("bic_for_iban", { iban: iban }) : null } catch (e) { return null } }
    function accountBankDetails(a, bic) {
        const bank = bic && a.bank_name ? "BIC: " + bic + " (" + a.bank_name + ")" : bic ? "BIC: " + bic : a.bank_name ? "Bank: " + a.bank_name : ""
        return [a.owner ? "Name: " + a.owner : "", "IBAN: " + Fmt.iban(a.iban), bank].filter(x => x).join("\n")
    }

    // MARK: - Banking policy ("Sicherheitssperre", BankingPolicy.swift → src/banking_policy.rs)
    property var bankingPolicy: ({ enabled: false, gate: null, message: null })
    readonly property var bankingGateAll: bankingPolicy.gate
    property bool bankingPolicyChecking: false
    /// `withFreshBankingPolicy`: every bank entry point refreshes first (fresh for 15 min).
    function withFreshBankingPolicy(then, force) {
        bankingPolicyChecking = true
        coreAsync("banking_policy_refresh", { force: !!force }, st => { bankingPolicyChecking = false; bankingPolicy = st; if (then) then() },
                  err => { bankingPolicyChecking = false; if (then) then() })
    }
    /// The block for a connector key (`fints:<BLZ>`, `enablebanking:<country>:<bank>`, …) as
    /// { reason, all, …, title, detail, message } (BankingGate), or null.
    function bankingGateForKey(key) {
        const g = bankingPolicy.gate
        if (!g) return null
        const full = Object.assign({}, g, { title: bankingPolicy.title, detail: bankingPolicy.detail, message: bankingPolicy.message })
        if (g.all) return full
        return g.connector_ids.some(id => { id = id.trim(); return id !== "" && (key === id || key.startsWith(id + ":")) }) ? full : null
    }
    /// Why this login may not contact its bank (null = allowed).
    function bankingGate(connection) {
        if (!connection || isMagicDemoLogin(connection.login_name) || !bankingPolicy.gate) return null
        let key
        try { key = core("connection_key", { connection_id: connection.id }) }
        catch (e) { key = core("connector_kind_key", { kind: connection.connector }) }
        return bankingGateForKey(key)
    }
    /// Block for the login of an account (transfers).
    function bankingGateForAccount(accountID) {
        const a = accountID === null || accountID === undefined ? null : accountsByID[accountID]
        const c = a ? connections.find(x => x.id === a.connection_id) : null
        return c ? bankingGate(c) : null
    }
    /// Block for a login that is about to be added.
    function bankingGateForNew(kind) { return bankingPolicy.gate ? bankingGateForKey(core("connector_kind_key", { kind: kind })) : null }
    /// Shows the reason and returns false if adding is blocked.
    function bankingAllowsAdding(gate) {
        if (!gate) return true
        showBankingGate(gate)
        return false
    }
    /// showError(gate.title, gate.detail)
    function showBankingGate(gate) {
        errorTitle = gate.title || bankingGateTitle(gate)
        errorMessage = gate.detail || gate.message || ""
    }
    readonly property bool bankingBlocksAll: bankingPolicy.gate !== null && bankingPolicy.gate !== undefined && bankingPolicy.gate.all
    function bankingGateTitle(g) { return g.reason === "checkUnavailable" ? qsTr("Sicherheitsprüfung nicht möglich") : qsTr("Bankverbindungen gesperrt") }
    // MARK: - Rules & categories
    // CategorizerSheet, RuleEditorView, NewCategoryDialog, category context menu,
    // BudgetEditorSheet, SplitSheet (AppModel.swift › Editing, PlanningUI.swift).

    property bool showCategorizer: false
    property bool showRuleEditor: false
    /// Rule the editor opens with (`openRuleEditor(from:)`), null = none selected.
    property var ruleDraft: null
    property var rules: []
    /// NewCategoryRequest: `{ parent: id | null }`, null = no sheet.
    property var newCategoryRequest: null
    /// Category whose "Umbenennen …" / "Kategorie löschen …" dialog is open.
    property var renameCategoryTarget: null
    property var deleteCategoryTarget: null
    /// Category whose BudgetEditorSheet is open.
    property var budgetCategoryID: null
    /// Category whose "Symbol & Farbe" popover is open.
    property var iconColorCategoryID: null
    /// The booking whose SplitSheet is open.
    property var splitBooking: null
    readonly property var colorPalette: core("account_color_palette")
    /// AccountColorNames.names
    readonly property var colorNames: ({
        "#2563EB": qsTr("Blau"), "#D97706": qsTr("Bernstein"), "#059669": qsTr("Grün"), "#DB2777": qsTr("Pink"),
        "#7C3AED": qsTr("Violett"), "#0891B2": qsTr("Cyan"), "#DC2626": qsTr("Rot"), "#65A30D": qsTr("Limette"),
        "#C026D3": qsTr("Fuchsia"), "#EA580C": qsTr("Orange"), "#4F46E5": qsTr("Indigo"), "#0D9488": qsTr("Petrol"),
        "#CA8A04": qsTr("Gelb"), "#92400E": qsTr("Braun"), "#475569": qsTr("Schiefer")
    })
    /// ColorChoices.extras: offered in the picker only, never handed out as default colours
    /// (the core assigns defaults by palette slot, so extending it would recolour accounts).
    readonly property var colorExtras: ["#CA8A04", "#92400E", "#475569"]
    /// ColorChoices.swatches: the core palette plus the extras.
    readonly property var colorSwatches: {
        const p = colorPalette.map(h => h.toUpperCase())
        return p.concat(colorExtras.filter(h => p.indexOf(h) < 0))
    }
    /// ColorChoices.customColors: colours in use on accounts, groups and categories that are
    /// not swatches, most used first (ties: first seen), at most 14 (two rows).
    readonly property var customColors: {
        const used = allAccounts.map(a => a.color_hex).concat(userGroups.map(g => g.color_hex), categories.map(c => c.color_hex))
        const count = {}, order = []
        for (const raw of used) {
            const hex = (raw || "").trim().toUpperCase()
            if (hex === "" || colorSwatches.indexOf(hex) >= 0) continue
            if (count[hex] === undefined) { order.push(hex); count[hex] = 0 }
            count[hex]++
        }
        return order.map((h, i) => ({ h: h, i: i })).sort((a, b) => count[b.h] - count[a.h] || a.i - b.i).slice(0, 14).map(x => x.h)
    }
    /// AppModel+Editing.swift › removeColor: accounts go back to their default colour, groups to
    /// none, and a category (it always has a colour) takes its parent's, a main category slate grey.
    function removeColor(old) {
        old = old.toUpperCase()
        const matches = hex => (hex || "").toUpperCase() === old
        for (const a of allAccounts.slice()) if (matches(a.color_hex)) setAccountColor(a.id, null)
        for (const g of userGroups.slice()) if (matches(g.color_hex)) setUserGroupColor(g, null)
        const byID = {}
        for (const c of categories) if (byID[c.id] === undefined) byID[c.id] = c
        for (const c of categories.slice()) if (matches(c.color_hex)) {
            const p = c.parent_id !== null && byID[c.parent_id] ? byID[c.parent_id].color_hex : ""
            setCategoryColor(c.id, p && !matches(p) ? p : colorExtras[2])
        }
    }

    function reloadRules() {
        if (!isUnlocked) return
        loadOffMain("rules", () => [["list_rules"]], r => { rules = r[0] })
    }
    /// The rule editor reads `rules` right away (priority of a new rule).
    function reloadRulesNow() {
        if (!isUnlocked) return
        cancelLoad("rules")
        run(() => { rules = core("list_rules") })
    }
    function categoryByID(id) { return id === null || id === undefined ? null : (categories.find(c => c.id === id) || null) }

    /// One category for several bookings (multiple selection).
    function setCategoryFor(list, categoryID) {
        if (!list.length) return
        run(() => { for (const b of list) core("set_transaction_category", { transaction_id: b.id, category_id: categoryID }) })
        reloadAccounts(); reloadCategories(); reloadBookings(); reloadPlanning()
        // rule counts and the investment dashboard (Swift: rulesStale / dashboardStale)
        if (showRuleEditor) reloadRules()
        if (scope === "Investments") reloadDashboard()
    }

    function createCategory(name, parent, symbol, color) {
        run(() => core("create_category", { name: name, parent_id: parent, sf_symbol: symbol, color_hex: color }))
        reloadCategories()
    }
    /// Changes one property of a category and keeps the others (the core only
    /// updates a category as a whole).
    function editCategory(id, change) {
        const c0 = categoryByID(id)
        if (!c0) return
        const c = Object.assign({}, c0)
        change(c)
        if (!run(() => { core("update_category", { id: c.id, name: c.name, parent_id: c.parent_id, sf_symbol: c.sf_symbol, color_hex: c.color_hex }); return true })) return
        // categories reload off the GUI thread: patch the local copy at once, so a second quick
        // edit (symbol, then colour) starts from this one and does not write the old value back
        categories = categories.map(x => x.id === c.id ? Object.assign({}, x, { name: c.name, parent_id: c.parent_id, sf_symbol: c.sf_symbol, color_hex: c.color_hex }) : x)
        reloadCategories()
        reloadBookings() // the booking list shows a category's name, symbol and colour
        reloadPlanning() // budget cards show name, symbol, colour and the parent
    }
    function renameCategory(id, name) {
        const t = (name || "").trim()
        if (t === "") return
        editCategory(id, c => { c.name = t })
    }
    /// "Verschieben nach ▸": under another category, or to the top level (null).
    function moveCategory(id, parent) {
        editCategory(id, c => { c.parent_id = parent })
        if (parent !== null) SidebarModel.expandCategoryPath(parent) // keep it in view, also inside a folded main category (#633)
    }
    function setCategorySymbol(id, symbol) { editCategory(id, c => { c.sf_symbol = symbol }) }
    function setCategoryColor(id, hex) { editCategory(id, c => { c.color_hex = hex }) }
    /// Where a category can move: every category except itself and its own sub-categories.
    function categoryMoveTargets(id) {
        const byID = {}
        for (const c of categories) byID[c.id] = c
        return categories.filter(c => {
            let cur = c.id
            for (let steps = 0; cur !== null && cur !== undefined && steps < 64; steps++) {
                if (cur === id) return false
                cur = byID[cur] ? byID[cur].parent_id : null
            }
            return true
        })
    }
    function categoryPath(id) { const c = categoryByID(id); return c ? c.path : null }
    function deleteCategory(id) {
        run(() => core("delete_category", { id: id }))
        if (sidebarSelection === "category:" + id) sidebarSelection = "all"
        reloadAll()
    }

    // PlanningUI.swift › AppModel extension
    function setBudget(cents, categoryID) {
        run(() => core("set_category_budget", { category_id: categoryID, budget_cents: cents }))
        reloadCategories(); reloadPlanning()
    }
    /// parseAmountToCents(text:): null when not a valid amount.
    function parseCents(text) {
        try { return core("parse_amount_to_cents", { text: text }) } catch (e) { return null }
    }
    /// `done(null)` = could not be read – not the same as "not split".
    function splits(booking, done) {
        coreAsync("list_transaction_splits", { transaction_id: booking.id }, r => done(r), () => done(null))
    }
    /// Error text, or "" on success.
    function saveSplits(parts, booking) {
        try { core("set_transaction_splits", { transaction_id: booking.id, splits: parts }) } catch (e) { return e.message }
        reloadBookings(); reloadCategories(); reloadAccounts(); reloadPlanning()
        return ""
    }

    function newRuleDraft(booking) {
        const b = booking || null
        const firstSub = categories.find(c => c.depth > 0)
        const known = b && b.merchant && b.merchant.known
        return {
            id: 0,
            name: b ? b.merchant.name : qsTr("Neue Regel"),
            category_id: b && b.category_id !== null ? b.category_id : (firstSub ? firstSub.id : (categories.length ? categories[0].id : 0)),
            field: b ? "Counterparty" : "Any",
            match_kind: "Keyword",
            pattern: b ? (known ? b.merchant.name : b.counterparty.split(" ").filter(x => x !== "").slice(0, 2).join(" ")) : "",
            case_sensitive: false,
            // Direction follows the booking (#295): a rule made from an income
            // must not also categorise the outgoing transfers to the same payee.
            min_amount_cents: b && b.amount_cents > 0 ? 1 : null,
            max_amount_cents: b && b.amount_cents < 0 ? 0 : null,
            account_ids: [],   // all accounts (#562)
            priority: rules.reduce((m, r) => Math.max(m, r.priority), 0) + 10,
            enabled: true,
            match_count: 0
        }
    }
    function openRuleEditor(booking) {
        reloadRulesNow()
        ruleDraft = booking ? newRuleDraft(booking) : null
        showRuleEditor = true
    }
    /// The id of the saved rule, else null (error shown).
    function saveRule(rule) {
        try {
            const id = core("save_rule", { rule: rule })
            core("apply_rules", { only_uncategorized: false })
            // the editor reads the saved rule (its new id) right away: rules now, the rest async
            reloadRulesNow()
            reloadAll()
            return id
        } catch (e) {
            errorTitle = qsTr("Regel konnte nicht gespeichert werden")
            errorMessage = e.message
            return null
        }
    }
    function deleteRule(id) {
        run(() => core("delete_rule", { rule_id: id }))
        reloadAll()
    }
    /// `{ matches, error }` of the live preview.
    function preview(rule) {
        try { return { matches: core("preview_rule", { rule: rule, limit: 200 }), error: "" } }
        catch (e) { return { matches: [], error: e.message } }
    }
    function applyAllRules() {
        run(() => {
            const n = core("apply_rules", { only_uncategorized: false })
            toast = qsTr("Regeln angewendet – %1 Buchungen neu zugeordnet").arg(n)
        })
        reloadAll()
    }


    // MARK: - Import & export

    /// (demo, `--demo-sheet moneymoney`) canned preview so the sheet can be shown without a file.
    property var demoImportPreview: null
    /// (demo, `--demo-sheet moneymoney-db`) canned database preview with invented data.
    property var demoDbPreview: null
    /// Enable Banking connections the last MoneyMoney import set up, with the
    /// accounts to link to the user's application.
    property var psd2LinkHints: []
    property bool showPrintSheet: false
    property bool showCSVExporter: false
    // Umsatz-Export (PDF / CSV / CAMT.053): ExportSheet.qml, logic in BookingExport.qml.
    property bool showExportSheet: false
    /// When set, the export sheet defaults to these booking ids (context menu).
    property var exportBookingIDs: null
    property bool exportPreferSelection: false
    /// A built export file waiting for the save dialog („Unter …“, Main.qml).
    property var exportSaveRequest: null
    /// Opens the export dialog; `selection`: use the current multi-/single selection (context menu).
    function openBookingExport(selection) {
        if (!isUnlocked) return
        if (selection) {
            exportBookingIDs = Object.keys(selectedBookingIDs).map(Number)
            if (!exportBookingIDs.length && selectedBookingID !== null) exportBookingIDs = [selectedBookingID]
            exportPreferSelection = exportBookingIDs.length > 0
        } else {
            exportBookingIDs = null
            exportPreferSelection = false
        }
        showExportSheet = true
    }

    /// Opens the MoneyMoney import. An import belongs in the own vault, so the
    /// demo data an empty vault starts with is left first.
    function openMoneyMoneyImport() {
        if (isDemo && !optionDemoSeed) showOwnVault()
        showMoneyMoneyImport = true
    }

    function finishMoneyMoneyImport(r) {
        reloadAll()
        if (r.account_created) sidebarSelection = "account:" + r.account_id
        toast = qsTr("Import: %1 Buchungen importiert").arg(r.imported)
            + (r.enriched > 0 ? qsTr(", %1 ergänzt").arg(r.enriched) : "")
            + (r.duplicates > 0 ? qsTr(", %1 bereits vorhanden").arg(r.duplicates) : "")
    }

    function finishMoneyMoneyDbImport(r) {
        reloadAll()
        if (r.psd2_connections.length) psd2LinkHints = r.psd2_connections
        toast = qsTr("Import: %1 Buchungen in %2 Konten importiert").arg(r.bookings_new).arg(r.accounts_created + r.accounts_merged)
            + (r.bookings_duplicates > 0 ? qsTr(", %1 bereits vorhanden").arg(r.bookings_duplicates) : "")
    }

    /// Exports every booking matching the current list filter as Excel-friendly
    /// CSV: the save dialog (Main.qml) calls `writeFilteredBookingsCSV`.
    function exportFilteredBookingsCSV() { if (isUnlocked) showCSVExporter = true }
    function writeFilteredBookingsCSV(path) {
        try {
            core("export_bookings_csv_file", { filter: currentFilter(), path: path })
        } catch (e) {
            errorTitle = qsTr("Export fehlgeschlagen")
            errorMessage = e.message
        }
    }

    // Printing (GitHub #53)

    /// Everything the list shows (account, period, search, scope, status) without the list limit.
    function printSource() {
        if (!isUnlocked) return null
        const filter = currentFilter()
        filter.limit = 0
        try {
            const rows = core("list_transactions", { filter: filter }).transactions.map(b => PrintReport.rowFromBooking(b))
            const accounts = allAccounts.map(a => ({ id: a.id, name: a.name, iban: a.iban, balance_cents: a.balance_cents,
                                                     balance_date: a.balance_date, currency: a.currency }))
            // The list's own income/expense filter is named in the printout header.
            const notes = directionFilter === "Income" ? [PrintReport.directionTitle("income")]
                        : directionFilter === "Expense" ? [PrintReport.directionTitle("expenses")] : []
            // and the tag filter (#228), as far as this list applies it
            const tagNote = TagFilter.noteFor(filter)
            if (tagNote !== "") notes.push(tagNote)
            return { title: title, period_label: periodTitle, rows: rows, accounts: accounts, daily_balances: ({}),
                     generated_at: new Date(), filter_notes: notes }
        } catch (e) {
            errorTitle = qsTr("Drucken nicht möglich")
            errorMessage = e.message
            return null
        }
    }

    /// End-of-day balances since `from` for each account (key = id) and combined (key 0).
    function printDailyBalances(accountIds, from) {
        if (!accountIds.length) return ({})
        const today = new Date(); today.setHours(0, 0, 0, 0)
        const start = Fmt.date(from) || today
        const days = Math.max(1, Math.round((today - start) / 86400000) + 1)
        function map(ids) {
            const out = {}
            try { for (const p of core("balance_history", { account_ids: ids, days: days })) out[p.date] = p.balance_cents } catch (e) {}
            return out
        }
        const out = { 0: map(accountIds) }
        for (const id of accountIds) out[id] = map([id])
        return out
    }

    /// PrintSheet › print(): the report of the selected rows to the print dialog. QtWebEngine
    /// lays it out like WebKit on the Mac (Main.qml › WebPrinter.qml renders `printJob`
    /// into a PDF), then finishPrintJob() hands the PDF to the print dialog.
    property var printJob: null   // { html, path, job_name } while the report is rendered
    function printReport(report, options) {
        const r = Object.assign({}, report, { generated_at: new Date() })
        if (options.show_daily_balance && r.rows.length) {
            const earliest = r.rows.map(x => x.date).reduce((a, b) => (a < b ? a : b))
            const ids = Object.keys(r.rows.reduce((m, x) => { m[x.account_id] = true; return m }, {})).map(Number).sort((a, b) => a - b)
            r.daily_balances = printDailyBalances(ids, earliest)
        }
        let assets
        try {
            assets = core("print_web_assets")
        } catch (e) {
            errorTitle = qsTr("Drucken nicht möglich"); errorMessage = e.message
            return null
        }
        if (!assets.pdf_supported) {
            errorTitle = qsTr("Drucken nicht möglich")
            errorMessage = qsTr("Zum Drucken fehlt das PDF-Modul von Qt (Paket „qt6-image-formats-plugin-pdf“).")
            return null
        }
        const tmp = decodeURIComponent(String(StandardPaths.writableLocation(StandardPaths.TempLocation)).replace(/^file:\/\//, ""))
        printJob = {
            html: PrintReport.html(r, options, "qrc:/qt/qml/GiroCrab/assets/app-icon.png", assets),
            path: tmp + "/girocrab-druck-" + Date.now() + ".pdf",
            job_name: "GiroCrab – " + r.title + " – " + r.period_label
        }
        return "rendering"
    }
    /// WebPrinter.qml is done (or could not be loaded): print dialog, or the reason it failed.
    function finishPrintJob(ok, failure) {
        const job = printJob
        printJob = null
        if (!job) return
        if (job.done) { job.done(ok, failure, job.path); return }   // PDF export (BookingExport.qml)
        if (!ok) {
            errorTitle = qsTr("Drucken nicht möglich")
            errorMessage = failure || qsTr("Der Bericht konnte nicht gesetzt werden.")
            return
        }
        try {
            core("print_pdf", { path: job.path, job_name: job.job_name })
        } catch (e) {
            errorTitle = qsTr("Drucken nicht möglich")
            errorMessage = e.message
        }
    }

    /// Invented MoneyMoney database (public example IBANs, fictional names) for
    /// `--demo-sheet moneymoney-db` screenshots.
    function demoMoneyMoneyDbPreview(existingGiro) {
        function acc(id, name, bank, code, o) {
            return {
                mm_id: id, name: name, iban: o.iban || "", account_number: o.number || "", currency: o.currency || "EUR",
                bank_code: code, bank_name: bank, owner: "Max Mustermann", group_name: o.group !== undefined ? o.group : "Privat",
                mm_bank_id: o.bankId, is_portfolio: !!o.portfolio, suggested_kind: o.kind,
                existing_account_id: o.existing ? o.existing.id : null, existing_account_name: o.existing ? o.existing.name : null,
                booking_count: o.count, pending_count: o.pending || 0, holding_count: o.portfolio ? 3 : 0, notes_count: 0,
                first_date: o.from || null, last_date: o.to || null, statement_count: o.kind === "Checking" ? 48 : 0,
                computed_balance_cents: o.balance, balance_source: "Anfangssaldo laut Saldenverlauf der Datenbank + Umsätze"
            }
        }
        const accounts = [
            acc(1, "Girokonto", "comdirect", "20041111", { iban: "DE89370400440532013000", kind: "Checking", existing: existingGiro,
                count: 4812, pending: 3, from: "2009-04-01", to: "2026-11-09", balance: 324518, bankId: 1 }),
            acc(2, "Visa-Karte", "comdirect", "20041111", { number: "4871 •••• 1234", kind: "CreditCard", count: 1204, pending: 2,
                from: "2011-02-14", to: "2026-11-08", balance: -41290, bankId: 1 }),
            acc(3, "Tagesgeld", "1822direkt", "50050201", { iban: "DE02120300000000202051", kind: "Savings", count: 96,
                from: "2014-06-30", to: "2026-10-31", balance: 1250000, group: "Sparen", bankId: 2 }),
            acc(4, "Depot", "comdirect", "20041111", { number: "123456789", kind: "Brokerage", portfolio: true, count: 0,
                balance: 0, group: "Sparen", bankId: 1 }),
            acc(5, "PayPal", "PayPal", "", { number: "max@example.com", kind: "PaymentService", count: 640,
                from: "2013-08-02", to: "2026-11-05", balance: 2345, bankId: 3 }),
            acc(6, "Reisekasse USD", "Musterbank", "12345678", { number: "1000200", currency: "USD", kind: "Checking", count: 38,
                from: "2019-07-10", to: "2025-09-22", balance: 31075, group: "", bankId: 4 }),
            acc(7, "C24 Smartkonto", "C24 Bank", "50024024", { iban: "DE02500105170137075030", kind: "Checking", count: 57,
                from: "2024-03-01", to: "2026-11-07", balance: 184012, bankId: 5 })
        ]
        function direct(via, kind, key) { return { via: via, connector: kind, key: key, psd2_bank: null } }
        function bank(id, code, name, login, customer, connector, pin, count, options) {
            return { mm_id: id, bank_code: code, name: name, login_name: login, customer_id: customer, connector: connector,
                     existing_connection_id: null, account_count: count, connection_options: options }
        }
        let psd2 = []
        try { psd2 = core("psd2_catalog") } catch (e) {}
        const banks = [
            bank(1, "20041111", "comdirect", "12345678", "12345678", "Comdirect", true, 3, [direct("OwnApi", "Comdirect", "comdirect")]),
            bank(2, "50050201", "1822direkt", "max.mustermann", "", "Direkt1822", true, 1, [direct("FinTs", "Direkt1822", "direkt1822")]),
            bank(3, "", "PayPal", "max@example.com", "", "PayPal", false, 1, [direct("OwnApi", "PayPal", "paypal")]),
            bank(4, "12345678", "Musterbank", "mustermann", "", null, false, 1, []),
            // no own connector, but reachable through Enable Banking (PSD2)
            bank(5, "50024024", "C24 Bank", "max.mustermann", "", null, false, 1, psd2.filter(b => b.aspsp === "C24").map(b => ({
                via: "EnableBanking", connector: "EnableBanking", key: "enablebanking:" + b.country + ":" + b.aspsp, psd2_bank: b })))
        ]
        const categories = ["Gehalt", "Haushalt › Lebensmittel", "Haushalt › Drogerie", "Wohnen › Miete", "Wohnen › Nebenkosten › Strom",
                            "Mobilität › Tanken", "Mobilität › ÖPNV", "Freizeit › Restaurants", "Freizeit › Sport", "Versicherungen",
                            "Steuern", "Reisen", "Abos", "Gesundheit", "Geschenke", "Sparen"]
        const statements = accounts.reduce((s, a) => s + a.statement_count, 0)
        return {
            accounts: accounts, banks: banks, category_paths: categories,
            total_bookings: accounts.reduce((s, a) => s + a.booking_count, 0),
            pending_bookings: 5, notes_count: 312, split_parts: 14,
            date_range_start: "2009-04-01", date_range_end: "2026-11-09", replaces_starter_categories: true,
            database_path: "Finanzdaten.sqlite", statement_count: statements, statements_dir: "Statements",
            statements_found: statements, statements_bytes: 38400000, transfer_template_count: 4
        }
    }

    /// What the result looks like, for the demo (nothing is read or stored).
    function demoMoneyMoneyDbResult(p, statements, psd2) {
        return {
            accounts_created: p.accounts.length, accounts_merged: 0, groups_created: 2, bookings_new: p.total_bookings,
            bookings_duplicates: 0, bookings_enriched: 0, pending_skipped: p.pending_bookings, notes_imported: p.notes_count,
            categories_created: p.category_paths.length, connections_created: 4, connections_existing: 0,
            banks_skipped_unsupported: 1, banks_skipped_names: ["Musterbank"], documents_imported: statements ? p.statements_found : 0,
            documents_existing: 0, documents_missing: 0, documents_mismatched: 0, documents_too_large: 0, documents_unlisted: 0,
            holdings_imported: 3, rules_created: 0, rules_disabled: 0,
            transfer_templates_created: p.transfer_template_count, transfer_templates_existing: 0, transfer_templates_skipped: 0,
            account_checks: p.accounts.map(a => ({
                mm_id: a.mm_id, account_id: 0, name: a.name, currency: a.currency, mm_bookings: a.booking_count,
                found_bookings: a.booking_count, deleted_bookings: 0, other_bookings: 0,
                mm_balance_cents: a.computed_balance_cents, mm_balance_shown: true,
                girocrab_balance_cents: a.computed_balance_cents, balance_from_bank: false, ok: true })),
            psd2_connections: psd2
        }
    }

    // MARK: - SEPA-Überweisung

    /// TransferDraft (AppModel.swift). Replaced as a whole (`setDraft`) so bindings update.
    function newTransferDraft(accountID) {
        const t = new Date(); t.setDate(t.getDate() + 1)
        return { account_id: accountID === undefined ? null : accountID, name: "", iban: "", bic: "", amountText: "",
                 purpose: "", pin: "",
                 // SEPA Instant: off by default; if the bank declines, nothing is sent automatically.
                 instant: false,
                 // Terminüberweisung: `executionDate` (YYYY-MM-DD) is sent when on (FinTS only).
                 scheduled: false, executionDate: Fmt.iso(t) }
    }
    property var transferDraft: newTransferDraft(null)
    function setDraft(patch) { transferDraft = Object.assign({}, transferDraft, patch) }
    /// `YYYY-MM-DD` for the core, or null when not scheduled.
    function executionDateISO(d) { d = d || transferDraft; return d.scheduled && !d.instant ? d.executionDate : null }
    property bool showTransferSheet: false
    property string transferError: ""
    /// Instant was not possible and nothing was sent: the bank's reason (null = none).
    property var transferInstantDeclined: null
    /// The order reached the bank but its result is unclear: { reason, end_to_end_id }.
    property var transferUnknown: null
    property bool transferBusy: false
    /// Handle of the running TransferSession (src/sessions.rs).
    property var activeTransfer: null
    /// Name check (Verification of Payee) awaiting the user's decision: { connection, check, busy }.
    property var payeeCheck: null
    /// The running session is a Sammelüberweisung / a Dauerauftrag (Payments.qml), not the
    /// single transfer: its own sheet comes back and its own ending runs.
    property bool activeTransferIsBatch: false
    property bool activeTransferIsStanding: false
    /// … or a collection run of „Lastschriften“ (DirectDebits.qml, AppModel+DirectDebitSubmit.swift).
    property bool activeTransferIsDebit: false

    /// ConnectorKind.offersInstantChoice: FinTS banks; bunq decides on its own.
    function offersInstantChoice(connector) { return ["VolksbankKasselGoettingen", "Direkt1822", "FinTsGeneric"].indexOf(connector) >= 0 }
    /// Checking accounts that are connected via FinTS (HKCCS-capable banks) or bunq.
    readonly property var transferAccounts: allAccounts.filter(a => a.kind === "Checking" && a.iban !== "" && a.archived_unix === null
        && ["VolksbankKasselGoettingen", "Direkt1822", "FinTsGeneric", "Bunq"].indexOf(a.connector) >= 0)
    /// Checking accounts GiroCrab cannot send from (e.g. C24 via Enable Banking):
    /// the form ends with a GiroCode to scan in the bank's app instead of a TAN.
    readonly property var giroCodeAccounts: allAccounts.filter(a => a.kind === "Checking" && a.iban !== "" && a.archived_unix === null
        && !transferAccounts.some(t => t.id === a.id))
    /// Accounts offered in the transfer form: sending ones first.
    readonly property var transferFormAccounts: transferAccounts.concat(giroCodeAccounts)
    function canSendTransfer(accountID) { void bankingPolicy; return transferAccounts.some(a => a.id === accountID) && bankingGateForAccount(accountID) === null }
    readonly property bool canTransfer: isUnlocked && transferFormAccounts.length > 0 && syncingConnectionID === null && activeTransfer === null

    /// TransferSheetView.groupedIBAN: upper-case, only A–Z/0–9, groups of four.
    function groupedIBAN(raw) { return (raw || "").toUpperCase().replace(/[^A-Z0-9]/g, "").replace(/(.{4})(?=.)/g, "$1 ") }
    function looksLikeBIC(s) { return /^[A-Z]{6}[A-Z0-9]{2}([A-Z0-9]{3})?$/.test((s || "").toUpperCase()) }
    function validateIban(iban) { try { return core("validate_iban", { iban: iban }) } catch (e) { return false } }
    /// ClipboardPaymentCheck (#663): opt-in (off), per device. Only a hash of the content last
    /// offered is kept, in memory; the text itself is never stored or logged.
    property bool clipboardCheck: String(defaults.value("transfer/clipboardCheck", false)) === "true"
    onClipboardCheckChanged: defaults.setValue("transfer/clipboardCheck", clipboardCheck)
    property var clipboardHandledHash: null
    /// parseAmountToCents: null when unparsable.
    /// True when a password manager marked the clipboard entry secret (mime types only, no content);
    /// also when the check fails, so the entry is then not read.
    function clipboardConcealed() { try { return core("clipboard_concealed") !== false } catch (e) { return true } }
    function clipboardText() { clipboardHelper.text = ""; clipboardHelper.paste(); const t = clipboardHelper.text; clipboardHelper.text = ""; return t }

    /// Pre-selects the sidebar account if it is offered in the form, else the first one.
    function openTransfer() {
        if (!canTransfer) return
        const offered = transferFormAccounts
        const has = id => offered.some(a => a.id === id)
        const selected = sidebarSelection.startsWith("account:") && has(Number(sidebarSelection.slice(8))) ? Number(sidebarSelection.slice(8)) : null
        if (transferDraft.account_id === null || !has(transferDraft.account_id))
            setDraft({ account_id: selected !== null ? selected : (offered.length ? offered[0].id : null) })
        else if (selected !== null)
            setDraft({ account_id: selected })
        transferError = ""
        transferInstantDeclined = null
        showTransferSheet = true
    }
    /// After an instant transfer was declined (nothing sent): send the same draft
    /// as a regular SEPA transfer – a new order with its own approval.
    function sendAsRegularTransfer() {
        transferInstantDeclined = null
        setDraft({ instant: false })
        startTransfer()
    }
    /// Context menu: new transfer pre-filled from a booking.
    function openTransferFrom(b) {
        if (!canTransfer) return
        setDraft({ name: b.counterparty === "" ? b.merchant_name : b.counterparty, iban: groupedIBAN(b.counterparty_iban), bic: "",
                   amountText: Fmt.plain(Math.abs(b.amount_cents)), purpose: (b.purpose || "").slice(0, 140) })
        openTransfer()
    }
    /// Sammelüberweisung / Dauerauftrag (Payments.qml): the TransferSession `session` of the
    /// core runs through the same steps as a single transfer. `kind` = "batch" | "standing".
    function runPaymentSession(session, connection, kind) {
        transferError = ""
        transferInstantDeclined = null
        transferBusy = true
        activeTransferIsBatch = kind === "batch"
        activeTransferIsStanding = kind === "standing"
        activeTransferIsDebit = kind === "debit"
        activeTransfer = session
        coreAsync("session_start", { session: session }, step => { if (activeTransfer === session) handleTransfer(step, connection) },
                  err => { if (activeTransfer === session) finishTransfer(err) })
    }
    function suggestRecipients(query) { try { return core("suggest_recipients", { query: query, limit: 5 }) } catch (e) { return [] } }
    // AppModel+Transfer.swift › knownRecipients / setRecipientHidden ("Empfänger verwalten", #978)
    // throws: a failed read must not read as "no recipients"
    function knownRecipients(query) { return core("list_known_recipients", { query: query }) }
    function setRecipientHidden(iban, hidden) { run(() => core("set_recipient_hidden", { iban: iban, hidden: hidden })) }

    function startTransfer() {
        withFreshBankingPolicy(() => {
            const gate = bankingGateForAccount(transferDraft.account_id)
            if (gate) { transferError = gate.message; return }
            startTransferChecked()
        })
    }
    function startTransferChecked() {
        const d = transferDraft
        const accID = d.account_id
        if (accID === null || !canSendTransfer(accID)) return
        const account = accountsByID[accID]
        const connection = account ? connections.find(c => c.id === account.connection_id) : null
        if (!connection) return
        let cents
        try { cents = core("parse_amount_to_cents", { text: d.amountText }) } catch (e) { transferError = e.message; return }
        const bic = d.bic.trim()
        // Instant and scheduled transfers are FinTS-only.
        const offersInstant = offersInstantChoice(account.connector)
        const request = { account_id: accID, recipient_name: d.name, recipient_iban: d.iban, recipient_bic: bic === "" ? null : bic,
                          amount_cents: cents, purpose: d.purpose, instant: d.instant && offersInstant,
                          execution_date: offersInstant ? executionDateISO(d) : null }
        const st = connectionStatus(connection.id)
        const stored = st ? st.has_pin : false
        if (!isDemo && !stored && d.pin === "") { transferError = qsTr("Bitte die Banking-PIN eingeben."); return }
        let session
        try { session = core("begin_transfer", { request: request, secret: isDemo ? "demo" : d.pin, demo: isDemo }) }
        catch (e) { transferError = e.message; return }
        transferError = ""
        transferInstantDeclined = null
        transferBusy = true
        showTransferSheet = false
        activeTransfer = session
        coreAsync("session_start", { session: session }, step => { if (activeTransfer === session) handleTransfer(step, connection) },
                  err => { if (activeTransfer === session) finishTransfer(err) })
    }

    function handleTransfer(step, connection) {
        if (!step.NeedsPayeeConfirmation) payeeCheck = null
        if (step.NeedsPayeeConfirmation) {
            pollTimer.stop()
            activeChallenge = null
            payeeCheck = { connection: connection, check: step.NeedsPayeeConfirmation.check, busy: false }
        } else if (step.NeedsChallenge) {
            const c = step.NeedsChallenge.challenge
            activeChallenge = { connection: connection, challenge: c, waiting: false, busy: false, error: "" }
            if (c.kind === "DecoupledPush" || c.kind === "Progress") schedulePoll(connection, c.poll_interval_seconds)
        } else if (step.StillWaiting) {
            const c = step.StillWaiting.challenge
            if (activeChallenge === null) activeChallenge = { connection: connection, challenge: c, waiting: true, busy: false, error: "" }
            else activeChallenge = Object.assign({}, activeChallenge, { challenge: c, waiting: true, busy: false })
            schedulePoll(connection, c.poll_interval_seconds)
        } else if (step.Finished) {
            pollTimer.stop()
            activeChallenge = null; activeTransfer = null; transferBusy = false
            // #808: executed but not stored – still finish as sent (no resend), then warn
            const recordError = step.Finished.outcome.record_error
            if (recordError) {
                const debit = activeTransferIsDebit
                Qt.callLater(() => reportUnrecordedOrder(recordError, debit))
            }
            if (activeTransferIsDebit) { activeTransferIsDebit = false; DirectDebits.finishGroup(); return }
            if (activeTransferIsStanding) {
                activeTransferIsStanding = false
                Payments.finishStandingOrder()
                reloadAll()
                return
            }
            Payments.plannedVersion++ // a scheduled transfer now shows under „Geplant“
            transferDraft = newTransferDraft(transferDraft.account_id)
            if (activeTransferIsBatch) { activeTransferIsBatch = false; Payments.resetBatchDraft() }
            toast = step.Finished.outcome.instant ? qsTr("Echtzeitüberweisung freigegeben und vorgemerkt")
                                                  : qsTr("Überweisung freigegeben und vorgemerkt")
            reloadAll()
        } else if (step.InstantDeclined) {
            // Nothing was sent. Keep the draft and let the user decide.
            pollTimer.stop()
            activeChallenge = null; activeTransfer = null; transferBusy = false
            transferError = ""
            transferInstantDeclined = step.InstantDeclined.reason
            showTransferSheet = true
        } else if (step.OutcomeUnknown) {
            // May have been executed: no retry button, clear the draft so it
            // cannot be resent with one click, and say so clearly.
            pollTimer.stop()
            activeChallenge = null; activeTransfer = null; transferBusy = false
            if (activeTransferIsDebit) { activeTransferIsDebit = false; DirectDebits.outcomeUnknown(step.OutcomeUnknown.reason); reloadAll(); return }
            if (activeTransferIsStanding) {
                activeTransferIsStanding = false
                Payments.standingOrderOutcomeUnknown(step.OutcomeUnknown.reason)
                reloadAll()
                return
            }
            transferDraft = newTransferDraft(transferDraft.account_id)
            if (activeTransferIsBatch) { activeTransferIsBatch = false; Payments.resetBatchDraft() }
            transferUnknown = { reason: step.OutcomeUnknown.reason, end_to_end_id: step.OutcomeUnknown.end_to_end_id }
            reloadAll()
        }
    }
    /// schedulePoll / submitCode / cancelSync: transfer branches.
    function pollTransfer(connection) {
        const t = activeTransfer
        coreAsync("session_poll", { session: t }, step => { if (activeTransfer === t) handleTransfer(step, connection) },
                  err => { if (activeTransfer === t) finishTransfer(err) })
    }
    function submitTransferCode(code) {
        const t = activeTransfer, ch = activeChallenge
        activeChallenge = Object.assign({}, ch, { busy: true, error: "" })
        // cancelled while the TAN was on its way: the cancel notice owns the sheet
        coreAsync("session_submit", { session: t, code: code }, step => { if (activeTransfer === t) handleTransfer(step, ch.connection) },
                  err => { if (activeTransfer === t) finishTransfer(err) })
    }
    function cancelTransfer() {
        pollTimer.stop()
        const t = activeTransfer
        activeTransfer = null; activeChallenge = null; transferBusy = false
        transferError = ""
        // A collection run: its sheet comes back with a notice (AppModel+Sync.swift › cancelSync).
        if (activeTransferIsDebit) {
            activeTransferIsDebit = false
            DirectDebits.cancelRun()
            cancelTransferSession(t, null)
            return
        }
        // A batch or a standing order (#533): its own sheet comes back (the rows are still
        // there), and the flag must not make the next single transfer end as one of them.
        if (activeTransferIsBatch || activeTransferIsStanding) {
            const batch = activeTransferIsBatch
            activeTransferIsBatch = false; activeTransferIsStanding = false
            if (batch) Payments.cancelBatchRun(); else Payments.cancelStandingOrderRun()
            cancelTransferSession(t, null)
            return
        }
        // The bank has to confirm: a bunq draft stays approvable until it is rejected.
        cancelTransferSession(t, qsTr("Überweisung abgebrochen – es wurde nichts gesendet, solange die TAN nicht bestätigt wurde."))
    }

    // MARK: transfer cancellation (AppModel+Transfer.swift › cancelTransferSession,
    // AppModelTypes.swift › UnconfirmedTransferCancel, RootView "Stornierung nicht bestätigt")
    /// `{ session, outcome, busy }` while the bank has not confirmed a cancellation.
    property var transferCancelNotice: null
    /// Cancels the session and tells apart a confirmed cancellation from one the bank has
    /// not confirmed. `confirmedMessage` replaces the sheet's error line only when confirmed.
    function cancelTransferSession(session, confirmedMessage) {
        if (session === null || session === undefined) return
        coreAsync("session_cancel", { session: session },
                  r => settleTransferCancel(session, r ? r.outcome : null, confirmedMessage),
                  () => settleTransferCancel(session, null, confirmedMessage))
    }
    function settleTransferCancel(session, outcome, confirmedMessage) {
        if (outcome === null || outcome === undefined || outcome === "Cancelled") {
            transferCancelNotice = null
            if (confirmedMessage) { transferError = confirmedMessage; showTransferSheet = true }
        } else {
            transferError = ""
            transferCancelNotice = { session: session, outcome: outcome, busy: false }
        }
    }
    /// "Status erneut prüfen": cancel again; the core re-queries before it resends anything.
    function recheckTransferCancel() {
        const n = transferCancelNotice
        if (!n || n.busy) return
        transferCancelNotice = Object.assign({}, n, { busy: true })
        cancelTransferSession(n.session, qsTr("Überweisung bei bunq storniert (bestätigt)."))
    }
    function transferCancelMessage(outcome) {
        if (!outcome || outcome === "Cancelled") return ""
        const kind = Object.keys(outcome)[0], reason = outcome[kind].reason || ""
        const head = kind === "CancellationPending" ? qsTr("Die Stornierung wurde von der Bank angenommen, aber noch nicht bestätigt.")
                   : kind === "CancellationUnknown" ? qsTr("Es ist unklar, ob die Überweisung storniert wurde (keine eindeutige Antwort der Bank).")
                   : qsTr("Die Überweisung konnte nicht storniert werden.")
        const advice = qsTr("Sie kann möglicherweise noch in der bunq-App freigegeben werden. Prüfen Sie den Status erneut oder lehnen Sie sie dort ab.")
        return head + "\n\n" + reason + "\n\n" + advice
    }

    /// Name check: send anyway (the bank then asks for the approval).
    function confirmPayee() {
        const t = activeTransfer, pending = payeeCheck
        if (t === null || !pending) return
        payeeCheck = Object.assign({}, pending, { busy: true })
        coreAsync("session_confirm_payee", { session: t }, step => { if (activeTransfer === t) handleTransfer(step, pending.connection) },
                  err => { if (activeTransfer === t) finishTransfer(err) })
    }
    /// Name check: do not send. `useBankName` takes over the account holder's
    /// name reported by the bank and reopens the form.
    function cancelPayee(useBankName) {
        const pending = payeeCheck
        if (!pending) return
        const t = activeTransfer
        payeeCheck = null; activeTransfer = null; transferBusy = false
        if (activeTransferIsDebit) {
            activeTransferIsDebit = false
            DirectDebits.failRun(qsTr("Überweisung abgebrochen – es wurde nichts gesendet."))
            if (t !== null) cancelTransferSession(t, null)
            return
        }
        // Linux: a batch or standing order goes back to its own sheet (the Mac reopens the transfer form)
        if (activeTransferIsBatch || activeTransferIsStanding) {
            const batch = activeTransferIsBatch
            activeTransferIsBatch = false; activeTransferIsStanding = false
            transferError = qsTr("Überweisung abgebrochen – es wurde nichts gesendet.")
            if (batch) Payments.failBatch(); else Payments.failStandingOrder()
            if (t !== null) cancelTransferSession(t, null)
            return
        }
        if (useBankName && pending.check.bank_name) {
            setDraft({ name: pending.check.bank_name })
            transferError = qsTr("Name des Kontoinhabers übernommen – bitte prüfen und erneut freigeben. Es wurde nichts gesendet.")
        } else {
            transferError = qsTr("Überweisung abgebrochen – es wurde nichts gesendet.")
        }
        showTransferSheet = true
        if (t !== null) cancelTransferSession(t, null)
    }
    function finishTransfer(err) {
        pollTimer.stop()
        payeeCheck = null; activeChallenge = null; activeTransfer = null; transferBusy = false
        setDraft({ pin: "" })
        transferError = parseCoreError(err).message
        if (activeTransferIsDebit) { activeTransferIsDebit = false; transferError = ""; DirectDebits.failRun(parseCoreError(err).message) }
        else if (activeTransferIsStanding) { activeTransferIsStanding = false; Payments.failStandingOrder() }
        else if (activeTransferIsBatch) { activeTransferIsBatch = false; Payments.failBatch() }
        else showTransferSheet = true
    }
    /// endBankDialogsForLock (transfer part): no session keeps running, no PIN stays in memory.
    function endTransferForLock() {
        pollTimer.stop()
        const t = activeTransfer
        if (t !== null) activeChallenge = null
        activeTransfer = null; payeeCheck = null; transferBusy = false; showTransferSheet = false
        activeTransferIsBatch = false; activeTransferIsStanding = false; activeTransferIsDebit = false
        setDraft({ pin: "" })
        Payments.endForLock()
        DirectDebits.endForLock()
        if (t !== null) coreAsync("session_cancel", { session: t }, null, () => {})
    }

    /// AppModel.endBankDialogsForLock: locking ends every bank dialog – no session keeps
    /// writing into the vault, no PIN/TAN prompt comes back after unlocking, no PIN stays
    /// in memory. The core cancels the sessions themselves in `lock` (sessions::cancel_all).
    function endBankDialogsForLock() {
        pollTimer.stop()
        const s = activeSession
        endTransferForLock()
        transferCancelNotice = null
        activeSession = null; activeChallenge = null; payeeCheck = null
        pendingLogin = null; setupRequest = null; webLoginConnection = null
        syncingConnectionID = null; statementsAccountID = null; postboxConnectionID = null
        quietSyncConnectionID = null
        failedSyncSecret = ""
        syncGeneration++
        resetSyncBatch()
        if (s !== null) coreAsync("session_cancel", { session: s }, null, () => {})
    }

    // Transfer templates (PlanningUI.swift)
    function transferTemplates() { try { return core("list_transfer_templates") } catch (e) { return [] } }
    function saveTransferTemplate(name) {
        const d = transferDraft
        const c = parseCents(d.amountText)
        const t = { id: 0, name: name, recipient_name: d.name, recipient_iban: d.iban, recipient_bic: d.bic,
                    default_amount_cents: c !== null && c > 0 ? c : null, default_purpose: d.purpose }
        try { core("save_transfer_template", { template: t }) } catch (e) { transferError = e.message }
    }
    function applyTransferTemplate(t) {
        setDraft({ name: t.recipient_name, iban: groupedIBAN(t.recipient_iban), bic: t.recipient_bic,
                   amountText: t.default_amount_cents !== null ? Fmt.plain(t.default_amount_cents) : "",
                   purpose: t.default_purpose.slice(0, 140) })
    }
    function deleteTransferTemplate(id) { try { core("delete_transfer_template", { id: id }) } catch (e) {} }

    // MARK: - Settings & recurring
    // SettingsView.swift, ConnectionsUI.swift (edit sheet), RecurringUI.swift,
    // PlanningUI.swift › CustomPeriodForm, SyncLog.swift, HelpView.swift, AutoReview.swift.

    /// The settings window (macOS `Settings` scene) and its tab: general | banks | security.
    property bool showSettings: false
    property string settingsTab: "general"
    /// `--demo-settings banks:edit|add|remove`: sheet the bank settings open on appear.
    property string demoSettingsSheet: ""
    function openSettings(tab) { if (tab) settingsTab = tab; showSettings = true }
    property bool showSyncLog: false
    property bool showHelp: false
    /// Hilfe › Problem melden … (BugReportWindow, #301)
    property bool showBugReport: false
    /// PrivacyRedaction.swift `ReportRedaction` as bits: set while the bug report takes its
    /// screenshot, so the main window draws `Sensitive {}` content as opaque boxes.
    readonly property int redactPersonal: 1
    readonly property int redactLabels: 2
    /// `.amounts` (#719): money, balances, sums, shares, chart axis values and readouts.
    readonly property int redactAmounts: 4
    property int reportRedaction: 0
    /// (developer) `--redact` / `--redact=labels`: main window drawn as in the screenshot;
    /// `--privacy[=all]` (LaunchOptions.swift): as in privacy mode (amounts, with `=all` personal data too).
    readonly property int launchRedaction: (launchArgs.indexOf("--redact=labels") >= 0 ? redactPersonal | redactLabels
        : launchArgs.indexOf("--redact") >= 0 ? redactPersonal : 0)
        | (launchArgs.indexOf("--privacy=all") >= 0 ? PrivacyMode.scopeRedaction("personal") : 0)
        | (launchArgs.indexOf("--privacy") >= 0 ? PrivacyMode.scopeRedaction("amounts") : 0)
    /// The bug report's redaction united with privacy mode (`privacyModeEnvironment()`).
    readonly property int redaction: reportRedaction | launchRedaction | PrivacyMode.redaction
    /// `ReportRedaction.effective`: what is actually hidden, `.personal` implies `.amounts`.
    readonly property int effectiveRedaction: (redaction & redactPersonal) !== 0 ? redaction | redactAmounts : redaction
    /// `ReportRedaction.masking(_:_:)`: `text`, or the placeholder while `kind` (default
    /// personal) is hidden – for tooltips and other parameter texts with names or amounts.
    function masking(text, kind) {
        return (effectiveRedaction & (kind === undefined ? redactPersonal : kind)) === 0 || !text ? text : searchPlaceholder
    }
    /// BugReport.searchPlaceholder: stands in for typed search text in the picture.
    readonly property string searchPlaceholder: "•••"
    /// `text`, or the placeholder while the screenshot hides personal data (parameter texts
    /// such as an empty state that repeats the search).
    function redactedSearch(text) { return (redaction & redactPersonal) !== 0 && text !== "" ? searchPlaceholder : text }
    property bool showRecurringSheet: false
    /// Booking whose custom recurrence dates are being edited (CustomDatesSheet).
    property var customRecurringBooking: null
    /// The start/end date dialog of the period filter.
    property bool showCustomPeriod: false
    /// Title of the separate window `--screenshot` grabs ("" = main window).
    property string screenshotWindow: ""

    function selectFromSidebar(selection) {
        const previous = sidebarSelection
        sidebarSelection = selection
        if (autoReviewDelay === "account" && previous !== selection
                && previous.startsWith("account:") && selection.startsWith("account:"))
            markAllChecked([Number(previous.slice(8))], true)
    }

    // AutoReview.swift: "Als geprüft markieren" (per device, like UserDefaults).
    readonly property var autoReviewDelays: [
        { id: "manual", title: qsTr("Nur manuell"), seconds: -1 }, { id: "now", title: qsTr("Sofort"), seconds: 0 },
        { id: "1s", title: qsTr("Nach 1 Sekunde"), seconds: 1 }, { id: "5s", title: qsTr("Nach 5 Sekunden"), seconds: 5 },
        { id: "account", title: qsTr("Bei Kontoauswahl"), seconds: -1 }]
    property string autoReviewDelay: String(defaults.value("bookings/autoReviewDelay", "manual"))
    onAutoReviewDelayChanged: {
        defaults.setValue("bookings/autoReviewDelay", autoReviewDelay)
        if (autoReviewDelay === "account" && autoReviewTimer) autoReviewTimer.stop()
    }
    property Timer autoReviewTimer: Timer {
        property var bookingID: null
        onTriggered: {
            const b = model.selectedBooking
            if (model.selectedBookingID === bookingID && !model.isMultiSelection && b && !b.checked) model.setChecked(true, b)
        }
    }
    property Connections autoReviewHook: Connections {
        target: model
        function onSelectedBookingIDChanged() { model.scheduleAutoReview() }
    }
    /// Marks the selected booking as checked after `autoReviewDelay`, unless the
    /// selection moved on (or became a multiple one) in the meantime.
    function scheduleAutoReview() {
        autoReviewTimer.stop()
        const d = autoReviewDelays.find(x => x.id === autoReviewDelay)
        const b = bookingByID(selectedBookingID)   // the binding may still be stale here
        if (!d || d.seconds < 0 || selectedBookingID === null || isMultiSelection || !b || b.checked) return
        autoReviewTimer.bookingID = selectedBookingID
        autoReviewTimer.interval = Math.max(1, d.seconds * 1000)
        autoReviewTimer.start()
    }

    // Custom period (GitHub #53): remembered across launches.
    Component.onCompleted: {
        AutoSync.stop()   // creates the schedule singleton; it starts once the own vault is open
        const d = periodRange(previousMonthPeriod())
        customPeriodFrom = String(defaults.value("period/customFrom", "")) || d[0]
        customPeriodTo = String(defaults.value("period/customTo", "")) || d[1]
    }
    /// Switches to the user-defined period; swaps the dates if needed (ISO strings).
    function applyCustomPeriod(from, to) {
        const a = from <= to ? from : to, b = from <= to ? to : from
        customPeriodFrom = a
        customPeriodTo = b
        defaults.setValue("period/customFrom", a)
        defaults.setValue("period/customTo", b)
        if (period === "custom") { listLimit = defaultListLimit; reloadSelection(true) } else period = "custom"
    }

    // FinTS product registration (GiroCrab's own vs. the user's).
    readonly property bool fintsBuiltinAvailable: core("fints_builtin_product_id_available")
    /// Stored choice: "builtin", "own" or "" (not decided yet).
    property string fintsProductIDMode: ""
    property bool offerProductIDChoice: false
    onSettingsChanged: reloadProductIDState()
    /// New users start with GiroCrab's registration; users who already entered their own are asked once.
    function reloadProductIDState() {
        if (!isUnlocked) return
        try {
            let mode = core("fints_product_id_mode")
            const own = settingEntry("fints.product_id")
            const hasOwn = !!(own && own.has_value)
            if (fintsBuiltinAvailable && mode === "" && !hasOwn && !isDemo) {
                core("set_fints_product_id_mode", { mode: "builtin" })
                mode = "builtin"
                fintsProductIDSource = core("fints_product_id_source")
            }
            fintsProductIDMode = mode
            offerProductIDChoice = fintsBuiltinAvailable && mode === "" && hasOwn && !isDemo
        } catch (e) { console.warn(e.message) }
    }
    /// Stores the choice. The user's own ID always stays in the vault.
    function setFintsProductIDMode(mode) {
        run(() => core("set_fints_product_id_mode", { mode: mode }))
        offerProductIDChoice = false
        reloadSettings()
    }

    readonly property var settingLabels: ({
        "fints.product_id": qsTr("FinTS-Produktregistrierung"),
        "fints.url.52090000": qsTr("FinTS-Server Volksbank Kassel Göttingen"),
        "fints.url.50050201": qsTr("FinTS-Server 1822direkt"),
        "comdirect.client_id": qsTr("comdirect Client-ID"),
        "comdirect.client_secret": qsTr("comdirect Client-Secret"),
        "paypal.client_id": qsTr("PayPal Client-ID"),
        "paypal.client_secret": qsTr("PayPal Secret"),
        "kontist.client_id": qsTr("Kontist Client-ID"),
        "kontist.client_secret": qsTr("Kontist Client-Secret"),
        "enablebanking.application_id": qsTr("Enable Banking Application-ID"),
        "enablebanking.private_key_pem": qsTr("Enable Banking Private Key"),
        "enablebanking.redirect_url": qsTr("Enable Banking Redirect-URL") })
    function settingLabel(key) {
        const e = settingEntry(key)
        return e ? e.label : (settingLabels[key] || (key.startsWith("fints.url.") ? qsTr("FinTS-URL") : key))
    }
    /// Value of a (secret) setting, "" when unset.
    function revealSetting(key) { try { return core("get_setting", { key: key }) || "" } catch (e) { return "" } }
    function clearSession(id) { run(() => core("clear_connection_session", { connection_id: id })); toast = qsTr("Sitzung gelöscht") }
    /// PSD2Consent.swift › reconnect: new consent at the bank.
    function reconnect(connection) {
        if (!run(() => { core("clear_connection_session", { connection_id: connection.id }); return true })) return
        requestSync(connections.find(c => c.id === connection.id) || connection)
    }
    /// Enable Banking: checks application ID + key before the first bank login (network, off the GUI thread).
    function checkEnableBankingApplication(id, pem, redirect, done, failed) {
        coreAsync("check_enable_banking_application", { application_id: id, private_key_pem: pem, redirect_url: redirect }, done, failed)
    }

    /// EnableBankingKeyFile.read: PEM text and, if the file name carries it, the application ID.
    /// Throws a readable message for anything else.
    function readEnableBankingKeyFile(url) {
        const path = localPath(url)
        const text = core("read_text_file", { path: path })
        if (text.indexOf("-----BEGIN") < 0 || text.indexOf("PRIVATE KEY-----") < 0)
            throw new Error(qsTr("Das ist kein privater Schlüssel. Bitte die .pem-Datei wählen, die enablebanking.com beim Registrieren heruntergeladen hat."))
        const name = path.split("/").pop()
        const m = /[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/.exec(name)
        return { pem: text.trim(), applicationID: m ? m[0].toLowerCase() : null }
    }

    // Vault key (VaultKeyManager): Secret Service instead of the Keychain.
    /// Base64 key for the backup; `done(key)`. The keyring may ask for its password.
    function exportVaultKey(done) { coreAsync("export_key", {}, done) }
    /// Clipboard that forgets the key after 60 s (VaultKeyManager.copyToPasteboard).
    function copySecret(text) { run(() => core("copy_secret", { text: text })) }
    /// Restores / adopts a key: rekeys the open vault, or checks it against the locked vault file.
    function importVaultKey(text, done, failed) {
        coreAsync("import_key", { text: text }, () => {
            toast = qsTr("Tresor-Schlüssel übernommen")
            vaultInfo = core("vault_info")
            if (done) done()
            // restored while locked: open the vault with it right away
            if (!hasOwnVault && (vaultState === "locked" || vaultState === "failed")) { vaultState = "locked"; unlock() }
        }, failed)
    }

    // Recurring (RecurringUI.swift): series hidden with "Nicht wiederkehrend".
    /// Set by refreshRecurring() (`recurring_snapshot`).
    property var recurringHidden: []
    /// "Monatlich" or "Individuell (15.03., 15.08.)".
    function intervalText(s) {
        if (s.interval !== "Custom" || !s.custom_dates || !s.custom_dates.length) return intervalLabels[s.interval]
        return intervalLabels[s.interval] + " (" + s.custom_dates.map(customDateShort).join(", ") + ")"
    }
    /// "15.03." for "03-15".
    function customDateShort(mmdd) { const p = mmdd.split("-"); return p.length === 2 ? p[1] + "." + p[0] + "." : mmdd }
    /// "15. März" for "03-15".
    function customDateDisplay(mmdd) {
        const p = mmdd.split("-").map(Number)
        if (p.length !== 2 || isNaN(p[0]) || isNaN(p[1])) return mmdd
        return new Date(2024, p[0] - 1, p[1]).toLocaleDateString(Fmt.locale, Fmt.pattern("d. MMMM"))
    }
    /// CustomDates.parse: "15.03.", "15.3", "03-15" → valid MM-DD (sorted) and unreadable tokens.
    function parseCustomDates(text) {
        const valid = [], invalid = []
        for (const token of text.split(/[,;\s]+/).filter(t => t !== "")) {
            const t = token.endsWith(".") ? token.slice(0, -1) : token
            let m = NaN, d = NaN
            if (t.indexOf("-") >= 0) { const p = t.split("-"); if (p.length === 2 && /^\d+$/.test(p[0]) && /^\d+$/.test(p[1])) { m = Number(p[0]); d = Number(p[1]) } }
            else { const p = t.split("."); if (p.length === 2 && /^\d+$/.test(p[0]) && /^\d+$/.test(p[1])) { d = Number(p[0]); m = Number(p[1]) } }
            // a leap year accepts 29.02.
            const date = new Date(2024, m - 1, d)
            if (m >= 1 && m <= 12 && d >= 1 && date.getMonth() === m - 1 && date.getDate() === d) {
                const key = (m < 10 ? "0" : "") + m + "-" + (d < 10 ? "0" : "") + d
                if (valid.indexOf(key) < 0) valid.push(key)
            } else invalid.push(token)
        }
        return { valid: valid.sort(), invalid: invalid }
    }
    /// Selects a booking in the list, widening the filters if it is not loaded.
    function revealBooking(id) {
        if (!bookings.some(b => b.id === id)) {
            sidebarSelection = "all"; period = "all"; directionFilter = "All"; statusFilter = "all"; TagFilter.clear(); searchText = ""
        }
        revealTimer.bookingID = id
        revealTimer.restart()
    }
    property Timer revealTimer: Timer {
        property var bookingID: null
        interval: 350
        onTriggered: {
            // an older booking is not on the first page: load back to its day
            const id = bookingID
            if (!model.bookings.some(b => b.id === id)) {
                const day = model.run(() => model.core("booking_date", { booking_id: id }))
                if (day && model.page) {
                    model.listLimit = Math.max(model.listLimit, model.page.total_count)
                    model.reloadBookings(() => model.selectBooking(id, false, false))
                    return
                }
            }
            model.selectBooking(id, false, false)
        }
    }

    // Abruf-Protokoll (SyncLog.swift, GitHub #238)
    /// Bumped whenever a sync wrote a row; the log window reloads on it.
    property int syncLogVersion: 0
    /// Appends a finished sync; the core scrubs logins, PINs, TANs and long numbers.
    /// SyncLog.swift › syncLogSource(for:): what started the running sync (`connectionID`
    /// defaults to the running one).
    function syncLogSource(connectionID) {
        if (statementsAccountID !== null) return "Statements"
        const id = connectionID === undefined ? syncingConnectionID : connectionID
        if (id !== null && quietSyncConnectionID === id) return "Automatic"
        return "Manual"
    }
    function logSync(connectionID, outcome, message, imported, source) {
        if (!isUnlocked) return
        try {
            core("log_sync", { connection_id: connectionID, source: source || syncLogSource(), outcome: outcome, message: message || "", imported: Math.max(0, imported || 0) })
            syncLogVersion++
        } catch (e) { /* the log must never get in the way of a sync */ }
    }
    readonly property var syncLogOutcomeTitles: ({ Ok: qsTr("Erfolgreich"), Error: qsTr("Fehler"), Cancelled: qsTr("Abgebrochen") })
    readonly property var syncLogSourceTitles: ({ Manual: qsTr("Manuell"), Automatic: qsTr("Automatisch"), Statements: qsTr("Kontoauszüge") })
    /// CSV of the rows the log window shows.
    function syncLogCSV(entries) {
        const field = s => "\"" + String(s).replace(/"/g, "\"\"") + "\""
        const pad = n => (n < 10 ? "0" : "") + n
        const lines = ["Zeit;Konto;Auslöser;Ergebnis;Neue Buchungen;Meldung"]
        for (const e of entries) {
            const d = new Date(e.timestamp_ms)
            const off = -d.getTimezoneOffset(), sign = off >= 0 ? "+" : "-"
            const date = Fmt.iso(d) + "T" + pad(d.getHours()) + ":" + pad(d.getMinutes()) + ":" + pad(d.getSeconds())
                + sign + pad(Math.floor(Math.abs(off) / 60)) + ":" + pad(Math.abs(off) % 60)
            lines.push([date, field(e.connection_name), syncLogSourceTitles[e.source], syncLogOutcomeTitles[e.outcome], String(e.imported), field(e.message)].join(";"))
        }
        return lines.join("\n") + "\n"
    }

    // MARK: - Demo launch options (LaunchOptions.swift, AppModel.swift › init)
    /// `--view`, `--select`, `--demo-filter`, `--pick`, `--demo-sync`: right after the demo opened.
    function applyLaunchSelection() {
        switch (launchValue("--view")) {
        case "alltag": scope = "Everyday"; break
        case "sparplaene": case "sparpläne": case "investments": scope = "Investments"; break
        case "belege": case "documents": sidebarSelection = "documents"; break
        case "vermoegen": case "vermögen": case "wealth": sidebarSelection = "wealth"; break
        case "vermoegen-cash": sidebarSelection = "wealth"; wealthScope = "cash"; break
        case "vermoegen-wertpapiere": sidebarSelection = "wealth"; wealthScope = "securities"; break
        case "vermoegen-sachwerte": sidebarSelection = "wealth"; wealthScope = "tangibles"; break
        case "geplant": case "planned": sidebarSelection = "planned"; break
        case "lastschriften": case "debits": sidebarSelection = "directDebits"; break
        }
        const q = launchValue("--select")
        const account = q ? allAccounts.find(a => a.name.toLowerCase().includes(q)) : null
        if (account) {
            sidebarSelection = "account:" + account.id
            selectedBookingID = null
        }
        switch (launchValue("--demo-filter")) {
        case "income": directionFilter = "Income"; break
        case "expense": case "expenses": directionFilter = "Expense"; break
        }
        reloadBookings()
        if (account && selectedBookingID === null && bookings.length) selectedBookingID = bookings[0].id
        const p = launchValue("--pick")
        const pick = p ? bookings.find(b => (b.merchant_name || "").toLowerCase().includes(p) || (b.counterparty || "").toLowerCase().includes(p)) : null
        if (pick) selectedBookingID = pick.id
        // --merchant-list: with --pick, open the list of that booking's merchant (#495)
        if (pick && launchArgs.indexOf("--merchant-list") >= 0) MerchantFocus.focusMerchant(pick)
        // Sachwerte (#87): --pick also selects an item by name
        const item = p && account && isTangible(account.id) ? assetItems(account.id).find(i => i.setup.name.toLowerCase().includes(p)) : null
        if (item) selectAssetItem(item.id)
        if (optionDemoSync) Qt.callLater(requestSyncAll)
    }
    /// `--demo-sheet` / `--demo-settings`: presented once, shortly after the vault is open.
    property bool demoDialogsPresented: false
    property Timer demoDialogTimer: Timer {
        interval: 600
        running: model.vaultState === "open" && !model.demoDialogsPresented
        onTriggered: model.presentLaunchOptions()
    }
    function presentLaunchOptions() {
        demoDialogsPresented = true
        if (demoSettings !== "") {
            const parts = demoSettings.split(":")
            if (parts[0] === "groups") {
                // the group editor is a sheet of the main window (Swift: showGroupManager)
                showGroupManager = true
            } else {
                const tabs = { general: "general", banks: "banks", security: "security" }
                settingsTab = tabs[parts[0]] || "general"
                demoSettingsSheet = parts.length > 1 ? parts[1] : ""
                showSettings = true
                screenshotWindow = qsTr("Einstellungen")
            }
        }
        if (demoSheet !== "" && !Payments.presentDemoSheet(demoSheet) && !DirectDebits.presentDemoSheet(demoSheet)) presentDemoSheet(demoSheet)
    }
    /// The one switch for `--demo-sheet <name>` (AppModel.swift › presentDemoSheet):
    /// opens that sheet pre-filled with fictional data.
    function presentDemoSheet(name) {
        const top = categories.find(c => c.depth === 0)
        switch (name) {
        case "account-selection":
            // choice of accounts another login already delivers (#346)
            accountSelectionConnection = connections.length ? connections[connections.length - 1] : null
            break
        case "icon-color": {
            // the "Symbol & Farbe" popover of the first account (#167)
            const a = allAccounts.length ? allAccounts[0] : null
            if (a) iconColorTarget = { type: "account", id: a.id }
            break
        }
        case "icon-color-category": if (top) iconColorCategoryID = top.id; break
        // Sachwerte: asset (new item) | asset-edit | asset-values | asset-quantities (the car)
        case "asset": case "asset-edit": case "asset-values": case "asset-quantities": {
            const account = Object.keys(tangibleAccountIDs).map(Number)[0]
            if (account === undefined) break
            const car = assetItems(account).find(i => i.setup.symbol === "car.side.fill")
            if ((name === "asset-values" || name === "asset-quantities") && car) openAssetAction(name === "asset-values" ? "values" : "quantities", car.id)
            else openAssetEditor(account, name === "asset-edit" && car ? car.id : null)
            break
        }
        case "category": if (top) newCategoryRequest = { parent: top.id }; break
        // Konten › Tags verwalten … (#228)
        // `tags-accounts`: the first tag's bookings per account (TagManagerSheet.qml)
        case "tags": case "tags-accounts": TagManager.open(); break
        // the header's tag filter (#228, part 3) with the first tag ticked
        case "tag-filter":
            if (TagsModel.tags.length) TagFilter.set([TagsModel.tags[0].id], false)
            TagFilter.shown = true
            break
        case "print":
            // Print dialog (GitHub #53) on last month's demo bookings.
            period = previousMonthPeriod()
            showPrintSheet = true
            break
        case "transfer": case "transfer-confirm": case "transfer-clipboard":
            if (transferAccounts.length) sidebarSelection = "account:" + transferAccounts[0].id
            openTransfer()
            // transfer-clipboard: empty form, the sheet offers the payment in its clipboard banner (#663)
            if (demoSheet !== "transfer-clipboard") setDraft({ name: "Stadtwerke München", iban: groupedIBAN("DE89370400440532013000"), amountText: Fmt.plain(8500),
                       purpose: "Abschlag Strom Kd-Nr 482910" })
            break
        case "moneymoney":
            demoImportPreview = {
                total_rows: 4820, new_bookings: 4820, duplicates_skipped_or_enriched: 0, notes_count: 312,
                date_range_start: "2012-01-03", date_range_end: "2026-10-30",
                detected_categories: ["Haushalt > Lebensmittel", "Gehalt", "Steuern", "Wohnen > Miete", "Mobilität > Tanken", "Freizeit > Restaurants", "Versicherungen", "Reisen"],
                sample_bookings: [], suggested_account_name: "Import-Archiv", suggested_account_id: null
            }
            showMoneyMoneyImport = true
            break
        case "moneymoney-db":
            demoDbPreview = demoMoneyMoneyDbPreview(allAccounts.find(a => a.kind === "Checking") || null)
            showMoneyMoneyImport = true
            break
        case "recurring": showRecurringSheet = true; break
        case "rules": openRuleEditor(null); break
        case "rule-edit": reloadRules(); ruleDraft = rules.length ? rules[0] : null; showRuleEditor = true; break
        case "palette": showCommandPalette = true; break
        case "categorizer":
            if (selectedBookingID === null && bookings.length) selectedBookingID = bookings[0].id
            showCategorizer = true
            break
        case "error":
            // the sync error alert of a rejected FinTS registration (product-ID alternatives)
            if (!connections.length) break
            errorTitle = qsTr("Abruf von „%1“ fehlgeschlagen").arg(connections[0].display_name)
            errorRetry = () => {}
            errorAlternatives = [{ title: qsTr("Eigene Registrierung ersetzen …"), action: enterOwnProductID },
                                 { title: qsTr("Mit GiroCrab-Registrierung erneut versuchen"), action: () => {} }]
            errorMessage = "Bank meldet 9078: Das Produkt ist nicht registriert."
            break
        case "payee-match": case "payee-close": case "payee-nomatch": {
            const connection = connections.length ? connections[0] : null
            if (!connection) break
            const result = name === "payee-match" ? "Match" : name === "payee-close" ? "CloseMatch" : "NoMatch"
            payeeCheck = { connection: connection, busy: false, check: {
                result: result, entered_name: "Stadtwerke Muenchen", iban: "DE89370400440532013000",
                bank_name: result === "CloseMatch" ? "Stadtwerke München GmbH" : null, reason: null,
                notice: result === "NoMatch" ? "Wenn Sie die Überweisung trotzdem freigeben, kann das Geld an einen anderen Empfänger gehen." : null } }
            break
        }
        }
    }

    // MARK: - Web logins
    // AppModel.swift › webLoginConnection, applyBrowserSession, PortalUI.swift (model glue),
    // AmazonUI.swift › importAmazonOrders. The sheet is WebLoginSheet.qml (loaded on demand
    // by Main.qml, QtWebEngine stays optional). Cookies stay in the off-the-record browser
    // profile and, for Trade Republic, in the vault; QML never sees them.
    function cardUsesEnableBanking(optIn, hasApp, hasKey) { return (optIn || "").trim() === "1" && hasApp && hasKey }
    function enableBankingOptInKey(kind) { return kind === "AmazonVisa" ? "enablebanking.use.amazonvisa" : "enablebanking.use.amex" }
    /// ConnectorKind.portalURL (+ the Trade Republic login page of WebLoginSheet).
    function portalURL(kind) {
        switch (kind) {
        case "AmericanExpress": return "https://global.americanexpress.com/activity/recent"
        // Amazon Visa customer portal (Open Bank / Zinia); www.openbank.de has no card login.
        case "AmazonVisa": return "https://kunden.openbankpay.com/amazon/login"
        case "Quirion": return "https://banking.quirion.de/login/"
        // starts the B2C sign-in (302 to login.schwaebisch-hall.de), SchwaebischHallScripts.loginURL
        case "SchwaebischHall": return "https://www.schwaebisch-hall.de/bin/bshweb/authentication"
        case "EquatePlus": return "https://www.equateplus.com/EquatePlusParticipant2/?login"
        case "Fnz": return "https://portal.fnz.de/"
        case "AmazonOrders": return "https://www.amazon.de/gp/css/order-history"
        case "TradeRepublic": return "https://app.traderepublic.com/login"
        default: return ""
        }
    }
    /// ConnectorKind.portalTransactionsURL: opened once after sign-in when the portal lands elsewhere.
    function portalTransactionsURL(kind) { return kind === "AmazonVisa" ? "https://kunden.openbankpay.com/amazon/transactions" : "" }
    /// ConnectorKind.portalHosts
    function portalHosts(kind) {
        switch (kind) {
        case "AmericanExpress": return ["americanexpress.com", "aexp.com"]
        case "AmazonVisa": return ["openbankpay.com", "openbank.de", "openbank.com", "zinia.de", "zinia.com"]
        case "AmazonOrders": return ["amazon.de", "amazon.com"]
        case "Fnz": return ["portal.fnz.de"]
        case "Quirion": return ["banking.quirion.de"]
        case "SchwaebischHall": return ["www.schwaebisch-hall.de", "login.schwaebisch-hall.de"]
        case "EquatePlus": return ["www.equateplus.com", "www.emea.equateplus.com", "www.na.equateplus.com"]
        case "TradeRepublic": return ["traderepublic.com"]
        default: return []
        }
    }
    /// PortalDomains(kind:): the portal's hosts, its start page's host and the base domain of each.
    function portalDomains(kind) {
        const all = {}
        let hosts = portalHosts(kind)
        if (kind !== "AmazonOrders" && portalURL(kind)) hosts = hosts.concat([new URL(portalURL(kind)).hostname])
        for (const h0 of hosts) {
            const h = h0.toLowerCase()
            if (!h) continue
            all[h] = true
            all[h.split(".").slice(-2).join(".")] = true
        }
        return Object.keys(all).sort()
    }
    /// PortalDomains.allows: matched on label boundaries (`openbankpay.com.evil.io` is not allowed).
    function domainAllows(domains, host) {
        let h = (host || "").toLowerCase()
        if (!h) return false
        if (h.endsWith(".")) h = h.slice(0, -1)
        return domains.some(d => h === d || h.endsWith("." + d))
    }

    /// Embedded web-portal login unless the card was switched to Enable Banking (PSD2)
    /// under "Erweitert" *and* the credentials are set (PortalUI.swift › usesPortal).
    function usesPortalLogin(c) {
        if (isMagicDemoLogin(c.login_name)) return false
        if (!isDemo && ["AmazonOrders", "Fnz", "Quirion", "SchwaebischHall", "EquatePlus"].indexOf(c.connector) >= 0) return true
        if (isDemo || (c.connector !== "AmericanExpress" && c.connector !== "AmazonVisa")) return false
        const optIn = settingEntry(enableBankingOptInKey(c.connector))
        // the card's own application (#824) or the default one
        const app = connectionEnableBankingApp(c.id)
        return !cardUsesEnableBanking(optIn ? optIn.value : "", !!(app && app.application_id), !!(app && app.has_key))
    }

    /// Not opened while the banking policy blocks this login (security lock); a lock
    /// or vault switch ends the browser login like every other bank dialog.
    property Connections webLoginWatch: Connections {
        target: model
        function onWebLoginConnectionChanged() {
            const c = model.webLoginConnection
            if (!c) return
            const gate = model.bankingGate(c)
            if (gate) {
                model.webLoginConnection = null
                model.errorTitle = model.bankingGateTitle(gate)
                model.errorMessage = model.bankingPolicy.message
            }
        }
        function onVaultStateChanged() { if (model.vaultState !== "open") { model.webLoginConnection = null; model.accountSelectionConnection = null } }
    }

    /// Portal sheet closed after its work (Swift: `model.webLoginConnection = nil`): a batch
    /// ("Konten abrufen" of a group) goes on with the next login instead of stalling.
    function closeWebLogin() {
        webLoginConnection = null
        if (syncBatchTotal > 1) syncNextInQueue()
        else resetSyncBatch()
    }

    /// Trade Republic: cookies of the browser login -> vault -> sync without 2FA.
    function applyBrowserSession(connection) {
        webLoginConnection = null
        try {
            core("apply_web_session", { connection_id: connection.id, domains: portalHosts("TradeRepublic") })
            toast = qsTr("Trade-Republic-Sitzung übernommen")
            startSync(connections.find(c => c.id === connection.id) || connection, "")
        } catch (e) {
            resetSyncBatch()   // like any sync error: ends "Alle abrufen" instead of stalling it
            errorTitle = qsTr("Anmeldung konnte nicht übernommen werden")
            errorMessage = e.message
        }
    }
    /// WebLoginSheet.removeSessionCookies (`sessionOnly`) / clearAllTradeRepublicWebData
    /// (with `connectionId`: also the cookies kept in the vault for that login).
    function clearTradeRepublicWebData(sessionOnly, connectionId) {
        try {
            core("web_clear_cookies", { domains: portalHosts("TradeRepublic"), name: sessionOnly ? "tr_session" : null,
                                        connection_id: sessionOnly ? null : (connectionId === undefined ? null : connectionId) })
        } catch (e) {}
    }
    /// clearPortalSession(kind): the portal's cookies in the browser profile and in the vault.
    function clearPortalSession(kind, connectionId) {
        try { core("web_clear_cookies", { domains: portalDomains(kind), name: null, connection_id: connectionId === undefined ? null : connectionId }) } catch (e) {}
    }
    // Web sessions across restarts (WKWebsiteDataStore.default() on the Mac): the portal's
    // cookies are kept encrypted in the vault per login and put back into the
    // off-the-record profile before the login page loads. Trade Republic's `tr_session`
    // has its own place (the connection's session), so it is left out here.
    function webCookieArgs(c) {
        return { connection_id: c.id, domains: portalDomains(c.connector), skip_names: c.connector === "TradeRepublic" ? ["tr_session"] : [] }
    }
    function saveWebCookies(c) { if (c && isUnlocked) { try { core("web_save_cookies", webCookieArgs(c)) } catch (e) { console.warn("web cookies not saved:", e.message) } } }
    function restoreWebCookies(c) { if (c && isUnlocked) { try { return core("web_restore_cookies", webCookieArgs(c)) } catch (e) { console.warn("web cookies not restored:", e.message) } } return 0 }
    function forgetWebCookies(id) { try { core("web_forget_cookies", { connection_id: id }) } catch (e) {} }
    /// Returns true when bookings were recognised (even if all were already known).
    function importPortalCapture(c, bodies, rowsJSON) {
        try {
            const s = core("import_portal_capture", { connection_id: c.id, json_bodies: bodies, dom_rows_json: rowsJSON })
            let text = qsTr("%1: %2 neue Buchungen, %3 kategorisiert").arg(c.display_name).arg(s.transactions_imported).arg(s.transactions_categorized)
            if (s.transactions_skipped_archived) text += " · " + qsTr("%1 Buchungen archivierter Konten übersprungen").arg(s.transactions_skipped_archived)
            toast = text
            saveWebCookies(c)   // signed in: keep the session
            reloadAll()
            return true
        } catch (e) {
            console.warn("portal import failed:", e.message)
            return false
        }
    }
    /// Number of bookings recognised in the capture right now (nothing stored), off the GUI thread.
    function previewPortalCapture(c, bodies, rowsJSON, done) {
        coreAsync("preview_portal_capture", { connection_id: c.id, json_bodies: bodies, dom_rows_json: rowsJSON },
                  n => done(n || 0), () => done(0))
    }
    /// PortalUI.swift › importStatement: CSV or PDF statement of a portal card.
    function importStatement(c, fileUrl) {
        const path = decodeURIComponent(String(fileUrl).replace(/^file:\/\//, ""))
        const name = path.split("/").pop()
        try {
            const s = core("import_statement_file", { connection_id: c.id, path: path })
            toast = qsTr("%1: %2 neue Buchungen aus %3").arg(c.display_name).arg(s.transactions_imported).arg(name)
            reloadAll()
        } catch (e) {
            if (e.message.startsWith("Ungültige Eingabe")) {
                errorTitle = qsTr("Keine Umsätze erkannt")
                errorMessage = qsTr("In %1 wurden keine Umsätze erkannt. Erwartet: CSV mit Datum, Beschreibung, Betrag oder ein Kontoauszug als PDF mit Textebene.").arg(name)
            } else {
                errorTitle = qsTr("Kontoauszug-Import fehlgeschlagen")
                errorMessage = e.message
            }
        }
    }
    /// AmazonUI.swift › importAmazonOrders. Returns true when orders were recognised.
    function importAmazonOrders(c, json) {
        try {
            const s = core("import_amazon_orders", { connection_id: c.id, orders_json: json })
            toast = qsTr("%1: %2 neue Buchungen, %3 kategorisiert").arg(c.display_name).arg(s.transactions_imported).arg(s.transactions_categorized)
            saveWebCookies(c)   // signed in: keep the session
            reloadAll()
            return true
        } catch (e) {
            errorTitle = qsTr("Amazon-Bestellungen konnten nicht übernommen werden")
            errorMessage = e.message
            return false
        }
    }
    /// `coreAsync` as a promise (FNZ / quirion import steps).
    function corePromise(method, args) {
        return new Promise((resolve, reject) => coreAsync(method, args, resolve, e => reject(new Error(parseCoreError(e).message))))
    }
}
