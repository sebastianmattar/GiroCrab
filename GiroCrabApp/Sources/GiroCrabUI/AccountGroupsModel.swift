import Foundation
import SwiftUI
import GiroCrabCore

extension UserGroup: @retroactive Identifiable {
    public var id: String { uuid }
}

extension Account {
    /// Archived ("aufgelöst"): kept with all bookings, no syncs or rule runs.
    var isArchived: Bool { archivedUnix != nil }
    var archivedDate: Date? { archivedUnix.map { Date(timeIntervalSince1970: TimeInterval($0)) } }

    /// Whether the sidebar row marks the account as outside the overall money
    /// totals (eye.slash + dimming, #337) — its own switch is off, an above
    /// group is excluded or it is archived; `inTotal` is the effective flag.
    var showsTotalExclusionIndicator: Bool { !inTotal }

    /// IBAN and account number without spaces, for sidebar / list search.
    var compactAccountIds: [String] {
        [iban, accountNumber]
            .map { $0.filter { !$0.isWhitespace }.lowercased() }
            .filter { !$0.isEmpty }
    }

    /// Name, Kontoinhaber, bank, IBAN or account number. Spaces in IBANs /
    /// Kontonummern are ignored so "DE89 3704 …" finds the stored value.
    func matchesSidebarSearch(_ raw: String) -> Bool {
        let q = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return true }
        let compactQ = q.filter { !$0.isWhitespace }.lowercased()
        if compactQ.count >= 3, compactAccountIds.contains(where: { $0.contains(compactQ) }) {
            return true
        }
        let terms = q.split(whereSeparator: \.isWhitespace).map { String($0).lowercased() }
        let text = [name, owner, bankName].map { $0.lowercased() }
        return terms.allSatisfy { term in
            let compactTerm = term.filter { !$0.isWhitespace }
            return text.contains { $0.contains(term) }
                || (!compactTerm.isEmpty && compactAccountIds.contains { $0.contains(compactTerm) })
        }
    }
}

extension View {
    /// `.help` with the text only when there is one (no empty tooltip frame).
    @ViewBuilder
    func helpIfPresent(_ text: String?) -> some View {
        if let text { help(text) } else { self }
    }
}

extension AppModel {
    /// Non-empty sidebar account filter.
    var isSidebarAccountSearching: Bool {
        !sidebarAccountSearch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Active + not archived (unless shown) + matches the sidebar search.
    func isSidebarAccountListed(_ account: Account) -> Bool {
        (showArchivedAccounts || !account.isArchived) && account.matchesSidebarSearch(sidebarAccountSearch)
    }

    /// Whether a kind or bank section is shown. While searching: only with a
    /// hit. Otherwise it stays unless archived accounts are hidden and it holds
    /// nothing but archived accounts (#814); a section without accounts stays.
    func isSidebarSectionListed(_ accounts: [Account]) -> Bool {
        let listed = accounts.contains(where: isSidebarAccountListed)
        if isSidebarAccountSearching { return listed }
        return listed || accounts.isEmpty || showArchivedAccounts
    }

    /// Archived accounts are hidden and this group (with its subgroups) holds
    /// accounts, all of them archived (#814). A group without accounts stays.
    func isUserGroupOnlyArchived(_ g: UserGroup) -> Bool {
        guard !showArchivedAccounts else { return false }
        let accounts = userGroupSubtree(g.uuid).flatMap(\.accounts)
        return !accounts.isEmpty && accounts.allSatisfy(\.isArchived)
    }

    /// `isUserGroupOnlyArchived` for the group or any group above it, so a
    /// subgroup never stays behind under a hidden parent.
    private func isUserGroupHiddenAsArchived(_ g: UserGroup) -> Bool {
        var node: UserGroup? = g
        while let n = node {
            if isUserGroupOnlyArchived(n) { return true }
            node = n.parentUuid.flatMap { p in userGroups.first { $0.uuid == p } }
        }
        return false
    }

    /// Shown in the sidebar while searching: name matches, or a listed account
    /// lives in this group or a subgroup. Collapse is ignored so matches stay reachable.
    func isUserGroupListed(_ g: UserGroup) -> Bool {
        guard isSidebarAccountSearching else { return isUserGroupVisible(g) && !isUserGroupHiddenAsArchived(g) }
        let q = sidebarAccountSearch
        return userGroupSubtree(g.uuid).contains { node in
            Self.sidebarTextMatches(node.name, query: q)
                || node.accounts.contains(where: isSidebarAccountListed)
        }
    }

    /// Kind-section open while searching, or according to the saved collapse set.
    func isKindGroupOpen(_ groupID: Int64) -> Bool {
        isSidebarAccountSearching || !collapsedGroups.contains(groupID)
    }

    /// Bank-section open while searching, or according to the saved collapse set.
    func isBankGroupOpen(_ key: String) -> Bool {
        isSidebarAccountSearching || !collapsedBankGroups.contains(key)
    }

    /// User-group open while searching, or according to the saved collapse set.
    func isUserGroupOpen(_ g: UserGroup) -> Bool {
        isSidebarAccountSearching || !collapsedUserGroups.contains(g.uuid)
    }

    /// Bank key for sidebar grouping: `bankName`, else the connection name.
    func bankGroupKey(for account: Account) -> String {
        let name = account.bankName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty { return name }
        if let cid = account.connectionId,
           let conn = connections.first(where: { $0.id == cid }) {
            let dn = conn.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
            if !dn.isEmpty { return dn }
        }
        return ""
    }

    /// Accounts grouped by bank, sorted by display name ("Ohne Bank" last).
    var bankGroups: [BankGroup] {
        var buckets: [String: [Account]] = [:]
        for a in allAccounts {
            buckets[bankGroupKey(for: a), default: []].append(a)
        }
        return buckets.keys.sorted { a, b in
            if a.isEmpty != b.isEmpty { return b.isEmpty }
            return a.localizedCaseInsensitiveCompare(b) == .orderedAscending
        }.map { key in
            let accounts = buckets[key] ?? []
            // like the other group sums: only effectively included accounts
            // count (#337; `inTotal` also covers archiving and the own switch)
            let balance = accounts
                .filter { $0.currency == "EUR" && $0.inTotal }
                .reduce(Int64(0)) { $0 + $1.balanceCents }
            let symbol: String = {
                if let cid = accounts.first?.connectionId,
                   let conn = connections.first(where: { $0.id == cid }) {
                    return conn.sfSymbol
                }
                return accounts.first?.sfSymbol ?? "building.columns"
            }()
            return BankGroup(
                id: key,
                name: key.isEmpty ? String(localized: "Ohne Bank") : key,
                sfSymbol: symbol,
                balanceCents: balance,
                accounts: accounts
            )
        }
    }

    func bankGroup(for key: String) -> BankGroup? {
        bankGroups.first { $0.id == key }
    }

    func toggleCollapsedBank(_ key: String) {
        if collapsedBankGroups.contains(key) { collapsedBankGroups.remove(key) }
        else { collapsedBankGroups.insert(key) }
    }

    /// Group title matches every whitespace-separated term (case-insensitive).
    static func sidebarTextMatches(_ text: String, query: String) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return true }
        let hay = text.lowercased()
        return q.split(whereSeparator: \.isWhitespace)
            .map { String($0).lowercased() }
            .allSatisfy { hay.contains($0) }
    }
}

/// How the accounts section of the sidebar is organised.
enum AccountSidebarMode: String, CaseIterable, Identifiable {
    case byGroup
    case byBank
    case byKind
    var id: String { rawValue }
    var title: LocalizedStringKey {
        switch self {
        case .byGroup: "Nach Gruppe"
        case .byBank: "Nach Bank"
        case .byKind: "Nach Kontoart"
        }
    }
    var symbol: String {
        switch self {
        case .byGroup: "folder"
        case .byBank: "building.columns"
        case .byKind: "square.stack.3d.up"
        }
    }

    /// Short label for the segmented control in the sidebar.
    var shortTitle: LocalizedStringKey {
        switch self {
        case .byGroup: "Gruppen"
        case .byBank: "Banken"
        case .byKind: "Kontoart"
        }
    }

    /// Command-palette / menu title for switching to this mode.
    var switchTitle: String {
        switch self {
        case .byGroup: String(localized: "Konten nach Gruppe anzeigen")
        case .byBank: String(localized: "Konten nach Bank anzeigen")
        case .byKind: String(localized: "Konten nach Kontoart anzeigen")
        }
    }
}

/// Sidebar section when accounts are shown by bank name.
struct BankGroup: Identifiable {
    /// Stable key: trimmed `bankName`, else connection display name, else "".
    var id: String
    var name: String
    var sfSymbol: String
    var balanceCents: Int64
    var accounts: [Account]
}

/// Group dialogs. One state for all of them, set from menus, ⇧⌘N, the
/// editor sheet and swipe actions, presented in one place.
enum GroupDialog {
    /// "Neue Gruppe …", optionally moving an account into the new group;
    /// with `parent`: "Neue Untergruppe …" of that group.
    case create(movingAccount: Int64? = nil, parent: UserGroup? = nil)
    case rename(UserGroup)
    /// "Umbenennen …" of an account (same alert as for groups).
    case renameAccount(Account)
    /// "Kontoinhaber …" of an account (#280); empty text clears it.
    case editOwner(Account)
    /// "Umbenennen …" of a category (same alert as for groups and accounts).
    case renameCategory(GiroCrabCore.Category)
    /// "Umbenennen …" of the „Ohne Kategorie“ row (#597); empty text restores the default.
    case renameUncategorized
    case delete(UserGroup)
    /// iOS swipe "Gruppe …".
    case moveAccount(Account)
    /// Renaming a group to the name of another one combines them.
    case merge(from: UserGroup, into: UserGroup)

    /// Text-input alert vs. confirmation dialog.
    var needsText: Bool {
        switch self {
        case .create, .rename, .renameAccount, .renameCategory, .renameUncategorized, .editOwner: true
        case .delete, .moveAccount, .merge: false
        }
    }

    var title: Text {
        switch self {
        case .create(_, let parent?): Text("Neue Untergruppe in „\(parent.name)“")
        case .create: Text("Neue Gruppe")
        case .rename: Text("Gruppe umbenennen")
        case .renameAccount: Text("Konto umbenennen")
        case .renameCategory: Text("Kategorie umbenennen")
        case .renameUncategorized: Text("Bezeichnung ändern")
        case .editOwner: Text("Kontoinhaber")
        case .delete(let g): Text("Gruppe „\(g.name)“ löschen?")
        case .moveAccount(let a): Text("„\(a.name)“ verschieben nach")
        case .merge(_, let into): Text("Mit „\(into.name)“ zusammenführen?")
        }
    }

    var isRename: Bool {
        switch self {
        case .rename, .renameAccount, .renameCategory: true
        default: false
        }
    }

    /// Prefilled text of the input alert.
    var initialText: String {
        switch self {
        case .rename(let g): g.name
        case .renameAccount(let a): a.name
        case .editOwner(let a): a.owner
        case .renameCategory(let c): c.name
        case .renameUncategorized: ""
        default: ""
        }
    }
}

/// One section of a group's "+" menu (#229): the accounts of another group,
/// or of "Ohne Gruppe" (`id` "").
struct AddableAccounts: Identifiable {
    let id: String
    let title: String
    let accounts: [Account]
}

/// What tells accounts of the same name apart (several "Girokonto"): the
/// IBAN, else the account number (depots), else the bank – marked when the
/// account is archived (closed), so it doesn't pass for an active one.
func accountDetail(_ a: Account) -> String {
    let id = accountIdentity(a)
    return a.isArchived ? String(localized: "Archiviert · \(id)") : id
}

/// IBAN, else account number, else bank.
func accountIdentity(_ a: Account) -> String {
    if !a.iban.isEmpty { return formatIBAN(a.iban) }
    if !a.accountNumber.isEmpty { return String(localized: "Konto \(a.accountNumber)") }
    return a.bankName
}

/// What the "Symbol & Farbe" popover edits.
enum IconColorTarget: Equatable {
    case account(Int64)
    case group(String)
    case category(Int64)
}

extension AppModel {
    /// The view shown: the explicit choice, else "Nach Gruppe" once a group exists.
    var accountView: AccountSidebarMode {
        get { accountViewChoice ?? (hasUserGroups ? .byGroup : .byKind) }
        set { accountViewChoice = newValue }
    }

    var hasUserGroups: Bool { userGroups.contains { !$0.isUngrouped } }

    /// Real groups only (without "Ohne Gruppe"), for menus.
    var namedUserGroups: [UserGroup] { userGroups.filter { !$0.isUngrouped } }

    /// The group and all its subgroups (the list is a flattened tree: a
    /// group's subgroups follow it with a greater depth).
    func userGroupSubtree(_ uuid: String) -> [UserGroup] {
        guard !uuid.isEmpty, let i = userGroups.firstIndex(where: { $0.uuid == uuid }) else {
            return userGroups.filter { $0.uuid == uuid }
        }
        let depth = userGroups[i].depth
        return [userGroups[i]] + userGroups[(i + 1)...].prefix { $0.depth > depth && !$0.isUngrouped }
    }

    /// Accounts of the group including its subgroups.
    func userGroupAccountIDs(_ uuid: String) -> [Int64] {
        userGroupSubtree(uuid).flatMap { $0.accounts.map(\.id) }
    }

    /// Accounts behind the group's sum, like the core's `balance_cents`: no
    /// archived accounts, no subgroups taken out of the total on their own
    /// (#208), and only effectively included accounts (#337 — the account's
    /// own switch, its group or a group that group is in, archiving).
    func userGroupBalanceAccountIDs(_ uuid: String) -> [Int64] {
        var skipBelow: UInt32?
        var ids: [Int64] = []
        // an excluded selection sums its own accounts (review point 1 on
        // #1100): the chart follows the header's own-sum rule, not the
        // effective flag that the group's exclusion has switched off
        let ownSum = userGroups.first(where: { $0.uuid == uuid })?.inTotal == false
        for (n, g) in userGroupSubtree(uuid).enumerated() {
            if let d = skipBelow {
                if g.depth > d { continue }
                skipBelow = nil
            }
            if n > 0 && !g.includeInTotal { skipBelow = g.depth; continue }
            ids += ownSum
                ? g.accounts.filter { $0.includeInTotal && !$0.isArchived }.map(\.id)
                : g.accounts.filter(\.inTotal).map(\.id)
        }
        return ids
    }

    /// Groups `g` can be moved into: all but itself and its own subgroups.
    func parentCandidates(for g: UserGroup) -> [UserGroup] {
        let own = Set(userGroupSubtree(g.uuid).map(\.uuid))
        return namedUserGroups.filter { !own.contains($0.uuid) }
    }

    /// Shown in the sidebar: no group above it is collapsed.
    func isUserGroupVisible(_ g: UserGroup) -> Bool {
        var parent = g.parentUuid
        while let p = parent {
            if collapsedUserGroups.contains(p) { return false }
            parent = userGroups.first { $0.uuid == p }?.parentUuid
        }
        return true
    }

    /// Makes `g` a subgroup of `parent` (`nil` = top level).
    func setUserGroupParent(_ g: UserGroup, parent: String?) {
        guard let vault, !g.isUngrouped else { return }
        do {
            try vault.setUserGroupParent(uuid: g.uuid, parentUuid: parent)
            if let parent { collapsedUserGroups.remove(parent) }
            afterGroupChange()
        } catch {
            errorMessage = describe(error)
        }
    }

    func userGroup(of account: Account) -> UserGroup? {
        account.userGroupUuid.flatMap { u in userGroups.first { $0.uuid == u } }
    }

    /// Another group with this name (case-insensitive), if any.
    func userGroup(named name: String, except uuid: String) -> UserGroup? {
        let key = name.trimmingCharacters(in: .whitespaces).lowercased()
        return namedUserGroups.first { $0.uuid != uuid && $0.name.lowercased() == key }
    }

    /// Renames a group. If another group already has that name, nothing is
    /// changed and the merge question is returned instead.
    func renameUserGroup(_ g: UserGroup, to name: String) -> GroupDialog? {
        if let other = userGroup(named: name, except: g.uuid) { return .merge(from: g, into: other) }
        updateUserGroup(g, name: name)
        return nil
    }

    /// Combines `from` into `into`: its accounts move over, `from` is removed.
    func mergeUserGroups(_ from: UserGroup, into: UserGroup) {
        guard let vault, !from.isUngrouped, !into.isUngrouped else { return }
        do {
            try vault.mergeUserGroups(fromUuid: from.uuid, intoUuid: into.uuid)
            if sidebarSelection == .userGroup(from.uuid) { sidebarSelection = .userGroup(into.uuid) }
            collapsedUserGroups.remove(from.uuid)
            afterGroupChange()
            toast = String(localized: "„\(from.name)“ mit „\(into.name)“ zusammengeführt")
        } catch {
            errorMessage = describe(error)
        }
    }

    private func afterGroupChange() {
        invalidateTotals()
        reloadAccounts()
        reloadHistory()
        // A cancelled category refresh may not have published its totals yet.
        reloadCategories()
        reloadBookings()
        // the dashboard shows overall money figures: reload it when visible,
        // else mark it stale so the next visit loads it fresh (#337)
        if scope == .investments { loadDashboard() } else { dashboardStale = true }
    }

    /// Creates a group (and moves `accountID` into it). Returns the uuid.
    @discardableResult
    func createUserGroup(name: String, symbol: String = "folder", moving accountID: Int64? = nil, parent: String? = nil) -> String? {
        guard let vault else { return nil }
        do {
            let uuid = try vault.createUserGroup(name: name, sfSymbol: symbol)
            if let parent {
                try vault.setUserGroupParent(uuid: uuid, parentUuid: parent)
                collapsedUserGroups.remove(parent)
            }
            if let accountID { try vault.moveAccountToGroup(accountId: accountID, groupUuid: uuid, index: nil) }
            accountView = .byGroup
            collapsedUserGroups.remove(uuid)
            afterGroupChange()
            return uuid
        } catch {
            errorMessage = describe(error)
            return nil
        }
    }

    func updateUserGroup(_ g: UserGroup, name: String? = nil, symbol: String? = nil, includeInTotal: Bool? = nil) {
        guard let vault, !g.isUngrouped else { return }
        do {
            try vault.updateUserGroup(
                uuid: g.uuid, name: name ?? g.name, sfSymbol: symbol ?? g.sfSymbol,
                includeInTotal: includeInTotal ?? g.includeInTotal)
            afterGroupChange()
        } catch {
            errorMessage = describe(error)
        }
    }

    func setUserGroupColor(_ g: UserGroup, hex: String?) {
        guard let vault, !g.isUngrouped else { return }
        do {
            try vault.setUserGroupColor(uuid: g.uuid, colorHex: hex)
            afterGroupChange()
        } catch {
            errorMessage = describe(error)
        }
    }

    func setAccountOwner(_ accountId: Int64, to owner: String) {
        guard let vault else { return }
        do {
            try vault.setAccountOwner(accountId: accountId, owner: owner)
            reloadAccounts()
        } catch {
            errorMessage = describe(error)
        }
    }

    /// The account's own switch for the overall money totals (#337). The same
    /// full refresh as a group change: accounts, group sums, bookings / KPIs,
    /// history — and the dashboard when it is (or becomes) visible.
    func setAccountIncludeInTotal(_ account: Account, _ include: Bool) {
        guard let vault else { return }
        do {
            try vault.setAccountIncludeInTotal(accountId: account.id, includeInTotal: include)
            afterGroupChange()
        } catch {
            errorMessage = describe(error)
        }
    }

    /// Why the account is outside the overall money figures despite its own
    /// switch being on (the group it is in, or a group that group is in), for
    /// the account row's tooltip; `nil` when nothing needs explaining (#337).
    /// Archiving is named as its own reason. Names the group that wins.
    func totalExclusionReason(_ account: Account, redaction: ReportRedaction = []) -> String? {
        if account.isArchived {
            return String(localized: "Dieses Konto ist archiviert und zählt nicht zu den Gesamtsummen.")
        }
        guard account.includeInTotal, !account.inTotal else { return nil }
        var cur = account.userGroupUuid
        while let u = cur {
            guard let g = userGroups.first(where: { $0.uuid == u }) else { break }
            if !g.includeInTotal {
                // `.help` cannot carry `.sensitive(.labels)`: substitute the
                // name for both report captures and the view's redaction mode.
                if redaction.union(reportRedaction).contains(.labels) {
                    return String(localized: "Eine Gruppe ist ausgeschlossen – dieses Konto zählt deshalb trotz eingeschaltetem Kontoschalter nicht zu den Gesamtsummen.")
                }
                return String(localized: "Gruppe „\(g.name)“ ist ausgeschlossen – dieses Konto zählt deshalb trotz eingeschaltetem Kontoschalter nicht zu den Gesamtsummen.")
            }
            cur = g.parentUuid
        }
        return nil
    }

    /// The exclusion indicator has its own help region, independent of the row.
    func totalExclusionIconHelp(_ account: Account, redaction: ReportRedaction = []) -> String {
        totalExclusionReason(account, redaction: redaction)
            ?? String(localized: "Nicht in der Gesamtsumme")
    }

    func setAccountKind(_ accountId: Int64, to kind: AccountKind) {
        guard let vault else { return }
        do {
            try vault.setAccountKind(accountId: accountId, kind: kind)
            reloadAccounts()
            dashboardStale = true
        } catch {
            errorMessage = describe(error)
        }
    }

    /// „Sichern“ in Kontoeinstellungen: writes the fields that differ from `original`, the account
    /// when the sheet opened, so a value a sync brought in meanwhile is no edit (#980). One reload.
    func saveAccountEdits(_ original: Account, name: String, owner: String, kind: AccountKind, note: String) {
        guard let vault else { return }
        let id = original.id
        let kindChanged = kind != original.kind && !isOffline(id)
        do {
            if name != original.name.trimmingCharacters(in: .whitespaces) { try vault.renameAccount(accountId: id, name: name) }
            if owner != original.owner.trimmingCharacters(in: .whitespaces) { try vault.setAccountOwner(accountId: id, owner: owner) }
            if kindChanged { try vault.setAccountKind(accountId: id, kind: kind) }
            if note != original.note.trimmingCharacters(in: .whitespacesAndNewlines) { try vault.setAccountNote(accountId: id, note: note) }
        } catch {
            errorMessage = describe(error)
        }
        reloadAccounts()
        if kindChanged { dashboardStale = true }
    }

    func renameAccount(_ accountId: Int64, to name: String) {
        guard let vault else { return }
        do {
            try vault.renameAccount(accountId: accountId, name: name)
            reloadAccounts()
        } catch {
            errorMessage = describe(error)
        }
    }

    /// Popover binding for one row: shown while `iconColorTarget` is it.
    func iconColorBinding(_ target: IconColorTarget) -> Binding<Bool> {
        Binding(
            get: { self.iconColorTarget == target },
            set: { if !$0, self.iconColorTarget == target { self.iconColorTarget = nil } }
        )
    }

    func deleteUserGroup(_ g: UserGroup) {
        guard let vault, !g.isUngrouped else { return }
        do {
            try vault.deleteUserGroup(uuid: g.uuid)
            if sidebarSelection == .userGroup(g.uuid) { sidebarSelection = .allAccounts }
            collapsedUserGroups.remove(g.uuid)
            afterGroupChange()
            toast = String(localized: "Gruppe „\(g.name)“ gelöscht")
        } catch {
            errorMessage = describe(error)
        }
    }

    /// "Ohne Gruppe" for the settings list (#229): always there, also when
    /// every account is in a group (the core then leaves it out).
    var ungroupedForSettings: UserGroup {
        userGroups.first(where: \.isUngrouped) ?? UserGroup(
            uuid: "", name: String(localized: "Ohne Gruppe"), sfSymbol: "tray", colorHex: "",
            sortOrder: .max, includeInTotal: true, inTotal: true, isUngrouped: true,
            parentUuid: nil, depth: 0, balanceCents: 0, accounts: []
        )
    }

    /// Accounts that can be added to group `uuid`: those without a group
    /// first, then the members of every other group in sidebar order.
    /// Archived (closed) accounts are left out – they stay where they are.
    func addableAccounts(for uuid: String) -> [AddableAccounts] {
        let others = userGroups.filter { $0.uuid != uuid }
        return (others.filter(\.isUngrouped) + others.filter { !$0.isUngrouped }).compactMap {
            let active = $0.accounts.filter { !$0.isArchived }
            guard !active.isEmpty else { return nil }
            return AddableAccounts(id: $0.uuid, title: $0.isUngrouped ? String(localized: "Ohne Gruppe") : $0.name, accounts: active)
        }
    }

    /// `uuid` nil or empty = "Ohne Gruppe"; `index` nil = at the end.
    func moveAccount(_ accountID: Int64, to uuid: String?, at index: Int? = nil) {
        guard let vault else { return }
        let target = (uuid?.isEmpty ?? true) ? nil : uuid
        do {
            try vault.moveAccountToGroup(accountId: accountID, groupUuid: target, index: index.map { UInt32(max(0, $0)) })
            afterGroupChange()
        } catch {
            errorMessage = describe(error)
        }
    }

    /// A group dropped onto a group row in the sidebar: into `target` (as its
    /// subgroup), or – `before` – right above it as its sibling; onto "Ohne
    /// Gruppe" it moves to the top level, at the end. Refuses moving a group
    /// into itself or its own subgroups. Returns whether anything happened.
    @discardableResult
    func dropUserGroup(_ uuid: String, onto target: UserGroup, before: Bool) -> Bool {
        guard !uuid.isEmpty, uuid != target.uuid,
              let moving = userGroups.first(where: { $0.uuid == uuid }),
              !userGroupSubtree(uuid).contains(where: { $0.uuid == target.uuid })
        else { return false }
        let parent: String? = target.isUngrouped ? nil : (before ? target.parentUuid : target.uuid)
        if moving.parentUuid != parent { setUserGroupParent(moving, parent: parent) }
        if target.isUngrouped || before {
            var order = namedUserGroups.map(\.uuid).filter { $0 != uuid }
            let at = target.isUngrouped ? order.count : (order.firstIndex(of: target.uuid) ?? order.count)
            order.insert(uuid, at: at)
            reorderUserGroups(order)
        }
        return true
    }

    func reorderUserGroups(_ uuids: [String]) {
        guard let vault else { return }
        run2 { try vault.reorderUserGroups(uuids: uuids.filter { !$0.isEmpty }) }
        reloadAccounts()
    }

    func reorderAccounts(in uuid: String, ids: [Int64]) {
        guard let vault else { return }
        run2 { try vault.reorderAccountsInGroup(groupUuid: uuid.isEmpty ? nil : uuid, accountIds: ids) }
        reloadAccounts()
    }

    /// Whether the sidebar may show a drop mark for group `uuid` over `target`
    /// (see `dropUserGroup`): `into` = becomes its subgroup (lower half of the
    /// header or the group's accounts), else the line above its header. Never
    /// into itself or its own subgroups; "Ohne Gruppe" only takes groups above it.
    func canDropGroup(_ uuid: String, on target: UserGroup, into: Bool) -> Bool {
        guard !uuid.isEmpty, !userGroupSubtree(uuid).contains(where: { $0.uuid == target.uuid }) else { return false }
        return !(into && target.isUngrouped)
    }

    /// List `.onMove` for the accounts of group `uuid` (settings, #229).
    func moveAccounts(in uuid: String, from: IndexSet, to: Int) {
        guard let g = userGroups.first(where: { $0.uuid == uuid }) else { return }
        var ids = g.accounts.map(\.id)
        ids.move(fromOffsets: from, toOffset: to)
        reorderAccounts(in: uuid, ids: ids)
    }

    /// List `.onMove` for groups (indices into `namedUserGroups`).
    func moveUserGroups(from: IndexSet, to: Int) {
        var list = namedUserGroups.map(\.uuid)
        list.move(fromOffsets: from, toOffset: to)
        reorderUserGroups(list)
    }

    /// "Gruppen bearbeiten …": the group editor as a sheet of the main window.
    func openGroupManager() { showGroupManager = true }

    /// Opens Settings on the given tab (macOS: Settings window, iOS: sheet).
    func openSettings(_ tab: SettingsTab) {
        settingsTab = tab
        #if os(macOS)
        settingsRequested = true
        #else
        showSettings = true
        #endif
    }

    func toggleCollapsed(_ g: UserGroup) {
        if collapsedUserGroups.contains(g.uuid) { collapsedUserGroups.remove(g.uuid) } else { collapsedUserGroups.insert(g.uuid) }
    }

    private func run2(_ body: () throws -> Void) {
        do { try body() } catch { errorMessage = describe(error) }
    }
}

/// One section of an account menu (e.g. the Belege filter, #412): the
/// accounts of a sidebar group in sidebar order.
struct AccountMenuSection: Identifiable {
    var id: String
    /// Group name; subgroups carry their path ("Familie › Kinder") because a
    /// menu cannot indent its entries.
    var title: String
    var accounts: [Account]
}

extension AppModel {
    /// Another account carries the same name (two "Konto flex" of different holders): the owner tells
    /// them apart, so menus and lists add it.
    func hasNamesake(_ account: Account) -> Bool {
        accountsByID.values.contains { $0.id != account.id && $0.name == account.name }
    }

    /// What tells the account apart from its namesakes: its owner, or – when it has none or a namesake
    /// has the same – the end of its IBAN / account number. Empty without a namesake.
    func accountDistinction(_ account: Account) -> String {
        let others = accountsByID.values.filter { $0.id != account.id && $0.name == account.name }
        guard !others.isEmpty else { return "" }
        if !account.owner.isEmpty, !others.contains(where: { $0.owner == account.owner }) { return account.owner }
        let number = (account.iban.isEmpty ? account.accountNumber : account.iban).filter { !$0.isWhitespace }
        return number.isEmpty ? account.owner : "…" + number.suffix(4)
    }

    /// The account's name, with what tells it apart when another account has the same name.
    func accountLabel(_ account: Account) -> String {
        let distinction = accountDistinction(account)
        return distinction.isEmpty ? account.name : "\(account.name) · \(distinction)"
    }
}

extension AppModel {
    /// All accounts grouped and ordered exactly like the sidebar shows them
    /// in the current `accountView` (by group, bank or kind). Empty sections
    /// are dropped.
    var accountMenuSections: [AccountMenuSection] {
        let sections: [AccountMenuSection]
        switch accountView {
        case .byKind:
            sections = groups.map { AccountMenuSection(id: "k\($0.id)", title: $0.name, accounts: $0.accounts) }
        case .byBank:
            sections = bankGroups.map { AccountMenuSection(id: "b" + $0.id, title: $0.name, accounts: $0.accounts) }
        case .byGroup:
            // a flattened tree: subgroups follow their group with a greater depth
            var path: [String] = []
            sections = userGroups.map { g in
                path = Array(path.prefix(Int(g.depth)))
                path.append(g.name)
                return AccountMenuSection(id: "g" + g.uuid, title: path.joined(separator: " › "), accounts: g.accounts)
            }
        }
        return sections.filter { !$0.accounts.isEmpty }
    }
}
