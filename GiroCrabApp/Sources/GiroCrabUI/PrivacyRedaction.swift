import SwiftUI

/// What the bug report screenshot hides (GiroCrab/GiroCrab#301). The main
/// window reads it from `AppModel.reportRedaction`; views mark their personal
/// content with `.sensitive()` / `.sensitive(.labels)`, so the redaction is
/// decided where the data is drawn instead of guessed afterwards (OCR).
public struct ReportRedaction: OptionSet, Sendable, Hashable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }

    /// Amounts, balances, IBANs, account numbers, account names (banks often
    /// name an account after its holder), owners, counterparties, purposes,
    /// references, notes and documents.
    public static let personal = ReportRedaction(rawValue: 1 << 0)
    /// Category, group, budget and security names (off by default: they help
    /// to find a bug and say nothing about a person).
    public static let labels = ReportRedaction(rawValue: 1 << 1)
}

private struct ReportRedactionKey: EnvironmentKey {
    static let defaultValue: ReportRedaction = []
}

extension EnvironmentValues {
    /// Set on the main window while a bug report screenshot is taken.
    var reportRedaction: ReportRedaction {
        get { self[ReportRedactionKey.self] }
        set { self[ReportRedactionKey.self] = newValue }
    }
}

extension View {
    /// Marks personal or financial content: covered by a grey box while the
    /// bug report screenshot is taken, and `privacySensitive` for the system
    /// (widgets, locked screen). `kind` `.labels` is for category and account
    /// names, which are only hidden on request.
    func sensitive(_ kind: ReportRedaction = .personal) -> some View {
        modifier(SensitiveContent(kind: kind))
    }
}

private struct SensitiveContent: ViewModifier {
    @Environment(\.reportRedaction) private var redaction
    let kind: ReportRedaction

    func body(content: Content) -> some View {
        let hidden = !redaction.isDisjoint(with: kind)
        // An opaque box instead of `.redacted(reason:)`: that one leaves
        // AppKit/UIKit-backed views (text editors, PDFView) and chart marks
        // visible. The content itself is hidden too, as such a platform view
        // may draw above a SwiftUI overlay; opacity keeps its identity and
        // state (scroll position, editing).
        content
            .privacySensitive(kind.contains(.personal))
            .opacity(hidden ? 0 : 1)
            .overlay {
                if hidden {
                    RoundedRectangle(cornerRadius: 3, style: .continuous)
                        .fill(Color.redactionFill)
                        .accessibilityHidden(true)
                }
            }
    }
}

extension Color {
    /// The grey of a redaction box: opaque, and adapted to light, dark and
    /// increased-contrast appearance by the system.
    static var redactionFill: Color {
        #if os(macOS)
        Color(nsColor: .systemGray)
        #else
        Color(uiColor: .systemGray)
        #endif
    }
}
