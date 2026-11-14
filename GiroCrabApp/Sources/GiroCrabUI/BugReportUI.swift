import SwiftUI
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// Hilfe › Problem melden … (#301). macOS: its own window, so the main window
/// stays free to be captured; iOS: a sheet (`BugReportSheet`).
struct BugReportView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL
    @Environment(\.undoManager) private var undoManager
    /// Closes the window (macOS) or the sheet (iOS).
    @Environment(\.dismiss) private var dismiss
    #if os(iOS)
    @State private var markingUp = false
    #endif

    /// Off only in the demo vault (fictitious data).
    @State private var autoRedact = true
    @State private var hideLabels = false
    @State private var includeLog = true
    @State private var base: CGImage?
    @State private var capturing = false
    @State private var captureRun = 0
    /// Extra black boxes, unit rectangles with the origin top left.
    @State private var masks: [CGRect] = []
    /// Owner of the mask undo actions (Bearbeiten › Widerrufen, ⌘Z).
    @State private var undoTarget = MaskUndoTarget()
    /// Bumped on every undo registration: `canUndo` is not observable.
    @State private var undoTick = 0
    /// Header and sanitized log lines; nil while they are being prepared
    /// (sending waits for them). Built off the main thread, once per change.
    @State private var prepared: PreparedDiagnostics?
    @State private var exporting = false
    @State private var export = PNGFileDocument(data: Data())

    private var redaction: ReportRedaction {
        var r: ReportRedaction = autoRedact || !model.isDemo ? .personal : []
        if hideLabels { r.insert(.labels) }
        return r
    }

    private var captureKey: CaptureKey { CaptureKey(redaction: redaction, run: captureRun) }

    /// Reload when the log option changes, or the vault opens, locks or is
    /// swapped (demo ↔ own). A new screenshot leaves the diagnostics alone.
    private var diagnosticsKey: DiagnosticsKey {
        DiagnosticsKey(includeLog: includeLog, unlocked: model.isUnlocked, demo: model.isDemo)
    }

    private var canUndo: Bool {
        _ = undoTick
        return undoManager?.canUndo ?? false
    }

    private var canSend: Bool { !capturing && prepared != nil }

    var body: some View {
        Form {
            Section {
                #if os(macOS)
                Text("Ziehe mit der Maus Rechtecke über Stellen, die zusätzlich schwarz übermalt werden sollen.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                #endif
                screenshot
                maskControls
            } header: {
                Text("Screenshot")
            } footer: {
                Text("Eine neue Aufnahme verwirft die Markierungen, weil sich der Inhalt verschoben haben kann.")
            }

            Section {
                if model.isDemo {
                    Toggle("Persönliche Daten schwärzen", isOn: $autoRedact)
                } else {
                    Label("Persönliche Daten werden geschwärzt", systemImage: "eye.slash")
                }
                Toggle("Kategorien ausblenden", isOn: $hideLabels)
            } footer: {
                Text(model.isDemo
                     ? "Beträge, IBANs, Namen, Kontonamen, Verwendungszwecke und Belege werden abgedeckt; „Kategorien ausblenden“ deckt auch Kategorien, Gruppen und Wertpapiernamen ab. Im Demo-Tresor sind alle Daten erfunden, deshalb lässt sich die Schwärzung hier abschalten."
                     : "Beträge, IBANs, Namen, Kontonamen, Verwendungszwecke und Belege werden abgedeckt; „Kategorien ausblenden“ deckt auch Kategorien, Gruppen und Wertpapiernamen ab. Prüfe das Bild trotzdem, bevor du es teilst.")
            }

            Section {
                Toggle("Abruf-Protokoll anhängen", isOn: $includeLog)
                if let prepared {
                    Text(verbatim: BugReport.render(prepared.info, log: prepared.log))
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if prepared.logUnavailable {
                        Label("Das Abruf-Protokoll wird nicht angehängt: Es konnte nicht vollständig geladen und bereinigt werden.",
                              systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else {
                    ProgressView("Diagnose wird bereinigt …")
                }
            } header: {
                Text("Diagnose")
            } footer: {
                Text("Beim Öffnen des Issues liegt der geschwärzte Screenshot in der Zwischenablage. Es wird nichts automatisch gesendet: Der Browser zeigt ein vorausgefülltes Issue, das du vor dem Absenden noch bearbeiten kannst.")
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Problem melden")
        #if os(macOS)
        // HIG: the primary action stays in view, bottom trailing, bound to Return.
        .safeAreaInset(edge: .bottom, spacing: 0) { actionBar }
        .background(NonRestorableWindow())
        .frame(minWidth: 620, minHeight: 640)
        #else
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Abbrechen") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Issue öffnen") { send() }
                    .disabled(!canSend)
            }
            ToolbarItem(placement: .bottomBar) {
                Button("Als PNG sichern …", systemImage: "square.and.arrow.down") { startExport() }
                    .disabled(base == nil || capturing)
            }
        }
        .fullScreenCover(isPresented: $markingUp) {
            if let base {
                MaskEditorScreen(image: base, masks: masks, canUndo: canUndo,
                                 onAdd: addMask, onUndo: { undoManager?.undo(); undoTick &+= 1 })
            }
        }
        #endif
        .task(id: captureKey) { await capture() }
        .task(id: diagnosticsKey) { await prepareDiagnostics() }
        .fileExporter(isPresented: $exporting, document: export, contentType: .png,
                      defaultFilename: String(localized: "GiroCrab-Fehlerbericht")) { _ in }
    }

    #if os(macOS)
    private var actionBar: some View {
        VStack(spacing: 0) {
            Divider()
            HStack {
                Button("Als PNG sichern …") { startExport() }
                    .disabled(base == nil || capturing)
                Spacer()
                Button { send() } label: {
                    Label("Issue auf GitHub öffnen", systemImage: "arrow.up.right.square")
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canSend)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        // opaque: the form scrolls underneath
        .background(Color(nsColor: .windowBackgroundColor))
    }
    #endif

    @ViewBuilder
    private var maskControls: some View {
        HStack {
            #if os(macOS)
            Button("Rückgängig", systemImage: "arrow.uturn.backward") { undoManager?.undo(); undoTick &+= 1 }
                .disabled(!canUndo)
            Button("Alle Markierungen entfernen") {
                replaceMasks(with: [], actionName: String(localized: "Markierungen entfernen"))
            }
            .disabled(masks.isEmpty)
            #else
            Button("Bereiche schwärzen …", systemImage: "rectangle.dashed") { markingUp = true }
                .disabled(base == nil || capturing)
            #endif
            Spacer()
            Button("Neu aufnehmen", systemImage: "camera") { captureRun += 1 }
                .disabled(capturing)
        }
        .buttonStyle(.borderless)
    }

    @ViewBuilder
    private var screenshot: some View {
        if let base {
            #if os(macOS)
            MaskCanvas(image: base, masks: masks, onAdd: addMask)
                .opacity(capturing ? 0.5 : 1)
            #else
            // Drawing inside the scrolling form would fight the scroll gesture:
            // marking up happens full screen ("Bereiche schwärzen …").
            MaskCanvas(image: base, masks: masks, interactive: false)
                .opacity(capturing ? 0.5 : 1)
                .onTapGesture { if !capturing { markingUp = true } }
            #endif
        } else {
            HStack {
                Spacer()
                if capturing {
                    ProgressView("Screenshot wird erstellt …")
                } else {
                    Text("Kein Screenshot: Das Hauptfenster ist geschlossen, minimiert, gesperrt oder wurde nicht rechtzeitig geschwärzt. „Neu aufnehmen“ versucht es erneut; sonst wird das Issue ohne Screenshot erstellt.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .frame(minHeight: 160)
        }
    }

    // MARK: Masks (undoable)

    private func addMask(_ rect: CGRect) {
        replaceMasks(with: masks + [rect], actionName: String(localized: "Markierung hinzufügen"))
    }

    /// Every change goes through here, so Bearbeiten › Widerrufen / ⌘Z and the
    /// button undo it (and redo it: the undo registers the opposite change).
    private func replaceMasks(with new: [CGRect], actionName: String) {
        let old = masks
        masks = new
        undoManager?.registerUndo(withTarget: undoTarget) { _ in
            replaceMasks(with: old, actionName: actionName)
        }
        undoManager?.setActionName(actionName)
        undoTick &+= 1
    }

    private func capture() async {
        capturing = true
        let result = await BugReport.captureMainWindow(model: model, redaction: redaction)
        guard !Task.isCancelled else { return }
        switch result {
        case .superseded:
            // the newer capture updates the picture; not stuck "busy" if none follows
            capturing = false
            return
        case .noWindow: base = nil
        case .image(let image): base = image
        }
        // Unit rectangles drawn on the old picture may now cover something
        // else (the list scrolled, the window was resized): start over.
        masks.removeAll()
        undoManager?.removeAllActions(withTarget: undoTarget)
        undoTick &+= 1
        capturing = false
    }

    /// Log entries and payee names load together, or the log is left out:
    /// without the names a log line could carry a payee into the issue.
    private func prepareDiagnostics() async {
        prepared = nil
        var entries: [SyncLogEntry] = []
        var counterparties: [String] = []
        var logUnavailable = false
        if includeLog {
            let filter = SyncLogFilter(connectionId: nil, onlyErrors: false, search: nil, limit: BugReport.logEntries)
            let limit = BugReport.maxCounterpartyTerms
            if let (log, names) = try? await model.vaultOffMain({ v in (try v.listSyncLog(filter: filter), try v.counterpartyNames(limit: limit)) }) {
                entries = log
                counterparties = names
            } else {
                logUnavailable = model.isUnlocked
            }
        }
        guard !Task.isCancelled else { return }
        let catalog = bankCatalog()
        let info = BugReport.info(model: model, log: entries, catalog: catalog)
        let terms = BugReport.knownTerms(model: model, counterparties: counterparties, catalog: catalog)
        let lines = info.logLines
        // thousands of terms: compiling and matching them can take well over a frame
        let log = await Task.detached(priority: .userInitiated) { BugReport.sanitizedLog(lines, knownTerms: terms) }.value
        guard !Task.isCancelled else { return }
        prepared = PreparedDiagnostics(info: info, log: log, logUnavailable: logUnavailable)
    }

    private func finalPNG() -> Data? {
        base.flatMap { BugReport.compose($0, masks: masks) }.flatMap(BugReport.png)
    }

    private func startExport() {
        guard let png = finalPNG() else { return }
        export = PNGFileDocument(data: png)
        exporting = true
    }

    private func send() {
        guard canSend, let prepared else { return }
        let png = finalPNG()
        if let png { BugReport.copyToPasteboard(png) }
        let url = BugReport.issueURL(info: prepared.info, sanitizedLog: prepared.log, withScreenshot: png != nil)
        openURL(url)
        // Done: the picture is on the clipboard and the issue body says how to
        // paste it, so the report closes like any finished task.
        dismiss()
    }
}

private final class MaskUndoTarget {}

private struct CaptureKey: Hashable {
    var redaction: ReportRedaction
    var run: Int
}

private struct DiagnosticsKey: Hashable {
    var includeLog: Bool
    var unlocked: Bool
    var demo: Bool
}

private struct PreparedDiagnostics {
    var info: BugReport.Info
    /// Already sanitized.
    var log: [String]
    /// The log was asked for but could not be loaded with its payee names.
    var logUnavailable: Bool
}

/// The captured picture with the user's black boxes; dragging adds a box.
private struct MaskCanvas: View {
    let image: CGImage
    let masks: [CGRect]
    var interactive = true
    var onAdd: (CGRect) -> Void = { _ in }
    @State private var draft: CGRect?
    #if os(macOS)
    @State private var crosshair = false
    #endif

    var body: some View {
        let aspect = CGFloat(image.width) / CGFloat(max(image.height, 1))
        Image(decorative: image, scale: 1)
            .resizable()
            .aspectRatio(aspect, contentMode: .fit)
            .overlay {
                GeometryReader { geo in
                    let size = geo.size
                    let shown = masks + (draft.map { [$0] } ?? [])
                    ZStack(alignment: .topLeading) {
                        Color.clear
                        ForEach(shown.indices, id: \.self) { i in
                            let m = shown[i]
                            Rectangle()
                                .fill(.black)
                                .frame(width: m.width * size.width, height: m.height * size.height)
                                .position(x: m.midX * size.width, y: m.midY * size.height)
                        }
                    }
                    .contentShape(Rectangle())
                    .gesture(
                        DragGesture(minimumDistance: 2)
                            .onChanged { v in draft = unitRect(v.startLocation, v.location, in: size) }
                            .onEnded { v in
                                let r = unitRect(v.startLocation, v.location, in: size)
                                draft = nil
                                if r.width > 0.004, r.height > 0.004 { onAdd(r) }
                            }
                    )
                    .allowsHitTesting(interactive)
                }
            }
            .overlay { RoundedRectangle(cornerRadius: 4).strokeBorder(.secondary.opacity(0.4)) }
            #if os(macOS)
            // HIG: show that the picture takes a drag (crosshair over it).
            .onHover { inside in setCrosshair(interactive && inside) }
            .onDisappear { setCrosshair(false) }
            #endif
            .accessibilityElement()
            .accessibilityLabel("Vorschau des Screenshots")
            .accessibilityValue(masks.isEmpty
                ? String(localized: "Persönliche Daten sind abgedeckt.")
                : String(localized: "Persönliche Daten sind abgedeckt, dazu \(masks.count) eigene Markierungen."))
    }

    #if os(macOS)
    private func setCrosshair(_ on: Bool) {
        guard on != crosshair else { return }
        crosshair = on
        if on { NSCursor.crosshair.push() } else { NSCursor.pop() }
    }
    #endif

    private func unitRect(_ a: CGPoint, _ b: CGPoint, in size: CGSize) -> CGRect {
        guard size.width > 0, size.height > 0 else { return .zero }
        func clamp(_ v: CGFloat) -> CGFloat { min(max(v, 0), 1) }
        let x0 = clamp(min(a.x, b.x) / size.width), x1 = clamp(max(a.x, b.x) / size.width)
        let y0 = clamp(min(a.y, b.y) / size.height), y1 = clamp(max(a.y, b.y) / size.height)
        return CGRect(x: x0, y: y0, width: x1 - x0, height: y1 - y0)
    }
}

/// Registers the window that shows `MainSplitView` for the bug report
/// (`BugReport.mainWindow()`), and with it the redaction its views were last
/// built with – per window, so a capture checks the window it captures. Gone
/// with the view (window closed, vault locked): nothing left to capture.
#if os(macOS)
struct MainWindowTracker: NSViewRepresentable {
    @Environment(\.reportRedaction) private var redaction

    func makeNSView(context: Context) -> TrackingView { TrackingView() }

    func updateNSView(_ view: TrackingView, context: Context) {
        view.renderedRedaction = redaction
    }

    final class TrackingView: NSView {
        var renderedRedaction: ReportRedaction = []

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window != nil { BugReport.track(self) } else { BugReport.untrack(self) }
        }
    }
}
#else
struct MainWindowTracker: UIViewRepresentable {
    @Environment(\.reportRedaction) private var redaction

    func makeUIView(context: Context) -> TrackingView { TrackingView() }

    func updateUIView(_ view: TrackingView, context: Context) {
        view.renderedRedaction = redaction
    }

    final class TrackingView: UIView {
        var renderedRedaction: ReportRedaction = []

        override init(frame: CGRect) {
            super.init(frame: frame)
            isUserInteractionEnabled = false
        }

        required init?(coder: NSCoder) { fatalError("not used") }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window != nil { BugReport.track(self) } else { BugReport.untrack(self) }
        }
    }
}
#endif

#if os(macOS)

/// The report is a one-off task: its window is not reopened at the next launch.
private struct NonRestorableWindow: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView { WindowView() }
    func updateNSView(_ nsView: NSView, context: Context) {}

    private final class WindowView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            window?.isRestorable = false
        }
    }
}
#endif

#if os(iOS)
/// Full-screen markup: dragging in the scrolling form would fight scrolling.
private struct MaskEditorScreen: View {
    let image: CGImage
    let masks: [CGRect]
    let canUndo: Bool
    let onAdd: (CGRect) -> Void
    let onUndo: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                Text("Ziehe Rechtecke über Stellen, die zusätzlich schwarz übermalt werden sollen.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                MaskCanvas(image: image, masks: masks, onAdd: onAdd)
                Spacer(minLength: 0)
            }
            .padding()
            .navigationTitle("Bereiche schwärzen")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Fertig") { dismiss() }
                }
                ToolbarItem(placement: .bottomBar) {
                    Button("Rückgängig", systemImage: "arrow.uturn.backward", action: onUndo)
                        .disabled(!canUndo)
                }
            }
        }
    }
}

/// iOS/iPadOS: Einstellungen › Problem melden.
struct BugReportSheet: View {
    var body: some View {
        NavigationStack { BugReportView() }
    }
}
#endif

/// Opens the bug report: a window on macOS, a sheet on iOS.
struct BugReportButton: View {
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    #else
    @Environment(AppModel.self) private var model
    @State private var showing = false
    #endif

    var body: some View {
        #if os(macOS)
        Button { openWindow(id: "bug-report") } label: {
            Label("Problem melden …", systemImage: "ladybug")
        }
        #else
        // iOS: a row that opens a sheet carries no ellipsis.
        Button { showing = true } label: {
            Label("Problem melden", systemImage: "ladybug")
        }
        .sheet(isPresented: $showing) { BugReportSheet().environment(model) }
        #endif
    }
}
