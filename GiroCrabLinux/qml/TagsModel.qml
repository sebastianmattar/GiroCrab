pragma Singleton
import QtQuick

// AppModel+Tags.swift (#228): booking tags – free labels across categories („für wen?“,
// „welcher Urlaub?“). Tag names often name people, so they are hidden like other personal
// data in bug report pictures (`Sensitive {}`, not `labels`).
// The tag list comes from `list_tags` (one indexed aggregate, cheap enough for the GUI thread).
// A change patches the loaded rows at once (no list reload) and can be undone exactly
// (Bearbeiten › Rückgängig, Strg+Z, via UndoManager: just the bookings that changed).
QtObject {
    id: tm

    /// Live tags in list order with their booking counts: [{ id, name, color_hex, sort_order, booking_count }].
    property var tags: []
    /// Booking whose tag field (inspector) should take the focus once shown.
    property var pendingTagFocus: null

    /// Colours a new tag gets in turn (the user can change it later).
    readonly property var palette: ["#2E86DE", "#27AE60", "#E67E22", "#8E44AD", "#C0392B", "#16A085", "#D4AC0D", "#7F8C8D"]
    /// Longest tag name the core accepts (characters, after collapsing spaces).
    readonly property int maxTagNameLength: 64

    function reload() {
        if (!AppModel.isUnlocked) { tags = []; return }
        // quiet: a lock empties the vault before vaultReplaced fires (no error over the lock screen)
        try { tags = AppModel.core("list_tags") || [] } catch (e) { tags = [] }
    }
    Component.onCompleted: reload()
    /// Another vault (demo / own, lock, reset): other tags, nothing to undo.
    property Connections vaultWatch: Connections {
        target: AppModel
        // (UndoManager drops the undo steps itself)
        function onVaultStateChanged() { tm.reload() }
        function onDemoActiveChanged() { tm.reload() }
        function onVaultReplaced() { tm.reload() }
    }

    // MARK: names (same rules as the core)
    /// A name as the core stores it: inner runs of spaces collapsed, trimmed.
    function cleanTagName(name) { return (name || "").split(/\s+/).filter(x => x !== "").join(" ") }
    /// The core's key for "the same name" (case-insensitive, spaces collapsed).
    function tagKey(name) { return cleanTagName(name).toLowerCase() }
    /// Looser than `tagKey`: only letters and digits, without case and accents, so
    /// „test 1“, „test-1“, „test_1“ and „Test1“ meet („Urlaub 2025“ / „2026“ don't).
    function lookAlikeKey(name) {
        let s = (name || "").toLowerCase()
        try { s = s.normalize("NFD").replace(/[̀-ͯ]/g, "") } catch (e) {}
        return BookingDetails.normalized(s)
    }
    /// TagEntryIntent: what Return in a tag field will do with `text`:
    /// { kind: "none" | "create" | "add" | "already" | "similar" | "tooLong", name, id, existing }.
    function entryIntent(text, carried) {
        const name = cleanTagName(text)
        if (name === "") return { kind: "none" }
        if ([...name].length > maxTagNameLength) return { kind: "tooLong" }
        const key = tagKey(name)
        const known = tags.find(t => tagKey(t.name) === key)
        if (known) return carried[known.id] ? { kind: "already", name: known.name } : { kind: "add", id: known.id, name: known.name }
        const loose = lookAlikeKey(name)
        const alike = loose !== "" ? tags.find(t => lookAlikeKey(t.name) === loose) : null
        if (alike) return { kind: "similar", name: name, id: alike.id, existing: alike.name }
        return { kind: "create", name: name }
    }
    /// Suggestions of a tag field: every open tag matching the text (also look-alikes).
    function suggestions(text, carried) {
        const open = tags.filter(t => !carried[t.id])
        const trimmed = cleanTagName(text)
        if (trimmed === "") return open
        const lower = trimmed.toLowerCase(), loose = lookAlikeKey(trimmed)
        return open.filter(t => t.name.toLowerCase().indexOf(lower) >= 0 || (loose !== "" && lookAlikeKey(t.name).indexOf(loose) >= 0))
    }

    // MARK: changes
    /// The live tag of that name, created (with the next palette colour) if there is none.
    function tagNamed(name) {
        const trimmed = cleanTagName(name)
        if (trimmed === "" || !AppModel.isUnlocked) return null
        const known = tags.find(t => tagKey(t.name) === tagKey(trimmed))
        if (known) return known
        const t = AppModel.run(() => AppModel.core("create_tag", { name: trimmed, color_hex: palette[tags.length % palette.length] }))
        if (!t) return null
        reload()
        return t
    }
    /// Tags that some or all of `bookings` carry, in tag list order: [{ tag, all }].
    function usage(bookings) {
        const count = {}, byID = {}
        for (const b of bookings) for (const t of (b.tags || [])) { count[t.id] = (count[t.id] || 0) + 1; byID[t.id] = t }
        return Object.keys(byID).map(id => byID[id]).sort((a, b) => rank(a) - rank(b))
            .map(t => ({ tag: t, all: count[t.id] === bookings.length }))
    }
    /// Adds the tag named `name` (created if new) to `bookings`.
    function addTagNamed(name, bookings) {
        const t = tagNamed(name)
        if (t) changeTag(t.id, bookings.map(b => b.id), true)
    }
    function addTag(tagID, bookingIDs) { changeTag(tagID, bookingIDs, true) }
    function removeTag(tagID, bookingIDs) { changeTag(tagID, bookingIDs, false) }
    /// Context menu: removes the tag when every booking carries it, otherwise adds it to the rest.
    function toggleTag(tagID, bookings) {
        const all = bookings.length > 0 && bookings.every(b => (b.tags || []).some(t => t.id === tagID))
        changeTag(tagID, bookings.map(b => b.id), !all)
    }
    /// Select the booking and put the cursor into the tag field of its inspector card
    /// (of the multiple-selection card when the booking is part of one).
    function editTags(b) {
        if (!b) return
        AppModel.showInspector = true
        // a multiple selection that contains the booking stays (its card has the field)
        if (!AppModel.selectedBookingIDs[b.id]) AppModel.selectedBookingID = b.id
        pendingTagFocus = b.id
    }
    /// What the row menu and the "Tags" submenu act on: the multiple selection when the row is part of it.
    function targets(b) { return BookingDetails.categorizeTargets(b) }

    /// One small transaction; the rows show the change at once, the counts follow from `reload`.
    /// Undoable (UndoManager) for exactly the bookings that changed; undo and redo keep the
    /// name of the original step (`historyName`).
    function changeTag(tagID, bookingIDs, adding, historyName) {
        if (!AppModel.isUnlocked || !bookingIDs.length) return
        let changed
        try {
            changed = AppModel.core(adding ? "add_tag_to_bookings" : "remove_tag_from_bookings", { booking_ids: bookingIDs, tag_id: tagID })
        } catch (e) {
            AppModel.errorMessage = e.message
            AppModel.reloadBookings(); reload()
            return
        }
        if (!changed.length) return
        showTag(tagID, changed, adding)
        if (TagFilter.isActive && AppModel.sidebarSelection !== "newBookings") {
            if (AppModel.selectedBookingID !== null && changed.indexOf(AppModel.selectedBookingID) >= 0) {
                const cur = AppModel.bookings.find(b => b.id === AppModel.selectedBookingID)
                if (cur) AppModel.stickyBooking = cur
            }
            AppModel.reloadBookings()
        }
        reload()
        const name = historyName || (adding ? qsTr("Tag hinzufügen") : qsTr("Tag entfernen"))
        UndoManager.register(name, () => tm.changeTag(tagID, changed, !adding, name))
    }
    /// Optimistic update of the loaded rows and a sticky inspector booking.
    function showTag(tagID, ids, adding) {
        const t = tags.find(x => x.id === tagID)
        if (!t) { AppModel.reloadBookings(); return }
        const tag = { id: t.id, name: t.name, color_hex: t.color_hex }
        const set = {}
        for (const id of ids) set[id] = true
        function patched(b) {
            const c = Object.assign({}, b)
            c.tags = (b.tags || []).filter(x => x.id !== tagID)
            if (adding) { c.tags.push(tag); c.tags.sort((a, b2) => rank(a) - rank(b2)) }
            return c
        }
        AppModel.bookings = AppModel.bookings.map(b => set[b.id] ? patched(b) : b)
        AppModel.rebuildStream()
        if (AppModel.stickyBooking !== null && set[AppModel.stickyBooking.id]) AppModel.stickyBooking = patched(AppModel.stickyBooking)
    }
    /// Position in the tag list (the core's order), unknown tags last.
    function rank(t) {
        const i = tags.findIndex(x => x.id === t.id)
        return i < 0 ? Number.MAX_SAFE_INTEGER : i
    }
}
