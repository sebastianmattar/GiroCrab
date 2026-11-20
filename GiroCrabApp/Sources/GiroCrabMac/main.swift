// macOS entry point for `swift run GiroCrabMac` / `cargo run` (the Xcode
// multiplatform target uses Xcode/App/GiroCrabApp.swift instead).
//
//   GiroCrabMac --demo-seed                 explore with demo data
//   GiroCrabMac --screenshot out.png        render the window to a PNG and quit
//
// Compiled empty on other platforms, so the package scheme (and its tests)
// also builds for the iOS Simulator.
#if os(macOS)
import AppKit
import SwiftUI
import GiroCrabUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    #if canImport(Sparkle)
    /// Kept alive for the app's lifetime; nil unless this is a configured
    /// release bundle (see Updater.swift).
    private var updater: SparkleUpdateBackend?
    #endif

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        if LaunchOptions.current.dark {
            NSApp.appearance = NSAppearance(named: .darkAqua)
        } else if CommandLine.arguments.contains("--light") {
            NSApp.appearance = NSAppearance(named: .aqua)
        }
        if let icon = Self.appIcon() {
            NSApp.applicationIconImage = icon
        }
        // In --screenshot mode don't steal the keyboard focus from the user.
        if LaunchOptions.current.screenshotPath == nil {
            NSApp.activate(ignoringOtherApps: true)
        }
        observeWindowsForDockPresence()
        MainActor.assumeIsolated { Self.applyWindowBackground() }
        NotificationCenter.default.addObserver(forName: NSWindow.didBecomeMainNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { Self.applyWindowBackground() }
        }
        MainActor.assumeIsolated {
            WindowScreenshot.scheduleIfRequested()
            #if canImport(Sparkle)
            // Not in screenshot / demo-seed automation runs.
            if LaunchOptions.current.screenshotPath == nil {
                updater = SparkleUpdateBackend.startIfConfigured()
            }
            #endif
        }
    }

    /// Window background = launch background (#0B1120 dark / #F8FAFC light), so no white frame or flash around the start and lock screen.
    @MainActor
    static func applyWindowBackground() {
        let canvas = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(srgbRed: 0x0F / 255, green: 0x17 / 255, blue: 0x2A / 255, alpha: 1)
                : NSColor(srgbRed: 0xF8 / 255, green: 0xFA / 255, blue: 0xFC / 255, alpha: 1)
        }
        for window in NSApp.windows where window.styleMask.contains(.titled) && window.backgroundColor != canvas {
            window.backgroundColor = canvas
        }
    }

    /// The GiroCrab icon, also when started outside an .app bundle
    /// (`swift run`, `cargo run`): bundle icns → asset catalog → source PNG.
    static func appIcon() -> NSImage? {
        if let url = Bundle.main.url(forResource: "AppIcon", withExtension: "icns"), let img = NSImage(contentsOf: url) {
            return img
        }
        if let img = NSImage(named: "AppIcon") {
            return img
        }
        // .build/<config>/GiroCrabMac -> GiroCrabApp/Xcode/App/Assets.xcassets/…
        var dir = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath().deletingLastPathComponent()
        for _ in 0..<6 {
            let candidate = dir.appendingPathComponent("Xcode/App/Assets.xcassets/AppIcon.appiconset/icon-1024.png")
            if let img = NSImage(contentsOf: candidate) { return img }
            let nested = dir.appendingPathComponent("GiroCrabApp/Xcode/App/Assets.xcassets/AppIcon.appiconset/icon-1024.png")
            if let img = NSImage(contentsOf: nested) { return img }
            dir = dir.deletingLastPathComponent()
        }
        return nil
    }

    private var windowObservers: [NSObjectProtocol] = []

    /// With only the menu bar item enabled (GitHub #546), hide the Dock icon
    /// while every window is closed.
    private func observeWindowsForDockPresence() {
        let center = NotificationCenter.default
        for name in [NSWindow.willCloseNotification, NSWindow.didBecomeKeyNotification, NSWindow.didMiniaturizeNotification] {
            windowObservers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                // after the close has finished: the window is no longer visible then
                DispatchQueue.main.async { MainActor.assumeIsolated { self?.updateDockPresence() } }
            })
        }
    }

    @MainActor
    private func updateDockPresence() {
        // titled = real windows (main, settings, help); the popover and status item are not
        let hasWindow = NSApp.windows.contains { $0.styleMask.contains(.titled) && ($0.isVisible || $0.isMiniaturized) }
        let menuBarOnly = MenuBarKeepAlive.isEnabled && !DockKeepAlive.isEnabled && !hasWindow
        let target: NSApplication.ActivationPolicy = menuBarOnly ? .accessory : .regular
        guard NSApp.activationPolicy() != target else { return }
        NSApp.setActivationPolicy(target)
        if target == .regular { NSApp.activate(ignoringOtherApps: true) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !DockKeepAlive.isEnabled && !MenuBarKeepAlive.isEnabled
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard DockKeepAlive.isEnabled, !flag, !NSApp.windows.contains(where: { $0.isMiniaturized }) else { return true }
        MenuBarActions.showMain()
        return false
    }
}

struct GiroCrabMacApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate

    // The model is built here, once and outside the body's dependency tracking
    // (see GiroCrabScenes.prepare).
    init() {
        // Frontmost before the first Touch ID prompt, also via `swift run`.
        NSApplication.shared.setActivationPolicy(.regular)
        if LaunchOptions.current.screenshotPath == nil {
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
        GiroCrabScenes.prepare()
    }

    var body: some Scene {
        GiroCrabScenes()
    }
}

GiroCrabMacApp.main()
#endif
