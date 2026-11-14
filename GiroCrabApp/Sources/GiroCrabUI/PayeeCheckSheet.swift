import SwiftUI
import GiroCrabCore

/// Result of the recipient name check (Verification of Payee) before a
/// transfer: the user decides whether to send it anyway.
struct PayeeCheckSheet: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let pending = model.payeeCheck {
            let c = pending.check
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    Image(systemName: symbol(c.result))
                        .font(.system(size: 34))
                        .foregroundStyle(tint(c.result))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(title(c.result)).appFont(.title3, weight: .bold)
                        Text("Empfängerüberprüfung · \(pending.connection.displayName)")
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                }

                VStack(alignment: .leading, spacing: 8) {
                    row("Eingegeben", c.enteredName)
                    if let bank = c.bankName { row("Kontoinhaber", bank) }
                    row("IBAN", formatIBAN(c.iban))
                }
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 12).fill(Theme.card))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.border))

                Text(explanation(c))
                    .fixedSize(horizontal: false, vertical: true)
                if let notice = c.notice {
                    Label(notice, systemImage: "building.columns")
                        .appFont(.callout).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                // HIG: Cancel on the left, the safe default (Return) on the far
                // right; sending despite a mismatch is destructive and never
                // the default.
                HStack {
                    Button("Abbrechen", role: .cancel) { model.cancelPayee() }
                        .keyboardShortcut(.cancelAction)
                    Spacer()
                    if pending.busy { ProgressView().controlSize(.small) }
                    if c.result == .match {
                        Button("Weiter zur Freigabe") { model.confirmPayee() }
                            .keyboardShortcut(.defaultAction)
                            .disabled(pending.busy)
                    } else {
                        Button("Trotzdem senden", role: .destructive) { model.confirmPayee() }
                            .disabled(pending.busy)
                        if c.result == .closeMatch, c.bankName != nil {
                            Button("Namen übernehmen") { model.cancelPayee(useBankName: true) }
                                .keyboardShortcut(.defaultAction)
                                .disabled(pending.busy)
                        }
                    }
                }
            }
            .padding(24)
            .desktopFrame(width: 480)
        }
    }

    private func row(_ k: LocalizedStringKey, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(k).foregroundStyle(Theme.secondary).frame(width: 110, alignment: .leading)
            Text(v).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    private func symbol(_ r: PayeeMatch) -> String {
        switch r {
        case .match: return "checkmark.seal.fill"
        case .closeMatch: return "exclamationmark.triangle.fill"
        case .noMatch: return "xmark.octagon.fill"
        case .notPossible: return "questionmark.circle.fill"
        }
    }

    private func tint(_ r: PayeeMatch) -> Color {
        switch r {
        case .match: return .green
        case .closeMatch: return .orange
        case .noMatch: return .red
        case .notPossible: return .secondary
        }
    }

    private func title(_ r: PayeeMatch) -> String {
        switch r {
        case .match: return String(localized: "Name stimmt überein")
        case .closeMatch: return String(localized: "Name stimmt nur teilweise überein")
        case .noMatch: return String(localized: "Name stimmt nicht überein")
        case .notPossible: return String(localized: "Name konnte nicht geprüft werden")
        }
    }

    private func explanation(_ c: PayeeCheck) -> String {
        switch c.result {
        case .match:
            return String(localized: "Die Bank verlangt trotzdem eine ausdrückliche Bestätigung, bevor sie die Freigabe anfordert.")
        case .closeMatch:
            return String(localized: "Der eingegebene Name weicht vom Namen des Kontoinhabers ab. Übernehmen Sie den Namen, wenn das der richtige Empfänger ist.")
        case .noMatch:
            return String(localized: "Das Konto gehört nicht zum eingegebenen Namen. Wenn Sie trotzdem senden, kann das Geld beim falschen Empfänger landen und lässt sich womöglich nicht zurückholen.")
        case .notPossible:
            let reason = c.reason.map { " (\($0))" } ?? ""
            return String(localized: "Die Empfängerbank konnte den Namen nicht prüfen\(reason). Senden Sie nur, wenn Sie sich bei IBAN und Empfänger sicher sind.")
        }
    }
}
