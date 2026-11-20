import SwiftUI

/// Colour scheme of amounts in the booking list (GitHub #218, #571,
/// Einstellungen › Allgemein). The minus (and plus) sign always stays, so the
/// direction never depends on colour alone.
enum NegativeAmountStyle: String, CaseIterable, Identifiable {
    /// Text colour, like the rest of the row (default).
    case neutral
    /// Red, like negative balances in the sidebar.
    case red
    /// Colour-blind-safe: negative amounts orange, positive ones blue (no red/green pair).
    case contrast

    static let defaultsKey = "negativeAmountStyle"
    var id: String { rawValue }

    var title: String {
        switch self {
        case .neutral: String(localized: "Neutral")
        case .red: String(localized: "Rot")
        case .contrast: String(localized: "Kontraststark")
        }
    }

    /// Colour of negative amounts.
    var negativeSpec: ThemeShade {
        switch self {
        case .neutral: Theme.outflowSpec
        case .red: Theme.negativeSpec
        case .contrast: Self.contrastNegative
        }
    }

    /// Text colour of positive amounts (on `positiveBadgeSpec`).
    var positiveSpec: ThemeShade {
        switch self {
        case .neutral, .red: Theme.inflowSpec
        case .contrast: Self.contrastPositive
        }
    }

    var positiveBadgeSpec: ThemeShade {
        switch self {
        case .neutral, .red: Theme.inflowBadgeSpec
        case .contrast: Self.contrastPositiveBadge
        }
    }

    // Orange vs. blue stays distinguishable for red-green colour blindness.
    private static let contrastNegative = ThemeShade(light: "#9A3412", dark: "#FB923C", lightHighContrast: "#7C2D12", darkHighContrast: "#FDBA74")
    private static let contrastPositive = ThemeShade(light: "#1D4ED8", dark: "#60A5FA", lightHighContrast: "#1E3A8A", darkHighContrast: "#93C5FD")
    private static let contrastPositiveBadge = ThemeShade(light: "#EFF6FF", dark: "#172554", lightHighContrast: "#DBEAFE", darkHighContrast: "#0B1B4D")
    // Built once: every booking row reads these on each render.
    private static let contrastColors = (negative: contrastNegative.color, positive: contrastPositive.color, badge: contrastPositiveBadge.color)

    var color: Color {
        switch self {
        case .neutral: Theme.outflow
        case .red: Theme.negative
        case .contrast: Self.contrastColors.negative
        }
    }

    var positiveColor: Color { self == .contrast ? Self.contrastColors.positive : Theme.inflow }
    var positiveBadge: Color { self == .contrast ? Self.contrastColors.badge : Theme.inflowBadge }

    /// The stored choice; unknown or missing values fall back to `.neutral`.
    static func current(_ defaults: UserDefaults = AppDefaults.store) -> NegativeAmountStyle {
        defaults.string(forKey: defaultsKey).flatMap(NegativeAmountStyle.init(rawValue:)) ?? .neutral
    }
}
