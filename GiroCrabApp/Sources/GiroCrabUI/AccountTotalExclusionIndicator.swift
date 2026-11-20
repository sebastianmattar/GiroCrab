import SwiftUI

/// Explanatory only: opening this help never changes inclusion in totals.
struct AccountTotalExclusionIndicator: View {
    let help: String
    @State private var showingHelp = false

    var body: some View {
        #if os(macOS)
        Button {
            showingHelp = true
        } label: {
            indicator.padding(2).contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .capturablePopover(isPresented: $showingHelp, arrowEdge: .trailing) {
            Text(verbatim: help)
                .appFont(.callout)
                .foregroundStyle(.primary)
                .lineLimit(nil) // do not inherit the sidebar row's one-line limit
                .frame(width: 280, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(12)
        }
        .accessibilityLabel("Nicht in der Gesamtsumme")
        .accessibilityHint(Text(verbatim: help))
        #else
        indicator.help(help)
        #endif
    }

    private var indicator: some View {
        Image(systemName: "eye.slash")
            .appFont(.caption2)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Nicht in der Gesamtsumme")
            .accessibilityHint(Text(verbatim: help))
    }
}
