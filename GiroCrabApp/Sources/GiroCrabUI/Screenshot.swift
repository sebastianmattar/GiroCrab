#if os(macOS)
import AppKit
import SwiftUI

/// `--screenshot <file.png>`: waits ~120 frames (2 s at 60 Hz), renders the
/// main window (including title bar and toolbar) into a PNG and quits.
public enum WindowScreenshot {
    @MainActor
    public static func scheduleIfRequested(_ options: LaunchOptions = .current) {
        guard let path = options.screenshotPath else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.2) {
            capture(to: path, attempt: 0)
        }
    }

    @MainActor
    private static func capture(to path: String, attempt: Int) {
        // --demo-settings: capture the Settings window instead of the main window
        // (`groups` is the group editor, a sheet of the main window: captured below).
        let wantsSettings = LaunchOptions.current.demoSettings.map { !$0.hasPrefix("groups") } ?? false
        let candidates = NSApp.windows.filter { $0.isVisible && $0.contentView != nil && $0.frame.width > 300 && !$0.isSheet }
        let settingsWindow = candidates.first { $0.identifier?.rawValue.localizedCaseInsensitiveContains("settings") == true }
        if wantsSettings && settingsWindow == nil && attempt < 20 {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { capture(to: path, attempt: attempt + 1) }
            return
        }
        guard let window = wantsSettings ? settingsWindow : candidates.first(where: { $0.frame.width > 400 && $0 != settingsWindow }),
              let content = window.contentView else {
            if attempt < 20 {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { capture(to: path, attempt: attempt + 1) }
            } else {
                FileHandle.standardError.write("screenshot: no window found\n".data(using: .utf8)!)
                exit(2)
            }
            return
        }
        // With a sheet open (e.g. --demo-sync), capture the sheet instead.
        let target = window.attachedSheet ?? window
        // GIROCRAB_SCREENSHOT_NATIVE=1: activate the app and let `screencapture`
        // grab the real window, so default buttons and focus look as on screen.
        if ProcessInfo.processInfo.environment["GIROCRAB_SCREENSHOT_NATIVE"] == "1" {
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
            if let title = LaunchOptions.current.demoMenu {
                captureMenu(title, over: window, to: path)
                return
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.6) {
                // A sheet or alert may have appeared meanwhile: take the topmost one.
                let target = window.attachedSheet?.attachedSheet ?? window.attachedSheet ?? window
                window.makeKeyAndOrderFront(nil) // looks active, not greyed out
                // Alerts come out blank with `screencapture -l`: draw those in-process.
                if target != window, target.className.contains("Alert") || target.contentViewController == nil {
                    render(target, to: path)
                    return
                }
                let task = Process()
                task.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
                task.arguments = ["-x", "-o", "-l", String(target.windowNumber), path]
                try? task.run()
                task.waitUntilExit()
                print("screenshot saved (native): \(path)")
                exit(task.terminationStatus == 0 ? 0 : 6)
            }
            return
        }
        _ = content
        render(target, to: path)
    }

    /// Draws `window` (title bar, toolbar and content) in-process. Also used by
    /// the bug report (#301), so nothing else on screen ends up in the picture.
    @MainActor
    static func bitmap(of window: NSWindow) -> NSBitmapImageRep? {
        guard let content = window.contentView else { return nil }
        let view = content.superview ?? content // frame view = title bar + toolbar + content
        view.layoutSubtreeIfNeeded()
        view.displayIfNeeded()
        let bounds = view.bounds
        guard let rep = view.bitmapImageRepForCachingDisplay(in: bounds) else { return nil }
        view.cacheDisplay(in: bounds, to: rep)
        return rep
    }

    @MainActor
    private static func render(_ target: NSWindow, to path: String) {
        guard target.contentView != nil else { exit(2) }
        guard let rep = bitmap(of: target) else { exit(3) }
        guard let png = rep.representation(using: .png, properties: [:]) else { exit(4) }
        let url = URL(fileURLWithPath: path)
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        do {
            try png.write(to: url)
            print("screenshot saved: \(url.path) (\(rep.pixelsWide)x\(rep.pixelsHigh))")
            exit(0)
        } catch {
            FileHandle.standardError.write("screenshot: \(error)\n".data(using: .utf8)!)
            exit(5)
        }
    }

    /// `--demo-menu <title>`: renders the items of that menu-bar menu (titles,
    /// key equivalents, separators, enabled state) into a PNG. Drawn by the app
    /// itself, so no other window on screen can end up in the picture.
    @MainActor
    private static func captureMenu(_ title: String, over window: NSWindow, to path: String) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            guard let menu = NSApp.mainMenu?.items.first(where: { $0.title == title })?.submenu else {
                FileHandle.standardError.write("screenshot: menu \(title) not found\n".data(using: .utf8)!)
                exit(7)
            }
            menu.update()
            // Like NSMenu: no leading, trailing or doubled separators, no empty items.
            var rows: [MenuRow] = []
            for row in menu.items.filter({ !$0.isHidden }).map(MenuRow.init) {
                if row.separator && (rows.isEmpty || rows.last!.separator) { continue }
                if !row.separator && row.title.isEmpty { continue }
                rows.append(row)
            }
            if rows.last?.separator == true { rows.removeLast() }
            let renderer = ImageRenderer(content: MenuPicture(title: title, rows: rows))
            renderer.scale = 2
            guard let image = renderer.nsImage, let tiff = image.tiffRepresentation,
                  let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else { exit(4) }
            do {
                try png.write(to: URL(fileURLWithPath: path))
                print("screenshot saved (menu): \(path)")
                exit(0)
            } catch { exit(5) }
        }
    }
}

private struct MenuRow: Identifiable {
    let id = UUID()
    let title: String
    let shortcut: String
    let separator: Bool
    let enabled: Bool
    let submenu: Bool
    let checked: Bool

    @MainActor
    init(_ item: NSMenuItem) {
        title = item.title
        separator = item.isSeparatorItem
        enabled = item.isEnabled
        submenu = item.hasSubmenu
        checked = item.state == .on
        var s = ""
        let m = item.keyEquivalentModifierMask
        if !item.keyEquivalent.isEmpty {
            if m.contains(.control) { s += "⌃" }
            if m.contains(.option) { s += "⌥" }
            if m.contains(.shift) || item.keyEquivalent != item.keyEquivalent.lowercased() { s += "⇧" }
            if m.contains(.command) { s += "⌘" }
            let k = item.keyEquivalent
            switch k.unicodeScalars.first?.value {
            case 0xF700: s += "↑"
            case 0xF701: s += "↓"
            case 0xF702: s += "←"
            case 0xF703: s += "→"
            case 0x08, 0x7F: s += "⌫"
            default: s += k.uppercased()
            }
        }
        shortcut = s
    }
}

private struct MenuPicture: View {
    let title: String
    let rows: [MenuRow]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.system(size: 13, weight: .semibold))
                .padding(.horizontal, 10).padding(.vertical, 4)
                .background(RoundedRectangle(cornerRadius: 5).fill(Color.primary.opacity(0.1)))
                .padding(.bottom, 6)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(rows) { r in
                    if r.separator {
                        Divider().padding(.vertical, 5).padding(.horizontal, 10)
                    } else {
                        HStack(spacing: 6) {
                            Text(r.checked ? "✓" : "").frame(width: 12)
                            Text(r.title)
                            Spacer(minLength: 30)
                            if r.submenu { Text("›") } else { Text(r.shortcut).monospaced() }
                        }
                        .font(.system(size: 13))
                        .foregroundStyle(r.enabled ? Color.primary : Color.secondary.opacity(0.6))
                        .padding(.horizontal, 8).padding(.vertical, 3)
                    }
                }
            }
            .padding(.vertical, 5)
            .frame(minWidth: 280)
            .background(RoundedRectangle(cornerRadius: 9).fill(Color(nsColor: .windowBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(Color.primary.opacity(0.15)))
        }
        .padding(14)
        .background(Color(nsColor: .underPageBackgroundColor))
        .environment(\.colorScheme, .light)
    }
}
#endif
