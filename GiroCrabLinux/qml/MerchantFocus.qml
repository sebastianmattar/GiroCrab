pragma Singleton
import QtQuick

// AppModel+Selection.swift › focusMerchant(of:) / leaveMerchantFocus() (#495): the list of every
// booking of one merchant in the current period – all accounts, no category, status, direction, tag,
// scope or search filter. The merchant key covers differing counterparty spellings of one
// merchant. AppModel.currentFilter() reads `key`; any other sidebar choice ends the list
// (AppModel.onSidebarSelectionChanged calls `sidebarChanged`).
QtObject {
    id: mf
    /// MerchantFocus: { key, name } or null.
    property var focus: null
    readonly property var key: focus ? focus.key : null
    /// MerchantFocusReturn: sidebar selection and list filters to return to.
    property var back: null
    /// True while focusMerchant / leaveMerchantFocus set the sidebar selection themselves.
    property bool applying: false

    function focusMerchant(b) {
        if (!b || !b.merchant.key) return
        // A second merchant from within a merchant list still returns to the original view.
        const ret = back || { selection: AppModel.sidebarSelection, scope: AppModel.scope, status: AppModel.statusFilter,
                              direction: AppModel.directionFilter, search: AppModel.searchText, tags: TagFilter.snapshot() }
        // Filters first: their reloads still run on the old, cheaper selection.
        AppModel.scope = "All"
        AppModel.statusFilter = "all"
        AppModel.directionFilter = "All"
        TagFilter.clear()
        if (AppModel.searchText !== "") { AppModel.searchText = ""; AppModel.searchTimer.stop() }
        focus = { key: b.merchant.key, name: b.merchant_name || b.merchant.name || b.counterparty }
        back = ret
        applySelection("all")
    }
    /// Back to the sidebar selection and filters from before `focusMerchant`.
    function leave() {
        if (focus === null) return
        const ret = back
        focus = null
        back = null
        if (!ret) { AppModel.reloadBookings(); return }
        applySelection(ret.selection)
        AppModel.scope = ret.scope
        AppModel.statusFilter = ret.status
        AppModel.directionFilter = ret.direction
        TagFilter.restore(ret.tags)
        if (AppModel.searchText !== ret.search) AppModel.searchText = ret.search
    }
    /// The sidebar selection changed: unless set from here, the merchant list ends.
    function sidebarChanged() {
        if (applying) return
        focus = null
        back = null
    }
    /// Another vault's data (demo ↔ own, unlock, restore): its merchant list ends without
    /// restoring anything – the selection is set to "all" anyway (no change event then).
    property Connections vaultWatch: Connections {
        target: AppModel
        function onVaultReplaced() { mf.focus = null; mf.back = null }
    }
    function applySelection(selection) {
        applying = true
        if (AppModel.sidebarSelection === selection) AppModel.reloadSelection(false)
        else AppModel.sidebarSelection = selection
        applying = false
    }
}
