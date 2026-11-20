import Foundation
#if os(macOS)
import AppKit
#endif

/// Dock badge with the number of unchecked bookings (the sidebar's
/// "Neu" count). Only the own vault sets it; while it is locked or the demo
/// is shown, the last known state stays – also across a relaunch.
struct DockBadge {
    static let defaultsKey = "dock.uncheckedCount"
    let defaults: UserDefaults

    init(defaults: UserDefaults = AppDefaults.store) { self.defaults = defaults }

    /// Last known count of unchecked bookings.
    var count: Int { defaults.integer(forKey: Self.defaultsKey) }

    /// Records `unchecked` when it comes from the open own vault. Returns
    /// whether the badge has to change.
    @discardableResult
    func record(_ unchecked: Int, ownVaultOpen: Bool) -> Bool {
        guard ownVaultOpen, unchecked != count else { return false }
        defaults.set(unchecked, forKey: Self.defaultsKey)
        return true
    }

    /// Badge text: the count, nothing at 0.
    static func label(_ count: Int) -> String? { count > 0 ? String(count) : nil }

    /// Shows the last known count on the Dock icon (macOS only).
    @MainActor
    func show() {
        #if os(macOS)
        NSApp?.dockTile.badgeLabel = Self.label(count)
        #endif
    }
}
