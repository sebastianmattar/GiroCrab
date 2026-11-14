import SwiftUI
import GiroCrabCore

// Startsaldo of an account (GiroCrab/GiroCrab#212), as in MoneyMoney: for
// accounts whose balance GiroCrab computes from the bookings, the balance is
// the Startsaldo plus every booking from its date on. "Kalibrieren" fills in
// the Startsaldo that yields a given balance, e.g. the one on the last
// statement. Nothing is stored before "Speichern"; saving and removing are
// undoable.

/// The account whose Startsaldo sheet is open.
struct OpeningBalanceRequest: Identifiable {
    let id = UUID()
    let accountId: Int64
}

extension AppModel {
    func openOpeningBalance(_ account: Account) {
        openingBalanceRequest = OpeningBalanceRequest(accountId: account.id)
    }

    /// Sets (or clears with `nil`) the Startsaldo; `date` "" = all bookings.
    /// Undoable via Bearbeiten › Widerrufen.
    func setOpeningBalance(_ accountId: Int64, cents: Int64?, date: String) throws {
        guard let vault else { throw CoreError.Database(message: "vault locked") }
        let old = account(for: accountId)
        try vault.setAccountOpeningBalance(accountId: accountId, cents: cents, date: date)
        if let old, let um = windowUndoManager {
            let (oldCents, oldDate) = (old.openingBalanceCents, old.openingBalanceDate)
            um.registerUndo(withTarget: self) { m in
                try? m.setOpeningBalance(accountId, cents: oldCents, date: oldDate)
            }
            um.setActionName(cents == nil ? String(localized: "Startsaldo entfernen") : String(localized: "Startsaldo ändern"))
        }
        reloadAccounts(); reloadHistory()
    }

    /// What a Startsaldo at `date` would count; nothing is stored.
    /// Runs off the main thread (sums every booking of the account); `nil` when the
    /// vault was locked or swapped meanwhile – keep what is shown.
    func previewOpeningBalance(_ accountId: Int64, date: String) async throws -> OpeningBalancePreview? {
        try await vaultOffMain { try $0.previewAccountOpeningBalance(accountId: accountId, date: date) }
    }
}

/// "Startsaldo …" in an account's context menu, only where it applies.
struct OpeningBalanceMenuItem: View {
    let model: AppModel
    let account: Account

    var body: some View {
        if account.balanceDerived, !account.isArchived {
            Button("Startsaldo …", systemImage: "clock.arrow.circlepath") { model.openOpeningBalance(account) }
        }
    }
}

extension View {
    /// The Startsaldo sheet, driven by `model.openingBalanceRequest`.
    func openingBalanceSheet(_ model: AppModel) -> some View {
        modifier(OpeningBalanceSheetModifier(model: model))
    }
}

private struct OpeningBalanceSheetModifier: ViewModifier {
    @Bindable var model: AppModel

    func body(content: Content) -> some View {
        content.sheet(item: $model.openingBalanceRequest) { req in
            OpeningBalanceSheet(request: req).environment(model)
        }
    }
}

struct OpeningBalanceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let request: OpeningBalanceRequest

    @State private var amountText = ""
    @State private var hasDate = false
    @State private var date = Date()
    @State private var targetText = ""
    @State private var preview: OpeningBalancePreview?
    @State private var error: String?
    @State private var loaded = false
    @FocusState private var focus: Field?
    private enum Field { case amount, target }

    private var account: Account? { model.account(for: request.accountId) }
    private var currency: String { account?.currency ?? "EUR" }
    private var amount: Int64? { Money.parseCents(amountText) }
    private var target: Int64? { Money.parseCents(targetText) }
    private var dateString: String { hasDate ? DateText.iso(date) : "" }
    private var canCalibrate: Bool { target != nil && preview != nil }

    var body: some View {
        #if os(macOS)
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Startsaldo")
                    .appFont(.headline)
                    .accessibilityAddTraits(.isHeader)
                if let account {
                    Text(account.name)
                        .appFont(.callout)
                        .foregroundStyle(Theme.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding([.horizontal, .top], 20)
            form
            Divider()
            HStack {
                // destructive action on the leading edge, away from Cancel / Save
                removeButton
                statusLine
                Spacer()
                cancelButton
                saveButton
            }
            .padding(16)
        }
        .desktopFrame(minWidth: 500, idealWidth: 540, minHeight: 520)
        .onAppear(perform: load)
        .task(id: dateString) { await refreshPreview() }
        #else
        NavigationStack {
            form
                .navigationTitle("Startsaldo")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { cancelButton }
                    ToolbarItem(placement: .confirmationAction) { saveButton }
                    ToolbarItem(placement: .bottomBar) { removeButton }
                }
                .safeAreaInset(edge: .bottom) {
                    statusLine.padding(.horizontal).padding(.vertical, 8)
                }
        }
        .onAppear(perform: load)
        .task(id: dateString) { await refreshPreview() }
        #endif
    }

    private var form: some View {
        Form {
            Section {
                amountField("Startsaldo", text: $amountText, example: 1_250_00)
                    .focused($focus, equals: .amount)
                Toggle("Ab Stichtag", isOn: $hasDate)
                if hasDate {
                    DatePicker("Stichtag", selection: $date, in: ...Date(), displayedComponents: .date)
                }
            } header: {
                Text("Startsaldo").appSectionHeader()
            } footer: {
                FormFooter("Der Kontostand zu Beginn des Stichtags, z. B. aus dem ältesten vorhandenen Kontoauszug. Umsätze ab dem Stichtag werden addiert, ältere nicht. Ohne Stichtag zählen alle Umsätze.")
            }
            Section {
                amountField("Kontostand", text: $targetText, example: 2_064_83)
                    .focused($focus, equals: .target)
                    .onSubmit(calibrate)
                Button("Kalibrieren", systemImage: "scope", action: calibrate)
                    .disabled(!canCalibrate)
            } header: {
                Text("Kalibrieren").appSectionHeader()
            } footer: {
                FormFooter("Trägt den Startsaldo ein, mit dem der Kontostand diesem Betrag entspricht, z. B. dem Saldo auf dem letzten Kontoauszug. Gespeichert wird erst mit „Speichern“.")
            }
            Section { previewRows } header: { Text("Vorschau").appSectionHeader() }
        }
        .formStyle(.grouped)
    }

    /// Amount input; on iPhone / iPad a keyboard with digits, separators and minus.
    private func amountField(_ label: LocalizedStringKey, text: Binding<String>, example: Int64) -> some View {
        TextField(label, text: text, prompt: Text("z. B. \(Money.plain(example))"))
            #if os(iOS)
            .keyboardType(.numbersAndPunctuation)
            #endif
    }

    @ViewBuilder
    private var previewRows: some View {
        if let account {
            LabeledContent("Kontostand bisher", value: Money.format(account.balanceCents, currency: currency))
        }
        if let preview, let amount {
            LabeledContent("Kontostand danach", value: Money.format(amount + preview.sumCents, currency: currency))
                .appFont(.headline)
            LabeledContent("Umsätze ab Stichtag", value: "\(preview.counted) · \(Money.format(preview.sumCents, currency: currency))")
            if preview.older > 0 {
                LabeledContent("Ältere Umsätze (im Startsaldo enthalten)", value: "\(preview.older)")
            }
        } else {
            Text("Vorschau erscheint, sobald ein Startsaldo eingetragen ist.").foregroundStyle(Theme.secondary)
        }
    }

    @ViewBuilder
    private var statusLine: some View {
        if let error {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .appFont(.callout)
                .foregroundStyle(Theme.warning)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isStaticText)
        }
    }

    private var cancelButton: some View {
        Button("Abbrechen", role: .cancel) { dismiss() }
            .keyboardShortcut(.cancelAction)
    }

    private var saveButton: some View {
        Button("Speichern", action: save)
            // Return in the Kontostand field calibrates instead of saving
            .keyboardShortcut(focus == .target ? nil : .defaultAction)
            .disabled(amount == nil)
    }

    @ViewBuilder
    private var removeButton: some View {
        if account?.openingBalanceCents != nil {
            Button("Entfernen", role: .destructive, action: remove)
        }
    }

    private func load() {
        guard !loaded, let account else { return }
        loaded = true
        amountText = account.openingBalanceCents.map(Money.plain) ?? ""
        if let d = DateText.date(account.openingBalanceDate) {
            hasDate = true
            date = d
        }
        targetText = Money.plain(account.balanceCents)
        focus = .amount
    }

    private func refreshPreview() async {
        do {
            guard let p = try await model.previewOpeningBalance(request.accountId, date: dateString),
                  !Task.isCancelled else { return }
            preview = p
            error = nil
        } catch is CancellationError {
        } catch {
            preview = nil
            self.error = describe(error)
        }
    }

    /// Fills in the Startsaldo for the entered balance; stores nothing.
    private func calibrate() {
        guard let target, let preview else { return }
        amountText = Money.plain(target - preview.sumCents)
        focus = .amount
    }

    private func save() {
        guard let amount else { return }
        do {
            try model.setOpeningBalance(request.accountId, cents: amount, date: dateString)
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }

    private func remove() {
        do {
            try model.setOpeningBalance(request.accountId, cents: nil, date: "")
            dismiss()
        } catch {
            self.error = describe(error)
        }
    }
}
