import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore

// WealthView.swift: „Vermögen“ – cash and assets (depots, Sachwerte) at a glance, minus
// liabilities. The scope picker (Alles / Cash / Wertpapiere / Sachwerte) sits in the toolbar
// (Main.qml), like on the Mac. Since #351 (AssetClassificationUI.swift) the values can be
// grouped by asset class or instrument instead of by account, the donut and its legend
// filter the cards below, direct property can be left out, and with several account owners
// chips filter the whole view by owner.
Rectangle {
    id: wv
    color: Theme.canvas
    readonly property string scope: AppModel.wealthScope
    /// @AppStorage("wealth.includeLiabilities") / ("wealth.grouping") / ("wealth.hideUnclassifiedInChart") /
    /// ("wealth.hideDirectProperty"). „Nach Konto“ keeps the cash history, gains and income cards; the class view is opt-in.
    Settings {
        id: prefs
        category: "wealth"
        property bool includeLiabilities: true
        property string grouping: "account"
        property bool hideUnclassifiedInChart: false
        property bool hideDirectProperty: false
    }
    property int historyDays: 90
    property var history: []
    /// ClassifiedAsset lists per depot / Sachwerte account id (loaded for all owners).
    property var classified: ({})
    property var income: ({ dividends: 0, fees: 0, taxes: 0 })
    readonly property bool incomeEmpty: income.dividends === 0 && income.fees === 0 && income.taxes === 0

    // MARK: WealthGrouping
    /// The wrapper (ETF, single security, fund) only tells securities apart.
    readonly property var groupings: scope === "securities" ? ["account", "assetClass", "instrument"] : ["account", "assetClass"]
    /// The stored choice, or the class view where the stored one is not offered.
    readonly property string grouping: groupings.indexOf(prefs.grouping) >= 0 ? prefs.grouping : "assetClass"
    /// Inside one account type the "account" grouping lists the single positions, so it says so.
    function groupingTitle(g) {
        if (g === "account") return scope === "securities" || scope === "tangibles" ? qsTr("Nach Position") : qsTr("Nach Konto")
        return g === "assetClass" ? qsTr("Nach Anlageklasse") : qsTr("Nach Instrument")
    }
    /// Grouped by a classification (asset class or instrument) instead of by account.
    readonly property bool classMode: grouping !== "account" && scope !== "cash"
    /// The selected donut slice: a class or instrument in the grouped view, else a section, account or position.
    property string classFilter: "all"
    /// WealthOwnerFilter: "all", "owner:<name>" or "unassigned".
    property string ownerFilter: "all"

    onScopeChanged: classFilter = "all"
    onOwnerFilterChanged: classFilter = "all"
    Connections {
        target: prefs
        function onGroupingChanged() { wv.classFilter = "all" }
        function onHideDirectPropertyChanged() { wv.classFilter = "all" }
        function onHideUnclassifiedInChartChanged() { if (prefs.hideUnclassifiedInChart && wv.classFilter === "unclassified") wv.classFilter = "all" }
    }

    // MARK: WealthSummary / WealthBucket
    /// Only active EUR accounts that count towards the total; other currencies are a note.
    function makeSummary(accounts) {
        const b = { cash: [], securities: [], tangibles: [], liabilities: [] }
        let foreign = 0
        for (const a of accounts) {
            if (a.archived_unix !== null || !a.in_total) continue
            if (a.currency !== "EUR") { foreign++; continue }
            b[bucket(a)].push(a)
        }
        return summarize(b, foreign)
    }
    function summarize(b, foreign) {
        const sum = list => list.reduce((t, a) => t + a.balance_cents, 0)
        const s = { cash: b.cash, securities: b.securities, tangibles: b.tangibles, liabilities: b.liabilities, foreign: foreign }
        s.cashCents = sum(s.cash)
        s.securitiesCents = sum(s.securities)
        s.tangiblesCents = sum(s.tangibles)
        s.assetsCents = s.securitiesCents + s.tangiblesCents
        /// Debts: the negative balances of cards and loans.
        s.liabilitiesCents = s.liabilities.reduce((t, a) => t + Math.min(a.balance_cents, 0), 0)
        /// A positive card balance (credit) is an asset.
        s.creditCents = s.liabilities.reduce((t, a) => t + Math.max(a.balance_cents, 0), 0)
        s.grossCents = s.cashCents + s.assetsCents + s.creditCents
        return s
    }
    /// WealthSummary.scoped(to:): only the bucket of the selected tab.
    function scoped(s, sc) {
        if (sc === "all") return s
        const b = { cash: [], securities: [], tangibles: [], liabilities: [] }
        b[sc] = s[sc]
        return summarize(b, 0)
    }
    /// WealthBucket.bucket(for:tangible:): Sachwerte are depot-like accounts, but no securities.
    function bucket(a) {
        switch (a.kind) {
        case "Brokerage": return AppModel.isTangible(a.id) ? "tangibles" : "securities"
        case "CreditCard": case "Loan": return "liabilities"
        default: return "cash"
        }
    }
    function netCents(s) { return s.grossCents + (prefs.includeLiabilities ? s.liabilitiesCents : 0) }

    // MARK: WealthOwnerFilter
    function ownerName(a) { return (a.owner || "").trim() }
    function ownerKeeps(option, a) {
        if (option === "all") return true
        if (option === "unassigned") return ownerName(a) === ""
        return ownerName(a) === option.slice(6)
    }
    function accountsOf(option) { return option === "all" ? AppModel.allAccounts : AppModel.allAccounts.filter(a => ownerKeeps(option, a)) }
    /// Every counted account: what is loaded, and what the owner choices are built from.
    readonly property var allSummary: { void AppModel.tangibleAccountIDs; return makeSummary(AppModel.allAccounts) }
    /// The choices worth offering: nothing while all accounts belong to at most one person.
    readonly property var ownerOptions: {
        const s = allSummary
        const names = {}
        for (const a of [].concat(s.cash, s.securities, s.tangibles, s.liabilities)) names[ownerName(a)] = true
        const owners = Object.keys(names).filter(n => n !== "").sort((x, y) => x.toLowerCase().localeCompare(y.toLowerCase()))
        if (owners.length <= 1) return []
        return ["all"].concat(owners.map(o => "owner:" + o)).concat(names[""] ? ["unassigned"] : [])
    }
    /// An owner that no longer exists (renamed, account archived) shows everybody.
    readonly property string activeOwner: ownerOptions.indexOf(ownerFilter) >= 0 ? ownerFilter : "all"
    /// The accounts of the selected owner; everything below derives from this.
    readonly property var fullSummary: { void AppModel.tangibleAccountIDs; return makeSummary(accountsOf(activeOwner)) }

    // MARK: WealthAssetSelection
    /// One view-level selection, shared by totals, charts and both position presentations.
    /// Removing a property subtracts its value once; a missing-position remainder stays intact.
    function select(s, assets, hideDirectProperty) {
        const out = { assets: Object.assign({}, assets), directProperty: { securities: 0, tangibles: 0 } }
        const b = { cash: s.cash, securities: s.securities, tangibles: s.tangibles, liabilities: s.liabilities }
        for (const k of ["securities", "tangibles"]) {
            b[k] = s[k].map(account => {
                const items = assets[account.id] || []
                const properties = items.filter(AssetClassification.isDirectProperty)
                // counted whether hidden or not (the switch must stay reachable)
                out.directProperty[k] += properties.length
                if (!hideDirectProperty || !properties.length) return account
                const kept = items.filter(x => !AssetClassification.isDirectProperty(x))
                out.assets[account.id] = kept
                const adjusted = Object.assign({}, account, { balance_cents: account.balance_cents - properties.reduce((t, p) => t + p.holding.value_cents, 0) })
                return kept.length === 0 && adjusted.balance_cents === 0 ? null : adjusted
            }).filter(a => a !== null)
        }
        out.summary = summarize(b, s.foreign)
        out.directPropertyCount = out.directProperty.securities + out.directProperty.tangibles
        return out
    }
    readonly property var selection: select(fullSummary, classified, prefs.hideDirectProperty)
    /// Limited to the selected tab.
    readonly property var summary: scoped(selection.summary, scope)
    readonly property var positions: aggregate(selection.summary.securities.map(a => (selection.assets[a.id] || []).map(x => x.holding)))
    /// Class view only: the rows of the selected tab, before the class filter.
    readonly property var classRows: classMode ? makeRows(selection.summary, selection.assets).filter(r => scope === "all" || r.scope === scope) : []

    // MARK: WealthClassRow
    function rowClassification(assetClass, source) {
        return { security_key: null, asset_class: assetClass, asset_class_source: source || "Rule", instrument: "unclassified", instrument_source: "Unknown" }
    }
    function makeRows(s, assets) {
        const accountRow = (prefix, a) => ({ id: prefix + a.id, name: a.name, accountName: a.bank_name, cents: a.balance_cents, scope: "cash",
                                             classification: rowClassification("cash"), quantity: null, identifier: "" })
        const rows = s.cash.map(a => accountRow("cash:", a))
            .concat(s.liabilities.filter(a => a.balance_cents > 0).map(a => accountRow("credit:", a)))
        for (const [list, sc] of [[s.securities, "securities"], [s.tangibles, "tangibles"]]) {
            for (const account of list) {
                const items = assets[account.id] || []
                items.forEach((item, i) => rows.push({
                    id: "asset:" + account.id + ":" + i, name: item.holding.name, accountName: account.name, cents: item.holding.value_cents,
                    scope: sc, classification: item.classification, quantity: item.holding.quantity,
                    identifier: item.holding.isin || item.holding.wkn }))
                const remainder = account.balance_cents - items.reduce((t, x) => t + x.holding.value_cents, 0)
                if (remainder !== 0)
                    rows.push({ id: "rest:" + account.id, name: qsTr("Ohne Positionsdaten / Bewertungsdifferenz"), accountName: account.name,
                                cents: remainder, scope: sc, classification: rowClassification("unclassified", "Unknown"), quantity: null, identifier: "" })
            }
        }
        return rows
    }
    /// Under an instrument heading the class is the missing half, and the other way round.
    function rowSubtitle(row) {
        const c = row.classification
        const last = grouping === "instrument"
            ? (c.asset_class === "unclassified" ? null : AssetClassification.classTitle(c.asset_class))
            : AssetClassification.instrumentTitleOf(c)
        const q = row.quantity
        // one piece of a Sachwert is the normal case, not information
        const units = q !== null && isFinite(q) && q > 0 && !(AssetClassification.isTangible(c) && q === 1) ? qsTr("%1 Stück").arg(NumberText.format(q)) : null
        return [units, row.identifier || null, row.accountName, last].filter(x => x).join(" · ")
    }
    /// WealthGroup: key, title, order (stable colours and card order), unclassified.
    function groupOf(row) {
        if (grouping === "instrument") {
            const k = row.classification.instrument
            return { key: k, title: AssetClassification.instrumentTitle(k), order: AssetClassification.instrumentIndex(k), unclassified: k === "unclassified" }
        }
        const k = row.classification.asset_class
        return { key: k, title: AssetClassification.classTitle(k), order: AssetClassification.classIndex(k), unclassified: k === "unclassified" }
    }
    /// The rows of each group of the active grouping, in the fixed order of its dimension.
    function groups(rows) {
        const by = {}
        for (const r of rows) {
            const g = groupOf(r)
            if (!by[g.key]) by[g.key] = { group: g, rows: [] }
            by[g.key].rows.push(r)
        }
        return Object.values(by).sort((x, y) => x.group.order - y.group.order)
    }
    /// A filter whose class no longer exists (last position reassigned) shows everything.
    readonly property string activeFilter: classRows.some(r => groupOf(r).key === classFilter) ? classFilter : "all"

    // MARK: loading (.task(id:))
    readonly property string syncStamp: AppModel.connections.map(c => c.id + ":" + (c.last_sync_unix || 0)).join(",")
    /// Positions and transactions can change while the depot total stays the same; loaded for all
    /// owners (switching the owner filters what is already there).
    readonly property string positionsKey: allSummary.securities.concat(allSummary.tangibles).map(a => a.id + ":" + a.balance_cents + ":" + a.balance_date).join(",")
                                           + "|" + syncStamp + "|" + AssetClassification.version
    /// Late bookings can change the history while today's balance stays the same.
    readonly property string historyKey: summary.cash.map(a => a.id + ":" + a.balance_cents + ":" + a.transaction_count + ":" + a.balance_date).join(",")
                                         + "|" + historyDays + "|" + syncStamp
    onPositionsKeyChanged: Qt.callLater(loadPositions)
    onHistoryKeyChanged: Qt.callLater(loadHistory)
    Component.onCompleted: {
        loadPositions(); loadHistory()
        // (developer) `--demo-filter wealth:<slice id>`: a selected donut slice for screenshots
        const f = AppModel.launchValue("--demo-filter")
        if (f.startsWith("wealth:")) demoFilter = f.slice(7)
    }
    /// The launch value comes lower-cased: picked once the slice it names exists (positions load late).
    property string demoFilter: ""
    onCompositionChanged: {
        if (demoFilter === "") return
        const hit = composition.slices.find(p => p.id.toLowerCase() === demoFilter)
        if (hit) { demoFilter = ""; Qt.callLater(() => wv.classFilter = hit.id) }
    }
    property int historyGeneration: 0
    property int positionsGeneration: 0
    property bool demoQueueShown: false

    function loadHistory() {
        const ids = summary.cash.map(a => a.id)
        const gen = ++historyGeneration
        if (!ids.length) { history = []; return }
        AppModel.coreAsync("balance_history", { account_ids: ids, days: historyDays }, pts => {
            if (gen === historyGeneration) history = pts
        }, () => {})
    }
    function loadPositions() {
        const depots = allSummary.securities.map(a => a.id)
        const all = depots.concat(allSummary.tangibles.map(a => a.id))
        const gen = ++positionsGeneration
        const since = new Date(); since.setFullYear(since.getFullYear() - 1)
        const sinceISO = Fmt.iso(since)
        Promise.all([
            Promise.all(all.map(id => AppModel.corePromise("classified_assets", { account_id: id }))),
            Promise.all(depots.map(id => AppModel.corePromise("list_security_transactions", { account_id: id })))
        ]).then(r => {
            if (gen !== positionsGeneration) return
            const map = {}
            all.forEach((id, i) => map[id] = r[0][i])
            classified = map
            income = makeIncome([].concat(...r[1]), sinceISO)
            // (developer) `--demo-sheet classification`: the assign queue of the open positions
            if (AppModel.demoSheet === "classification" && !demoQueueShown) {
                demoQueueShown = true
                AssetClassification.editAll([].concat(...Object.values(map))
                    .filter(a => a.classification.asset_class === "unclassified")
                    .map(a => ({ name: a.holding.name, classification: a.classification })))
            }
        }, () => {})
    }

    // MARK: WealthPosition.aggregate
    function addOpt(a, b) { return a === null ? b : b === null ? a : a + b }
    /// HoldingsCard.calculatedGain: cost = quantity × purchase price.
    function calculatedGain(h) {
        if (!(h.quantity > 0) || h.purchase_price_cents <= 0 || h.value_cents < 0) return null
        const cost = Math.round(h.quantity * h.purchase_price_cents)
        if (!(cost >= 1)) return null
        return h.value_cents - cost
    }
    function aggregate(perDepot) {
        const byKey = {}
        for (const depot of perDepot) for (const h of depot) {
            const key = h.isin ? h.isin : (h.wkn ? h.wkn : h.name)
            const day = h.prev_day_value_cents !== null && h.prev_day_value_cents !== undefined ? h.value_cents - h.prev_day_value_cents : null
            const reported = h.profit_loss_purchase_cents
            const gain = reported !== null && reported !== undefined ? reported : calculatedGain(h)
            const p = byKey[key]
            if (p) {
                p.quantity += h.quantity; p.valueCents += h.value_cents
                p.dayChangeCents = addOpt(p.dayChangeCents, day); p.gainCents = addOpt(p.gainCents, gain)
                p.lines += 1; p.dayLines += day === null ? 0 : 1; p.gainLines += gain === null ? 0 : 1
            } else {
                byKey[key] = { id: key, name: h.name || key, quantity: h.quantity, valueCents: h.value_cents,
                               dayChangeCents: day, gainCents: gain, lines: 1, dayLines: day === null ? 0 : 1, gainLines: gain === null ? 0 : 1 }
            }
        }
        return Object.values(byKey).sort((a, b) => a.valueCents !== b.valueCents ? b.valueCents - a.valueCents : a.name.localeCompare(b.name))
    }
    // MARK: WealthIncome.make
    function makeIncome(txs, since) {
        const r = { dividends: 0, fees: 0, taxes: 0 }
        for (const t of txs) {
            if (t.currency !== "EUR" || t.booking_date < since) continue
            if (t.direction === "Dividend") r.dividends += t.value_cents
            else if (t.direction === "Fee") r.fees += Math.abs(t.value_cents)
            else if (t.direction === "Tax") r.taxes += Math.abs(t.value_cents)
            if (t.direction !== "Fee") r.fees += t.fees_cents
            if (t.direction !== "Tax") r.taxes += t.taxes_cents
        }
        return r
    }

    // MARK: Sachwerte rows
    /// One row per tangible item (not sold), with a subline that is never the item's own name;
    /// hidden direct property stays out.
    readonly property var tangibleRows: {
        const hidden = {}
        if (prefs.hideDirectProperty)
            for (const id in classified)
                for (const a of classified[id])
                    if (AssetClassification.isDirectProperty(a) && a.asset_item_id !== null && a.asset_item_id !== undefined) hidden[a.asset_item_id] = true
        const out = []
        for (const a of summary.tangibles)
            for (const item of AppModel.assetItems(a.id))
                if (!item.sold && !hidden[item.id]) out.push({ id: "i" + item.id, name: item.setup.name, subtitle: tangibleSubtitle(item), valueCents: item.holding.value_cents })
        return out.sort((a, b) => b.valueCents - a.valueCents)
    }
    /// WealthView.tangibleSubtitle: pieces (when not exactly one), „Edelmetall“, the note.
    function tangibleSubtitle(item) {
        const parts = []
        const q = item.holding.quantity
        if (q !== 1) parts.push(qsTr("%1 Stück").arg(qty(q)))
        if (item.setup.price_source !== "manual") parts.push(qsTr("Edelmetall"))
        if (item.setup.note) parts.push(item.setup.note)
        return parts.join(" · ")
    }

    // MARK: formatting
    function qty(q) { return Number(q).toLocaleString(Qt.locale(), "f", 4).replace(/[.,]?0+$/, "") }
    /// .percent.precision(.fractionLength(1))
    function percent(x) {
        const n = Number(x * 100).toLocaleString(Qt.locale(), "f", 1)
        return Fmt.english ? n + "%" : n + " %"
    }
    function tone(c) { return c > 0 ? Theme.inflow : c < 0 ? Theme.negative : Theme.secondary }

    // MARK: WealthPalette
    /// Distinct, colour-blind friendly hues for donut slices (index wraps).
    readonly property var hues: [Theme.c("#2F6FDB", "#6EA0F5"), Theme.c("#1F7A5C", "#5EC79F"), Theme.c("#D98A00", "#F5B94A"),
                                 Theme.c("#8E4FC4", "#B98AE6"), Theme.c("#D1495B", "#F27F8E"), Theme.c("#0E8FA8", "#5CC6DB"),
                                 Theme.c("#7C9A2D", "#A9C95C"), Theme.c("#B0568F", "#DB8FC0")]
    function hue(i) { return hues[i % hues.length] }
    /// The folded remainder („Sonstige“): neutral, so it never looks like one of the named slices.
    readonly property color restColor: Theme.c("#B4BEC8", "#6B7785")
    /// Largest seven, the rest folded into „Sonstige“.
    function topSlices(rows) {
        const sorted = rows.filter(r => r.cents > 0).sort((a, b) => b.cents - a.cents)
        const out = sorted.slice(0, 7).map((r, i) => ({ id: r.id, label: r.label, cents: r.cents, color: hue(i) }))
        const rest = sorted.slice(7).reduce((t, r) => t + r.cents, 0)
        if (rest > 0) out.push({ id: "rest", label: qsTr("Sonstige"), cents: rest, color: restColor })
        return out
    }
    /// The donut: a class or instrument in the grouped view, else a bucket, an account or a position per slice.
    readonly property var composition: {
        const s = summary
        if (classMode)
            return { title: qsTr("Zusammensetzung"), slices: groups(classRows).map(g => ({
                id: g.group.key, label: g.group.title, cents: g.rows.reduce((t, r) => t + r.cents, 0),
                color: g.group.unclassified ? Theme.secondary : hue(g.group.order) })) }
        switch (scope) {
        case "cash":
            return { title: qsTr("Cash nach Konto"), slices: topSlices(s.cash.map(a => ({ id: String(a.id), label: a.name, cents: a.balance_cents }))) }
        case "securities": {
            // depots that only report a total (no positions): visible remainder
            const rest = s.securitiesCents - positions.reduce((t, p) => t + p.valueCents, 0)
            const parts = topSlices(positions.map(p => ({ id: p.id, label: p.name, cents: p.valueCents })))
            if (rest > 0) parts.push({ id: "uncovered", label: qsTr("Ohne Positionsdaten"), cents: rest, color: Theme.neutralTile })
            return { title: qsTr("Wertpapiere nach Position"), slices: parts }
        }
        case "tangibles":
            return { title: qsTr("Sachwerte nach Position"), slices: topSlices(tangibleRows.map(r => ({ id: r.id, label: r.name, cents: r.valueCents }))) }
        default: {
            // one fixed colour per bucket, so the legend colours never collide
            const out = [
                { id: "cash", label: qsTr("Cash"), cents: s.cashCents, color: hue(1) },
                { id: "securities", label: qsTr("Wertpapiere"), cents: s.securitiesCents, color: hue(0) },
                { id: "tangibles", label: qsTr("Sachwerte"), cents: s.tangiblesCents, color: hue(2) },
                // a positive card balance is an asset too, so the donut adds up to the assets bar
                { id: "credit", label: qsTr("Kartenguthaben"), cents: s.creditCents, color: hue(3) }
            ]
            return { title: qsTr("Zusammensetzung"), slices: out.filter(x => x.cents > 0) }
        }
        }
    }
    function toggleClassFilter(id) { classFilter = classFilter === id ? "all" : id }

    // MARK: SliceFilter – the selected donut slice of the account view, applied to the lists below it
    readonly property var sliceFilter: {
        const slices = classMode ? [] : composition.slices
        if (!slices.some(p => p.id === classFilter)) return { id: null, named: [] }
        return { id: classFilter, named: slices.map(p => p.id).filter(i => i !== "rest") }
    }
    /// „Sonstige“ stands for everything not shown by name.
    function keeps(candidate) {
        const f = sliceFilter
        if (f.id === null) return true
        return f.id === "rest" ? f.named.indexOf(candidate) < 0 : f.id === candidate
    }
    /// "Alles": a slice is a whole section; in a tab it is one account or position.
    function section(id) { return scope !== "all" || keeps(id) }
    readonly property bool showCash: !classMode && (scope === "all" || scope === "cash") && section("cash")
    readonly property var cashAccounts: scope === "cash" ? summary.cash.filter(a => keeps(String(a.id))) : summary.cash
    readonly property bool showSecurities: !classMode && (scope === "all" || scope === "securities") && section("securities")
    readonly property var shownPositions: scope === "securities" ? positions.filter(p => keeps(p.id)) : positions
    readonly property var shownTangibleRows: tangibleRows.filter(r => keeps(r.id))
    readonly property var debtAccounts: classMode ? summary.liabilities.filter(a => a.balance_cents < 0) : summary.liabilities

    Flickable {
        id: flick
        anchors.fill: parent
        contentWidth: width
        contentHeight: content.implicitHeight + 40
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        ScrollBar.vertical: AppScrollBar {}

        ColumnLayout {
            id: content
            x: 20; y: 20
            width: flick.width - 40
            spacing: 16

            Text { text: qsTr("Vermögen"); font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text }
            OwnerChips { visible: wv.ownerOptions.length > 0; Layout.fillWidth: true }

            // side by side when there is room, else stacked
            GridLayout {
                Layout.fillWidth: true
                columns: content.width >= 340 + 16 + 420 ? 2 : 1
                columnSpacing: 16; rowSpacing: 16
                SummaryCard {
                    Layout.fillWidth: parent.columns === 1
                    Layout.preferredWidth: parent.columns === 1 ? -1 : 340
                    Layout.alignment: Qt.AlignTop
                }
                CompositionCard { Layout.fillWidth: true; Layout.alignment: Qt.AlignTop }
            }
            // income and costs come from the depots: next to them in the securities tab (every grouping)
            IncomeCard { visible: wv.scope === "securities" && !wv.incomeEmpty; Layout.fillWidth: true }

            // MARK: grouped by class or instrument
            RowLayout {
                visible: wv.classMode
                Layout.fillWidth: true
                Text { text: wv.grouping === "instrument" ? qsTr("Instrumente · Vermögenswerte (brutto)") : qsTr("Anlageklassen · Vermögenswerte (brutto)")
                       font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true; elide: Text.ElideRight }
                Text { text: Fmt.money(wv.classRows.reduce((t, r) => t + r.cents, 0)); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
            }
            ClassCards { visible: wv.classMode; Layout.fillWidth: true }
            // after the classes, where cash and property are listed too
            IncomeCard { visible: wv.classMode && wv.scope === "all" && !wv.incomeEmpty; Layout.fillWidth: true }

            // MARK: by account
            CashHistoryCard { visible: wv.showCash && wv.summary.cash.length > 0; Layout.fillWidth: true }
            ListCard { shown: wv.showCash; Layout.fillWidth: true
                       title: qsTr("Cash"); total: wv.cashAccounts.reduce((t, a) => t + a.balance_cents, 0); accounts: wv.cashAccounts; tint: Theme.inflow }
            ListCard { shown: wv.showSecurities; Layout.fillWidth: true
                       title: qsTr("Wertpapiere"); total: wv.summary.securitiesCents; accounts: wv.summary.securities; tint: Theme.invest }
            IncomeCard { visible: wv.showSecurities && wv.scope === "all" && !wv.incomeEmpty; Layout.fillWidth: true }
            PositionsCard { visible: wv.showSecurities && wv.shownPositions.length > 0; Layout.fillWidth: true }
            ListCard { shown: !wv.classMode && wv.scope === "all" && wv.section("tangibles"); Layout.fillWidth: true
                       title: qsTr("Sachwerte"); total: wv.summary.tangiblesCents; accounts: wv.summary.tangibles; tint: Theme.invest }
            TangiblesCard { visible: !wv.classMode && wv.scope === "tangibles" && wv.shownTangibleRows.length > 0; Layout.fillWidth: true }

            Text {
                visible: wv.classMode && wv.scope === "all" && wv.debtAccounts.length > 0
                Layout.fillWidth: true; wrapMode: Text.Wrap
                text: prefs.includeLiabilities
                      ? qsTr("Die Anlageklassen zeigen das Bruttovermögen. Abzüglich der Verbindlichkeiten ergibt sich das Nettovermögen oben.")
                      : qsTr("Die Anlageklassen zeigen das Bruttovermögen. Verbindlichkeiten sind oben derzeit nicht abgezogen.")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
            ListCard { shown: wv.scope === "all"; Layout.fillWidth: true
                       title: qsTr("Verbindlichkeiten"); total: wv.debtAccounts.reduce((t, a) => t + a.balance_cents, 0); accounts: wv.debtAccounts; tint: Theme.negative }
            Text {
                visible: wv.scope === "all" && wv.summary.foreign > 0
                text: qsTr("%1 Konten in Fremdwährung sind nicht enthalten.").arg(wv.summary.foreign)
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
        }
    }

    // MARK: owner filter
    /// One chip per owner with that owner's share of the shown total (the same figure as the
    /// summary card would show for this owner: it follows the tab and the property switch).
    component OwnerChips: Flickable {
        implicitHeight: chipRow.implicitHeight
        contentWidth: chipRow.implicitWidth
        flickableDirection: Flickable.HorizontalFlick
        boundsBehavior: Flickable.StopAtBounds
        clip: true
        Accessible.name: qsTr("Inhaber")
        RowLayout {
            id: chipRow
            spacing: 8
            Repeater {
                model: wv.ownerOptions
                Rectangle {
                    id: chip
                    required property string modelData
                    readonly property bool selected: wv.activeOwner === modelData
                    readonly property real net: {
                        void AppModel.tangibleAccountIDs
                        return wv.netCents(wv.scoped(wv.select(wv.makeSummary(wv.accountsOf(modelData)), wv.classified, prefs.hideDirectProperty).summary, wv.scope))
                    }
                    implicitWidth: chipCol.implicitWidth + 24
                    implicitHeight: chipCol.implicitHeight + 14
                    radius: 9
                    color: selected ? Theme.withAlpha(Theme.accent, 0.14) : Theme.neutralTile
                    border.color: selected ? Theme.accent : "transparent"
                    ColumnLayout {
                        id: chipCol
                        anchors.centerIn: parent
                        spacing: 1
                        Text {
                            text: chip.modelData === "all" ? qsTr("Alle") : chip.modelData === "unassigned" ? qsTr("Ohne Inhaber") : chip.modelData.slice(6)
                            font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; color: Theme.text
                            Sensitive { when: chip.modelData.startsWith("owner:") }
                        }
                        Text { text: Fmt.money(chip.net); font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } }
                    }
                    TapHandler { cursorShape: Qt.PointingHandCursor; onTapped: wv.ownerFilter = chip.modelData }
                    Accessible.role: Accessible.Button
                    Accessible.checked: selected
                }
            }
        }
    }

    // MARK: summary (net worth + assets / liabilities bars)
    component SummaryCard: Card {
        id: sc
        padding: 18
        readonly property var s: wv.summary
        readonly property real assets: Math.max(s.grossCents, 0)
        readonly property real debts: -Math.min(s.liabilitiesCents, 0)
        readonly property real maxCents: Math.max(assets, debts, 1)
        /// Direct property of the selected tab; the switch is offered where it can have an effect.
        readonly property int properties: wv.scope === "all" ? wv.selection.directPropertyCount
            : wv.scope === "securities" ? wv.selection.directProperty.securities
            : wv.scope === "tangibles" ? wv.selection.directProperty.tangibles : 0
        readonly property bool offerPropertySwitch: properties > 0 || (wv.scope !== "cash" && wv.scope !== "securities" && wv.fullSummary.tangibles.length > 0)
        readonly property bool propertyHidden: prefs.hideDirectProperty && properties > 0
        /// No loan is tied to a property: say so instead of showing a silent, too low figure.
        readonly property bool loanWarning: propertyHidden && wv.scope === "all" && prefs.includeLiabilities && debts > 0
        implicitWidth: 340
        ColumnLayout {
            width: parent.width; spacing: 14
            Text {
                text: wv.scope === "all" ? (sc.propertyHidden ? qsTr("Nettovermögen ohne Immobilien") : qsTr("Nettovermögen"))
                    : wv.scope === "cash" ? qsTr("Cash") : wv.scope === "securities" ? qsTr("Wertpapiere") : qsTr("Sachwerte")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
            Text {
                Layout.fillWidth: true
                text: Fmt.money(wv.netCents(sc.s))
                font.pixelSize: 36; font.weight: Font.DemiBold; font.features: { "tnum": 1 }
                color: Theme.text
                fontSizeMode: Text.HorizontalFit; minimumPixelSize: 22; elide: Text.ElideNone
                Sensitive { amounts: true }
            }
            Bar { visible: wv.scope === "all"; title: qsTr("Vermögenswerte"); cents: sc.assets; fraction: sc.assets / sc.maxCents; tint: Theme.accent }
            Bar { visible: wv.scope === "all"; title: qsTr("Verbindlichkeiten"); cents: -sc.debts; fraction: sc.debts / sc.maxCents; tint: Theme.negative
                  opacity: prefs.includeLiabilities ? 1 : 0.45 }
            CheckBox {
                visible: sc.debts > 0
                text: qsTr("Kredite und Karten abziehen")
                font.pixelSize: Theme.callout + 1
                checked: prefs.includeLiabilities
                onToggled: prefs.includeLiabilities = checked
                leftPadding: 0
            }
            ColumnLayout {
                visible: sc.offerPropertySwitch
                Layout.fillWidth: true
                spacing: 4
                CheckBox {
                    text: qsTr("Direkt gehaltene Immobilien ausblenden")
                    font.pixelSize: Theme.callout + 1
                    checked: prefs.hideDirectProperty
                    onToggled: prefs.hideDirectProperty = checked
                    leftPadding: 0
                }
                Text {
                    visible: sc.properties === 0
                    Layout.fillWidth: true; wrapMode: Text.Wrap
                    text: qsTr("Gilt für Positionen mit Anlageklasse Immobilien und Instrument Direktbesitz.")
                    font.pixelSize: Theme.caption2; color: Theme.secondary
                }
                RowLayout {
                    visible: sc.properties > 0 && sc.loanWarning
                    Layout.fillWidth: true
                    spacing: 6
                    SFIcon { symbol: "exclamationmark.triangle.fill"; size: 12; color: Theme.warning; Layout.alignment: Qt.AlignTop }
                    Text {
                        Layout.fillWidth: true; wrapMode: Text.Wrap
                        text: qsTr("Immobilien sind ausgeblendet, ihre Kredite aber weiterhin abgezogen. Ohne „Kredite und Karten abziehen“ bleibt der Wert vergleichbar.")
                        font.pixelSize: Theme.caption2; color: Theme.warning
                    }
                }
                Text {
                    visible: sc.properties > 0 && sc.propertyHidden && !sc.loanWarning
                    Layout.fillWidth: true; wrapMode: Text.Wrap
                    text: qsTr("Immobilien sind aus dieser Ansicht ausgeblendet.")
                    font.pixelSize: Theme.caption2; color: Theme.secondary
                }
            }
        }
    }
    component Bar: ColumnLayout {
        id: bar
        property string title
        property real cents
        property real fraction
        property color tint
        Layout.fillWidth: true
        spacing: 4
        RowLayout {
            Layout.fillWidth: true
            Text { text: bar.title; font.pixelSize: Theme.subheadline + 1; font.weight: Font.Medium; color: Theme.text; Layout.fillWidth: true }
            Text { text: Fmt.money(bar.cents); font.pixelSize: Theme.subheadline + 1; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
        }
        Item {
            Layout.fillWidth: true; implicitHeight: 6
            Rectangle { anchors.fill: parent; radius: 3; color: Theme.neutralTile }
            Rectangle { height: parent.height; radius: 3; color: bar.tint
                        width: Math.max(parent.width * bar.fraction, bar.fraction > 0 ? 4 : 0) }
        }
    }

    // MARK: composition donut
    component CompositionCard: Card {
        id: cc
        padding: 18
        readonly property var allParts: wv.composition.slices
        readonly property var parts: wv.classMode && prefs.hideUnclassifiedInChart ? allParts.filter(p => p.id !== "unclassified") : allParts
        readonly property real total: parts.filter(p => p.cents > 0).reduce((t, p) => t + p.cents, 0)
        readonly property var selected: parts.find(p => p.id === wv.classFilter) || null
        /// ViewThatFits: donut and legend side by side, else stacked
        readonly property bool vertical: width - 36 < 190 + 36 + legend.implicitWidth
        ColumnLayout {
            width: parent.width; spacing: 12
            RowLayout {
                Layout.fillWidth: true
                spacing: 10
                Text { text: wv.composition.title; font.pixelSize: Theme.caption; color: Theme.secondary; Layout.fillWidth: true; elide: Text.ElideRight }
                IconButton {
                    visible: cc.selected !== null
                    symbol: "xmark.circle.fill"; symbolSize: 12; tint: Theme.accent
                    text: qsTr("Alle anzeigen"); textColor: Theme.accent; font.pixelSize: Theme.caption
                    onClicked: wv.classFilter = "all"
                }
                // always names the active grouping; one more grouping is one more entry
                Segmented {
                    visible: wv.scope !== "cash"
                    options: wv.groupings.map(g => ({ id: g, title: wv.groupingTitle(g) }))
                    current: wv.grouping
                    onActivated: id => prefs.grouping = id
                }
            }
            Text { visible: cc.parts.length === 0; text: qsTr("Keine Daten"); color: Theme.secondary; font.pixelSize: Theme.body
                   Layout.fillWidth: true; Layout.minimumHeight: 120; horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter }
            GridLayout {
                visible: cc.parts.length > 0
                Layout.alignment: Qt.AlignHCenter
                columns: cc.vertical ? 1 : 2
                columnSpacing: 36; rowSpacing: 16
                Donut { parts: cc.parts; total: cc.total; selected: cc.selected; Layout.alignment: Qt.AlignHCenter }
                ColumnLayout {
                    id: legend
                    Layout.alignment: Qt.AlignVCenter | (cc.vertical ? Qt.AlignHCenter : Qt.AlignLeft)
                    spacing: 12
                    Repeater {
                        model: cc.parts
                        RowLayout {
                            id: entry
                            required property var modelData
                            readonly property bool isSelected: cc.selected !== null && cc.selected.id === modelData.id
                            spacing: 8
                            Item {
                                implicitWidth: 14; implicitHeight: 14
                                Rectangle { visible: !entry.isSelected; anchors.centerIn: parent; width: 9; height: 9; radius: 4.5; color: entry.modelData.color }
                                SFIcon { visible: entry.isSelected; anchors.centerIn: parent; symbol: "checkmark.circle.fill"; size: 13; color: entry.modelData.color }
                            }
                            ColumnLayout { spacing: 0
                                Text { text: entry.modelData.label; font.pixelSize: Theme.callout + 1; font.weight: entry.isSelected ? Font.DemiBold : Font.Normal
                                       color: Theme.text; Sensitive {} }
                                Text { text: Fmt.money(entry.modelData.cents) + " (" + wv.percent(entry.modelData.cents / Math.max(cc.total, 1)) + ")"
                                       font.pixelSize: Theme.caption; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } } }
                            TapHandler { cursorShape: Qt.PointingHandCursor; onTapped: wv.toggleClassFilter(entry.modelData.id) }
                            Accessible.role: Accessible.Button
                            Accessible.name: entry.modelData.label
                            Accessible.checked: entry.isSelected
                            Accessible.description: qsTr("Positionen dieser Gruppe anzeigen oder Filter aufheben")
                        }
                    }
                }
            }
            CheckBox {
                visible: wv.classMode && cc.allParts.some(p => p.id === "unclassified")
                text: qsTr("Nicht klassifizierte im Diagramm ausblenden")
                font.pixelSize: Theme.caption + 1
                checked: prefs.hideUnclassifiedInChart
                onToggled: prefs.hideUnclassifiedInChart = checked
                leftPadding: 0
            }
            Text {
                visible: wv.classMode && prefs.hideUnclassifiedInChart && cc.allParts.some(p => p.id === "unclassified")
                Layout.fillWidth: true; wrapMode: Text.Wrap
                text: qsTr("Diagramm und Anteile zeigen nur zugeordnete Werte. Summen und Positionen bleiben vollständig.")
                font.pixelSize: Theme.caption2; color: Theme.secondary
            }
            Text {
                visible: wv.scope === "securities" && wv.classMode
                Layout.fillWidth: true; wrapMode: Text.Wrap
                text: wv.grouping === "instrument"
                      ? qsTr("Instrumente innerhalb der Depotkonten; Cash-Konten sind nicht enthalten.")
                      : qsTr("Anlageklassen innerhalb der Depotkonten; Cash-Konten sind nicht enthalten.")
                font.pixelSize: Theme.caption2; color: Theme.secondary
            }
        }
    }
    component Donut: Item {
        id: donut
        property var parts: []
        property real total: 0
        property var selected: null
        /// Only positive values are drawn (negative class values stay in the sums).
        readonly property var drawn: parts.filter(p => p.cents > 0)
        implicitWidth: 190; implicitHeight: 190
        onPartsChanged: canvas.requestPaint()
        onSelectedChanged: canvas.requestPaint()
        Connections { target: Theme; function onDarkChanged() { canvas.requestPaint() } }
        Canvas {
            id: canvas
            anchors.fill: parent
            onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                const sum = donut.drawn.reduce((t, p) => t + p.cents, 0)
                if (sum <= 0) return
                const cx = width / 2, cy = height / 2, r = Math.min(width, height) / 2, inner = r * 0.62
                // angularInset 1.5 pt on each side of a sector
                const inset = donut.drawn.length > 1 ? 1.5 : 0
                let a = -Math.PI / 2
                for (const p of donut.drawn) {
                    const span = p.cents / sum * 2 * Math.PI
                    const o = inset / r, i = inset / inner
                    if (span > 2 * o) {
                        ctx.beginPath()
                        ctx.arc(cx, cy, r, a + o, a + span - o, false)
                        ctx.arc(cx, cy, inner, a + span - Math.min(i, span / 2), a + Math.min(i, span / 2), true)
                        ctx.closePath()
                        ctx.globalAlpha = donut.selected === null || donut.selected.id === p.id ? 1 : 0.25
                        ctx.fillStyle = p.color
                        ctx.fill()
                    }
                    a += span
                }
                ctx.globalAlpha = 1
            }
        }
        /// A click on a sector filters by it (again: shows all); the hole clears the filter.
        TapHandler {
            onTapped: ev => {
                const dx = ev.position.x - donut.width / 2, dy = ev.position.y - donut.height / 2
                const distance = Math.hypot(dx, dy), radius = Math.min(donut.width, donut.height) / 2
                if (distance < radius * 0.62) { wv.classFilter = "all"; return }
                if (distance > radius) return
                let angle = Math.atan2(dx, -dy)
                if (angle < 0) angle += 2 * Math.PI
                const sum = donut.drawn.reduce((t, p) => t + p.cents, 0)
                let end = 0
                for (const p of donut.drawn) {
                    end += p.cents / sum * 2 * Math.PI
                    if (angle < end) { wv.toggleClassFilter(p.id); return }
                }
            }
        }
        ColumnLayout {
            anchors.centerIn: parent
            width: 100; spacing: 0
            Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; text: Fmt.money(donut.selected ? donut.selected.cents : donut.total)
                   font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: Theme.text
                   fontSizeMode: Text.HorizontalFit; minimumPixelSize: 8; Sensitive { amounts: true } }
            Text { Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; text: donut.selected ? qsTr("Auswahl") : qsTr("Summe")
                   font.pixelSize: Theme.caption2; color: Theme.secondary }
        }
    }

    // MARK: class cards
    component ClassCards: ColumnLayout {
        id: cards
        spacing: 16
        readonly property var rows: wv.activeFilter === "all" ? wv.classRows : wv.classRows.filter(r => wv.groupOf(r).key === wv.activeFilter)
        Text { visible: cards.rows.length === 0; text: qsTr("Keine Daten"); color: Theme.secondary; font.pixelSize: Theme.body }
        Text {
            visible: cards.rows.some(r => r.cents < 0)
            Layout.fillWidth: true; wrapMode: Text.Wrap
            text: qsTr("Negative Werte sind in den Summen enthalten, im Donut werden nur positive Klassenwerte dargestellt.")
            font.pixelSize: Theme.caption; color: Theme.secondary
        }
        Repeater {
            model: wv.groups(cards.rows)
            Card {
                id: groupCard
                required property var modelData
                /// Every open position in one pass: the sheet steps through them.
                readonly property var open: modelData.group.unclassified
                    ? AssetClassification.storable(modelData.rows.map(r => ({ name: r.name, classification: r.classification }))) : []
                padding: 18
                Layout.fillWidth: true
                ColumnLayout {
                    width: parent.width; spacing: 10
                    RowLayout {
                        Layout.fillWidth: true
                        Text { text: groupCard.modelData.group.title; font.pixelSize: Theme.subheadline + 2; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
                        Text { text: Fmt.money(groupCard.modelData.rows.reduce((t, r) => t + r.cents, 0)); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                    }
                    Divider {}
                    RowLayout {
                        visible: groupCard.open.length > 0
                        Layout.fillWidth: true
                        Text {
                            Layout.fillWidth: true
                            text: groupCard.open.length === 1 ? qsTr("1 Position noch nicht zugeordnet") : qsTr("%1 Positionen noch nicht zugeordnet").arg(groupCard.open.length)
                            font.pixelSize: Theme.caption; color: Theme.secondary
                        }
                        IconButton {
                            text: qsTr("Jetzt zuordnen"); textColor: Theme.accent; font.pixelSize: Theme.caption
                            onClicked: AssetClassification.editAll(groupCard.open)
                        }
                    }
                    Repeater {
                        model: groupCard.modelData.rows
                        RowLayout {
                            id: classRow
                            required property var modelData
                            Layout.fillWidth: true
                            ColumnLayout { spacing: 2; Layout.fillWidth: true
                                Text { text: classRow.modelData.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true
                                       Sensitive { labels: classRow.modelData.scope !== "tangibles" } }
                                Text { text: wv.rowSubtitle(classRow.modelData); font.pixelSize: Theme.caption; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                                       ToolTip.visible: subtitleHover.hovered; ToolTip.delay: 600; ToolTip.text: AssetClassification.sourceDescription(classRow.modelData.classification)
                                       HoverHandler { id: subtitleHover }
                                       Sensitive {} } }
                            Text { text: Fmt.money(classRow.modelData.cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                            IconButton {
                                visible: !!classRow.modelData.classification.security_key
                                symbol: "pencil"; symbolSize: 14
                                ToolTip.text: qsTr("Klassifizierung bearbeiten")
                                Accessible.name: qsTr("Klassifizierung bearbeiten")
                                onClicked: AssetClassification.edit(classRow.modelData.name, classRow.modelData.classification)
                            }
                        }
                    }
                }
            }
        }
    }

    // MARK: lists
    component CardTitle: RowLayout {
        property string title
        property var total: null
        Layout.fillWidth: true
        Text { text: parent.title; font.pixelSize: Theme.subheadline + 2; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
        Text { visible: parent.total !== null; text: parent.total !== null ? Fmt.money(parent.total) : ""
               font.pixelSize: Theme.subheadline + 2; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
    }
    component Divider: Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }

    component ListCard: Card {
        id: lc
        property string title
        property real total
        property var accounts: []
        property color tint
        property bool shown: true
        padding: 18
        visible: shown && accounts.length > 0
        ColumnLayout {
            width: parent.width; spacing: 8
            CardTitle { title: lc.title; total: lc.total }
            Divider {}
            Repeater {
                model: lc.accounts.slice().sort((a, b) => Math.abs(b.balance_cents) - Math.abs(a.balance_cents))
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: 8
                    SFIcon { symbol: modelData.sf_symbol; size: 15; color: lc.tint; Layout.preferredWidth: 20 }
                    ColumnLayout { spacing: 1; Layout.fillWidth: true
                        Text { text: modelData.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                        Text { visible: modelData.bank_name !== ""; text: modelData.bank_name; font.pixelSize: Theme.caption2; color: Theme.secondary
                               elide: Text.ElideRight; Layout.fillWidth: true } }
                    Text { text: Fmt.money(modelData.balance_cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                    TapHandler { cursorShape: Qt.PointingHandCursor; onTapped: AppModel.sidebarSelection = "account:" + modelData.id }
                }
            }
        }
    }

    component PositionsCard: Card {
        id: pc
        padding: 18
        readonly property var shown: wv.shownPositions
        readonly property var day: shown.filter(p => p.dayChangeCents !== null)
        readonly property var gain: shown.filter(p => p.gainCents !== null)
        readonly property var cov: ({ lines: shown.reduce((t, p) => t + p.lines, 0), day: shown.reduce((t, p) => t + p.dayLines, 0),
                                      gain: shown.reduce((t, p) => t + p.gainLines, 0) })
        readonly property real total: wv.summary.securitiesCents
        ColumnLayout {
            width: parent.width; spacing: 8
            CardTitle { title: qsTr("Positionen aller Depots") }
            RowLayout {
                visible: pc.day.length > 0 || pc.gain.length > 0
                spacing: 18
                Change { visible: pc.day.length > 0; title: qsTr("Heute"); cents: pc.day.reduce((t, p) => t + p.dayChangeCents, 0) }
                Change { visible: pc.gain.length > 0; title: qsTr("Gewinn/Verlust"); cents: pc.gain.reduce((t, p) => t + p.gainCents, 0) }
            }
            // sums of the reported values only, not the whole depot value
            Text {
                visible: (pc.day.length > 0 || pc.gain.length > 0) && (pc.cov.day < pc.cov.lines || pc.cov.gain < pc.cov.lines)
                Layout.fillWidth: true; wrapMode: Text.Wrap
                text: qsTr("Nur Bestände mit verfügbaren Daten: Heute %1 von %2, Gewinn/Verlust %3 von %4.")
                      .arg(pc.cov.day).arg(pc.cov.lines).arg(pc.cov.gain).arg(pc.cov.lines)
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
            Divider {}
            Repeater {
                model: pc.shown
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    ColumnLayout { spacing: 1; Layout.fillWidth: true
                        Text { text: modelData.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { labels: true } }
                        Text { text: wv.qty(modelData.quantity) + " · " + modelData.id; font.pixelSize: Theme.caption2; color: Theme.secondary
                               elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true } } }
                    ColumnLayout { spacing: 1
                        Text { Layout.alignment: Qt.AlignRight; text: Fmt.money(modelData.valueCents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                        Text { Layout.alignment: Qt.AlignRight; visible: modelData.dayChangeCents !== null
                               text: modelData.dayChangeCents !== null ? Fmt.signed(modelData.dayChangeCents) : ""
                               font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }; color: wv.tone(modelData.dayChangeCents); Sensitive { amounts: true } }
                        Text { Layout.alignment: Qt.AlignRight; visible: pc.total > 0; text: wv.percent(modelData.valueCents / Math.max(pc.total, 1))
                               font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } } }
                }
            }
        }
    }
    component Change: RowLayout {
        property string title
        property real cents
        spacing: 6
        Text { text: parent.title; font.pixelSize: Theme.caption; color: Theme.secondary }
        Text { text: Fmt.signed(parent.cents); font.pixelSize: Theme.callout + 1; font.weight: Font.Medium; font.features: { "tnum": 1 }; color: wv.tone(parent.cents); Sensitive { amounts: true } }
    }

    component TangiblesCard: Card {
        id: tc
        padding: 18
        readonly property real total: wv.summary.tangiblesCents
        ColumnLayout {
            width: parent.width; spacing: 8
            CardTitle { title: qsTr("Positionen der Sachwerte") }
            Divider {}
            Repeater {
                model: wv.shownTangibleRows
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    ColumnLayout { spacing: 1; Layout.fillWidth: true
                        Text { text: modelData.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true; Sensitive {} }
                        Text { visible: modelData.subtitle !== ""; text: modelData.subtitle; font.pixelSize: Theme.caption2; color: Theme.secondary
                               elide: Text.ElideRight; Layout.fillWidth: true; Sensitive { amounts: true } } }
                    ColumnLayout { spacing: 1
                        Text { Layout.alignment: Qt.AlignRight; text: Fmt.money(modelData.valueCents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                        Text { Layout.alignment: Qt.AlignRight; visible: tc.total > 0; text: wv.percent(modelData.valueCents / Math.max(tc.total, 1))
                               font.pixelSize: Theme.caption2; font.features: { "tnum": 1 }; color: Theme.secondary; Sensitive { amounts: true } } }
                }
            }
        }
    }

    /// Distributions, fees and taxes of the depots over the last twelve months.
    component IncomeCard: Card {
        padding: 18
        ColumnLayout {
            width: parent.width; spacing: 8
            CardTitle { title: qsTr("Erträge und Kosten") }
            Text { text: qsTr("Letzte 12 Monate, aus den Wertpapierumsätzen der Depots."); font.pixelSize: Theme.caption; color: Theme.secondary
                   Layout.fillWidth: true; wrapMode: Text.Wrap }
            Divider {}
            IncomeRow { title: qsTr("Ausschüttungen"); cents: wv.income.dividends; tint: Theme.inflow }
            IncomeRow { title: qsTr("Gebühren"); cents: -wv.income.fees; tint: Theme.negative }
            IncomeRow { title: qsTr("Steuern"); cents: -wv.income.taxes; tint: Theme.negative }
        }
    }
    component IncomeRow: RowLayout {
        property string title
        property real cents
        property color tint
        visible: cents !== 0
        Layout.fillWidth: true
        Text { text: parent.title; font.pixelSize: Theme.body; color: Theme.text; Layout.fillWidth: true }
        Text { text: Fmt.money(parent.cents); font.pixelSize: Theme.body; font.features: { "tnum": 1 }; color: parent.tint; Sensitive { amounts: true } }
    }

    // MARK: cash history (area chart, back-calculated from bookings)
    component CashHistoryCard: Card {
        id: hc
        padding: 18
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout {
                Layout.fillWidth: true
                ColumnLayout { spacing: 1; Layout.fillWidth: true
                    Text { text: qsTr("Cash-Verlauf"); font.pixelSize: Theme.subheadline + 2; font.weight: Font.DemiBold; color: Theme.text }
                    Text { text: qsTr("Aus den Buchungen zurückgerechnet; Depots sind nicht enthalten."); font.pixelSize: Theme.caption; color: Theme.secondary
                           Layout.fillWidth: true; wrapMode: Text.Wrap } }
                Segmented {
                    options: [{ id: "90", title: qsTr("3 Monate") }, { id: "365", title: qsTr("1 Jahr") },
                              { id: "1095", title: qsTr("3 Jahre") }, { id: "1825", title: qsTr("5 Jahre") }]
                    current: String(wv.historyDays)
                    onActivated: id => wv.historyDays = Number(id)
                }
            }
            Text { visible: wv.history.length < 2; text: qsTr("Keine Daten"); color: Theme.secondary; font.pixelSize: Theme.body
                   Layout.fillWidth: true; Layout.minimumHeight: 120; horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter }
            CashChart { visible: wv.history.length >= 2; points: wv.history; Layout.fillWidth: true; Layout.preferredHeight: 190 }
        }
    }
    /// Area + line with a value axis on the trailing side (Swift Charts' AxisMarks(position: .trailing)).
    component CashChart: Item {
        id: cash
        property var points: []
        FontMetrics { id: axisFont; font.pixelSize: Theme.caption2 }
        readonly property var stats: {
            let lo = Infinity, hi = -Infinity
            for (const p of points) { lo = Math.min(lo, p.balance_cents); hi = Math.max(hi, p.balance_cents) }
            const pad = Math.max((hi - lo) * 0.1, 100)
            return { lo: lo - pad, hi: hi + pad }
        }
        /// About five round values inside the domain.
        readonly property var ticks: {
            const span = stats.hi - stats.lo
            if (!(span > 0)) return []
            const raw = span / 4, mag = Math.pow(10, Math.floor(Math.log10(raw)))
            const step = [1, 2, 2.5, 5, 10].map(f => f * mag).find(s => s >= raw) || raw
            const out = []
            for (let v = Math.ceil(stats.lo / step) * step; v <= stats.hi; v += step) out.push(v)
            return out
        }
        readonly property real axisWidth: ticks.reduce((w, t) => Math.max(w, axisFont.advanceWidth(Fmt.money(Math.round(t)))), 0) + 8
        readonly property real plotWidth: width - axisWidth
        function xAt(i) { return points.length < 2 ? 0 : i / (points.length - 1) * plotWidth }
        function yAt(c) { return height - (c - stats.lo) / (stats.hi - stats.lo) * height }
        onPointsChanged: canvas.requestPaint()
        onWidthChanged: canvas.requestPaint()
        Connections { target: Theme; function onDarkChanged() { canvas.requestPaint() } }
        Canvas {
            id: canvas
            anchors.fill: parent
            onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                const pts = cash.points
                if (pts.length < 2) return
                ctx.strokeStyle = Theme.withAlpha(Theme.tertiary, 0.25); ctx.lineWidth = 1
                for (const t of cash.ticks) {
                    const y = Math.round(cash.yAt(t)) + 0.5
                    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(cash.plotWidth, y); ctx.stroke()
                }
                const step = Math.max(1, Math.floor(pts.length / Math.max(2, cash.plotWidth / 2)))
                ctx.beginPath()
                ctx.moveTo(0, cash.yAt(pts[0].balance_cents))
                for (let i = step; i < pts.length; i += step) ctx.lineTo(cash.xAt(i), cash.yAt(pts[i].balance_cents))
                ctx.lineTo(cash.plotWidth, cash.yAt(pts[pts.length - 1].balance_cents))
                ctx.lineTo(cash.plotWidth, cash.height); ctx.lineTo(0, cash.height); ctx.closePath()
                const g = ctx.createLinearGradient(0, 0, 0, cash.height)
                g.addColorStop(0, Theme.withAlpha(Theme.accent, 0.35)); g.addColorStop(1, Theme.withAlpha(Theme.accent, 0.03))
                ctx.fillStyle = g; ctx.fill()
                ctx.beginPath()
                ctx.moveTo(0, cash.yAt(pts[0].balance_cents))
                for (let i = step; i < pts.length; i += step) ctx.lineTo(cash.xAt(i), cash.yAt(pts[i].balance_cents))
                ctx.lineTo(cash.plotWidth, cash.yAt(pts[pts.length - 1].balance_cents))
                ctx.strokeStyle = Theme.accent; ctx.lineWidth = 2; ctx.stroke()
            }
        }
        Repeater {
            model: cash.ticks
            Text {
                required property var modelData
                x: cash.plotWidth + 6
                y: Math.max(0, Math.min(cash.height - height, cash.yAt(modelData) - height / 2))
                text: Fmt.money(Math.round(modelData)); font.pixelSize: Theme.caption2; color: Theme.secondary
                Sensitive { amounts: true }   // chart axis values
            }
        }
    }
}
