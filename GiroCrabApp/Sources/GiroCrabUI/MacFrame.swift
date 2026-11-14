import SwiftUI

extension View {
    /// Split-view column widths are desktop-only; on iOS the system sizes the columns.
    @ViewBuilder
    func macColumnWidth(min: CGFloat? = nil, ideal: CGFloat, max: CGFloat? = nil) -> some View {
        #if os(macOS)
        navigationSplitViewColumnWidth(min: min, ideal: ideal, max: max)
        #else
        self
        #endif
    }

    /// Fixed/min sizes are for macOS windows & sheets only; on iOS the sheet fills the screen.
    /// They grow with the text size (#343), like the text in them.
    @ViewBuilder
    func macFrame(width: CGFloat? = nil, minWidth: CGFloat? = nil, idealWidth: CGFloat? = nil,
                  minHeight: CGFloat? = nil, height: CGFloat? = nil) -> some View {
        #if os(macOS)
        modifier(MacFrame(width: width, minWidth: minWidth, idealWidth: idealWidth, minHeight: minHeight, height: height))
        #else
        self
        #endif
    }
}

#if os(macOS)
private struct MacFrame: ViewModifier {
    @Environment(\.textScale) private var scale
    let width, minWidth, idealWidth, minHeight, height: CGFloat?

    func body(content: Content) -> some View {
        content
            .frame(minWidth: minWidth.map { $0 * scale }, idealWidth: idealWidth.map { $0 * scale },
                   minHeight: minHeight.map { $0 * scale })
            .frame(width: width.map { $0 * scale }, height: height.map { $0 * scale })
    }
}
#endif
