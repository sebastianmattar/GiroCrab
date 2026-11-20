pragma Singleton
import QtQuick

// AppModel+Tags.swift › „Tag filter of the booking list (#228, part 3)“ and AppModelTypes.swift ›
// BookingTagFilter: bookings carrying every tag in `tagIDs` (AND), or (`untagged`) only bookings
// without any tag; both at once never happens. Kept while the app runs, across sidebar
// selections, like the status filter; „Neue Umsätze“ ignores it (AppModel.currentFilter).
// The header button and its popover: StreamHeaderCard.qml / TagFilterPopover.qml.
QtObject {
    id: tf

    /// BookingTagFilter.tagIDs, sorted.
    property var tagIDs: []
    /// BookingTagFilter.untagged („Ohne Tag“).
    property bool untagged: false
    readonly property bool isActive: untagged || tagIDs.length > 0
    /// AppModel.showTagFilter: the popover of the stream header.
    property bool shown: false

    /// AppModel.tagFilter didSet: a changed filter reloads the list from its first page.
    function set(ids, onlyUntagged) {
        const sorted = ids.slice().sort((a, b) => a - b)
        if (sorted.join(",") === tagIDs.join(",") && onlyUntagged === untagged) return
        tagIDs = sorted
        untagged = onlyUntagged
        AppModel.listLimit = AppModel.defaultListLimit
        AppModel.reloadBookings()
    }
    /// toggleTagFilter: ticks or unticks a tag (several tags = bookings carrying all of them); ends „Ohne Tag“.
    function toggle(id) {
        set(tagIDs.indexOf(id) >= 0 ? tagIDs.filter(x => x !== id) : tagIDs.concat([id]), false)
    }
    /// toggleUntaggedFilter: only bookings without any tag, or back to all.
    function toggleUntagged() { set([], !untagged) }
    function clear() { set([], false) }
    /// The filter as an object, for MerchantFocus to restore (MerchantFocusReturn.tags).
    function snapshot() { return { tagIDs: tagIDs.slice(), untagged: untagged } }
    /// Restores a saved choice without tags deleted or merged meanwhile.
    function restore(s) {
        if (!s) return
        set(s.tagIDs.filter(id => TagsModel.tags.some(t => t.id === id)), s.untagged)
    }

    /// AppModel+Loading.swift › currentFilter: the filter's `tag_ids` / `untagged_only`.
    function applyTo(f) {
        f.tag_ids = untagged ? [] : tagIDs.slice()
        f.untagged_only = untagged
        return f
    }

    /// showsTagFilter: the header offers the tag filter once tags exist (or a filter is still
    /// on); not in „Neue Umsätze“, which ignores the list filters.
    readonly property bool available: AppModel.sidebarSelection !== "newBookings" && (TagsModel.tags.length > 0 || isActive)

    /// tagFilterTitle: label of the header's tag button. Never names a tag (the header is part
    /// of bug report screenshots), only how many are ticked.
    readonly property string title: untagged ? qsTr("Ohne Tag")
                                  : tagIDs.length === 0 ? qsTr("Tags") : qsTr("Tags: %1").arg(tagIDs.length)

    /// tagFilterNote: the tag filter as the printout header names it; "" = none.
    function note() {
        if (untagged) return qsTr("Ohne Tag")
        const names = TagsModel.tags.filter(t => tagIDs.indexOf(t.id) >= 0).map(t => t.name)
        return names.length === 0 ? "" : qsTr("Tags: %1").arg(names.join(", "))
    }
    /// The note for a filter as sent to the vault: none where the tag choice does not apply
    /// („Neue Umsätze“, CAMT.053), so a header never names a filter its rows ignore.
    function noteFor(filter) {
        if (filter.untagged_only) return qsTr("Ohne Tag")
        const ids = filter.tag_ids || []
        const names = TagsModel.tags.filter(t => ids.indexOf(t.id) >= 0).map(t => t.name)
        return names.length === 0 ? "" : qsTr("Tags: %1").arg(names.join(", "))
    }

    /// loadTagTotals: bookings and sum per tag (`tag_id` null = untagged) for the list's current
    /// filter without its tag choice – what ticking each tag would show. An aggregate over the
    /// whole filter, so off the GUI thread. `done(map)` gets { tagID | "null": { booking_count, sum_cents } },
    /// or null when the vault was locked or swapped meanwhile.
    function loadTotals(done) {
        if (!AppModel.isUnlocked) { done(null); return }
        const generation = AppModel.vaultGeneration
        AppModel.coreAsync("tag_totals", { filter: AppModel.currentFilter() }, rows => {
            if (generation !== AppModel.vaultGeneration) { done(null); return }
            const map = {}
            for (const r of rows) if (map[String(r.tag_id)] === undefined) map[String(r.tag_id)] = r
            done(map)
        }, () => done(null))
    }

    /// loadTagAccountCounts: the tag's bookings per account, most first (off the GUI thread).
    function loadAccountCounts(tagID, done) {
        if (!AppModel.isUnlocked) { done(null); return }
        const generation = AppModel.vaultGeneration
        AppModel.coreAsync("tag_account_counts", { tag_id: tagID }, rows => done(generation === AppModel.vaultGeneration ? rows : null),
                           () => done(null))
    }

    /// showBookings(ofTag:account:): closes the tag sheet and lists every booking of the account
    /// with this tag (all dates and statuses, no search), as the count promised.
    function showBookings(tagID, accountID) {
        // the sheet's draft (names, colours, order) is stored as on closing it
        if (TagManager.shown) TagManager.applyDraft()
        // (leaving a merchant list restores its filters, so that goes first)
        if (MerchantFocus.focus !== null) MerchantFocus.leave()
        // filters before the selection: their reloads still run on the old one
        set([tagID], false)
        AppModel.statusFilter = "all"
        AppModel.directionFilter = "All"
        // tag_account_counts counts every booking of the account, whatever the scope
        AppModel.scope = "All"
        if (AppModel.searchText !== "") { AppModel.searchText = ""; AppModel.searchTimer.restart() }
        AppModel.sidebarSelection = "account:" + accountID
        if (AppModel.period !== "all") AppModel.choosePeriod("all")
        TagManager.shown = false
    }

    /// reloadTags: a deleted or merged tag leaves the list filter.
    property Connections tagsWatch: Connections {
        target: TagsModel
        function onTagsChanged() {
            if (tf.tagIDs.length === 0) return
            const live = tf.tagIDs.filter(id => TagsModel.tags.some(t => t.id === id))
            if (live.length !== tf.tagIDs.length) tf.set(live, tf.untagged)
        }
    }
    /// releaseVaultState: another vault (demo / own, lock, reset) starts without a tag filter.
    property Connections vaultWatch: Connections {
        target: AppModel
        function onVaultReplaced() { tf.tagIDs = []; tf.untagged = false; tf.shown = false }
    }
}
