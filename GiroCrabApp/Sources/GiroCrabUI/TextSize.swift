import SwiftUI

/// App-wide text size (GitHub #343). macOS has no Dynamic Type, so the app
/// scales its text styles itself; on iOS the system setting stays in charge.
enum AppTextSize: String, CaseIterable, Identifiable {
    case standard
    case large
    case larger

    static let defaultsKey = "textSize"
    var id: String { rawValue }

    var title: String {
        switch self {
        case .standard: String(localized: "Standard")
        case .large: String(localized: "Groß")
        case .larger: String(localized: "Sehr groß")
        }
    }

    var scale: CGFloat {
        switch self {
        case .standard: 1
        case .large: 1.15
        case .larger: 1.3
        }
    }

    /// The size shown: `--text-size` wins; screenshot launches otherwise stay at
    /// `.standard` (reproducible marketing shots).
    var effective: AppTextSize {
        let options = LaunchOptions.current
        if let forced = options.textSize.flatMap(AppTextSize.init) { return forced }
        return options.screenshotPath == nil ? self : .standard
    }

    /// Ansicht › Größer / Kleiner; nil at the end of the range.
    var larger: AppTextSize? { step(+1) }
    var smaller: AppTextSize? { step(-1) }

    private func step(_ delta: Int) -> AppTextSize? {
        let cases = Self.allCases
        let i = cases.firstIndex(of: self)! + delta
        return cases.indices.contains(i) ? cases[i] : nil
    }
}

extension EnvironmentValues {
    /// Factor applied by `appFont`; 1 = system sizes.
    @Entry var textScale: CGFloat = 1
}

extension Font.TextStyle {
    /// Point size and weight of the macOS text styles (NSFont.preferredFont).
    var macMetrics: (size: CGFloat, weight: Font.Weight) {
        switch self {
        case .largeTitle: (26, .regular)
        case .title: (22, .regular)
        case .title2: (17, .regular)
        case .title3: (15, .regular)
        case .headline: (13, .bold)
        case .subheadline: (11, .regular)
        case .body: (13, .regular)
        case .callout: (12, .regular)
        case .footnote, .caption, .caption2: (10, .regular)
        @unknown default: (13, .regular)
        }
    }

    /// Whole points stay crisp: caption 10 → 12 → 13, body 13 → 15 → 17.
    func macSize(scale: CGFloat) -> CGFloat { (macMetrics.size * scale).rounded() }
}

extension Font {
    /// `style` scaled by `scale` (macOS) or the plain Dynamic Type style (iOS).
    static func app(_ style: TextStyle, scale: CGFloat = 1, weight: Weight? = nil,
                    design: Design? = nil, monospacedDigit: Bool = false) -> Font {
        #if os(macOS)
        let font = Font.system(size: style.macSize(scale: scale), weight: weight ?? style.macMetrics.weight, design: design)
        #else
        let font = Font.system(style, design: design, weight: weight)
        #endif
        return monospacedDigit ? font.monospacedDigit() : font
    }
}

private struct AppFont: ViewModifier {
    @Environment(\.textScale) private var scale
    let style: Font.TextStyle
    let weight: Font.Weight?
    let design: Font.Design?
    let monospacedDigit: Bool

    func body(content: Content) -> some View {
        content.font(.app(style, scale: scale, weight: weight, design: design, monospacedDigit: monospacedDigit))
    }
}

private struct AppDefaultFont: ViewModifier {
    @Environment(\.textScale) private var scale

    func body(content: Content) -> some View {
        content.font(scale == 1 ? nil : .app(.body, scale: scale))
    }
}

extension View {
    /// Use instead of `.font(.caption)` etc. so the text follows the text size
    /// setting (`scripts/lint-fonts.sh` enforces this).
    func appFont(_ style: Font.TextStyle, weight: Font.Weight? = nil,
                 design: Font.Design? = nil, monospacedDigit: Bool = false) -> some View {
        modifier(AppFont(style: style, weight: weight, design: design, monospacedDigit: monospacedDigit))
    }

    /// Body text at the current scale for text that doesn't inherit the scene's
    /// font: in macOS sidebar rows the List sets it (system sidebar size), so put
    /// this on the `Text` itself, not on the row. `.standard` keeps the system size.
    func appDefaultFont() -> some View {
        modifier(AppDefaultFont())
    }

    /// Form section header: on macOS the scaled scene font replaces the header's
    /// bold style, so `Section("…")` headers lose it; use `Section { } header: { Text("…").appSectionHeader() }`.
    @ViewBuilder func appSectionHeader() -> some View {
        #if os(macOS)
        appFont(.headline)
        #else
        self
        #endif
    }

    /// Scene root: sets the scale and scales the default font of unstyled text
    /// (form labels, pickers, list rows). `.standard` keeps the system default.
    func appTextSize(_ size: AppTextSize) -> some View {
        environment(\.textScale, size.scale)
            #if os(macOS)
            .font(size == .standard ? nil : .app(.body, scale: size.scale))
            #endif
    }
}
