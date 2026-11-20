pragma Singleton
import QtQuick
import QtCore

// Sidebar state that came to the Mac app after 0.7.0, kept out of AppModel.qml:
// the collapsible section headers (SidebarView.swift), the badge counts of „Geplant“ and
// „Lastschriften“ (AppModel+Loading.swift › refreshSidebarCounts), the custom name of
// „Ohne Kategorie“ (AppModel.swift › uncategorizedLabel, #597), the remembered expanded
// categories (#633) and „Duplizieren“ of a category (AppModel+CategoryDuplicate.swift, #510).
QtObject {
    id: sm

    /// SidebarView.swift › `@AppStorage("sidebar.accountsCollapsed")` etc.: read and written by
    /// key (see AppModel.sidebarDefaults – bound properties would write their defaults back).
    readonly property Settings store: Settings {
        category: "sidebar"
        function loadSet(key) {
            const out = {}
            try { for (const k of JSON.parse(value(key, "[]"))) out[k] = true } catch (e) {}
            return out
        }
    }
    property bool accountsCollapsed: String(store.value("accountsCollapsed", false)) === "true"
    onAccountsCollapsedChanged: store.setValue("accountsCollapsed", accountsCollapsed)
    property bool paymentsCollapsed: String(store.value("paymentsCollapsed", false)) === "true"
    onPaymentsCollapsedChanged: store.setValue("paymentsCollapsed", paymentsCollapsed)
    property bool documentsCollapsed: String(store.value("documentsCollapsed", false)) === "true"
    onDocumentsCollapsedChanged: store.setValue("documentsCollapsed", documentsCollapsed)

    // MARK: layout (SidebarView.swift › DisclosureChevron, rowInsets)
    /// The one leading column of the sidebar: every chevron sits in it, rows without one reserve it.
    readonly property int slot: 14
    /// Gap between the chevron column and the icon.
    readonly property int gap: 5
    /// Leading offset of a child row: its icon sits on the headers' icon axis.
    readonly property int iconInset: slot + gap
    /// Row insets inside the selection pill (EdgeInsets(leading: 2, trailing: 6) plus the list's own).
    readonly property int rowLeading: 4
    readonly property int rowTrailing: 6

    // MARK: badge counts (AppModel+Loading.swift › refreshSidebarCounts)
    property int plannedCount: 0
    property int mandateCount: 0
    property int countGeneration: 0
    /// Two `COUNT(*)`s off the GUI thread. Call after planned payments or mandates changed.
    function refreshCounts() {
        if (!AppModel.isUnlocked) { plannedCount = 0; mandateCount = 0; return }
        const gen = ++countGeneration
        AppModel.coreAsync("count_planned_payments", {}, n => { if (gen === sm.countGeneration) sm.plannedCount = n }, () => {})
        AppModel.coreAsync("count_active_mandates", {}, n => { if (gen === sm.countGeneration) sm.mandateCount = n }, () => {})
    }

    // MARK: „Ohne Kategorie“ (#597): vault setting `ui.uncategorized_name`, empty = default
    property string uncategorizedName: ""
    readonly property string uncategorizedLabel: uncategorizedName !== "" ? uncategorizedName : qsTr("Ohne Kategorie")
    /// AppModel.swift › setUncategorizedName: an empty text restores the default.
    function setUncategorizedName(name) {
        const t = (name || "").trim()
        if (AppModel.run(() => { AppModel.core("set_setting", { key: "ui.uncategorized_name", value: t }); return true }))
            uncategorizedName = t
    }

    /// The vault changed (unlocked, locked, demo ↔ own vault): its settings and counts.
    function reloadVaultState() {
        let name = ""
        if (AppModel.isUnlocked) {
            try { name = (AppModel.core("get_setting", { key: "ui.uncategorized_name" }) || "").trim() } catch (e) {}
        }
        uncategorizedName = name
        refreshCounts()
    }

    // MARK: expanded categories (#633): per vault, the demo vault has its own key
    function expandedKey(demo) { return demo ? "expandedCategories.demo" : "expandedCategories" }
    property bool loadingExpanded: false
    function loadExpanded() {
        loadingExpanded = true
        AppModel.expandedCategories = store.loadSet(expandedKey(AppModel.demoActive))
        loadingExpanded = false
    }
    /// AppModel+Editing.swift › expandCategoryPath(to:): opens `id` and every category above it.
    function expandCategoryPath(id) {
        const c = Object.assign({}, AppModel.expandedCategories)
        let next = id
        for (let steps = 0; next !== null && next !== undefined && steps < 64; steps++) {
            c[next] = true
            const cat = AppModel.categoryByID(next)
            next = cat ? cat.parent_id : null
        }
        AppModel.expandedCategories = c
    }

    readonly property Connections watch: Connections {
        target: AppModel
        function onExpandedCategoriesChanged() {
            if (sm.loadingExpanded) return
            sm.store.setValue(sm.expandedKey(AppModel.demoActive), JSON.stringify(Object.keys(AppModel.expandedCategories).map(Number)))
        }
        function onDemoActiveChanged() { sm.loadExpanded(); sm.reloadVaultState() }
        function onVaultStateChanged() { sm.reloadVaultState() }
        function onVaultReplaced() { sm.reloadVaultState() }
        // accounts reload after a sync, an import or an edit: the counts may have changed too
        function onGroupsChanged() { sm.refreshCounts() }
    }

    // MARK: AppModel+CategoryDuplicate.swift › duplicateCategory (#510)
    /// Copies the category with all its sub-categories (name, symbol, colour; no bookings,
    /// rules or budgets) next to the original; the copy of the top one is "<name> Kopie".
    function duplicateCategory(id) {
        const root = AppModel.categoryByID(id)
        if (!root) return null
        const children = {}
        for (const c of AppModel.categories) {
            if (c.parent_id === null || c.parent_id === undefined) continue
            if (!children[c.parent_id]) children[c.parent_id] = []
            children[c.parent_id].push(c)
        }
        function copy(c, parent, name, depth) {
            const created = AppModel.core("create_category", { name: name, parent_id: parent, sf_symbol: c.sf_symbol, color_hex: c.color_hex })
            if (depth < 16) for (const child of (children[c.id] || [])) copy(child, created, child.name, depth + 1)
            return created
        }
        const newRoot = AppModel.run(() => copy(root, root.parent_id, root.name + " " + qsTr("Kopie"), 0))
        AppModel.reloadCategories()
        if (root.parent_id !== null && root.parent_id !== undefined) expandCategoryPath(root.parent_id)
        return newRoot === undefined ? null : newRoot
    }

    Component.onCompleted: { loadExpanded(); reloadVaultState() }
}
