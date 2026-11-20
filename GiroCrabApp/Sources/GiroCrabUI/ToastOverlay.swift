import SwiftUI

/// Draws `model.toast` at the bottom of a window (GiroCrab/GiroCrab#1145).
///
/// Every window that shares `model` carries it: on macOS the Settings window and
/// the sync log lie in front of the main window, so a notice raised there would
/// otherwise appear behind them. Only one window draws a notice on macOS: the one
/// that was key last (kept while the app is in the background). On iOS/iPadOS
/// scenes do not stack as modal utility windows, so every visible scene draws it.
extension View {
    func toastOverlay() -> some View { modifier(ToastOverlay()) }
}

/// The windows that can draw a notice, the one that was key last at the end.
@MainActor @Observable
final class ToastHosts {
    static let shared = ToastHosts()
    private(set) var order: [UUID] = []

    /// A new window goes behind the others until it becomes key.
    func add(_ id: UUID) { if !order.contains(id) { order.insert(id, at: 0) } }
    func bringToFront(_ id: UUID) {
        guard order.last != id else { return }
        order.removeAll { $0 == id }
        order.append(id)
    }
    func remove(_ id: UUID) { order.removeAll { $0 == id } }
}

private struct ToastOverlay: ViewModifier {
    @Environment(AppModel.self) private var model
    #if os(macOS)
    @State private var id = UUID()
    @Environment(\.controlActiveState) private var activeState
    private var isHost: Bool { ToastHosts.shared.order.last == id }
    #else
    private var isHost: Bool { true }
    #endif

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .bottom) {
                if isHost, let toast = model.toast {
                    Label(toast, systemImage: "checkmark.circle.fill")
                        .padding(.horizontal, 16).padding(.vertical, 10)
                        .background(.regularMaterial, in: Capsule())
                        .shadow(color: .black.opacity(0.08), radius: 8, y: 2)
                        .padding(.bottom, 22)
                        .transition(Motion.slide(.bottom))
                        // id: a changed or extended notice gets its full time again
                        .task(id: toast) {
                            try? await Task.sleep(nanoseconds: 3_500_000_000)
                            // replaced meanwhile: the new notice's own task takes over
                            guard !Task.isCancelled else { return }
                            withAnimation { model.toast = nil }
                        }
                }
            }
            .animation(Motion.animation(.spring(duration: 0.3)), value: model.toast)
            #if os(macOS)
            .onAppear { ToastHosts.shared.add(id) }
            .onDisappear { ToastHosts.shared.remove(id) }
            .onChange(of: activeState, initial: true) { _, state in
                if state == .key { ToastHosts.shared.bringToFront(id) }
            }
            #endif
    }
}
