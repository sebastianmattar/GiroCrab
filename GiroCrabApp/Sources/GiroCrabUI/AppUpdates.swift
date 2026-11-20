import Foundation
import Observation

/// What the UI needs from an in-app updater. The macOS release bundle
/// (`GiroCrabMac`) registers a Sparkle-backed implementation at launch; the
/// Xcode / App Store / iOS builds register nothing, so the menu item and the
/// settings section stay hidden and no updater code is linked at all.
@MainActor
public protocol AppUpdateBackend: AnyObject {
    var canCheckForUpdates: Bool { get }
    var automaticallyChecksForUpdates: Bool { get set }
    var automaticallyDownloadsUpdates: Bool { get set }
    /// False when the user may not opt into automatic installation.
    var allowsAutomaticUpdates: Bool { get }
    var lastUpdateCheckDate: Date? { get }
    func checkForUpdates()
    /// Called after the user switched the update channel.
    func updateChannelDidChange()
}

/// Update channel. All channels share one signed appcast; Sparkle filters the
/// items by `<sparkle:channel>`. Beta and nightly are opt-ins for testers and
/// only reachable through a hidden settings control (⌥) or `defaults write`,
/// see docs/auto-updates.md.
public enum AppUpdateChannel: String, CaseIterable, Sendable {
    /// Only stable releases (`vX.Y.Z`).
    case stable
    /// Pre-releases (`vX.Y.Z-beta.N`, bundle version `X.Y.ZbN`) plus stable.
    case beta
    /// Signed nightly builds (bundle version `<last tag>.<UTC timestamp>`)
    /// plus beta and stable.
    case nightly

    /// UserDefaults key, shared with the Sparkle backend.
    public static let defaultsKey = "GiroCrabUpdateChannel"

    /// Sparkle channels this channel accepts in addition to the default one.
    public var sparkleChannels: Set<String> {
        switch self {
        case .stable: []
        case .beta: ["beta"]
        case .nightly: ["beta", "nightly"]
        }
    }

    /// Ordered from most to least stable.
    public var risk: Int {
        switch self {
        case .stable: 0
        case .beta: 1
        case .nightly: 2
        }
    }

    /// Channel a build belongs to, from its CFBundleVersion: `X.Y.Z` stable,
    /// `X.Y.ZbN` beta, anything with a 4th numeric component nightly. A beta
    /// or nightly installed directly (e.g. via Homebrew `girocrab@beta`) thus
    /// stays in its channel until the user picks another one.
    public static func inferred(fromBundleVersion version: String) -> AppUpdateChannel {
        if version.split(separator: ".").count >= 4 { return .nightly }
        if version.range(of: #"^\d+\.\d+\.\d+b\d+$"#, options: .regularExpression) != nil { return .beta }
        return .stable
    }

    /// The user's explicit choice, else the channel of the running build.
    public static var current: AppUpdateChannel {
        if let raw = AppDefaults.store.string(forKey: defaultsKey),
           let channel = AppUpdateChannel(rawValue: raw) {
            return channel
        }
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        return inferred(fromBundleVersion: build)
    }
}

/// Observable façade over the registered backend (if any), so SwiftUI menus
/// and settings refresh when the updater's state changes.
@MainActor
@Observable
public final class AppUpdates {
    public static let shared = AppUpdates()

    @ObservationIgnored private var backend: AppUpdateBackend?

    /// True once an updater is registered (Developer-ID release bundle only).
    public private(set) var isAvailable = false
    public private(set) var canCheckForUpdates = false
    public private(set) var allowsAutomaticUpdates = false
    public private(set) var lastUpdateCheckDate: Date?

    public var automaticallyChecksForUpdates = false {
        didSet {
            guard let backend, backend.automaticallyChecksForUpdates != automaticallyChecksForUpdates else { return }
            backend.automaticallyChecksForUpdates = automaticallyChecksForUpdates
        }
    }

    public var automaticallyDownloadsUpdates = false {
        didSet {
            guard let backend, backend.automaticallyDownloadsUpdates != automaticallyDownloadsUpdates else { return }
            backend.automaticallyDownloadsUpdates = automaticallyDownloadsUpdates
        }
    }

    /// The effective channel (explicit choice or the running build's channel).
    public private(set) var channel: AppUpdateChannel = AppUpdateChannel.current

    private init() {}

    /// Stores an explicit choice – also "stable" on a beta/nightly build.
    public func setChannel(_ newValue: AppUpdateChannel) {
        AppDefaults.store.set(newValue.rawValue, forKey: AppUpdateChannel.defaultsKey)
        guard newValue != channel else { return }
        channel = newValue
        backend?.updateChannelDidChange()
    }

    public func register(_ backend: AppUpdateBackend) {
        self.backend = backend
        isAvailable = true
        refresh()
    }

    /// Pull the backend's current state (called by the backend on changes).
    public func refresh() {
        guard let backend else { return }
        canCheckForUpdates = backend.canCheckForUpdates
        allowsAutomaticUpdates = backend.allowsAutomaticUpdates
        lastUpdateCheckDate = backend.lastUpdateCheckDate
        automaticallyChecksForUpdates = backend.automaticallyChecksForUpdates
        automaticallyDownloadsUpdates = backend.automaticallyDownloadsUpdates
        channel = AppUpdateChannel.current
    }

    public func checkForUpdates() {
        backend?.checkForUpdates()
    }
}
