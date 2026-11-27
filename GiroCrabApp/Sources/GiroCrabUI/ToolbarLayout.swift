import Foundation
import Observation

/// The macOS main toolbar as GiroCrab stores it (#2049): which buttons it shows in which order, in a leading
/// and a trailing group, and whether the scope control is shown. SwiftUI's own customizable toolbar
/// (`toolbar(id:)`) loses AppKit's silent reorders and moves items back or crashes (#1832, #2010, #2037), so
/// `MainToolbar` builds a plain, non-customizable toolbar from this layout.
struct ToolbarLayout: Codable, Equatable {
    /// The buttons; the raw values are the item IDs of the former customizable toolbar.
    enum Item: String, Codable, CaseIterable {
        case transfer, newBooking, sync, inspector, privacy, palette, export, print, rules, lock, settings
    }

    var showsScope: Bool
    var leading: [Item]
    var trailing: [Item]

    /// The former default toolbar: scope, five buttons, then (flexible space and) the search field.
    static let standard = ToolbarLayout(showsScope: true, leading: [.transfer, .newBooking, .sync, .inspector, .privacy], trailing: [])

    static let defaultsKey = "toolbar.main.layout"
    /// AppKit's saved layout of the former customizable toolbar `girocrab.main`.
    static let legacyDefaultsKey = "NSToolbar Configuration girocrab.main"

    init(showsScope: Bool, leading: [Item], trailing: [Item]) {
        self.showsScope = showsScope
        self.leading = leading
        self.trailing = trailing
    }

    /// Unknown IDs (from a newer or older version) are dropped, and each button is shown at most once.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        var seen = Set<Item>()
        func items(_ key: CodingKeys) throws -> [Item] {
            try c.decode([String].self, forKey: key).compactMap(Item.init).filter { seen.insert($0).inserted }
        }
        showsScope = try c.decode(Bool.self, forKey: .showsScope)
        leading = try items(.leading)
        trailing = try items(.trailing)
    }

    /// Converts the `TB Item Identifiers` of the former customizable toolbar: buttons before the first
    /// flexible space go to the leading group, those after it to the trailing group. SwiftUI's own items
    /// (sidebar toggle, separator, search), other spaces and unknown IDs are ignored.
    static func converting(legacyItems: [String]) -> ToolbarLayout {
        var layout = ToolbarLayout(showsScope: legacyItems.contains("scope"), leading: [], trailing: [])
        var afterFlexibleSpace = false
        for id in legacyItems {
            if id == "NSToolbarFlexibleSpaceItem" {
                afterFlexibleSpace = true
            } else if let item = Item(rawValue: id), !layout.leading.contains(item), !layout.trailing.contains(item) {
                if afterFlexibleSpace { layout.trailing.append(item) } else { layout.leading.append(item) }
            }
        }
        return layout
    }

    /// One-time takeover: when GiroCrab has no layout of its own yet, converts AppKit's saved layout of the former
    /// customizable toolbar and deletes it (that saved layout is what crashes SwiftUI at launch, #2010).
    static func migrateLegacy(in defaults: UserDefaults) {
        guard defaults.object(forKey: defaultsKey) == nil, let legacy = defaults.dictionary(forKey: legacyDefaultsKey) else { return }
        if let items = legacy["TB Item Identifiers"] as? [String] {
            converting(legacyItems: items).save(to: defaults)
        }
        defaults.removeObject(forKey: legacyDefaultsKey)
    }

    static func load(from defaults: UserDefaults) -> ToolbarLayout {
        defaults.data(forKey: defaultsKey).flatMap { try? JSONDecoder().decode(ToolbarLayout.self, from: $0) } ?? .standard
    }

    func save(to defaults: UserDefaults) {
        if let data = try? JSONEncoder().encode(self) { defaults.set(data, forKey: Self.defaultsKey) }
    }
}

/// The app's toolbar layout. `MainToolbar` reads `layout` from here rather than through `@AppStorage`: an observable
/// property re-renders the toolbar only when the layout itself changes, never on a selection (see `MainToolbar`).
/// Created on the first toolbar build, so the takeover of the old layout runs before the toolbar exists.
@MainActor @Observable
final class ToolbarLayoutStore {
    static let shared = ToolbarLayoutStore(defaults: AppDefaults.store)

    @ObservationIgnored private let defaults: UserDefaults
    var layout: ToolbarLayout {
        didSet { layout.save(to: defaults) }
    }

    init(defaults: UserDefaults) {
        self.defaults = defaults
        ToolbarLayout.migrateLegacy(in: defaults)
        layout = ToolbarLayout.load(from: defaults)
    }
}
