import SwiftUI
import GiroCrabCore

/// "Einstellungen …" of an account (sidebar context menu), design page `docs/KONTO_EINSTELLUNGEN_UI.md`.
///
/// One sheet, built like the former "Konto bearbeiten" dialog, that replaces the menu items
/// "Konto bearbeiten …", "Umbenennen …", "Symbol & Farbe …", "Kontoart ändern …", "Kontoinhaber …"
/// and "Startsaldo …". Changes are applied with Sichern; only Symbol & Farbe applies at once,
/// like its popover always did. With "Das Konto wurde aufgelöst" on, "Konto löschen …" deletes the account
/// after a confirmation (`AccountDeletionPrompt`, shared with the sidebar menu).
struct AccountSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let accountID: Int64

    @State private var name = ""
    @State private var owner = ""
    @State private var kind: AccountKind = .checking
    @State private var note = ""
    @State private var openingText = ""
    @State private var archived = false
    @State private var login = ""
    @State private var advanziaSettings = AdvanziaConnectionSettingsState()
    @State private var showIcons = false
    @State private var confirmDelete = false
    /// The account when the sheet opened: what a sync changes meanwhile is no edit here (#980).
    @State private var original: Account?
    private var account: Account? { model.account(for: accountID) }
    private var advanziaConnection: BankConnection? {
        guard let a = account, a.connector == .advanzia, let id = a.connectionId else { return nil }
        return model.connections.first { $0.id == id }
    }
    /// The Startsaldo only exists for accounts whose balance is computed from the bookings.
    private var showsOpeningBalance: Bool {
        guard let a = account else { return false }
        return a.balanceDerived && !a.isArchived
    }
    private var openingCents: Int64? { Money.parseCents(openingText) }
    private var openingValid: Bool {
        openingText.trimmingCharacters(in: .whitespaces).isEmpty || openingCents != nil
    }
    private var canSave: Bool {
        !name.trimmingCharacters(in: .whitespaces).isEmpty
            && openingValid
            && !(advanziaConnection != nil && (advanziaSettings.busy || !advanziaSettings.loaded))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Kontoeinstellungen").appFont(.title3, weight: .bold)
            Form {
                if let account {
                    LabeledContent("Kontoname") {
                        HStack(spacing: 8) {
                            Button { showIcons = true } label: {
                                HStack(spacing: 2) {
                                    AccountSignet(account: account, size: 20, color: model.accountColor(account.id), onSelection: false)
                                    Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold)).foregroundStyle(.secondary)
                                }
                            }
                            .buttonStyle(.bordered)
                            .help("Symbol & Farbe …")
                            .accessibilityLabel(Text("Symbol & Farbe …"))
                            .capturablePopover(isPresented: $showIcons, arrowEdge: .bottom) {
                                IconColorPopover(target: .account(accountID)).environment(model)
                            }
                            TextField("Kontoname", text: $name).labelsHidden().sensitive()
                        }
                    }
                    TextField("Kontoinhaber", text: $owner).sensitive()
                    // an offline account is always cash (#679)
                    if !model.isOffline(accountID) {
                        Picker("Kontoart", selection: $kind) {
                            ForEach(AccountKind.importChoices, id: \.self) { k in Text(k.label).tag(k) }
                        }
                    }
                    if showsOpeningBalance {
                        LabeledContent("Startsaldo") {
                            HStack(spacing: 6) {
                                TextField("Startsaldo", text: $openingText, prompt: Text("z. B. \(Money.plain(1_250_00))"))
                                    .labelsHidden()
                                    .multilineTextAlignment(.trailing)
                                    #if os(iOS)
                                    .keyboardType(.numbersAndPunctuation)
                                    #endif
                                    .sensitive(.amounts)
                                Text(verbatim: account.currency).foregroundStyle(Theme.secondary)
                            }
                        }
                        Button("Stichtag und Kalibrierung …", action: openCalibration)
                            .buttonStyle(.borderless)
                            .foregroundStyle(Theme.accent)
                            .help("Startsaldo ab einem Stichtag oder passend zu einem Kontostand eintragen")
                    }
                    readOnlyRow("Kontonummer", account.accountNumber)
                    readOnlyRow("IBAN", account.iban)
                    readOnlyRow("Bank", account.bankName)
                    if advanziaConnection != nil {
                        TextField("Advanzia-Benutzername", text: $login).autocorrectionDisabled().sensitive()
                        AdvanziaConnectionFields(state: advanziaSettings)
                    }
                    LabeledContent("Konto-Notiz") {
                        VStack(alignment: .leading, spacing: 4) {
                            TextEditor(text: $note)
                                .appFont(.body)
                                .frame(minHeight: 80)
                                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.tertiary.opacity(0.5)))
                                .sensitive()
                            Text("Erscheint unter dem Kontonamen in der Kontoansicht.")
                                .appFont(.caption).foregroundStyle(Theme.tertiary)
                        }
                        // the editor has no text baseline: put the label next to its top edge
                        .alignmentGuide(.firstTextBaseline) { $0[.top] + 16 }
                    }
                    Toggle("Das Konto wurde aufgelöst (archivieren)", isOn: $archived)
                }
            }
            HStack {
                // bottom left, level with Abbrechen/Sichern: it takes no row of the form, so the sheet keeps its height
                // (a row that comes and goes made macOS draw a black bar over the top of the sheet)
                if AccountDeletionPrompt.isOffered(archivedToggle: archived) {
                    // the red goes on the label: on the button itself the bordered style on macOS ignores it
                    Button(role: .destructive) { confirmDelete = true } label: {
                        Label("Konto löschen …", systemImage: "trash")
                            .appFont(.body)
                            .foregroundStyle(.red)
                    }
                }
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Sichern", action: saveAndFinish)
                    .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                    .disabled(!canSave)
            }
        }
        .padding(22)
        .macFrame(width: advanziaConnection == nil ? 440 : 560)
        .task {
            if let c = advanziaConnection {
                login = c.loginName
                await advanziaSettings.load(connectionID: c.id, model: model)
            }
        }
        .onAppear(perform: load)
        .confirmationDialog(
            account.map(AccountDeletionPrompt.title) ?? Text(verbatim: ""),
            isPresented: $confirmDelete,
            titleVisibility: .visible,
            presenting: account
        ) { a in
            AccountDeletionPrompt.confirmButton {
                model.deleteAccount(a.id, archiveFirst: true)
                dismiss()
            }
            Button("Abbrechen", role: .cancel) {}
        } message: { a in
            AccountDeletionPrompt.message(a)
        }
    }

    private func load() {
        guard original == nil, let a = account else { return }
        original = a
        name = a.name
        owner = a.owner
        kind = a.kind
        note = a.note
        archived = a.isArchived
        openingText = a.openingBalanceCents.map(Money.plain) ?? ""
    }

    /// Shown only when the account has a value; these come from the bank or the import.
    @ViewBuilder
    private func readOnlyRow(_ label: LocalizedStringKey, _ value: String) -> some View {
        if !value.isEmpty {
            LabeledContent(label) {
                Text(verbatim: value).textSelection(.enabled).sensitive()
            }        }
    }

    /// The Stichtag sheet is presented by the root view, so this sheet makes room first.
    private func openCalibration() {
        guard let a = account else { return }
        dismiss()
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(300))
            model.openOpeningBalance(a)
        }
    }

    // MARK: - Save

    private func saveAndFinish() {
        if let c = advanziaConnection {
            Task {
                guard await advanziaSettings.save(connection: c, name: c.displayName, login: login, model: model) != nil else { return }
                save()
            }
        } else {
            save()
        }
    }

    private func save() {
        guard let a = account else { dismiss(); return }
        let o = original ?? a
        model.saveAccountEdits(
            o,
            name: name.trimmingCharacters(in: .whitespaces),
            owner: owner.trimmingCharacters(in: .whitespaces),
            kind: kind,
            note: note.trimmingCharacters(in: .whitespacesAndNewlines))
        if showsOpeningBalance {
            let text = openingText.trimmingCharacters(in: .whitespaces)
            do {
                if text.isEmpty {
                    if o.openingBalanceCents != nil { try model.setOpeningBalance(a.id, cents: nil, date: "") }
                } else if let cents = openingCents, cents != o.openingBalanceCents {
                    try model.setOpeningBalance(a.id, cents: cents, date: a.openingBalanceDate)
                }
            } catch {
                model.errorMessage = describe(error)
                return
            }
        }
        // last: archiving reloads everything
        if archived != o.isArchived { model.setAccountArchived(a, archived) }
        dismiss()
    }
}
