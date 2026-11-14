import SwiftUI
import GiroCrabCore

/// Repayment progress of a loan plus the editable "Ursprünglicher Kreditbetrag"
/// (HKSAL only reports the remaining balance, so live loans need it entered once).
struct LoanProgressCard: View {
    @Environment(AppModel.self) private var model
    let account: Account
    var compact = false
    @State private var text = ""
    @State private var editing = false

    private var manual: ManualLoan? { model.manualLoan(account.id) }
    private var initial: Int64? { account.initialBalanceCents.map { abs($0) }.flatMap { $0 > 0 ? $0 : nil } }
    private var repaid: Double? {
        guard let p = initial else { return nil }
        return max(0, min(1, 1 - Double(abs(account.balanceCents)) / Double(p)))
    }

    private func commit() {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            model.setAccountInitialBalance(account.id, cents: nil)
        } else if let c = Money.parseCents(trimmed) {
            model.setAccountInitialBalance(account.id, cents: c)
        }
        editing = false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                if !compact { Label("Tilgung", systemImage: "house").appFont(.headline) }
                if let repaid {
                    Text("\(repaid.formatted(.percent.precision(.fractionLength(0)))) getilgt").appFont(.callout, weight: .semibold).foregroundStyle(Theme.loan)
                } else {
                    Text("Ursprünglicher Kreditbetrag fehlt").appFont(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
                if manual != nil {
                    // manual loans: amount, Stand and rate live in the loan editor
                    if compact {
                        Button("Stand neu setzen …") { model.openLoanEditor(.anchor, accountId: account.id) }
                            .buttonStyle(.borderless).appFont(.caption)
                    }
                } else if editing {
                    TextField("z. B. \(Money.plain(28_000_000))", text: $text)
                        .textFieldStyle(.roundedBorder).frame(width: 130).onSubmit(commit)
                    Button("Abbrechen") { editing = false }
                        .controlSize(.small).keyboardShortcut(.cancelAction)
                    Button("Speichern", action: commit)
                        .controlSize(.small).keyboardShortcut(.defaultAction)
                } else {
                    Button(initial == nil ? "Betrag eingeben" : "Ursprünglich: \(Money.format(initial!))") {
                        text = initial.map { Money.plain($0) } ?? ""
                        editing = true
                    }
                    .buttonStyle(.borderless).appFont(.caption)
                    .help("Ursprünglicher Kreditbetrag bearbeiten")
                }
            }
            if let repaid {
                ProgressView(value: repaid).progressViewStyle(.linear).tint(Theme.loan)
                    .accessibilityLabel("Tilgung")
                    .accessibilityValue("\(repaid.formatted(.percent.precision(.fractionLength(0)))) getilgt")
            }
            if let manual, !compact {
                ManualLoanDetails(loan: manual)
            }
        }
        .modifier(CardIf(enabled: !compact))
    }
}

private struct CardIf: ViewModifier {
    let enabled: Bool
    func body(content: Content) -> some View {
        if enabled { content.card() } else { content }
    }
}
