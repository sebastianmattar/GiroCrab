import SwiftUI
import GiroCrabCore

/// 36×36 squircle signet for a merchant:
///  Tier 2 – cached favicon (only if the user enabled online logos),
///  Tier 1 – monogram in a subtle brand colour,
///  Tier 3 – category SF Symbol for unknown merchants.
struct MerchantSignet: View {
    @Environment(AppModel.self) private var model
    let merchant: MerchantInfo
    var categorySymbol: String = ""
    @Environment(\.textScale) private var textScale
    var size: CGFloat = 36
    var showNewDot = false

    /// Only for logos that arrive after the first frame; a logo already in the
    /// model's cache is drawn right away (a row built while scrolling would
    /// otherwise show its monogram for one frame and lay out twice).
    @State private var loadedLogo: (image: Image, pixels: Int)?

    var body: some View {
        let size = self.size * textScale   // grows with the text size (#343)
        let logo = model.cachedLogo(for: merchant) ?? loadedLogo
        let shape = RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
        let brand = Color(hex: merchant.colorHex)
        let sharpLogo = logo.flatMap { $0.pixels >= 48 ? $0.image : nil }
        ZStack {
            if merchant.isPerson {
                // private person: calm circular initials, never a company logo
                Circle().fill(Theme.personFill)
                Text(merchant.monogram)
                    .font(.system(size: size * 0.36, weight: .semibold, design: .rounded))
                    .foregroundStyle(Theme.personText)
            } else if let img = sharpLogo {
                // downloaded favicon (Google s2, 128 px) – real brand logo
                shape.fill(Color.white)
                img.resizable().interpolation(.high).scaledToFit().padding(size * 0.14)
            } else if !merchant.svgPath.isEmpty {
                // simple-icons vector glyph in the brand colour
                shape.fill(brand.opacity(0.09))
                SVGPathShape(d: merchant.svgPath)
                    .fill(Color.readable(hex: merchant.colorHex), style: FillStyle(eoFill: false))
                    .padding(size * 0.2)
            } else if let small = logo?.image, merchant.domainSource == "dictionary" || merchant.domainSource == "override" {
                // small favicons only for known brands – a guessed domain must deliver a sharp logo
                shape.fill(Color.white)
                small.resizable().interpolation(.high).scaledToFit().padding(size * 0.2)
            } else if !merchant.known && !categorySymbol.isEmpty {
                shape.fill(Theme.neutralTile)
                Image(systemName: categorySymbol)
                    .font(.system(size: size * 0.42, weight: .medium))
                    .foregroundStyle(Theme.secondary)
            } else {
                shape.fill(brand.opacity(0.10))
                Text(merchant.monogram)
                    .font(.system(size: size * (merchant.monogram.count > 2 ? 0.30 : 0.40), weight: .bold, design: .rounded))
                    .foregroundStyle(Color.readable(hex: merchant.colorHex))
                    .minimumScaleFactor(0.6)
            }
            if !merchant.isPerson {
                shape.strokeBorder(sharpLogo == nil ? brand.opacity(0.16) : Theme.border, lineWidth: 1)
            }
        }
        .frame(width: size, height: size)
        .overlay(alignment: .topTrailing) {
            if showNewDot {
                Circle().fill(Theme.accent).frame(width: 8, height: 8)
                    .overlay(Circle().stroke(Theme.card, lineWidth: 1.5))
                    .offset(x: 2, y: -2)
            }
        }
        .task(id: merchant.domainCandidates.joined(separator: "|") + merchant.domain + model.logoProvider + "\(model.logoVersion)") {
            // cached: drawn above already – no second render
            guard model.cachedLogo(for: merchant) == nil else { return }
            loadedLogo = await model.logo(for: merchant)
        }
        // The blue dot is the only "new" signal on screen: say it, too.
        .accessibilityLabel(showNewDot ? Text("\(merchant.name), neu") : Text(merchant.name))
    }
}

/// Small signet for accounts (no bank logos: monogram + subtle tint).
struct AccountSignet: View {
    let account: Account
    @Environment(\.textScale) private var textScale
    var size: CGFloat = 22
    /// The account's colour (its marker in the booking list); nil = bank tint.
    var color: Color? = nil
    /// On the accent-coloured sidebar selection: system colour (white there).
    var onSelection = false

    var body: some View {
        let size = self.size * textScale
        let tint = onSelection ? Color.primary
            : color ?? (account.kind == .loan ? Theme.loan : (account.connector?.tint ?? Theme.secondary))
        RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
            .fill(tint.opacity(0.12))
            .overlay(
                Image(systemName: account.sfSymbol)
                    .font(.system(size: size * 0.48, weight: .semibold))
                    .foregroundStyle(tint)
            )
            .frame(width: size, height: size)
    }
}

/// Rounded square with an SF Symbol – used instead of bank logos.
struct SymbolTile: View {
    let symbol: String
    let tint: Color
    @Environment(\.textScale) private var textScale
    var size: CGFloat = 22

    var body: some View {
        let size = self.size * textScale
        Image(systemName: symbol)
            .font(.system(size: size * 0.5, weight: .semibold))
            .foregroundStyle(tint)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size * 0.28, style: .continuous).fill(tint.opacity(0.12)))
    }
}
