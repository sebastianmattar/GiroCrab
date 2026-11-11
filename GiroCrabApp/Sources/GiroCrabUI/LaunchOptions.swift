import Foundation

/// Command line / environment switches.
///
///  --demo-seed            fill a throw-away vault with demo data for all 7 banks
///  GIROCRAB_DEMO_SEED=1   same, via environment (works for Xcode schemes / iOS)
///  --demo-sync            (with demo data) start "Sync All" right away, to
///                         see the push-TAN / code / redirect 2FA sheets
///  --view alltag|sparplaene|belege  start in "Alltag & Konsum" / "Sparpläne & Perks" / "Belege & PostBox"
///  --fresh-vault          (developer) empty personal vault in a temp folder,
///                         fixed key, no Keychain – to try onboarding/setup
///  --try-add-bank <name>  (developer, with --fresh-vault) add a bank on start,
///                         e.g. 1822direkt, to see the setup form
///  --select <text>        (demo) preselect the account whose name contains <text>
///  --demo-sheet transfer|moneymoney|moneymoney-db|recurring
///                         (demo) open that sheet pre-filled with fictional data,
///                         for README screenshots
///  --dark                 (macOS) force the Dark Aqua appearance (README screenshots)
///  --light                (macOS, `swift run`) force the light Aqua appearance
///  --screenshot <file>    (macOS) render the main window, save a PNG, quit.
///                         Implies --demo-seed so the picture is reproducible.
public struct LaunchOptions: Sendable {
    public var demoSeed: Bool
    public var screenshotPath: String?
    public var dark: Bool
    public var demoSync: Bool
    /// "alle" (default), "alltag" or "sparplaene"
    public var initialView: String
    public var freshVault: Bool
    public var tryAddBank: String?
    /// (demo) preselect the account whose name contains this text
    public var selectAccount: String?
    /// (demo) preselect the booking whose merchant/counterparty contains this text
    public var pickBooking: String?
    /// (demo) "transfer", "moneymoney" or "moneymoney-db": open that sheet with fictional data
    public var demoSheet: String?

    public init(arguments: [String] = CommandLine.arguments, environment: [String: String] = ProcessInfo.processInfo.environment) {
        var demo = arguments.contains("--demo-seed")
        if let env = environment["GIROCRAB_DEMO_SEED"], ["1", "true", "yes"].contains(env.lowercased()) {
            demo = true
        }
        var shot: String?
        if let i = arguments.firstIndex(of: "--screenshot"), i + 1 < arguments.count {
            shot = arguments[i + 1]
        }
        if let i = arguments.firstIndex(of: "--select"), i + 1 < arguments.count {
            selectAccount = arguments[i + 1].lowercased()
        } else {
            selectAccount = nil
        }
        if let i = arguments.firstIndex(of: "--pick"), i + 1 < arguments.count {
            pickBooking = arguments[i + 1].lowercased()
        } else {
            pickBooking = nil
        }
        if let i = arguments.firstIndex(of: "--demo-sheet"), i + 1 < arguments.count {
            demoSheet = arguments[i + 1].lowercased()
        } else {
            demoSheet = nil
        }
        let sync = arguments.contains("--demo-sync")
        freshVault = arguments.contains("--fresh-vault")
        if let i = arguments.firstIndex(of: "--try-add-bank"), i + 1 < arguments.count {
            tryAddBank = arguments[i + 1].lowercased()
        } else {
            tryAddBank = nil
        }
        if (shot != nil || sync || demoSheet != nil) && !freshVault { demo = true }
        self.demoSync = sync
        if let i = arguments.firstIndex(of: "--view"), i + 1 < arguments.count {
            initialView = arguments[i + 1].lowercased()
        } else {
            initialView = "alle"
        }
        self.demoSeed = demo
        self.screenshotPath = shot
        self.dark = arguments.contains("--dark")
    }

    public static let current = LaunchOptions()
}
