pragma Singleton
import QtQuick
import QtCore

// Automatischer Abruf (GiroCrab/GiroCrab#546, #567): AutoSync.swift (interval rules) and
// PSD2Consent.swift › autoSyncHours / startAutoSync / autoSyncTick. While GiroCrab runs and the
// own vault is open, at most one login is fetched per tick with `begin_quiet_sync`: never a PIN
// or TAN prompt, never an error dialog (AppModel.quietSyncConnectionID, see startSync,
// handleStep and finishSync). The menu bar with balances of the Mac is macOS-only.
QtObject {
    id: auto

    /// `UserDefaults` of the Mac (`autoSync.hours`, `autoSync.attempt.<id>`): outside the vault
    /// on purpose, known while the vault is locked and right after a relaunch.
    property Settings store: Settings {
        category: "autoSync"
        /// Hours between two automatic fetches; -1 = never set (the former hidden vault
        /// setting `sync.psd2_auto_hours` applies), 0 = off.
        property int hours: -1
    }

    // MARK: AutoSync.swift
    /// Offered in the picker; 0 = off (default).
    readonly property var hourOptions: [0, 1, 2, 4, 6, 12, 24]
    /// Stored hours: a missing, negative or unparsable value means off.
    function hoursFromStored(stored) { return Math.max(0, parseInt(stored) || 0) }
    /// Due when no fetch or attempt is known, or the newer of the two is at least `hours`
    /// ago. The attempt counts too: a login that needs a TAN would otherwise be retried at
    /// every tick.
    function isDue(lastSyncUnix, lastAttemptUnix, hours, nowUnix) {
        if (hours <= 0) return false
        const known = [lastSyncUnix, lastAttemptUnix].filter(x => x !== null && x !== undefined)
        if (!known.length) return true
        return nowUnix - Math.max.apply(null, known) >= hours * 3600
    }
    /// "off" | "skippedLocked" | "notDue" | "run". Locked: nothing runs, the next tick after
    /// the unlock catches up (nothing is queued, the last fetch time is unchanged).
    function decide(hours, isUnlocked, lastSyncUnix, lastAttemptUnix, nowUnix) {
        if (hours <= 0) return "off"
        if (!isUnlocked) return "skippedLocked"
        return isDue(lastSyncUnix, lastAttemptUnix, hours, nowUnix) ? "run" : "notDue"
    }
    function attemptKey(connectionID) { return "attempt." + connectionID }
    /// Picker wording of an interval.
    function label(h) {
        if (h === 0) return qsTr("Aus")
        if (h === 1) return qsTr("Jede Stunde")
        return qsTr("Alle %1 Stunden").arg(h)
    }

    // MARK: PSD2Consent.swift › scheduled sync
    /// Hours between automatic fetches (Einstellungen › Allgemein, 0 = off). Falls back to the
    /// former hidden vault setting `sync.psd2_auto_hours` until the picker has been used once.
    readonly property int effectiveHours: {
        if (store.hours >= 0) return hoursFromStored(store.hours)
        const e = AppModel.settingEntry("sync.psd2_auto_hours")
        return parseInt(e ? (e.value || "").trim() : "") || 0
    }
    /// AutoSyncSettingsSection: the picker shows what the loop uses until it is set once.
    readonly property int selection: store.hours >= 0 ? store.hours : effectiveHours
    function setHours(h) { store.hours = h }

    /// startAutoSync / stopAutoSync: runs while the own vault is open, never in the demo.
    readonly property bool active: AppModel.isUnlocked && !AppModel.isDemo
    onActiveChanged: active ? start() : stop()
    // onActiveChanged does not fire for the initial value: a vault that is already open when the
    // singleton is created must start the schedule too
    Component.onCompleted: if (active) start()
    property Timer timer: Timer {
        repeat: true
        onTriggered: { interval = 15 * 60 * 1000; auto.tick() }
    }
    function start() {
        timer.stop()
        // let the launch / unlock settle first
        timer.interval = 60 * 1000
        timer.start()
    }
    function stop() { timer.stop() }

    /// Nothing else is going on: no sync, no TAN, no transfer, no prompt.
    function idle() {
        const m = AppModel
        // a transfer waiting for its TAN shows `activeChallenge` / `transferBusy`
        return m.isUnlocked && !m.isDemo && m.syncingConnectionID === null && m.activeChallenge === null
            && !m.transferBusy && m.pendingLogin === null && m.webLoginConnection === null
            && m.setupRequest === null && m.syncQueue.length === 0
    }
    /// autoSyncTick: starts at most one quiet sync when nothing else is going on. The
    /// connection states are read off the GUI thread, the sync runs there as well.
    function tick() {
        if (!idle()) return
        const hours = effectiveHours
        if (hours <= 0) return
        // Sachwerte: missing gold prices (at most one request a day, none without gold items)
        AppModel.refreshAssetPrices(false)
        const nowUnix = Math.floor(Date.now() / 1000)
        // never a login prompt: PSD2 needs a valid consent, the others a stored PIN or session
        const candidates = AppModel.connections.filter(c => {
            if (AppModel.usesPortal(c) || c.all_accounts_archived || AppModel.bankingGate(c) !== null) return false
            const attempt = Number(store.value(attemptKey(c.id), 0)) || 0
            return decide(hours, AppModel.isUnlocked, c.last_sync_unix, attempt > 0 ? attempt : null, nowUnix) === "run"
        })
        // the login that has waited longest first (never attempted: 0), so every login gets a turn
        const waited = c => Math.max(c.last_sync_unix || 0, Number(store.value(attemptKey(c.id), 0)) || 0)
        candidates.sort((a, b) => waited(a) - waited(b))
        pick(candidates, 0, AppModel.vaultGeneration, nowUnix)
    }
    /// The first candidate that needs no prompt, checked one after the other.
    function pick(candidates, i, generation, nowUnix) {
        if (i >= candidates.length || generation !== AppModel.vaultGeneration) return
        const c = candidates[i]
        AppModel.coreAsync("connection_status", { connection_id: c.id }, st => {
            if (generation !== AppModel.vaultGeneration) return
            let ok
            if (c.connector === "EnableBanking") {
                const end = st && st.consent_valid_until ? new Date(st.consent_valid_until) : null
                ok = end !== null && !isNaN(end) && end.getTime() > nowUnix * 1000
            } else {
                ok = !!st && (st.has_pin || st.has_session) && AppModel.missingSettings(c).length === 0
            }
            if (ok) run(c, generation, nowUnix)
            else pick(candidates, i + 1, generation, nowUnix)
        }, () => pick(candidates, i + 1, generation, nowUnix))
    }
    function run(due, generation, nowUnix) {
        // Quiet as well: a blocked login is simply not synced (the banner explains why).
        AppModel.withFreshBankingPolicy(() => {
            // switched off while a status check was pending: no fetch any more
            if (generation !== AppModel.vaultGeneration || !idle() || effectiveHours <= 0) return
            const c = AppModel.connections.find(x => x.id === due.id)
            if (!c || AppModel.bankingGate(c) !== null) return
            AppModel.quietSyncConnectionID = c.id
            store.setValue(attemptKey(c.id), nowUnix)
            AppModel.startSync(c, "")
        })
    }
}
