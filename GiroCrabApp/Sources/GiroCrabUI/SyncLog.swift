import SwiftUI
import UniformTypeIdentifiers
import GiroCrabCore

extension SyncLogEntry: Identifiable {}

extension AppModel {
    /// What started the sync that is running for `connectionID`.
    func syncLogSource(for connectionID: Int64?) -> SyncLogSource {
        if statementsAccountID != nil { return .statements }
        if let connectionID, quietSyncConnectionID == connectionID { return .automatic }
        return .manual
    }

    /// Appends a finished sync to the Abrufprotokoll (#238). The core scrubs the
    /// text of logins, PINs, TANs, tokens and long numbers before it is stored.
    func logSync(connectionID: Int64?, source: SyncLogSource, outcome: SyncLogOutcome, message: String = "", imported: Int = 0) {
        guard let vault else { return }
        do {
            try vault.logSync(connectionId: connectionID, source: source, outcome: outcome,
                              message: message, imported: UInt32(max(0, imported)))
            syncLogVersion &+= 1
        } catch {
            // the log must never get in the way of a sync
        }
    }

    /// CSV of the rows the log window shows.
    static func syncLogCSV(_ entries: [SyncLogEntry]) -> String {
        func field(_ s: String) -> String { "\"" + s.replacingOccurrences(of: "\"", with: "\"\"") + "\"" }
        let df = ISO8601DateFormatter()
        df.timeZone = .current
        var lines = ["Zeit;Konto;Auslöser;Ergebnis;Neue Buchungen;Meldung"]
        for e in entries {
            let date = df.string(from: Date(timeIntervalSince1970: TimeInterval(e.timestampMs) / 1000))
            lines.append([date, field(e.connectionName), e.source.title, e.outcome.title, String(e.imported), field(e.message)]
                .joined(separator: ";"))
        }
        return lines.joined(separator: "\n") + "\n"
    }
}

extension SyncLogOutcome {
    var title: String {
        switch self {
        case .ok: String(localized: "Erfolgreich")
        case .error: String(localized: "Fehler")
        case .cancelled: String(localized: "Abgebrochen")
        }
    }

    /// Shape and colour together: the result is not told by colour alone (HIG, Color and contrast).
    var symbol: String {
        switch self {
        case .ok: "checkmark.circle.fill"
        case .error: "exclamationmark.triangle.fill"
        case .cancelled: "minus.circle.fill"
        }
    }

    var tint: Color {
        switch self {
        case .ok: .green
        case .error: .red
        case .cancelled: .secondary
        }
    }
}

extension SyncLogSource {
    var title: String {
        switch self {
        case .manual: String(localized: "Manuell")
        case .automatic: String(localized: "Automatisch")
        case .statements: String(localized: "Kontoauszüge")
        }
    }
}

#if os(macOS)
/// "Abruf-Protokoll" window (Fenster ▸ Abruf-Protokoll, GitHub #238): when a fetch ran,
/// for which connection, how it ended and with which error text. Searchable,
/// filterable by connection and errors, exportable as CSV. Rows older than seven days
/// and anything beyond 10 MB are dropped by the core; no logins, PINs or TANs are kept.
struct SyncLogView: View {
    @Environment(AppModel.self) private var model
    @State private var entries: [SyncLogEntry] = []
    @State private var search = ""
    @State private var onlyErrors = false
    @State private var connectionID: Int64?
    @State private var sortOrder = [KeyPathComparator(\SyncLogEntry.timestampMs, order: .reverse)]
    @State private var selection: Set<SyncLogEntry.ID> = []
    @State private var confirmClear = false
    @State private var export = CSVFileDocument(data: Data())
    @State private var exporting = false
    @State private var reloadToken = 0

    private var filter: SyncLogFilter {
        SyncLogFilter(connectionId: connectionID, onlyErrors: onlyErrors,
                      search: search.isEmpty ? nil : search, limit: 0)
    }

    private var sorted: [SyncLogEntry] { entries.sorted(using: sortOrder) }

    var body: some View {
        Table(sorted, selection: $selection, sortOrder: $sortOrder) {
            TableColumn("Zeit", value: \.timestampMs) { e in
                Text(Date(timeIntervalSince1970: TimeInterval(e.timestampMs) / 1000)
                    .formatted(date: .abbreviated, time: .standard))
                    .monospacedDigit()
            }
            .width(min: 150, ideal: 170)
            TableColumn("Konto", value: \.connectionName) { e in
                Text(e.connectionName.isEmpty ? "–" : e.connectionName)
            }
            .width(min: 100, ideal: 150)
            TableColumn("Ergebnis") { e in
                Label(e.outcome.title, systemImage: e.outcome.symbol)
                    .foregroundStyle(e.outcome.tint)
            }
            .width(min: 110, ideal: 120)
            TableColumn("Meldung") { e in
                Text(message(of: e)).lineLimit(3).textSelection(.enabled)
            }
        }
        .contextMenu(forSelectionType: SyncLogEntry.ID.self) { ids in
            Button("Meldung kopieren", systemImage: "doc.on.doc") { copy(ids) }
                .disabled(ids.isEmpty)
        }
        .overlay {
            if entries.isEmpty {
                if search.isEmpty && !onlyErrors && connectionID == nil {
                    ContentUnavailableView("Noch keine Abrufe", systemImage: "list.bullet.clipboard",
                                           description: Text("Hier erscheinen Abrufe der letzten sieben Tage – mit Ergebnis und Fehlertext."))
                } else {
                    ContentUnavailableView.search
                }
            }
        }
        .searchable(text: $search, prompt: "Protokoll durchsuchen")
        .toolbar {
            ToolbarItem {
                Picker("Konto", selection: $connectionID) {
                    Text("Alle Konten").tag(Int64?.none)
                    Divider()
                    ForEach(model.connections) { c in Text(c.displayName).tag(Int64?.some(c.id)) }
                }
                .pickerStyle(.menu)
            }
            ToolbarItem {
                Toggle(isOn: $onlyErrors) { Label("Nur Fehler", systemImage: "exclamationmark.triangle") }
                    .help("Nur fehlgeschlagene Abrufe zeigen")
            }
            ToolbarItem {
                Button { startExport() } label: { Label("Exportieren …", systemImage: "square.and.arrow.up") }
                    .help("Angezeigte Einträge als CSV exportieren")
                    .disabled(entries.isEmpty)
            }
            ToolbarItem {
                Button { confirmClear = true } label: { Label("Protokoll leeren …", systemImage: "trash") }
                    .help("Alle Einträge löschen")
                    .disabled(entries.isEmpty && search.isEmpty && !onlyErrors && connectionID == nil)
            }
        }
        .confirmationDialog("Protokoll leeren?", isPresented: $confirmClear) {
            Button("Protokoll leeren", role: .destructive) {
                Task {
                    _ = try? await model.requireVaultOffMain { try $0.clearSyncLog() }
                    reload()
                }
            }
        } message: {
            Text("Alle Einträge des Abruf-Protokolls werden gelöscht. Das lässt sich nicht widerrufen.")
        }
        .fileExporter(isPresented: $exporting, document: export, contentType: .commaSeparatedText,
                      defaultFilename: "GiroCrab-Abrufprotokoll") { _ in }
        .navigationTitle("Abruf-Protokoll")
        .frame(minWidth: 680, minHeight: 320)
        .task(id: "\(model.syncLogVersion)/\(reloadToken)") { await load() }
        .onChange(of: search) { _, _ in reload() }
        .onChange(of: onlyErrors) { _, _ in reload() }
        .onChange(of: connectionID) { _, _ in reload() }
    }

    private func message(of e: SyncLogEntry) -> String {
        if !e.message.isEmpty { return e.message }
        if e.outcome == .ok {
            return e.imported == 0 ? String(localized: "Keine neuen Buchungen")
                : String(localized: "\(Int(e.imported)) neue Buchungen")
        }
        return ""
    }

    /// Restarts `load()` via `.task(id:)`, which cancels a load still in flight.
    private func reload() { reloadToken &+= 1 }

    private func load() async {
        let filter = filter
        let loaded = (try? await model.vaultOffMain { try $0.listSyncLog(filter: filter) }) ?? nil
        guard !Task.isCancelled else { return }
        entries = loaded ?? []
        selection = selection.filter { id in entries.contains { $0.id == id } }
    }

    private func copy(_ ids: Set<SyncLogEntry.ID>) {
        let text = sorted.filter { ids.contains($0.id) }.map(message(of:)).joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func startExport() {
        export = CSVFileDocument(data: Data(AppModel.syncLogCSV(sorted).utf8))
        exporting = true
    }
}
#endif
