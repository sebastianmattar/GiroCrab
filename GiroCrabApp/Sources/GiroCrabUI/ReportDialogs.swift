import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Dialogs in the bug report screenshot (GiroCrab/GiroCrab#384).
///
/// Every sheet and popover goes through `capturableSheet` / `capturablePopover`
/// (`scripts/lint-report-dialogs.sh`): the content is redacted like the main
/// window while a report is taken, and registers its window, so the capture
/// can draw it over the main window. Its content must mark personal data with
/// `.sensitive()` like any view in the main window; text fields and text
/// editors are covered by the capture itself, on macOS also pop-up menus. An
/// iOS menu-style `Picker` or `Menu` draws its value itself and is not
/// covered — mark it `.sensitive()`.
/// Alerts and confirmation dialogs are system-drawn and never captured: the
/// report then shows what lies under them and says so.
extension View {
    func capturableSheet<Content: View>(isPresented: Binding<Bool>, onDismiss: (() -> Void)? = nil,
                                        @ViewBuilder content: @escaping () -> Content) -> some View {
        sheet(isPresented: isPresented, onDismiss: onDismiss) { content().reportCapturable() }
    }

    func capturableSheet<Item: Identifiable, Content: View>(item: Binding<Item?>, onDismiss: (() -> Void)? = nil,
                                                            @ViewBuilder content: @escaping (Item) -> Content) -> some View {
        sheet(item: item, onDismiss: onDismiss) { content($0).reportCapturable() }
    }

    func capturablePopover<Content: View>(isPresented: Binding<Bool>,
                                          attachmentAnchor: PopoverAttachmentAnchor = .rect(.bounds),
                                          arrowEdge: Edge = .top,
                                          @ViewBuilder content: @escaping () -> Content) -> some View {
        popover(isPresented: isPresented, attachmentAnchor: attachmentAnchor, arrowEdge: arrowEdge) {
            content().reportCapturable()
        }
    }

    /// For content presented some other way (a `UIHostingController`).
    func reportCapturable() -> some View { modifier(ReportDialogContent()) }
}

/// The redaction of a running capture for views outside the main window's
/// environment: a sheet's environment does not reliably come from the view
/// that presents it. Mirrors `AppModel.reportRedaction`.
@MainActor @Observable
final class ReportRedactionState {
    static let shared = ReportRedactionState()
    var current: ReportRedaction = []
}

private struct ReportDialogContent: ViewModifier {
    /// Privacy mode applies to dialogs too: this modifier replaces the environment of what it wraps.
    @AppStorage(PrivacyMode.enabledKey) private var privacyEnabled = false
    @AppStorage(PrivacyMode.scopeKey) private var privacyScope = PrivacyMode.defaultScope

    func body(content: Content) -> some View {
        content
            .background(ReportWindowTracker(role: .dialog))
            .environment(\.reportRedaction, ReportRedactionState.shared.current
                .union(LaunchOptions.current.redaction)
                .union(PrivacyMode.redaction(enabled: privacyEnabled, scope: privacyScope)))
    }
}

/// Registers a window (macOS) or a presented view (iOS) for the bug report,
/// and the redaction its views were last built with, so a capture checks
/// what it draws. Gone with the view (window closed, sheet dismissed, vault
/// locked): nothing left to capture.
struct ReportWindowTracker {
    enum Role {
        /// `MainSplitView`: the window a capture starts from.
        case main
        /// A sheet or popover over it (`reportCapturable`).
        case dialog
        /// iOS: the report sheet itself; it and what lies above it are never drawn.
        case report
    }

    let role: Role
    @Environment(\.reportRedaction) private var redaction
}

#if os(macOS)
extension ReportWindowTracker: NSViewRepresentable {
    func makeNSView(context: Context) -> TrackingView { TrackingView(role: role) }

    func updateNSView(_ view: TrackingView, context: Context) {
        view.renderedRedaction = redaction
    }

    final class TrackingView: NSView {
        let role: Role
        var renderedRedaction: ReportRedaction = []

        init(role: Role) {
            self.role = role
            super.init(frame: .zero)
        }

        required init?(coder: NSCoder) { fatalError("not used") }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window != nil { BugReport.track(self) } else { BugReport.untrack(self) }
        }
    }
}
#else
extension ReportWindowTracker: UIViewRepresentable {
    func makeUIView(context: Context) -> TrackingView { TrackingView(role: role) }

    func updateUIView(_ view: TrackingView, context: Context) {
        view.renderedRedaction = redaction
    }

    final class TrackingView: UIView {
        let role: Role
        var renderedRedaction: ReportRedaction = []

        init(role: Role) {
            self.role = role
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }

        required init?(coder: NSCoder) { fatalError("not used") }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window != nil { BugReport.track(self) } else { BugReport.untrack(self) }
        }
    }
}

/// "Andere Ansicht aufnehmen" (#384): the "Hier aufnehmen" bar lives in a
/// window of its own above each main window, so it also floats over the
/// sheets the user opens to show a problem, and is never part of a capture
/// (that draws only the main window's views).
@MainActor
enum ReportPickOverlay {
    private static var windows: [ObjectIdentifier: UIWindow] = [:]

    /// Shows the bar while `model.reportPick` is set, hides it otherwise.
    static func sync(model: AppModel) {
        guard model.reportPick != nil else {
            // detach: the scene holds its windows strongly, hiding alone would
            // leave a full hosting tree per pick until the scene ends
            for window in windows.values {
                window.isHidden = true
                window.windowScene = nil
            }
            windows.removeAll()
            return
        }
        for main in BugReport.mainWindows() {
            guard let scene = main.windowScene, windows[ObjectIdentifier(scene)] == nil else { continue }
            let window = PassThroughWindow(windowScene: scene)
            let host = UIHostingController(rootView: ReportPickBar { [weak main] in
                main.map { presentReport(over: $0, model: model) } ?? false
            }.environment(model))
            host.view.backgroundColor = .clear
            window.rootViewController = host
            window.windowLevel = .normal + 1
            window.isHidden = false
            windows[ObjectIdentifier(scene)] = window
        }
    }

    /// A new report over whatever `window` shows on top, sheets included
    /// (a SwiftUI sheet of the main view could not be presented over them).
    /// `false`: an alert is on top — it cannot be covered by a second modal,
    /// and the bar tells the user instead.
    private static func presentReport(over window: UIWindow, model: AppModel) -> Bool {
        var top = window.rootViewController
        while let next = top?.presentedViewController, !next.isBeingDismissed { top = next }
        guard let top, !(top is UIAlertController) else { return false }
        // The presenter dismisses what it presented: the report.
        let host = UIHostingController(rootView: BugReportSheet(onClose: { [weak top] in top?.dismiss(animated: true) })
            .environment(model))
        top.present(host, animated: true)
        return true
    }

    /// Touches outside the bar reach the main window below.
    private final class PassThroughWindow: UIWindow {
        override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? {
            let hit = super.hitTest(point, with: event)
            return hit === rootViewController?.view ? nil : hit
        }
    }
}
#endif
