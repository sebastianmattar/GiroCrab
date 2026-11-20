import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Tags (#228)

    /// Colours a new tag gets in turn (the user can change it later).
    static let tagPalette = ["#2E86DE", "#27AE60", "#E67E22", "#8E44AD", "#C0392B", "#16A085", "#D4AC0D", "#7F8C8D"]

    /// The tag list with booking counts: one indexed aggregate, cheap enough for the main thread.
    func reloadTags() {
        guard let vault else { tags = []; return }
        runCore { tags = try vault.listTags() }
        // a deleted or merged tag leaves the list filter
        let live = tagFilter.tagIDs.intersection(tags.map(\.id))
        if live != tagFilter.tagIDs { tagFilter.tagIDs = live }
    }

    // MARK: - Tag filter of the booking list (#228, part 3)

    /// Ticks or unticks a tag (several tags = bookings carrying all of them); ends „Ohne Tag“.
    func toggleTagFilter(_ id: Int64) {
        var f = tagFilter
        f.untagged = false
        if !f.tagIDs.insert(id).inserted { f.tagIDs.remove(id) }
        tagFilter = f
    }

    /// Only bookings without any tag, or back to all.
    func toggleUntaggedFilter() {
        tagFilter = tagFilter.untagged ? BookingTagFilter() : BookingTagFilter(untagged: true)
    }

    func clearTagFilter() { tagFilter = BookingTagFilter() }

    /// The rules that add the tag `id` (#580), in rule order; for „Tags verwalten“.
    func rules(addingTag id: Int64) -> [Rule] {
        rules.filter { $0.tagIds.contains(id) }
    }

    /// Rule list (#580): per active rule that adds tags, its matching bookings that
    /// carry all of its tags (off the main thread, ~35 ms on 40k bookings).
    func loadRuleTagCounts() async -> [Int64: UInt32]? {
        guard let rows = try? await vaultOffMain({ try $0.ruleTagCounts() }) else { return nil }
        return Dictionary(rows.map { ($0.ruleId, $0.bookingCount) }, uniquingKeysWith: { a, _ in a })
    }

    /// The header offers the tag filter once tags exist (or a filter is still on);
    /// not in „Neue Umsätze“, which ignores the list filters.
    var showsTagFilter: Bool {
        shownSelection != .newBookings && (!tags.isEmpty || tagFilter.isActive)
    }

    /// Label of the header's tag button. Never names a tag (the header is part of
    /// bug report screenshots), only how many are ticked.
    var tagFilterTitle: String {
        if tagFilter.untagged { return String(localized: "Ohne Tag") }
        if tagFilter.tagIDs.isEmpty { return String(localized: "Tags") }
        return String(localized: "Tags: \(tagFilter.tagIDs.count)")
    }

    /// The tag filter as the printout header names it; nil = none.
    var tagFilterNote: String? {
        if tagFilter.untagged { return String(localized: "Ohne Tag") }
        let names = tags.filter { tagFilter.tagIDs.contains($0.id) }.map(\.name)
        return names.isEmpty ? nil : String(localized: "Tags: \(names.joined(separator: ", "))")
    }

    /// Bookings and sum per tag (`tagID` nil = untagged) for the list's current filter without its
    /// tag choice: what ticking each tag would show. An aggregate over the whole filter (~80 ms on
    /// 31k bookings), so off the main thread; nil when the vault is locked or was swapped.
    func loadTagTotals() async -> [Int64?: TagTotal]? {
        let filter = currentFilter
        guard let rows = try? await vaultOffMain({ try $0.tagTotals(filter: filter) }) else { return nil }
        return Dictionary(rows.map { ($0.tagId, $0) }, uniquingKeysWith: { a, _ in a })
    }

    /// „Tags verwalten“: the tag's bookings per account, most first (off the main thread).
    func loadTagAccountCounts(_ tagID: Int64) async -> [TagAccountCount]? {
        try? await vaultOffMain { try $0.tagAccountCounts(tagId: tagID) }
    }

    /// „Tags verwalten“ › bookings per account: closes the sheet and lists every booking of
    /// the account with this tag (all dates and statuses, no search), as the count promised.
    func showBookings(ofTag tagID: Int64, account accountID: Int64) {
        // (leaving a merchant list restores its filters, so that goes first)
        if merchantFocus != nil { leaveMerchantFocus() }
        // filters before the selection: their reloads still run on the old one
        tagFilter = BookingTagFilter(tagIDs: [tagID])
        statusFilter = .all
        directionFilter = .all
        if !searchText.isEmpty { searchText = "" }
        interactiveSidebarSelection = .account(accountID)
        if period != .all { interactivePeriod = .all }
        showTagManager = false
    }

    /// Longest tag name the core accepts (characters, after collapsing spaces).
    static let maxTagNameLength = 64

    /// A name as the core stores it: inner runs of spaces collapsed, trimmed.
    static func cleanTagName(_ name: String) -> String {
        name.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// The core's key for "the same name" (case-insensitive, spaces collapsed).
    static func tagKey(_ name: String) -> String { cleanTagName(name).lowercased() }

    /// Looser than `tagKey`: only letters and digits, without case and accents, so
    /// „test 1“, „test-1“, „test_1“ and „Test1“ meet („Urlaub 2025“ / „2026“ don't).
    static func tagLookAlikeKey(_ name: String) -> String {
        String(name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
            .unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) })
    }

    /// What Return in a tag field will do with `text` (shown below the field while typing).
    static func tagEntryIntent(_ text: String, tags: [GiroCrabCore.Tag], carried: Set<Int64>) -> TagEntryIntent {
        let name = cleanTagName(text)
        if name.isEmpty { return .none }
        if name.unicodeScalars.count > maxTagNameLength { return .tooLong }
        let key = tagKey(name)
        if let known = tags.first(where: { tagKey($0.name) == key }) {
            return carried.contains(known.id) ? .already(known.name) : .add(known.id, known.name)
        }
        let loose = tagLookAlikeKey(name)
        if !loose.isEmpty, let alike = tags.first(where: { tagLookAlikeKey($0.name) == loose }) {
            return .similar(new: name, existingID: alike.id, existing: alike.name)
        }
        return .create(name)
    }

    /// The live tag of that name, created (with the next palette colour) if there is none.
    func tag(named name: String) -> GiroCrabCore.Tag? {
        guard let vault else { return nil }
        let trimmed = Self.cleanTagName(name)
        guard !trimmed.isEmpty else { return nil }
        if let known = tags.first(where: { Self.tagKey($0.name) == Self.tagKey(trimmed) }) {
            return known
        }
        do {
            let color = Self.tagPalette[tags.count % Self.tagPalette.count]
            let t = try vault.createTag(name: trimmed, colorHex: color)
            reloadTags()
            return t
        } catch {
            errorMessage = describe(error)
            return nil
        }
    }

    /// Tags that some or all of `bookings` carry, in tag list order.
    func tagUsage(of bookings: [Booking]) -> [(tag: BookingTag, all: Bool)] {
        var count: [Int64: Int] = [:]
        var byID: [Int64: BookingTag] = [:]
        for b in bookings { for t in b.tags { count[t.id, default: 0] += 1; byID[t.id] = t } }
        return byID.values
            .sorted { tagRank($0) < tagRank($1) }
            .map { ($0, count[$0.id] == bookings.count) }
    }

    /// Adds the tag named `name` (created if new) to `bookings`.
    func addTag(named name: String, to bookings: [Booking]) {
        guard let t = tag(named: name) else { return }
        addTag(t.id, to: bookings.map(\.id))
    }

    func addTag(_ tagID: Int64, to bookingIDs: [Int64]) {
        changeTag(tagID, on: bookingIDs, adding: true)
    }

    func removeTag(_ tagID: Int64, from bookingIDs: [Int64]) {
        changeTag(tagID, on: bookingIDs, adding: false)
    }

    /// Context menu: removes the tag when every booking carries it, otherwise adds it to the rest.
    func toggleTag(_ tagID: Int64, on bookings: [Booking]) {
        let all = !bookings.isEmpty && bookings.allSatisfy { $0.tags.contains { $0.id == tagID } }
        changeTag(tagID, on: bookings.map(\.id), adding: !all)
    }

    /// Select the booking and put the cursor into the tag field of its inspector card
    /// (of the multiple-selection card when the booking is part of one).
    func editTags(for booking: Booking) {
        #if os(macOS)
        AppDefaults.store.set(true, forKey: ViewLayout.inspectorKey)
        #endif
        // a multiple selection that contains the booking stays (its card has the field)
        if !selectedBookingIDs.contains(booking.id) { selectedBookingID = booking.id }
        pendingTagFocus = booking.id
    }

    /// The write is one small transaction; the rows show the change at once
    /// (no list reload), the tag counts follow from `reloadTags`. Undo reverts
    /// exactly the bookings that changed.
    private func changeTag(_ tagID: Int64, on bookingIDs: [Int64], adding: Bool) {
        guard let vault, !bookingIDs.isEmpty else { return }
        let changed: [Int64]
        do {
            changed = adding
                ? try vault.addTagToBookings(bookingIds: bookingIDs, tagId: tagID)
                : try vault.removeTagFromBookings(bookingIds: bookingIDs, tagId: tagID)
        } catch {
            errorMessage = describe(error)
            reloadBookings(); reloadTags()
            return
        }
        guard !changed.isEmpty else { return }
        showTag(tagID, on: Set(changed), adding: adding)
        reloadTags()
        if let um = windowUndoManager {
            um.registerUndo(withTarget: self) { m in m.changeTag(tagID, on: changed, adding: !adding) }
            um.setActionName(adding ? String(localized: "Tag hinzufügen") : String(localized: "Tag entfernen"))
        }
    }

    /// Optimistic update of the loaded rows and a sticky inspector booking.
    private func showTag(_ tagID: Int64, on ids: Set<Booking.ID>, adding: Bool) {
        guard let t = tags.first(where: { $0.id == tagID }) else { reloadBookings(); return }
        let tag = BookingTag(id: t.id, name: t.name, colorHex: t.colorHex)
        func patch(_ b: inout Booking) {
            b.tags.removeAll { $0.id == tagID }
            if adding {
                b.tags.append(tag)
                b.tags.sort { tagRank($0) < tagRank($1) }
            }
        }
        var rows = bookings
        for i in rows.indices where ids.contains(rows[i].id) { patch(&rows[i]) }
        bookings = rows
        if var s = stickyBooking, ids.contains(s.id) { patch(&s); stickyBooking = s }
    }

    // MARK: Tag management window (#228, part 2b)

    /// What „Umbenennen“ would do with `text` for tag `id` (hint below the field).
    static func tagRenameIntent(_ text: String, tags: [GiroCrabCore.Tag], renaming id: Int64) -> TagRenameIntent {
        let name = cleanTagName(text)
        if name.isEmpty { return .empty }
        if name.unicodeScalars.count > maxTagNameLength { return .tooLong }
        let current = tags.first { $0.id == id }
        if current?.name == name { return .unchanged }
        let key = tagKey(name)
        if let other = tags.first(where: { $0.id != id && tagKey($0.name) == key }) { return .taken(other.name) }
        let loose = tagLookAlikeKey(name)
        if current.map({ tagKey($0.name) }) != key, !loose.isEmpty,
           let alike = tags.first(where: { $0.id != id && tagLookAlikeKey($0.name) == loose }) {
            return .similar(new: name, existing: alike.name)
        }
        return .rename(name)
    }

    /// Tags without bookings („Ungenutzte entfernen“).
    var unusedTags: [GiroCrabCore.Tag] { tags.filter { $0.bookingCount == 0 } }

    /// A new tag from the window; nil when the name is empty or taken (the sheet says so first).
    @discardableResult
    func createTag(named name: String) -> GiroCrabCore.Tag? {
        guard case .create = Self.tagEntryIntent(name, tags: tags, carried: []) else {
            if case .similar = Self.tagEntryIntent(name, tags: tags, carried: []) { return tag(named: name) }
            return nil
        }
        return tag(named: name)
    }

    @discardableResult
    func renameTag(_ id: Int64, to name: String) -> Bool {
        guard let vault else { return false }
        do { _ = try vault.renameTag(id: id, name: Self.cleanTagName(name)) } catch {
            errorMessage = describe(error)
            return false
        }
        afterTagEdit()
        return true
    }

    func setTagColor(_ id: Int64, hex: String) {
        guard let vault else { return }
        runCore { try vault.setTagColor(id: id, colorHex: hex) }
        afterTagEdit()
    }

    /// Drag in the window: the new order of all tags.
    /// The tag sheet's arrangement for the tags there now: `pending` without the
    /// tags gone since, the tags new since then after it (in their stored order).
    static func tagOrder(_ pending: [Int64], tags: [GiroCrabCore.Tag]) -> [Int64] {
        let live = Set(tags.map(\.id)), listed = Set(pending)
        return pending.filter(live.contains) + tags.map(\.id).filter { !listed.contains($0) }
    }

    /// „Sortieren › Nach Name“.
    static func tagIDsByName(_ tags: [GiroCrabCore.Tag]) -> [Int64] {
        tags.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }.map(\.id)
    }

    /// „Sortieren › Meiste / Wenigste Buchungen zuerst“; equal counts by name.
    static func tagIDsByBookings(_ tags: [GiroCrabCore.Tag], mostFirst: Bool) -> [Int64] {
        tags.sorted { a, b in
            if a.bookingCount != b.bookingCount { return mostFirst ? a.bookingCount > b.bookingCount : a.bookingCount < b.bookingCount }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }.map(\.id)
    }

    /// The tags as the tag sheet shows them: with its unsaved names and colours.
    static func tagsDrafted(_ tags: [GiroCrabCore.Tag], _ draft: TagDraft) -> [GiroCrabCore.Tag] {
        guard !draft.renames.isEmpty || !draft.colors.isEmpty else { return tags }
        return tags.map { t in
            var t = t
            if let name = draft.renames[t.id] { t.name = name }
            if let hex = draft.colors[t.id] { t.colorHex = hex }
            return t
        }
    }

    /// Stores the tag sheet's draft (on dismissing it) and clears it.
    func applyTagDraft() {
        let draft = tagDraft
        tagDraft = TagDraft()
        guard !draft.isEmpty, let vault else { return }
        if !draft.renames.isEmpty { applyTagRenames(draft.renames) }
        let live = Dictionary(tags.map { ($0.id, $0.colorHex) }, uniquingKeysWith: { a, _ in a })
        let colors = draft.colors.filter { live[$0.key] != nil && live[$0.key] != $0.value }
        if !colors.isEmpty {
            runCore { for (id, hex) in colors { try vault.setTagColor(id: id, colorHex: hex) } }
            afterTagEdit()
        }
        if let order = draft.order { applyTagOrder(order) }
    }

    /// Stores the names the tag sheet gave (on closing it). The core refuses a
    /// name another tag still has, so a rename waits until its name is free;
    /// names that only swap places go through a temporary name. A name held by
    /// a tag outside the sheet's changes (synced in meanwhile) is reported.
    @discardableResult
    func applyTagRenames(_ renames: [Int64: String]) -> Bool {
        guard let vault else { return false }
        var names = Dictionary(tags.map { ($0.id, $0.name) }, uniquingKeysWith: { a, _ in a })
        var pending: [Int64: String] = [:]
        for (id, name) in renames {
            let clean = Self.cleanTagName(name)
            if let stored = names[id], stored != clean, !clean.isEmpty { pending[id] = clean }
        }
        var ok = true
        func rename(_ id: Int64, _ name: String) {
            do {
                _ = try vault.renameTag(id: id, name: name)
                names[id] = name
            } catch {
                errorMessage = describe(error)
                ok = false
            }
        }
        func blockers(_ id: Int64) -> [Int64] {
            let key = Self.tagKey(pending[id] ?? "")
            return names.filter { $0.key != id && Self.tagKey($0.value) == key }.map(\.key)
        }
        while !pending.isEmpty {
            let ids = pending.keys.sorted()
            if let id = ids.first(where: { blockers($0).isEmpty }) {
                rename(id, pending[id] ?? "")
                pending[id] = nil
            } else if let id = ids.first(where: { blockers($0).allSatisfy { pending[$0] != nil } }) {
                // a swap: park the tag that blocks this one under a free name
                let parked = blockers(id)[0]
                rename(parked, "~\(parked)-\(UUID().uuidString.prefix(8))")
                if !ok { pending[parked] = nil }
            } else if let id = ids.first {
                rename(id, pending[id] ?? "") // refused by the core: reports the clash
                pending[id] = nil
            }
        }
        afterTagEdit()
        return ok
    }

    /// Stores the order the tag sheet arranged (on closing it).
    func applyTagOrder(_ ids: [Int64]) {
        reorderTags(Self.tagOrder(ids, tags: tags))
    }

    private func reorderTags(_ ids: [Int64]) {
        guard let vault, ids != tags.map(\.id) else { return }
        runCore { try vault.reorderTags(ids: ids) }
        afterTagEdit()
    }

    /// Deletes the tags; they drop off every booking (asked for beforehand, no undo:
    /// the core never revives a deleted tag, so other devices don't bring it back).
    func deleteTags(_ ids: Set<Int64>) {
        guard let vault, !ids.isEmpty else { return }
        runCore { for id in ids { try vault.deleteTag(id: id) } }
        afterTagEdit()
    }

    /// Every booking of `sources` carries `target` afterwards; the sources are gone.
    func mergeTags(_ sources: Set<Int64>, into target: Int64) {
        guard let vault else { return }
        runCore { for s in sources where s != target { try vault.mergeTagInto(source: s, target: target) } }
        afterTagEdit()
    }

    func removeUnusedTags() {
        deleteTags(Set(unusedTags.map(\.id)))
    }

    /// Names, colours and order show in the rows and the inspector: reload them.
    private func afterTagEdit() {
        reloadTags()
        reloadBookings()
        if let s = stickyBooking { stickyBooking = freshBooking(s) ?? s }
    }

    /// Position in the tag list (the core's order), unknown tags last.
    private func tagRank(_ t: BookingTag) -> Int {
        tags.firstIndex { $0.id == t.id } ?? Int.max
    }
}
