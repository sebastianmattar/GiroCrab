import SwiftUI
import GiroCrabCore

// Offline accounts and manual bookings (GitHub #679, #81): cash and other accounts without a bank
// connection. The user types the bookings; the balance is the starting balance plus the bookings
// (the core keeps it: `offline.rs`). Everything else (categories, rules, totals, charts) treats
// these accounts like any other.

/// Which accounts take manual bookings, and the sheets on screen. Refreshed with the accounts.
@Observable final class OfflineStore {
    var accountIDs: Set<Int64> = []
    /// "Neue Buchung …" / "Buchung bearbeiten …".
    var bookingRequest: ManualBookingRequest?
    /// "Neues Offline-Konto …".
    var showNewAccount = false
    /// "Saldo korrigieren …".
    var correctionRequest: BalanceCorrectionRequest?
    /// Waiting for the answer of "Buchung löschen …".
    var deleting: Booking?
}

struct ManualBookingRequest: Identifiable {
    let id = UUID()
    let accountId: Int64
    /// `nil` = a new booking.
    var booking: Booking?
}

struct BalanceCorrectionRequest: Identifiable {
    let id = UUID()
    let accountId: Int64
}

extension AppModel {
    func reloadOfflineAccounts() {
        guard let vault else {
            offlineStore.accountIDs = []
            return
        }
        do {
            offlineStore.accountIDs = Set(try vault.listOfflineAccountIds())
        } catch {
            errorMessage = describe(error)
        }
    }

    func isOffline(_ accountId: Int64) -> Bool {
        offlineStore.accountIDs.contains(accountId)
    }

    /// Offline accounts that take new bookings (not archived), in sidebar order.
    var offlineAccounts: [Account] {
        allAccounts.filter { isOffline($0.id) && !$0.isArchived }
    }

    /// The selected sidebar account, if it takes manual bookings.
    var selectedOfflineAccount: Account? {
        guard case .account(let id) = sidebarSelection, isOffline(id),
              let account = account(for: id), !account.isArchived else { return nil }
        return account
    }

    var canAddManualBooking: Bool { vault != nil && !offlineAccounts.isEmpty }

    /// Whether `booking` can be edited or deleted by hand.
    func isManuallyEditable(_ booking: Booking) -> Bool {
        isOffline(booking.accountId) && account(for: booking.accountId)?.isArchived == false
    }

    /// "Neue Buchung …": in the selected offline account, else the first one.
    func openManualBooking(accountId: Int64? = nil) {
        guard let target = accountId ?? selectedOfflineAccount?.id ?? offlineAccounts.first?.id else { return }
        offlineStore.bookingRequest = ManualBookingRequest(accountId: target, booking: nil)
    }

    func editManualBooking(_ booking: Booking) {
        guard isManuallyEditable(booking) else { return }
        offlineStore.bookingRequest = ManualBookingRequest(accountId: booking.accountId, booking: booking)
    }

    func openBalanceCorrection(_ accountId: Int64) {
        offlineStore.correctionRequest = BalanceCorrectionRequest(accountId: accountId)
    }

    /// Everything a changed booking or balance shows up in.
    private func reloadAfterManualChange() {
        reloadAccounts(); reloadBookings(); reloadHistory(); reloadCategories()
    }

    @discardableResult
    func saveManualBooking(accountId: Int64, bookingId: Int64?, setup: ManualBookingSetup) throws -> Int64 {
        guard let vault else { throw CoreError.Database(message: "vault locked") }
        let id = try vault.saveManualBooking(accountId: accountId, bookingId: bookingId, setup: setup)
        reloadAfterManualChange()
        return id
    }

    func deleteManualBooking(_ booking: Booking) {
        guard let vault else { return }
        do {
            try vault.deleteManualBooking(bookingId: booking.id)
        } catch {
            errorMessage = describe(error)
            return
        }
        if selectedBookingID == booking.id { selectedBookingID = nil }
        toast = String(localized: "Buchung gelöscht")
        reloadAfterManualChange()
    }

    /// "Neues Offline-Konto …": the account belongs in the own vault, not the demo (#487).
    func createOfflineAccount(name: String, note: String, openingCents: Int64,
                              kind: AccountKind = .cash, currency: String = OfflineAccountOptions.defaultCurrency) {
        runInOwnVault { [self] in
            guard let vault else { return }
            do {
                let id = try vault.createOfflineAccount(name: name, kind: kind, currency: currency, openingCents: openingCents)
                offlineStore.accountIDs.insert(id)
                // one reload for the account and its note
                if !note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    runCore { try vault.setAccountNote(accountId: id, note: note) }
                }
                reloadAccounts()
                sidebarSelection = .account(id)
            } catch {
                errorMessage = describe(error)
            }
        }
    }

    /// One-time copy of `booking` into `account` with the amount reversed and no category.
    func mirror(_ booking: Booking, to account: Account) {
        guard let vault else { return }
        do {
            _ = try vault.mirrorBookingToOfflineAccount(bookingId: booking.id, accountId: account.id)
        } catch {
            errorMessage = describe(error)
            return
        }
        toast = String(localized: "Buchung in „\(account.name)“ gespiegelt")
        reloadAfterManualChange()
    }

    func correctBalance(_ accountId: Int64, to targetCents: Int64) {
        guard let vault else { return }
        do {
            if try vault.correctOfflineBalance(accountId: accountId, targetCents: targetCents) == nil {
                toast = String(localized: "Der Saldo stimmt bereits.")
                return
            }
        } catch {
            errorMessage = describe(error)
            return
        }
        toast = String(localized: "Korrekturbuchung angelegt")
        reloadAfterManualChange()
    }
}

// MARK: menus

/// Edit, delete and mirror entries of a booking's context menu.
struct OfflineBookingMenu: View {
    let model: AppModel
    let booking: Booking

    var body: some View {
        if model.isManuallyEditable(booking) {
            Button("Buchung bearbeiten …", systemImage: "square.and.pencil") { model.editManualBooking(booking) }
            Button("Buchung löschen …", systemImage: "trash", role: .destructive) { model.offlineStore.deleting = booking }
        }
        let targets = model.offlineAccounts.filter { $0.id != booking.accountId }
        if !targets.isEmpty {
            Menu("In Offline-Konto spiegeln", systemImage: "arrow.left.arrow.right") {
                ForEach(targets, id: \.id) { account in
                    Button { model.mirror(booking, to: account) } label: { Text(account.name).sensitive() }
                }
            }
        }
    }
}

/// "Neue Buchung …" and "Saldo korrigieren …" in an account's context menu.
struct OfflineAccountMenuItems: View {
    let model: AppModel
    let account: Account

    var body: some View {
        if model.isOffline(account.id), !account.isArchived {
            Button("Neue Buchung …", systemImage: "plus") { model.openManualBooking(accountId: account.id) }
            Button("Saldo korrigieren …", systemImage: "equal.circle") { model.openBalanceCorrection(account.id) }
        }
    }
}

extension View {
    /// The sheets and the delete question of the offline accounts, driven by `model.offlineStore`.
    func offlineAccountSheets(_ model: AppModel) -> some View {
        modifier(OfflineAccountSheetsModifier(model: model))
    }
}

private struct OfflineAccountSheetsModifier: ViewModifier {
    @Bindable var model: AppModel

    func body(content: Content) -> some View {
        content
            .capturableSheet(item: Binding(get: { model.offlineStore.bookingRequest }, set: { model.offlineStore.bookingRequest = $0 })) { request in
                ManualBookingSheet(request: request).environment(model)
            }
            .capturableSheet(isPresented: Binding(get: { model.offlineStore.showNewAccount }, set: { model.offlineStore.showNewAccount = $0 })) {
                OfflineAccountSheet().environment(model)
            }
            .capturableSheet(item: Binding(get: { model.offlineStore.correctionRequest }, set: { model.offlineStore.correctionRequest = $0 })) { request in
                BalanceCorrectionSheet(request: request).environment(model)
            }
            .alert("Buchung löschen?", isPresented: Binding(
                get: { model.offlineStore.deleting != nil },
                set: { if !$0 { model.offlineStore.deleting = nil } }
            )) {
                Button("Löschen", role: .destructive) {
                    if let b = model.offlineStore.deleting { model.deleteManualBooking(b) }
                    model.offlineStore.deleting = nil
                }
                Button("Abbrechen", role: .cancel) { model.offlineStore.deleting = nil }
            } message: {
                Text("Die Buchung wird aus dem Konto entfernt und der Saldo neu berechnet. Das lässt sich nicht widerrufen.")
            }
    }
}

// MARK: sheets

/// "Neue Buchung …" / "Buchung bearbeiten …": date, amount, name, purpose, category and note.
struct ManualBookingSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let request: ManualBookingRequest

    private enum Flow: Hashable { case out, `in` }

    @State private var accountId: Int64 = 0
    @State private var date = Date()
    @State private var flow: Flow = .out
    @State private var amountText = ""
    @State private var counterparty = ""
    @State private var purpose = ""
    @State private var categoryId: Int64?
    @State private var note = ""
    @State private var saveError: String?
    @State private var loaded = false

    private var isNew: Bool { request.booking == nil }

    /// The typed amount with its sign, `nil` while it is not a usable number.
    private var amountCents: Int64? {
        guard let cents = Money.parseCents(amountText), cents != 0 else { return nil }
        let magnitude = abs(cents)
        return flow == .out ? -magnitude : magnitude
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(isNew ? "Neue Buchung" : "Buchung bearbeiten").appFont(.title3, weight: .bold)
            Form {
                if model.offlineAccounts.count > 1 && isNew {
                    Picker("Konto", selection: $accountId) {
                        ForEach(model.offlineAccounts, id: \.id) { a in Text(a.name).tag(a.id) }
                    }
                    .sensitive()
                }
                DatePicker("Datum", selection: $date, displayedComponents: .date)
                Picker("Art", selection: $flow) {
                    Text("Ausgabe").tag(Flow.out)
                    Text("Einnahme").tag(Flow.in)
                }
                .pickerStyle(.segmented)
                TextField("Betrag", text: $amountText)
                    .multilineTextAlignment(.trailing)
                    .sensitive(.amounts)
                TextField("Name oder Empfänger", text: $counterparty).sensitive()
                TextField("Verwendungszweck", text: $purpose).sensitive()
                Picker("Kategorie", selection: $categoryId) {
                    Text("Ohne Kategorie").tag(Int64?.none)
                    ForEach(model.categories, id: \.id) { c in Text(c.path).tag(Int64?.some(c.id)) }
                }
                .sensitive(.labels)
                TextField("Notiz", text: $note, axis: .vertical)
                    .lineLimit(1...4)
                    .sensitive()
            }
            if let saveError {
                Text(saveError).appFont(.caption).foregroundStyle(Theme.outflow)
            }
            HStack {
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Sichern") { save() }
                    .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                    .disabled(amountCents == nil)
            }
        }
        .padding(22)
        .macFrame(width: 460)
        .onAppear(perform: load)
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        accountId = request.accountId
        guard let b = request.booking else { return }
        date = DateText.date(b.bookingDate) ?? Date()
        flow = b.amountCents < 0 ? .out : .in
        amountText = Money.plain(abs(b.amountCents))
        counterparty = b.counterparty
        purpose = b.purpose
        categoryId = b.categoryId
        note = b.note
    }

    private func save() {
        guard let cents = amountCents else { return }
        let setup = ManualBookingSetup(
            bookingDate: DateText.iso(date), amountCents: cents,
            counterparty: counterparty, purpose: purpose, categoryId: categoryId, note: note)
        do {
            try model.saveManualBooking(accountId: accountId, bookingId: request.booking?.id, setup: setup)
            dismiss()
        } catch {
            saveError = describe(error)
        }
    }
}

/// What "Neues Offline-Konto …" offers (#1049): the kinds that can be kept by hand and the currencies.
enum OfflineAccountOptions {
    static let kinds: [AccountKind] = [.cash, .savings]
    static let defaultCurrency = "EUR"
    /// ISO 4217 codes offered when creating an account; the first is the default.
    static let currencies = ["EUR", "USD", "CHF", "GBP", "SEK", "DKK", "NOK", "PLN", "CZK"]
    /// Picker tag of "Andere …": the code is typed into its own field.
    static let otherTag = "__other"

    /// The code the sheet creates the account with: the picked one, or the typed ISO 4217 code
    /// (three letters, upper-cased). `nil` while the typed code is not valid.
    static func currency(picked: String, typed: String) -> String? {
        guard picked == otherTag else { return picked }
        let code = typed.trimmingCharacters(in: .whitespaces).uppercased()
        return code.count == 3 && code.allSatisfy({ $0.isASCII && $0.isLetter }) ? code : nil
    }

    /// "Sparkonto · USD": the line under the account name in the header.
    static func subtitle(_ account: Account) -> String {
        "\(title(account.kind)) · \(account.currency.isEmpty ? defaultCurrency : account.currency.uppercased())"
    }

    static func title(_ kind: AccountKind) -> String {
        kind == .savings ? String(localized: "Sparkonto") : String(localized: "Bargeld")
    }

    /// "USD – US Dollar" in the app language.
    static func title(currency code: String) -> String {
        guard let name = Locale.current.localizedString(forCurrencyCode: code) else { return code }
        return "\(code) – \(name)"
    }
}

/// "Neues Offline-Konto …": name, type, currency and starting balance.
struct OfflineAccountSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var name = ""
    @State private var balanceText = ""
    @State private var note = ""
    @State private var kind: AccountKind = .cash
    @State private var currency = OfflineAccountOptions.defaultCurrency
    @State private var customCurrency = ""

    /// An empty field is a starting balance of 0; a typo is not.
    private var openingCents: Int64? {
        let t = balanceText.trimmingCharacters(in: .whitespaces)
        return t.isEmpty ? 0 : Money.parseCents(t)
    }

    private var chosenCurrency: String? {
        OfflineAccountOptions.currency(picked: currency, typed: customCurrency)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Neues Offline-Konto").appFont(.title3, weight: .bold)
            Text("Ein Konto ohne Bankanbindung. Die Buchungen tragen Sie selbst ein.")
                .appFont(.callout).foregroundStyle(Theme.secondary)
            Form {
                TextField("Kontoname", text: $name, prompt: Text(OfflineAccountOptions.title(kind))).sensitive()
                Picker("Kontoart", selection: $kind) {
                    ForEach(OfflineAccountOptions.kinds, id: \.self) { kind in
                        Text(OfflineAccountOptions.title(kind)).tag(kind)
                    }
                }
                Picker("Währung", selection: $currency) {
                    ForEach(OfflineAccountOptions.currencies, id: \.self) { code in
                        Text(OfflineAccountOptions.title(currency: code)).tag(code)
                    }
                    Text("Andere …").tag(OfflineAccountOptions.otherTag)
                }
                if currency == OfflineAccountOptions.otherTag {
                    TextField("ISO-Code", text: $customCurrency, prompt: Text("JPY"))
                }
                TextField("Startsaldo", text: $balanceText, prompt: Text("0,00"))
                    .multilineTextAlignment(.trailing)
                    .sensitive(.amounts)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Konto-Notiz")
                    TextEditor(text: $note)
                        .appFont(.body)
                        .frame(minHeight: 60)
                        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.tertiary.opacity(0.5)))
                        .sensitive()
                }
            }
            HStack {
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Anlegen") {
                    if let cents = openingCents, let code = chosenCurrency {
                        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
                        model.createOfflineAccount(name: trimmed.isEmpty ? OfflineAccountOptions.title(kind) : name,
                                                   note: note, openingCents: cents, kind: kind, currency: code)
                        dismiss()
                    }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                .disabled(openingCents == nil || chosenCurrency == nil)
            }
        }
        .padding(22)
        .macFrame(width: 440)
    }
}

/// "Saldo korrigieren …": books the difference to the real balance as a correction booking.
struct BalanceCorrectionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let request: BalanceCorrectionRequest

    @State private var targetText = ""
    @State private var loaded = false

    private var account: Account? { model.account(for: request.accountId) }
    private var targetCents: Int64? { Money.parseCents(targetText) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Saldo korrigieren").appFont(.title3, weight: .bold)
            if let account {
                Text("Aktueller Saldo: \(Money.format(account.balanceCents, currency: account.currency))")
                    .appFont(.callout).foregroundStyle(Theme.secondary)
                    .sensitive(.amounts)
            }
            Form {
                TextField("Tatsächlicher Saldo", text: $targetText)
                    .multilineTextAlignment(.trailing)
                    .sensitive(.amounts)
            }
            Text("Der Unterschied wird als Buchung „Saldokorrektur“ mit heutigem Datum angelegt. Den Startsaldo ändern Sie über „Einstellungen …“ im Kontextmenü des Kontos.")
                .appFont(.caption).foregroundStyle(Theme.tertiary)
            HStack {
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Korrigieren") {
                    if let cents = targetCents {
                        model.correctBalance(request.accountId, to: cents)
                        dismiss()
                    }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                .disabled(targetCents == nil)
            }
        }
        .padding(22)
        .macFrame(width: 440)
        .onAppear {
            guard !loaded, let account else { return }
            loaded = true
            targetText = Money.plain(account.balanceCents)
        }
    }
}
