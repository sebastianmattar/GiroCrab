import SwiftUI
import GiroCrabCore

/// One figure of a holdings view. A missing figure says so; it is never shown as 0.
struct DepotMetricTile: View {
    let title: LocalizedStringKey
    let value: String?
    let detail: String?
    let tint: Color
    let footnote: String
    let missing: String
    let help: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).appFont(.caption, weight: .medium).foregroundStyle(Theme.secondary).lineLimit(2)
            if let value {
                Text(value).sensitive(.amounts)
                    .appFont(.title3, weight: .semibold, monospacedDigit: true).foregroundStyle(tint)
                    .lineLimit(1).minimumScaleFactor(0.75)
                if let detail {
                    Text(detail).sensitive(.amounts)
                        .appFont(.caption, monospacedDigit: true).foregroundStyle(tint == Theme.text ? Theme.secondary : tint)
                        .lineLimit(2)
                }
                Text(footnote).appFont(.caption2).foregroundStyle(Theme.tertiary).lineLimit(3).padding(.top, 2)
            } else {
                Text("Nicht verfügbar").appFont(.title3, weight: .semibold).foregroundStyle(Theme.tertiary).lineLimit(1)
                Text(missing).appFont(.caption2).foregroundStyle(Theme.tertiary).lineLimit(3).padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(Theme.card))
        .overlay(RoundedRectangle(cornerRadius: 9, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
        .help(help)
        .accessibilityElement(children: .combine)
    }
}

/// A row of figure tiles: one row when wide, rows of two otherwise, one per row where even two do not
/// fit (iPhone with very large text). The tiles of a row share their height.
struct DepotMetricGrid: View {
    @Environment(\.textScale) private var textScale
    let tiles: [DepotMetricTile]
    let wide: Bool
    /// Two tiles fit side by side.
    @State private var fitsTwo = true
    /// Dynamic Type (iPhone) scales the minimum width of a tile like the text inside it.
    @ScaledMetric(relativeTo: .body) private var minTileWidth: CGFloat = 170

    private var perRow: Int {
        if wide { return max(tiles.count, 1) }
        return fitsTwo ? 2 : 1
    }

    var body: some View {
        let rows = stride(from: 0, to: tiles.count, by: perRow).map { Array(tiles[$0..<min($0 + perRow, tiles.count)]) }
        Grid(horizontalSpacing: 10, verticalSpacing: 10) {
            ForEach(rows.indices, id: \.self) { r in
                GridRow { ForEach(rows[r].indices, id: \.self) { rows[r][$0] } }
            }
        }
        .frame(maxWidth: .infinity)
        .onGeometryChange(for: Bool.self) { [minTileWidth, textScale] in
            $0.size.width >= 2 * minTileWidth * textScale + 10
        } action: { fitsTwo = $0 }
    }
}

/// The figure tiles of the holdings views, built from finished data so the depot, the Sachwerte
/// account and the wealth tabs show the same tiles the same way.
enum DepotKPIs {
    private static func otherCurrencyNote(_ t: SecurityActivityTotals) -> String {
        t.otherCurrencyCount > 0 ? " · " + String(localized: "\(t.otherCurrencyCount) in Fremdwährung nicht enthalten") : ""
    }

    /// Unrealized P&L and day change of the current holdings; distributions and costs of the period.
    static func securities(assets: [ClassifiedAsset], transactions: [SecurityTransaction], currency: String,
                           periodTitle period: String, from: String?, to: String?) -> [DepotMetricTile] {
        let totals = SecurityActivityTotals.make(transactions, currency: currency, from: from, to: to)
        let unrealized = DepotMetrics.unrealized(assets)
        let day = DepotMetrics.dayChange(assets.map(\.holding))
        return [
            DepotMetricTile(
                title: "Unrealisierte GuV",
                value: unrealized.map { Money.signed($0.amountCents, currency: currency) },
                detail: unrealized?.percent.map { DepotFormat.percent($0, signed: true) },
                tint: unrealized.map { DepotFormat.tint($0.amountCents) } ?? Theme.text,
                footnote: [String(localized: "Aktueller Bestand seit Einstand"),
                           unrealized.flatMap(DepotFormat.coverage),
                           (unrealized?.calculated ?? 0) > 0 ? String(localized: "teils berechnet") : nil].compactMap { $0 }.joined(separator: " · "),
                missing: String(localized: "Keine Einstandsdaten geliefert"),
                help: String(localized: "Bewertung des aktuellen Bestands gegenüber dem Einstand, nicht realisiert und unabhängig vom Zeitraum. Bankwert, sonst aus dem Einstandskurs berechnet.")),
            DepotMetricTile(
                title: "Tagesveränderung",
                value: day.map { Money.signed($0.amountCents, currency: currency) },
                detail: day?.percent.map { DepotFormat.percent($0, signed: true) },
                tint: day.map { DepotFormat.tint($0.amountCents) } ?? Theme.text,
                footnote: [String(localized: "Aktueller Bestand zum Vortag"), day.flatMap(DepotFormat.coverage)].compactMap { $0 }.joined(separator: " · "),
                missing: String(localized: "Keine Vortagswerte geliefert"),
                help: String(localized: "Aus den Vortageswerten der Bank. Käufe und Verkäufe von heute sind nicht herausgerechnet.")),
            DepotMetricTile(
                title: "Ausschüttungen",
                value: totals.isEmpty ? nil : Money.format(totals.distributionsCents, currency: currency),
                detail: totals.isEmpty ? nil : String(localized: "\(totals.distributionCount) Zahlungen"),
                tint: Theme.inflow,
                footnote: String(localized: "Brutto · \(period)") + otherCurrencyNote(totals),
                missing: String(localized: "Keine Aktivitäten im Zeitraum erfasst"),
                help: String(localized: "Summe der erfassten Ausschüttungen vor Quellensteuer. Gilt für den gewählten Zeitraum, nicht für den Bestand. Fehlende Einträge bedeuten nicht, dass es keine Ausschüttungen gab.")),
            DepotMetricTile(
                title: "Gebühren & Steuern",
                value: totals.isEmpty ? nil : Money.format(-totals.costsCents, currency: currency),
                detail: totals.isEmpty ? nil : String(localized: "Gebühren \(Money.format(totals.feesCents, currency: currency)) · Steuern \(Money.format(totals.taxesCents, currency: currency))"),
                tint: totals.costsCents > 0 ? Theme.negative : Theme.text,
                footnote: String(localized: "Zeitraum: \(period)") + otherCurrencyNote(totals),
                missing: String(localized: "Keine Aktivitäten im Zeitraum erfasst"),
                help: String(localized: "Gebühren und Steuern aus den erfassten Wertpapieraktivitäten, eigene Zeilen und eingebettete Beträge ohne Doppelzählung. Fehlende Einträge bedeuten nicht, dass keine Kosten anfielen.")),
        ]
    }

    /// The Sachwerte tiles from the items themselves: gains by purchase data, the day change of the
    /// automatically priced items from yesterday's price of one piece (`previousUnits`, item id).
    static func tangibles(items: [AssetItem], previousUnits: [Int64: Int64], currency: String) -> [DepotMetricTile] {
        let held = items.filter { !$0.sold }
        let rows = held.map { item -> (value: Int64, gain: (cents: Int64, percent: Double)?, day: Int64?) in
            (item.holding.valueCents, WealthView.tangibleGain(item),
             WealthView.tangibleDayChange(item, previousUnitCents: previousUnits[item.id]))
        }
        return tangibles(
            valueCents: rows.reduce(0) { $0 + $1.value }, count: rows.count,
            gains: rows.map { r in r.gain.map { HoldingFigures.Gain(cents: $0.cents, costCents: r.value - $0.cents, reported: false) } },
            days: rows.compactMap { r in r.day.map { HoldingFigures.Day(cents: $0, previousValueCents: r.value - $0) } },
            automaticCount: held.filter { $0.setup.priceSource != "manual" }.count,
            largest: held.max { $0.holding.valueCents < $1.holding.valueCents }.map { ($0.holding.name, $0.holding.valueCents) }, currency: currency)
    }

    /// Value, unrealized P&L, day change and cost basis of Sachwerte. `gains` has one entry per item
    /// (nil without a purchase price); `days` one per automatically priced item that has a daily price.
    static func tangibles(valueCents: Int64, count: Int, gains: [HoldingFigures.Gain?], days: [HoldingFigures.Day], automaticCount: Int,
                          largest: (name: String, cents: Int64)? = nil, currency: String) -> [DepotMetricTile] {
        let largestShare = largest.flatMap { l in valueCents > 0 ? (name: l.name, percent: Double(l.cents) / Double(valueCents) * 100) : nil }
        let unrealized = DepotMetrics.unrealized(gains)
        let cost = gains.compactMap { $0 }.reduce(Int64(0)) { $0 + max($1.costCents, 0) }
        let covered = gains.compactMap { $0 }.count
        let dayCents = days.reduce(Int64(0)) { $0 + $1.cents }
        let dayBase = days.reduce(Int64(0)) { $0 + $1.previousValueCents }
        let coverage = { (covered: Int, total: Int) in covered < total ? String(localized: "für \(covered) von \(total) Positionen") : nil }
        return [
            DepotMetricTile(
                title: "Einstandswert",
                value: covered > 0 ? Money.format(cost, currency: currency) : nil,
                detail: nil,
                tint: Theme.text,
                footnote: [String(localized: "Aktueller Bestand"), coverage(covered, count)].compactMap { $0 }.joined(separator: " · "),
                missing: String(localized: "Keine Einstandsdaten vorhanden"),
                help: String(localized: "Summe der Einstandswerte der gehaltenen Objekte, soweit bekannt.")),
            DepotMetricTile(
                title: "Unrealisierte GuV",
                value: unrealized.map { Money.signed($0.amountCents, currency: currency) },
                detail: unrealized?.percent.map { DepotFormat.percent($0, signed: true) },
                tint: unrealized.map { DepotFormat.tint($0.amountCents) } ?? Theme.text,
                footnote: [String(localized: "Aktueller Bestand seit Einstand"), coverage(covered, count)].compactMap { $0 }.joined(separator: " · "),
                missing: String(localized: "Keine Einstandsdaten vorhanden"),
                help: String(localized: "Wert heute gegenüber dem Einstand, nicht realisiert. Nur Objekte mit bekanntem Einstandspreis.")),
            DepotMetricTile(
                title: "Tagesveränderung",
                value: days.isEmpty ? nil : Money.signed(dayCents, currency: currency),
                detail: days.isEmpty || dayBase <= 0 ? nil : DepotFormat.percent(Double(dayCents) / Double(dayBase) * 100, signed: true),
                tint: days.isEmpty ? Theme.text : DepotFormat.tint(dayCents),
                footnote: [String(localized: "Aktueller Bestand zum Vortag"), coverage(days.count, automaticCount)].compactMap { $0 }.joined(separator: " · "),
                missing: String(localized: "Keine Tagespreise vorhanden"),
                help: String(localized: "Nur automatisch bepreiste Objekte (Edelmetalle) haben einen Tagespreis; manuelle Bewertungen ändern sich nicht täglich.")),
            DepotMetricTile(
                title: "Größte Position",
                value: largestShare.map { DepotFormat.percent($0.percent, digits: 1) },
                detail: largestShare?.name,
                tint: Theme.text,
                footnote: String(localized: "Anteil am gesamten Sachwert"),
                missing: String(localized: "Noch keine Sachwerte"),
                help: String(localized: "Das Objekt mit dem höchsten Wert und sein Anteil am gesamten Sachwert.")),
        ]
    }
}
