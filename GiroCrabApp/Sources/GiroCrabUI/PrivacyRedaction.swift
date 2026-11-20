import SwiftUI

/// What is hidden on screen: in the bug report screenshot
/// (GiroCrab/GiroCrab#301, `AppModel.reportRedaction`) and in privacy mode
/// (`PrivacyMode`, for showing the app to someone). Views mark their content
/// with `.sensitive()` / `.sensitive(.amounts)` / `.sensitive(.labels)`, so
/// the redaction is decided where the data is drawn instead of guessed
/// afterwards (OCR).
public struct ReportRedaction: OptionSet, Sendable, Hashable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }

    /// IBANs, account numbers, account names (banks often name an account
    /// after its holder), owners, counterparties, purposes, references, notes
    /// and documents. As a requested redaction it includes `.amounts`
    /// (`effective`): whoever hides names hides the money too.
    public static let personal = ReportRedaction(rawValue: 1 << 0)
    /// Category, group, budget and security names (off by default: they help
    /// to find a bug and say nothing about a person).
    public static let labels = ReportRedaction(rawValue: 1 << 1)
    /// Amounts, balances, sums, shares, chart axis values and readouts. Text
    /// that mixes both kinds is marked `.sensitive([.personal, .amounts])`.
    public static let amounts = ReportRedaction(rawValue: 1 << 2)

    /// The kinds that are actually hidden: `.personal` implies `.amounts`.
    public var effective: ReportRedaction {
        contains(.personal) ? union(.amounts) : self
    }

    /// Stands in for a hidden name inside a text, and for search text in
    /// the captured window.
    static let placeholder = "•••"

    /// `text`, or a placeholder while `kind` is hidden: for a name inside a
    /// longer text ("Konten auswählen – <Zugang>"), so the rest stays readable.
    func masking(_ text: String, _ kind: ReportRedaction = .personal) -> String {
        effective.isDisjoint(with: kind) || text.isEmpty ? text : Self.placeholder
    }
}

/// Privacy mode: hides amounts (or all personal data) in every window while
/// the app is shown to someone. A view preference, stored in the defaults.
public enum PrivacyMode {
    public static let enabledKey = "privacy.enabled"
    public static let scopeKey = "privacy.scope"

    public enum Scope: String, CaseIterable, Identifiable, Sendable {
        case amounts, personal
        public var id: String { rawValue }

        var title: LocalizedStringKey {
            switch self {
            case .amounts: "Beträge"
            case .personal: "Beträge und persönliche Angaben"
            }
        }

        public var redaction: ReportRedaction {
            switch self {
            case .amounts: .amounts
            case .personal: [.personal, .amounts]
            }
        }
    }

    /// Scope until the user picks one in Einstellungen › Allgemein: names, IBANs and purposes
    /// too, so switching privacy mode on (⇧⌘H, toolbar) hides more than balances (#1007).
    public static let defaultScope: Scope = .personal

    /// What privacy mode adds to the redaction of a window.
    public static func redaction(enabled: Bool, scope: Scope) -> ReportRedaction {
        enabled ? scope.redaction : []
    }
}

/// Scene root: adds privacy mode (and `--privacy` / `--redact`) to the
/// redaction of everything below, sheets and popovers included.
private struct PrivacyModeEnvironment: ViewModifier {
    @AppStorage(PrivacyMode.enabledKey) private var enabled = false
    @AppStorage(PrivacyMode.scopeKey) private var scope = PrivacyMode.defaultScope

    func body(content: Content) -> some View {
        let added = PrivacyMode.redaction(enabled: enabled, scope: scope).union(LaunchOptions.current.redaction)
        content.transformEnvironment(\.reportRedaction) { $0.formUnion(added) }
    }
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
    /// Marks personal or financial content: replaced by a soft smudge in
    /// privacy mode and while the bug report screenshot is taken, and `privacySensitive` for the
    /// system (widgets, locked screen). `kind` `.amounts` is for money,
    /// `.labels` for category and security names, which are only hidden on
    /// request.
    func sensitive(_ kind: ReportRedaction = .personal) -> some View {
        modifier(SensitiveContent(kind: kind))
    }

    /// Scene root of every window that shows vault data.
    func privacyModeEnvironment() -> some View {
        modifier(PrivacyModeEnvironment())
    }
}

private struct SensitiveContent: ViewModifier {
    @Environment(\.reportRedaction) private var redaction
    let kind: ReportRedaction

    private static let smudge: [Gradient.Stop] = zip(RedactionSmudge.ink, RedactionSmudge.inkLocations)
        .map { .init(color: Color.primary.opacity($0), location: $1) }
    private static let fade: [Gradient.Stop] = RedactionSmudge.fade.map { .init(color: .black.opacity($0.alpha), location: $0.at) }

    func body(content: Content) -> some View {
        let hidden = !redaction.effective.isDisjoint(with: kind)
        // The content is not drawn at all (opacity 0) and a blurred shape of
        // its size takes its place. It looks like blurred text, but unlike a
        // real blur nothing can be recovered from a screenshot, and it also
        // covers AppKit/UIKit-backed views (text editors, PDFView) and chart
        // marks, which `.blur` and `.redacted(reason:)` leave readable.
        // Opacity keeps the view's identity and state (scroll position,
        // editing); a hidden view takes no clicks and is not read aloud.
        content
            .privacySensitive(!kind.isDisjoint(with: [.personal, .amounts]))
            .opacity(hidden ? 0 : 1)
            .accessibilityHidden(hidden)
            .allowsHitTesting(!hidden)
            .overlay {
                if hidden {
                    // Uneven like the glyphs of blurred text and faded out
                    // towards every edge. Gradients instead of `.blur`: the
                    // in-process window bitmap of the bug report draws no
                    // layer filters, and a list full of blurs costs frames.
                    LinearGradient(stops: Self.smudge, startPoint: .leading, endPoint: .trailing)
                        .mask(LinearGradient(stops: Self.fade, startPoint: .top, endPoint: .bottom))
                        .accessibilityHidden(true)
                }
            }
    }
}

/// The smudge that stands for hidden content: drawn by `.sensitive()` and,
/// over text inputs the report covers itself, by `draw(in:rect:background:)`.
enum RedactionSmudge {
    /// Ink opacity from left to right, evenly spaced. Fixed, so a smudge says
    /// nothing about the text below it.
    static let ink: [Double] = [0, 0.5, 0.3, 0.55, 0.25, 0.5, 0.35, 0.6, 0.3, 0.5, 0]
    static let inkLocations: [Double] = ink.indices.map { Double($0) / Double(ink.count - 1) }
    /// The cover where no field colour can be read, or the smudge cannot be
    /// drawn: opaque, so it still hides.
    static let fallback = CGColor(gray: 0.56, alpha: 1)
    /// Vertical fade (top to bottom) towards the edges.
    static let fade: [(at: Double, alpha: Double)] = [(0, 0), (0.3, 0.6), (0.5, 1), (0.7, 0.6), (1, 0)]

    /// Covers `rect` of `ctx` opaquely with `background` and draws the smudge
    /// on it, in ink that contrasts with the background (dark on light).
    static func draw(in ctx: CGContext, rect: CGRect, background: CGColor) {
        guard rect.width >= 1, rect.height >= 1, let space = CGColorSpace(name: CGColorSpace.sRGB),
              let bg = background.converted(to: space, intent: .defaultIntent, options: nil),
              let c = bg.components, c.count >= 3
        else {
            ctx.setFillColor(fallback) // fail closed: still opaque
            ctx.fill(rect)
            return
        }
        let opaque = CGColor(colorSpace: space, components: [c[0], c[1], c[2], 1])!
        let ink: CGFloat = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2] > 0.5 ? 0 : 1
        ctx.saveGState()
        defer { ctx.restoreGState() }
        ctx.setFillColor(opaque)
        ctx.fill(rect)
        // The vertical fade as a mask; CoreGraphics counts y from the bottom,
        // the fade is symmetric, so its direction does not matter.
        let h = max(1, Int(rect.height.rounded(.up)))
        guard let gray = CGColorSpace(name: CGColorSpace.linearGray),
              let maskCtx = CGContext(data: nil, width: 1, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: gray, bitmapInfo: CGImageAlphaInfo.none.rawValue),
              let fadeGradient = CGGradient(colorsSpace: gray, colors: fade.map { CGColor(gray: $0.alpha, alpha: 1) } as CFArray,
                                            locations: fade.map { CGFloat($0.at) }),
              let inkGradient = CGGradient(colorsSpace: space,
                                           colors: Self.ink.map { CGColor(colorSpace: space, components: [ink, ink, ink, $0])! } as CFArray,
                                           locations: inkLocations.map { CGFloat($0) })
        else { return }
        maskCtx.drawLinearGradient(fadeGradient, start: .zero, end: CGPoint(x: 0, y: h), options: [])
        guard let mask = maskCtx.makeImage() else { return }
        ctx.clip(to: rect, mask: mask)
        ctx.drawLinearGradient(inkGradient, start: CGPoint(x: rect.minX, y: rect.midY),
                               end: CGPoint(x: rect.maxX, y: rect.midY), options: [])
    }

    /// The colour of pixel `point` (pixels, origin top left) of `image`: the
    /// background a cover takes over, so it blends into the field around it.
    /// nil for a (partly) transparent pixel (window corner, vibrancy): its
    /// colour alone would turn into black.
    static func color(of image: CGImage, at point: CGPoint) -> CGColor? {
        let x = min(max(0, Int(point.x)), image.width - 1), y = min(max(0, Int(point.y)), image.height - 1)
        guard let pixel = image.cropping(to: CGRect(x: x, y: y, width: 1, height: 1)),
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4,
                                  space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        ctx.draw(pixel, in: CGRect(x: 0, y: 0, width: 1, height: 1))
        guard let p = ctx.data?.assumingMemoryBound(to: UInt8.self), p[3] == 255 else { return nil }
        return CGColor(colorSpace: space, components: [CGFloat(p[0]) / 255, CGFloat(p[1]) / 255, CGFloat(p[2]) / 255, 1])
    }
}
