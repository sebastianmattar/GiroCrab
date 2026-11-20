import SwiftUI
import GiroCrabCore

/// "Empfänger verwalten" (#978), from the transfer form: every IBAN the outgoing bookings went
/// to, the most recently used first, and whether the form still suggests it. Hiding changes no
/// booking. Layout and rules: `docs/EMPFAENGER_UI.md`.
struct KnownRecipientsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var recipients: [KnownRecipient] = []
    @State private var loaded = false
    @State private var search = ""
    /// Why the last read failed; the rows of the last good read stay.
    @State private var loadError: String?
    /// Counts the loads: only the newest one may show its result (search typed on, a toggle).
    @State private var loads = 0

    private var hidden: [KnownRecipient] { recipients.filter(\.hidden) }
    private var suggested: [KnownRecipient] { recipients.filter { !$0.hidden } }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Empfänger").appFont(.headline)
                Spacer()
                Button("Fertig") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(16)
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                Text("Aus Ihren bisherigen Zahlungen, je IBAN eine Zeile. Ausgeblendete IBANs schlägt „Neue Überweisung“ nicht mehr vor; die Buchungen bleiben.")
                    .appFont(.callout)
                    .foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                TextField("Name oder IBAN suchen", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .sensitive() // typed names / IBANs
                if let loadError {
                    VStack(alignment: .leading, spacing: 4) {
                        Label("Empfänger konnten nicht geladen werden", systemImage: "exclamationmark.triangle")
                            .appFont(.caption, weight: .semibold).foregroundStyle(Theme.warning)
                        Text(loadError).appFont(.caption2).foregroundStyle(Theme.tertiary).lineLimit(2)
                        Button("Erneut versuchen") { Task { await reload() } }
                            .buttonStyle(.borderless).appFont(.caption)
                    }
                }
            }
            .padding(16)
            if loaded && recipients.isEmpty && loadError == nil {
                Group {
                    if search.trimmingCharacters(in: .whitespaces).isEmpty {
                        ContentUnavailableView {
                            Label("Noch keine Empfänger", systemImage: "person.2")
                        } description: {
                            Text("Hier erscheinen die Empfänger Ihrer Zahlungen, sobald Buchungen mit IBAN abgerufen sind.")
                        }
                    } else {
                        ContentUnavailableView.search
                    }
                }
                .frame(maxHeight: .infinity)
            } else {
                list
            }
        }
        .macFrame(minWidth: 520, idealWidth: 560, minHeight: 480)
        // a vault swap (demo ↔ own) keeps the transfer form open: read the new vault's rows
        .task(id: "\(model.vaultRevision)|\(search)") { await reload() }
        #if os(macOS)
        .onExitCommand { dismiss() }
        #endif
    }

    private var list: some View {
        List {
            if !hidden.isEmpty {
                Section {
                    ForEach(hidden, id: \.iban) { row($0) }
                } header: {
                    Text("Ausgeblendet (\(hidden.count))")
                }
            }
            if !suggested.isEmpty {
                Section {
                    ForEach(suggested, id: \.iban) { row($0) }
                } header: {
                    Text("Vorgeschlagen (\(suggested.count))")
                }
            }
        }
    }

    private func row(_ r: KnownRecipient) -> some View {
        HStack(spacing: 10) {
            if r.hidden {
                Image(systemName: "eye.slash")
                    .foregroundStyle(Theme.tertiary)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(r.name).appDefaultFont().lineLimit(1)
                    .foregroundStyle(r.hidden ? Theme.secondary : .primary)
                    .sensitive()
                Text(formatIBAN(r.iban)).appFont(.caption, design: .monospaced)
                    .foregroundStyle(Theme.secondary).lineLimit(1)
                    .sensitive()
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 1) {
                // spelled out like the export sheet: the plural rule of the catalog is not in every build
                if r.timesUsed == 1 { Text("1 Buchung") } else { Text("\(Int(r.timesUsed)) Buchungen") }
                Text("zuletzt \(DateText.medium(r.lastUsed))")
            }
            .appFont(.caption)
            .foregroundStyle(Theme.tertiary)
            .fixedSize()
            Button(r.hidden ? "Einblenden" : "Ausblenden") { toggle(r) }
                .buttonStyle(.borderless)
                .help(r.hidden ? "Wieder vorschlagen" : "Nicht mehr vorschlagen")
        }
        .contextMenu {
            Button(r.hidden ? "Wieder vorschlagen" : "Nicht mehr vorschlagen",
                   systemImage: r.hidden ? "eye" : "eye.slash") { toggle(r) }
        }
    }

    private func toggle(_ r: KnownRecipient) {
        model.setRecipientHidden(r.iban, !r.hidden)
        Task { await reload() }
    }

    /// A load cancelled or overtaken by a newer one (typing on, a toggle) drops its result: a
    /// cancelled one would read as "nothing found", an older one would overwrite newer rows.
    /// A failed one keeps the rows and says so (with "Erneut versuchen").
    private func reload() async {
        loads += 1
        let load = loads
        do {
            let found = try await model.knownRecipients(search)
            guard !Task.isCancelled, load == loads, let found else { return }
            recipients = found
            loadError = nil
            loaded = true
        } catch {
            guard !Task.isCancelled, load == loads else { return }
            loadError = describe(error)
        }
    }
}
