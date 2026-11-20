import os
import SwiftUI
import GiroCrabCore

extension Booking: @retroactive Identifiable {}
extension Account: @retroactive Identifiable {}
extension AccountGroup: @retroactive Identifiable {}
extension GiroCrabCore.Category: @retroactive Identifiable {}
extension Rule: @retroactive Identifiable {}
extension BankConnection: @retroactive Identifiable {}
extension SettingEntry: @retroactive Identifiable { public var id: String { key } }
extension BankDescriptor: @retroactive Identifiable { public var id: ConnectorKind { kind } }
extension SavingsPlan: @retroactive Identifiable { public var id: String { merchant.key } }

/// Hex values of one theme colour in all four appearances (light, dark and
/// the "Kontrast erhöhen" variants). The tables below hold the hex strings so
/// `ThemeContrastTests` can audit the exact values the UI renders.
struct ThemeShade: Equatable {
    let light: String
    let dark: String
    var lightHighContrast: String?
    var darkHighContrast: String?

    init(light: String, dark: String, lightHighContrast: String? = nil, darkHighContrast: String? = nil) {
        self.light = light
        self.dark = dark
        self.lightHighContrast = lightHighContrast
        self.darkHighContrast = darkHighContrast
    }

    var color: Color {
        Color(light: light, dark: dark, lightHighContrast: lightHighContrast, darkHighContrast: darkHighContrast)
    }

    /// The four appearances with their hex (high-contrast falls back to the normal one).
    var variants: [(name: String, hex: String)] {
        [("light", light), ("dark", dark),
         ("lightHighContrast", lightHighContrast ?? light), ("darkHighContrast", darkHighContrast ?? dark)]
    }
}

/// "Daylight" / "Nightfall" design tokens: calm, high-contrast UI that follows
/// the system appearance (light: slate on white, dark: slate on deep navy).
enum Theme {
    static let canvasSpec = ThemeShade(light: "#F8FAFC", dark: "#0F172A")
    static let cardSpec = ThemeShade(light: "#FFFFFF", dark: "#1E293B")
    static let canvas = canvasSpec.color
    static let sidebar = Color(light: "#F3F4F6", dark: "#0B1220")
    static let card = cardSpec.color
    /// Surfaces text sits on (hex per appearance), for the contrast audit.
    static let surfaceHex: [(name: String, light: String, dark: String)] = [
        ("canvas", canvasSpec.light, canvasSpec.dark), ("card", cardSpec.light, cardSpec.dark),
    ]
    // Text colours meet 4.5:1 on canvas and card (HIG / WCAG AA); "Kontrast
    // erhöhen" gets stronger variants. `textSpecs` lists every token used as
    // text, `badgePairs` the text-on-badge combinations (ThemeContrastTests).
    static let borderSpec = ThemeShade(light: "#E2E8F0", dark: "#334155", lightHighContrast: "#94A3B8", darkHighContrast: "#64748B")
    static let textSpec = ThemeShade(light: "#0F172A", dark: "#F1F5F9", lightHighContrast: "#000000", darkHighContrast: "#FFFFFF")
    static let secondarySpec = ThemeShade(light: "#4B5A70", dark: "#A3B1C4", lightHighContrast: "#1E293B", darkHighContrast: "#E2E8F0")
    static let tertiarySpec = ThemeShade(light: "#64748B", dark: "#8494AA", lightHighContrast: "#334155", darkHighContrast: "#CBD5E1")
    static let outflowSpec = ThemeShade(light: "#0F172A", dark: "#F1F5F9", lightHighContrast: "#000000", darkHighContrast: "#FFFFFF")
    /// Emerald; light #047857 (was #059669, only 3.8:1 on white).
    static let inflowSpec = ThemeShade(light: "#047857", dark: "#34D399", lightHighContrast: "#065F46", darkHighContrast: "#6EE7B7")
    static let inflowBadgeSpec = ThemeShade(light: "#ECFDF5", dark: "#064E3B", lightHighContrast: "#D1FAE5", darkHighContrast: "#022C22")
    static let chargeSpec = ThemeShade(light: "#B91C1C", dark: "#F87171", lightHighContrast: "#991B1B", darkHighContrast: "#FCA5A5")
    static let outflowBadgeSpec = ThemeShade(light: "#FEF2F2", dark: "#5B1B1B", lightHighContrast: "#FEE2E2", darkHighContrast: "#450A0A")
    static let loanSpec = ThemeShade(light: "#64748B", dark: "#94A3B8", lightHighContrast: "#334155", darkHighContrast: "#CBD5E1")
    static let warningSpec = ThemeShade(light: "#B45309", dark: "#F59E0B", lightHighContrast: "#92400E", darkHighContrast: "#FBBF24")
    static let investSpec = ThemeShade(light: "#1F7A5C", dark: "#5EC79F", lightHighContrast: "#176B4F", darkHighContrast: "#86EFAC")
    static let negativeSpec = ThemeShade(light: "#B91C1C", dark: "#F87171", lightHighContrast: "#991B1B", darkHighContrast: "#FCA5A5")

    static let border = borderSpec.color
    static let text = textSpec.color
    static let secondary = secondarySpec.color
    static let tertiary = tertiarySpec.color
    static let outflow = outflowSpec.color
    static let inflow = inflowSpec.color
    static let inflowBadge = inflowBadgeSpec.color
    /// Order-account charge colour: the red counterpart of `inflow`, paired
    /// with `outflowBadge` so badged charges and refunds read symmetrically.
    static let charge = chargeSpec.color
    /// Red counterpart of `inflowBadge`, so the order-account rows can badge
    /// charges and refunds symmetrically (direction additionally via sign/symbol).
    static let outflowBadge = outflowBadgeSpec.color
    /// Every token that is drawn as text on `surfaceHex` (≥ 4.5:1 required).
    static let textSpecs: [(name: String, spec: ThemeShade)] = [
        ("text", textSpec), ("secondary", secondarySpec), ("tertiary", tertiarySpec),
        ("outflow", outflowSpec), ("inflow", inflowSpec), ("charge", chargeSpec),
        ("loan", loanSpec), ("warning", warningSpec), ("invest", investSpec), ("negative", negativeSpec),
    ]
    /// Text colour on its badge background (≥ 4.5:1 required).
    static let badgePairs: [(name: String, text: ThemeShade, background: ThemeShade)] = [
        ("inflow/inflowBadge", inflowSpec, inflowBadgeSpec),
        ("charge/outflowBadge", chargeSpec, outflowBadgeSpec),
    ]
    /// The app accent colour (HIG › Color › App accent colors): GiroCrab blue,
    /// defined as `AccentColor` in the asset catalog, so buttons, toggles and
    /// focus rings use it too. With the system default "Multicolour" everyone
    /// sees the brand blue; if someone picks another accent in System Settings ›
    /// Appearance, macOS applies that one app-wide instead.
    static let accent = Color.accentColor
    /// Fill under a value line (balance history, depot value): the accent fading out downwards.
    static let chartAreaFill = LinearGradient(colors: [accent.opacity(0.14), accent.opacity(0.01)], startPoint: .top, endPoint: .bottom)
    /// Soft selection/highlight fill in the current accent colour – brand blue by
    /// default, and it follows a user-chosen system accent.
    static let selection: Color = {
        #if os(macOS)
        Color(nsColor: NSColor(name: nil) { appearance in
            let dark = appearance.bestMatch(from: [.aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua])
                .map { $0 == .darkAqua || $0 == .accessibilityHighContrastDarkAqua } ?? false
            return NSColor.controlAccentColor.withAlphaComponent(dark ? 0.28 : 0.13)
        })
        #else
        Color.accentColor.opacity(0.15)
        #endif
    }()
    static let loan = loanSpec.color
    static let warning = warningSpec.color
    static let invest = investSpec.color
    static let negative = negativeSpec.color
    static let personFill = Color(light: "#E2E8F0", dark: "#334155")
    static let personText = Color(light: "#475569", dark: "#CBD5E1")
    static let neutralTile = Color(light: "#F1F3F5", dark: "#273449")
}

/// Card with a 1px hairline border.
struct CardBackground: ViewModifier {
    var padding: CGFloat = 16
    /// Overrides the top/bottom inset only (collapsed header cards, GitHub #603).
    var vertical: CGFloat?
    func body(content: Content) -> some View {
        content
            .padding(.horizontal, padding)
            .padding(.vertical, vertical ?? padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.card))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }
}

extension View {
    func card(padding: CGFloat = 16, vertical: CGFloat? = nil) -> some View { modifier(CardBackground(padding: padding, vertical: vertical)) }
}

enum Money {
    private static let currency: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .currency
        f.locale = .autoupdatingCurrent
        f.currencyCode = "EUR"
        return f
    }()
    private static let decimal: NumberFormatter = {
        let f = NumberFormatter()
        f.locale = .autoupdatingCurrent
        f.numberStyle = .decimal
        f.minimumFractionDigits = 2
        f.maximumFractionDigits = 2
        return f
    }()

    /// i64 cents -> "1.234,56 €" / "€1,234.56" in the user's locale (Decimal, never Double).
    static func format(_ cents: Int64, currency code: String = "EUR") -> String {
        currency.currencyCode = code.isEmpty ? "EUR" : code.uppercased()
        let s = currency.string(from: (Decimal(cents) / 100) as NSDecimalNumber) ?? "\(cents)"
        return s.replacingOccurrences(of: "-", with: "−")
    }

    /// "$" / "€" / "CHF": the symbol of `code` in the user's locale (chart axes).
    static func symbol(_ code: String) -> String {
        currency.currencyCode = code
        return currency.currencySymbol ?? code
    }

    /// "+1.234,56 €" for inflows.
    static func signed(_ cents: Int64, currency code: String = "EUR") -> String {
        cents > 0 ? "+" + format(cents, currency: code) : format(cents, currency: code)
    }

    /// "2.064,83" / "-15,00" for the clipboard (ASCII minus, no currency sign).
    static func clipboard(_ cents: Int64) -> String {
        plain(cents).replacingOccurrences(of: "−", with: "-")
    }

    static func plain(_ cents: Int64) -> String {
        (decimal.string(from: (Decimal(cents) / 100) as NSDecimalNumber) ?? "\(cents)").replacingOccurrences(of: "-", with: "−")
    }

    /// User input -> cents, accepting the user's locale ("280.000,50" in German,
    /// "280,000.50" in English) as well as plain "280000.5" (Decimal, never Double).
    /// Both separators present: the last one is the decimal separator. Only one
    /// kind present: it is a grouping separator if it is the locale's grouping
    /// separator and every group after it has exactly three digits. The minus sign `plain`
    /// writes ("−", U+2212) and an en dash count as "-": a field filled with `plain` parses back.
    static func parseCents(_ input: String) -> Int64? {
        let t = input.replacingOccurrences(of: "−", with: "-").replacingOccurrences(of: "–", with: "-")
            .filter { !($0.isCurrencySymbol || $0.isWhitespace || $0 == "'") }
        guard !t.isEmpty else { return nil }
        let seps: Set<Character> = [".", ","]
        let used = Set(t.filter { seps.contains($0) })
        var normalized = t
        if used.count == 2, let last = t.last(where: { seps.contains($0) }) {
            let grouping: Character = last == "," ? "." : ","
            normalized = t.replacingOccurrences(of: String(grouping), with: "").replacingOccurrences(of: String(last), with: ".")
        } else if let sep = used.first {
            let groups = t.split(separator: sep, omittingEmptySubsequences: false)
            let localeGrouping = Locale.autoupdatingCurrent.groupingSeparator.flatMap(\.first)
            let isGrouping = (sep == localeGrouping || groups.count > 2) && groups.dropFirst().allSatisfy { $0.count == 3 }
            normalized = isGrouping ? t.replacingOccurrences(of: String(sep), with: "") : t.replacingOccurrences(of: String(sep), with: ".")
        }
        guard let d = Decimal(string: normalized, locale: Locale(identifier: "en_US_POSIX")) else { return nil }
        return NSDecimalNumber(decimal: d * 100).rounding(accordingToBehavior: nil).int64Value
    }

    static func ink(_ cents: Int64) -> Color { cents > 0 ? Theme.inflow : Theme.outflow }
}

enum DateText {
    private static let iso: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()
    /// Display formats follow the user's locale: the templates give
    /// "9. November" / "09.11.2026" in German and "November 9" / "11/09/2026" in English.
    private static func make(template: String) -> DateFormatter {
        let f = DateFormatter()
        f.locale = .autoupdatingCurrent
        f.setLocalizedDateFormatFromTemplate(template)
        return f
    }
    private static let dayMonth = make(template: "dMMMM")
    private static let dayMonthYear = make(template: "dMMMMyyyy")
    private static let weekday = make(template: "EEEE")
    private static let medium = make(template: "ddMMyyyy")
    private static let full = make(template: "EEEEdMMMMyyyy")
    private static let monthShort = make(template: "MMMyy")
    private static let monthOnly = make(template: "MMM")

    /// Parsed ISO days. `DateFormatter` needs ~50 µs per string, and the balance
    /// chart parses every day of up to 15 years on each render.
    private static let parsed: OSAllocatedUnfairLock<[String: Date]> = {
        let cache = OSAllocatedUnfairLock<[String: Date]>(initialState: [:])
        // A day's midnight depends on the time zone.
        NotificationCenter.default.addObserver(forName: .NSSystemTimeZoneDidChange, object: nil, queue: nil) { _ in
            cache.withLock { $0 = [:] }
        }
        return cache
    }()

    /// Midnight of an ISO day in the current time zone (what `iso` yields) without a
    /// `DateFormatter`: ~10x faster, which matters for the first render of a long history.
    static func parseISODay(_ s: String) -> Date? {
        let b = Array(s.utf8)
        guard b.count == 10, b[4] == 0x2D, b[7] == 0x2D else { return nil }
        func num(_ r: Range<Int>) -> Int? {
            var n = 0
            for i in r { guard (0x30...0x39).contains(b[i]) else { return nil }; n = n * 10 + Int(b[i] - 0x30) }
            return n
        }
        guard let y = num(0..<4), let m = num(5..<7), let d = num(8..<10), (1...12).contains(m), d >= 1 else { return nil }
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
        guard d <= (m == 2 && leap ? 29 : daysInMonth[m - 1]) else { return nil }
        return isoCalendar.date(from: DateComponents(year: y, month: m, day: d))
    }

    private static let daysInMonth = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]

    /// Follows the system time zone like the `iso` formatter does.
    private static let isoCalendar: Calendar = {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = .autoupdatingCurrent
        return cal
    }()

    static func date(_ s: String) -> Date? {
        if let hit = parsed.withLock({ $0[s] }) { return hit }
        // anything but plain `yyyy-MM-dd` keeps the formatter's (lenient) reading
        guard let d = parseISODay(s) ?? iso.date(from: s) else { return nil }
        parsed.withLock { cache in
            if cache.count >= 50_000 { cache = [:] }
            cache[s] = d
        }
        return d
    }
    static func iso(_ d: Date) -> String { iso.string(from: d) }
    static func medium(_ s: String) -> String { date(s).map { medium.string(from: $0) } ?? s }
    static func long(_ s: String) -> String { date(s).map { full.string(from: $0) } ?? s }

    /// "Heute, 9. November" / "Gestern, 8. November" / "Montag, 2. November" (+ year if not this year).
    static func dayHeader(_ s: String) -> String {
        guard let d = date(s) else { return s }
        let cal = Calendar(identifier: .gregorian)
        let sameYear = cal.component(.year, from: d) == cal.component(.year, from: Date())
        let dm = sameYear ? dayMonth.string(from: d) : dayMonthYear.string(from: d)
        if cal.isDateInToday(d) { return String(localized: "Heute, \(dm)") }
        if cal.isDateInYesterday(d) { return String(localized: "Gestern, \(dm)") }
        let wd = weekday.string(from: d)
        return String(localized: "\(wd), \(dm)", comment: "Day header: weekday, day and month")
    }

    /// "2026-11" -> "Nov. 26"
    /// "Jun" – month name without year (compact tables).
    static func monthName(_ ym: String) -> String {
        date(ym + "-01").map { monthOnly.string(from: $0) } ?? ym
    }
    static func monthLabel(_ ym: String) -> String {
        date(ym + "-01").map { monthShort.string(from: $0) } ?? ym
    }
}

extension Color {
    /// `#RRGGBB` in sRGB (nil if the colour can't be converted) – the
    /// inverse of `init(hex:)`, for the system colour well.
    var hexRGB: String? {
        #if os(macOS)
        guard let c = NSColor(self).usingColorSpace(.sRGB) else { return nil }
        let (r, g, b) = (c.redComponent, c.greenComponent, c.blueComponent)
        #else
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        guard UIColor(self).getRed(&r, green: &g, blue: &b, alpha: &a) else { return nil }
        #endif
        func byte(_ v: CGFloat) -> Int { Int((min(max(v, 0), 1) * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(r), byte(g), byte(b))
    }

    init(hex: String) {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        var v: UInt64 = 0
        Scanner(string: s).scanHexInt64(&v)
        if s.count == 6 {
            self.init(red: Double((v >> 16) & 0xFF) / 255, green: Double((v >> 8) & 0xFF) / 255, blue: Double(v & 0xFF) / 255)
        } else {
            self.init(white: 0.55)
        }
    }

    /// Light/dark pair that follows the system appearance.
    /// Light/dark pair; the optional high-contrast pair is used when
    /// "Kontrast erhöhen" (Increase Contrast) is on.
    init(light: String, dark: String, lightHighContrast: String? = nil, darkHighContrast: String? = nil) {
        #if os(macOS)
        let l = NSColor(Color(hex: light)), d = NSColor(Color(hex: dark))
        let lh = lightHighContrast.map { NSColor(Color(hex: $0)) } ?? l
        let dh = darkHighContrast.map { NSColor(Color(hex: $0)) } ?? d
        self.init(nsColor: NSColor(name: nil) { appearance in
            switch appearance.bestMatch(from: [.aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua]) {
            case .darkAqua: d
            case .accessibilityHighContrastAqua: lh
            case .accessibilityHighContrastDarkAqua: dh
            default: l
            }
        })
        #else
        let l = UIColor(Color(hex: light)), d = UIColor(Color(hex: dark))
        let lh = lightHighContrast.map { UIColor(Color(hex: $0)) } ?? l
        let dh = darkHighContrast.map { UIColor(Color(hex: $0)) } ?? d
        self.init(uiColor: UIColor { t in
            let high = t.accessibilityContrast == .high
            return t.userInterfaceStyle == .dark ? (high ? dh : d) : (high ? lh : l)
        })
        #endif
    }

    /// Brand / category colours used as text or icons: too light for the light
    /// theme (yellow, pink) get a dark fallback; too dark for the dark theme
    /// are lightened just enough for text contrast – hue and saturation stay,
    /// so a muted blue stays blue instead of turning grey (as in MoneyMoney).
    static func readable(hex: String) -> Color {
        let s = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        var v: UInt64 = 0
        Scanner(string: s).scanHexInt64(&v)
        let r = Double((v >> 16) & 0xFF), g = Double((v >> 8) & 0xFF), b = Double(v & 0xFF)
        let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255
        let light = lum > 0.72 ? "#0F172A" : "#" + s
        return Color(
            light: light,
            dark: ReadableColor.onDark(hex: s, minContrast: 4.5),
            darkHighContrast: ReadableColor.onDark(hex: s, minContrast: 7)
        )
    }
}

/// Contrast helpers for `Color.readable` (pure, testable).
enum ReadableColor {
    /// The lightest dark-theme surface (`Theme.card`) – the worst case.
    static let darkSurface = "1E293B"

    /// WCAG relative luminance of `#RRGGBB` / `RRGGBB`.
    static func luminance(hex: String) -> Double {
        let (r, g, b) = rgb(hex)
        func lin(_ c: Double) -> Double { c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
        return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    /// WCAG contrast ratio of two colours (1…21).
    static func contrast(_ a: String, _ b: String) -> Double {
        let (x, y) = (luminance(hex: a), luminance(hex: b))
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }

    /// `hex` with its HSL lightness raised (hue and saturation unchanged)
    /// until it reaches `minContrast` against `darkSurface`; unchanged when it
    /// already does. Returns `#RRGGBB`.
    static func onDark(hex: String, minContrast: Double) -> String {
        let (r, g, b) = rgb(hex)
        let (h, s, l0) = hsl(r, g, b)
        var l = l0
        var out = hexString(r, g, b)
        while contrast(out, darkSurface) < minContrast && l < 1 {
            l = min(1, l + 0.01)
            let c = rgbFromHSL(h, s, l)
            out = hexString(c.0, c.1, c.2)
        }
        return out
    }

    // MARK: - conversions (components 0…1)

    static func rgb(_ hex: String) -> (Double, Double, Double) {
        let s = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        var v: UInt64 = 0
        Scanner(string: s).scanHexInt64(&v)
        return (Double((v >> 16) & 0xFF) / 255, Double((v >> 8) & 0xFF) / 255, Double(v & 0xFF) / 255)
    }

    static func hexString(_ r: Double, _ g: Double, _ b: Double) -> String {
        func byte(_ c: Double) -> Int { Int((min(1, max(0, c)) * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(r), byte(g), byte(b))
    }

    static func hsl(_ r: Double, _ g: Double, _ b: Double) -> (Double, Double, Double) {
        let mx = max(r, g, b), mn = min(r, g, b), l = (mx + mn) / 2
        guard mx != mn else { return (0, 0, l) }
        let d = mx - mn
        let s = l > 0.5 ? d / (2 - mx - mn) : d / (mx + mn)
        var h: Double
        switch mx {
        case r: h = (g - b) / d + (g < b ? 6 : 0)
        case g: h = (b - r) / d + 2
        default: h = (r - g) / d + 4
        }
        h /= 6
        return (h, s, l)
    }

    static func rgbFromHSL(_ h: Double, _ s: Double, _ l: Double) -> (Double, Double, Double) {
        guard s > 0 else { return (l, l, l) }
        let q = l < 0.5 ? l * (1 + s) : l + s - l * s
        let p = 2 * l - q
        func hue(_ t0: Double) -> Double {
            var t = t0
            if t < 0 { t += 1 }
            if t > 1 { t -= 1 }
            if t < 1 / 6 { return p + (q - p) * 6 * t }
            if t < 1 / 2 { return q }
            if t < 2 / 3 { return p + (q - p) * (2 / 3 - t) * 6 }
            return p
        }
        return (hue(h + 1 / 3), hue(h), hue(h - 1 / 3))
    }
}

extension BankConnection {
    /// Colour and monogram of well-known banks that run on the generic FinTS
    /// connector (keyed by BLZ); everything else uses its connector's.
    private var fintsBrand: (tintHex: String, monogram: String)? {
        guard connector == .finTsGeneric else { return nil }
        switch bankCode {
        case "76030080", "70120400": return ("#0A7EA4", "CB") // Consorsbank
        case "50070439": return ("#05164D", "MM") // Miles & More Kreditkarte (Deutsche Bank)
        default: return nil
        }
    }
    var tint: Color { fintsBrand.map { Color.readable(hex: $0.tintHex) } ?? connector.tint }
    var monogram: String { fintsBrand?.monogram ?? connector.monogram }
}

extension ConnectorKind {
    var displayName: String {
        switch self {
        case .volksbankKasselGoettingen: return "Volksbank Kassel Göttingen"
        case .direkt1822: return "1822direkt"
        case .finTsGeneric: return "FinTS-Bank"
        case .comdirect: return "comdirect"
        case .payPal: return "PayPal"
        case .tradeRepublic: return "Trade Republic"
        case .americanExpress: return "American Express"
        case .amazonVisa: return "Amazon Visa"
        case .bunq: return "bunq"
        case .enableBanking: return "Enable Banking (PSD2)"
        case .scalableCapital: return "Scalable Capital"
        case .amazonOrders: return "Amazon Bestellungen"
        case .fnz: return "FNZ Bank"
        case .ibkr: return "Interactive Brokers (IBKR)"
        case .advanzia: return "Advanzia Bank"
        case .equatePlus: return "EquatePlus"
        case .quirion: return "quirion"
        case .easybank: return "easybank"
        case .alteLeipziger: return "Alte Leipziger"
        case .volkswagenBank: return "Volkswagen Bank"
        case .uberspace: return "Uberspace"
        case .schwaebischHall: return "Schwäbisch Hall"
        case .ffb: return "FFB (FIL Fondsbank)"
        case .fordMoney: return "Ford Money"
        case .opelDirektbank: return "Opel Direktbank"
        case .hanseaticBank: return "Hanseatic Bank"
        case .bhw: return "BHW Bausparkasse"
        case .finpension: return "Finpension"
        case .kontist: return "Kontist"
        }
    }

    /// Subtle institute colour (no logos – a monogram signet instead).
    var tintHex: String {
        switch self {
        case .volksbankKasselGoettingen: return "#1D5FBF"
        case .direkt1822: return "#D63A2F"
        case .finTsGeneric: return "#4A6FA5"
        case .comdirect: return "#3C3C3C"
        case .payPal: return "#003087"
        case .tradeRepublic: return "#111111"
        case .americanExpress: return "#2E77BC"
        case .amazonVisa: return "#3C2A7A"
        case .bunq: return "#1F8F5F"
        case .enableBanking: return "#5B5BD6"
        case .scalableCapital: return "#0E2A47"
        case .amazonOrders: return "#B45F06"
        case .fnz: return "#265B55"
        case .ibkr: return "#D81222"
        case .advanzia: return "#243B77"
        case .equatePlus: return "#608B36"
        case .quirion: return "#D4005A"
        case .easybank: return "#00AEEF"
        case .alteLeipziger: return "#BE0D3E"
        case .volkswagenBank: return "#001E50"
        case .uberspace: return "#2C3E50"
        case .schwaebischHall: return "#004F9F"
        case .ffb: return "#1C5A8C"
        case .fordMoney: return "#20396E"
        case .opelDirektbank: return "#1A1A1A"
        case .hanseaticBank: return "#9B1B30"
        case .bhw: return "#C8102E"
        case .finpension: return "#00A68C"
        case .kontist: return "#2B2D73"
        }
    }

    var tint: Color { Color.readable(hex: tintHex) }

    /// FinTS banks: the user can choose SEPA Instant (HKIPZ, with fallback to
    /// a regular transfer). bunq decides on its own.
    var offersInstantChoice: Bool {
        [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric].contains(self)
    }

    var monogram: String {
        switch self {
        case .volksbankKasselGoettingen: return "VB"
        case .direkt1822: return "18"
        case .finTsGeneric: return "FT"
        case .comdirect: return "cd"
        case .payPal: return "P"
        case .tradeRepublic: return "TR"
        case .americanExpress: return "AX"
        case .amazonVisa: return "AV"
        case .bunq: return "bq"
        case .enableBanking: return "PS"
        case .scalableCapital: return "SC"
        case .amazonOrders: return "AZ"
        case .fnz: return "FNZ"
        case .ibkr: return "IB"
        case .advanzia: return "AD"
        case .equatePlus: return "EQ"
        case .quirion: return "Q"
        case .easybank: return "EB"
        case .alteLeipziger: return "AL"
        case .volkswagenBank: return "VW"
        case .uberspace: return "US"
        case .schwaebischHall: return "SH"
        case .ffb: return "FFB"
        case .fordMoney: return "FM"
        case .opelDirektbank: return "OD"
        case .hanseaticBank: return "HB"
        case .bhw: return "BHW"
        case .finpension: return "FP"
        case .kontist: return "KT"
        }
    }
}

extension FlowKind {
    var label: String {
        switch self {
        case .consumption: return String(localized: "Konsum")
        case .income: return String(localized: "Einnahme")
        case .investment: return String(localized: "Investment")
        case .perk: return String(localized: "Perk")
        case .transfer: return String(localized: "Umbuchung")
        case .loan: return String(localized: "Darlehen")
        }
    }
}

/// Puts plain text on the clipboard (stays there; for secrets use
/// `VaultKeyManager.copyToPasteboard`, which clears it after 60 s).
func copyToPasteboard(_ text: String) {
    #if os(macOS)
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
    #else
    UIPasteboard.general.string = text
    #endif
}

extension Booking {
    /// Tab-separated line for pasting into a spreadsheet:
    /// date · name · purpose · category · note · amount.
    var clipboardLine: String {
        [
            DateText.medium(bookingDate),
            counterparty.isEmpty ? merchantName : counterparty,
            purpose,
            categoryName,
            note,
            Money.clipboard(amountCents),
        ]
        .map { $0.replacingOccurrences(of: "\t", with: " ").replacingOccurrences(of: "\n", with: " ") }
        .joined(separator: "\t")
    }

    /// "Supermarkt · Kartenzahlung"
    var subtitle: String {
        let cat = categoryName.isEmpty ? String(localized: "Ohne Kategorie") : categoryName
        return paymentMethod.isEmpty ? cat : "\(cat) · \(paymentMethod)"
    }
}

/// HIG / accessibility: with "Bewegung reduzieren" on, nothing slides or
/// springs – changes appear at once or fade.
enum Motion {
    static var reduced: Bool {
        #if os(macOS)
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        #else
        UIAccessibility.isReduceMotionEnabled
        #endif
    }

    /// `animation`, or none when Reduce Motion is on.
    static func animation(_ animation: Animation) -> Animation? { reduced ? nil : animation }

    /// Slide in from `edge` with a fade; only the fade when Reduce Motion is on.
    static func slide(_ edge: Edge) -> AnyTransition {
        reduced ? .opacity : .move(edge: edge).combined(with: .opacity)
    }
}
