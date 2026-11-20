import SwiftUI
import GiroCrabCore

/// How the selected account is presented. THE single place that decides it
/// (`AppModel.presentation`): routing, toolbar, shortcuts and the command palette follow this.
enum AccountPresentation: Equatable {
    /// Bookings of a cash account, a group, a category …: `BookingStreamView`.
    case bookings
    /// A securities depot: holdings and securities activities (`DepotDetailView`).
    case securities(Int64)
    /// A Sachwerte account: items with their own valuations (`TangibleAccountDetailView`).
    case tangible(Int64)
}

/// Page frame of the "holdings" views (securities depot, Sachwerte): banners, header, figures,
/// analysis and the actual content, in one scroll view. The frame knows no account kind and loads
/// nothing: each kind brings its own header, figures and content as slots and owns its data. The
/// search belongs to the frame's host view, never to the booking search.
struct AssetAccountFrame<Top: View, Header: View, Metrics: View, Analysis: View, Content: View>: View {
    @Environment(AppModel.self) private var model
    @Environment(\.textScale) private var textScale

    let searchText: Binding<String>
    let searchPrompt: LocalizedStringKey
    /// A click on the empty space around the cards (Sachwerte: ends the item selection).
    var backgroundTap: (() -> Void)?
    var minContentHeight: CGFloat?
    let top: () -> Top
    let header: () -> Header
    /// Receives whether the window is wide enough for the table layouts.
    let metrics: (Bool) -> Metrics
    let analysis: () -> Analysis
    let content: (Bool) -> Content

    /// Wide enough for the table layouts. Updated only when the layout switches, so a resize
    /// does not re-run the frame at every step.
    @State private var wide = false

    init(
        searchText: Binding<String>,
        searchPrompt: LocalizedStringKey,
        backgroundTap: (() -> Void)? = nil,
        minContentHeight: CGFloat? = nil,
        @ViewBuilder top: @escaping () -> Top,
        @ViewBuilder header: @escaping () -> Header,
        @ViewBuilder metrics: @escaping (Bool) -> Metrics,
        @ViewBuilder analysis: @escaping () -> Analysis,
        @ViewBuilder content: @escaping (Bool) -> Content
    ) {
        self.searchText = searchText
        self.searchPrompt = searchPrompt
        self.backgroundTap = backgroundTap
        self.minContentHeight = minContentHeight
        self.top = top
        self.header = header
        self.metrics = metrics
        self.analysis = analysis
        self.content = content
    }

    var body: some View {
        VStack(spacing: 0) {
            BankingPolicyBanner()
            AnnouncementBanner()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    // Marketing screenshots (`--screenshot`) look like a real vault: no demo banner.
                    if model.demoActive && LaunchOptions.current.screenshotPath == nil {
                        DemoBanner()
                    }
                    top()
                    header()
                    metrics(wide)
                    analysis()
                    content(wide)
                }
                .frame(maxWidth: .infinity, minHeight: minContentHeight, alignment: .topLeading)
                .padding(.horizontal, 20)
                .padding(.top, 18)
                .padding(.bottom, 18)
                // a background layer, so it never sees clicks on the rows
                .background {
                    if let backgroundTap {
                        Color.clear.contentShape(Rectangle()).onTapGesture(perform: backgroundTap)
                    }
                }
                .onGeometryChange(for: Bool.self) { [textScale] in $0.size.width >= 700 * textScale } action: { wide = $0 }
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .background(Theme.canvas)
        #if os(iOS)
        // in the navigation bar, never as a capsule floating over the scrolling cards
        .searchable(text: searchText, placement: .navigationBarDrawer(displayMode: .automatic), prompt: searchPrompt)
        #else
        .searchable(text: searchText, placement: .toolbar, prompt: searchPrompt)
        #endif
        #if os(iOS)
        .modifier(MainToolbar())
        .modifier(WindowTitle())
        .refreshable { await model.refreshCurrentView() }
        #endif
        .bookingOutputSheets()
    }
}

/// Header card of a holdings view: title, current value, a value chart and the footer row with the
/// period menu, documents and kind-specific actions. Wide: the name, details and actions on the left,
/// the value with its chart below on the right (about 40 % of the card). Narrow and on iPhone: name
/// and value first, the chart across the full width below. Minimised: no chart. The cash views'
/// header (`StreamHeaderCard`) stays separate: it carries booking figures and filters.
struct AssetHeaderCard<Subtitle: View, ValueNote: View, Accessory: View, Actions: View>: View {
    @Environment(AppModel.self) private var model
    @Environment(\.textScale) private var textScale
    @AppStorage(ViewLayout.compactHeaderKey) private var compactHeader = false
    @State private var showDocuments = false
    /// Too narrow for name and value side by side (iPhone, large text): one below the other.
    @State private var stacked = false

    let account: Account
    let title: String
    let valueCents: Int64
    let valueLabel: String
    /// A securities depot has no pay cycle, so its period menu hides "since last salary".
    var periodForDepot = false
    var valueHelp: LocalizedStringKey?
    let subtitle: () -> Subtitle
    let valueNote: () -> ValueNote
    /// The chart below the value (an `AssetValueChart`): finished data, no query of its own.
    let accessory: () -> Accessory
    let actions: () -> Actions

    init(
        account: Account,
        title: String,
        valueCents: Int64,
        valueLabel: String,
        periodForDepot: Bool = false,
        valueHelp: LocalizedStringKey? = nil,
        @ViewBuilder subtitle: @escaping () -> Subtitle,
        @ViewBuilder valueNote: @escaping () -> ValueNote,
        @ViewBuilder accessory: @escaping () -> Accessory,
        @ViewBuilder actions: @escaping () -> Actions
    ) {
        self.account = account
        self.title = title
        self.valueCents = valueCents
        self.valueLabel = valueLabel
        self.periodForDepot = periodForDepot
        self.valueHelp = valueHelp
        self.subtitle = subtitle
        self.valueNote = valueNote
        self.accessory = accessory
        self.actions = actions
    }

    private var currency: String { account.currency.isEmpty ? "EUR" : account.currency }

    private var compactTitle: LocalizedStringKey {
        compactHeader ? "Kopfbereich einblenden" : "Kopfbereich minimieren"
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 9) {
            if stacked {
                // iPhone width: the name gets the full line, the value sits below it, nothing is cut off
                titleBlock
                HStack(alignment: .firstTextBaseline) {
                    valueBlock.frame(maxWidth: .infinity, alignment: .leading)
                }
                if !compactHeader {
                    accessory()
                }
                footer
            } else {
                HStack(alignment: .top, spacing: 12) {
                    titleBlock
                    Spacer(minLength: 8)
                    valueBlock
                }
                if !compactHeader {
                    accessory()
                }
                footer
            }
        }
        .card()
        .onGeometryChange(for: Bool.self) { [textScale] in $0.size.width > 0 && $0.size.width < 460 * textScale } action: { stacked = $0 }
        .capturableSheet(isPresented: $showDocuments) { AccountDocumentsSheet(account: account) }
        .capturablePopover(isPresented: $model.showCustomPeriod, arrowEdge: .bottom) { CustomPeriodForm() }
    }

    private var titleBlock: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).sensitive()
                    .appFont(compactHeader ? .headline : .title2, weight: .semibold)
                    .foregroundStyle(Theme.text).lineLimit(stacked ? 2 : 1).minimumScaleFactor(stacked ? 1 : 0.7)
                if !compactHeader {
                    subtitle()
                }
            }
            .contextMenu { AccountCopyMenu(account: account) }
            // Minimise the header: more room for the content (GitHub #358); the chart goes away with it.
            Button {
                withAnimation(Motion.animation(.easeInOut(duration: 0.25))) { compactHeader.toggle() }
            } label: {
                Image(systemName: compactHeader ? "chevron.down" : "chevron.up")
                    .appFont(.caption, weight: .semibold)
                    .foregroundStyle(Theme.secondary)
                    .padding(4)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(compactTitle)
            .accessibilityLabel(compactTitle)
            #if os(iOS)
            // long-press is invisible on iPhone/iPad: show an explicit entry too
            Menu { AccountCopyMenu(account: account) } label: {
                Image(systemName: "doc.on.doc").appFont(.caption).foregroundStyle(Theme.secondary)
            }
            .accessibilityLabel("In Zwischenablage kopieren")
            #endif
        }
    }

    /// The current value. It stays the current value: hovering the chart shows the value of that
    /// day in the chart's own readout and never replaces this number.
    private var valueBlock: some View {
        VStack(alignment: stacked ? .leading : .trailing, spacing: 2) {
            Text(Money.format(valueCents, currency: currency)).sensitive(.amounts)
                .appFont(compactHeader ? .title3 : .largeTitle, weight: .semibold, monospacedDigit: true)
                .foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.6)
            if !compactHeader {
                Text(valueLabel).appFont(.caption).foregroundStyle(Theme.secondary)
                valueNote()
            }
        }
        .frame(maxWidth: stacked ? .infinity : nil, alignment: stacked ? .leading : .trailing)
        .help(valueHelp ?? "")
        .contextMenu {
            Button("\(valueLabel) kopieren", systemImage: "doc.on.doc") { copyToPasteboard(Money.clipboard(valueCents)) }
        }
    }

    /// Period and documents stay at hand; the kind-specific actions follow, on a second line if the
    /// card is too narrow for one.
    private var footer: some View {
        let controls = HStack(spacing: 12) {
            PeriodMenuButton(forDepot: periodForDepot)
            Button { showDocuments = true } label: {
                Label("Dokumente", systemImage: "doc.text").lineLimit(1).fixedSize(horizontal: true, vertical: false)
            }
            .buttonStyle(.borderless)
        }
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) { controls; Spacer(minLength: 8); actions() }
            VStack(alignment: .leading, spacing: 6) {
                controls
                HStack(spacing: 12) { actions() }
            }
        }
        .appFont(.caption)
        .foregroundStyle(Theme.secondary)
    }
}

/// The value chart of a holdings view. It is handed finished data and runs no vault query: the
/// host decides which series it shows (account, selected item, a future depot valuation history) and
/// in which state. Missing valuations are never filled in: a single value is a sentence, not a line.
/// The chart reads only with real values; the big total in the header is not touched by hovering.
struct AssetValueChart: View {
    enum Content {
        case loading
        /// Not supported (yet): says so plainly instead of drawing anything.
        case unavailable(String)
        case failed(String, retry: (() -> Void)?)
        /// The value history of securities depots: a step line in the depots' currency, with the
        /// coverage of the sum (partial sums are dashed and say so).
        /// `from` / `to`: the shown period (ISO, nil = open); the axis spans it.
        case depot([DepotValuePoint], from: String?, to: String?)
    }

    /// iPhone: the chart grows with Dynamic Type, so the two axis labels never touch.
    @ScaledMetric(relativeTo: .caption2) private var scaledHeight: CGFloat = 44
    let content: Content
    var currency = "EUR"
    /// Small print under the chart (e.g. that a change in value is no return).
    var footnote: LocalizedStringKey?
    /// What a sum leaves out (depots in another currency), shown like a partial sum.
    var excludedNote: String?

    var body: some View {
        switch content {
        case .loading:
            ProgressView().controlSize(.small).frame(maxWidth: .infinity)
        case .unavailable(let text):
            note(text)
        case .failed(let message, let retry):
            VStack(alignment: .leading, spacing: 4) {
                Label("Verlauf konnte nicht geladen werden", systemImage: "exclamationmark.triangle")
                    .appFont(.caption, weight: .semibold).foregroundStyle(Theme.warning)
                Text(message).appFont(.caption2).foregroundStyle(Theme.tertiary).lineLimit(2)
                if let retry { Button("Erneut versuchen", action: retry).buttonStyle(.borderless).appFont(.caption) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .depot(let points, let from, let to):
            depotChart(points, from: from, to: to)
        }
    }

    @ViewBuilder
    private func depotChart(_ points: [DepotValuePoint], from: String?, to: String?) -> some View {
        if points.isEmpty {
            note(String(localized: "Keine Bewertungen im gewählten Zeitraum"))
        } else if points.count == 1, let only = points.first {
            Text("Nur eine Bewertung: \(DateText.medium(only.date)) · \(Money.format(only.valueCents, currency: currency))")
                .sensitive(.amounts)
                .appFont(.caption).foregroundStyle(Theme.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            let period = from.flatMap { DateText.parseISODay($0) }.map { start in
                start...(to.flatMap { DateText.parseISODay($0) } ?? Date())
            }
            #if os(iOS)
            DepotValueChart(points: points, currency: currency, period: period).frame(height: scaledHeight * 1.6).sensitive(.amounts)
            #else
            DepotValueChart(points: points, currency: currency, period: period).frame(height: 84).sensitive(.amounts)
            #endif
            if let start = DepotValueHistory.startNote(points, from: from) {
                Text(start).appFont(.caption2).foregroundStyle(Theme.secondary)
            }
            if let coverage = DepotValueHistory.coverageNote(points) {
                Label(coverage, systemImage: "chart.line.uptrend.xyaxis")
                    .appFont(.caption2, weight: .semibold).foregroundStyle(Theme.warning)
            }
            if let excludedNote {
                Label(excludedNote, systemImage: "dollarsign.circle")
                    .appFont(.caption2, weight: .semibold).foregroundStyle(Theme.warning)
            }
            if let footnote {
                Text(footnote).appFont(.caption2).foregroundStyle(Theme.tertiary)
            }
        }
    }

    private func note(_ text: String) -> some View {
        Text(text).appFont(.caption).foregroundStyle(Theme.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
