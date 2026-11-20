import SwiftUI
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// "Neue Überweisung": SEPA credit transfer via FinTS (HKCCS). After the
/// confirmation card the model starts the session; the bank's TAN request is
/// shown by the existing `TwoFactorSheet`.
struct TransferSheetView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.textScale) private var textScale
    @State private var step: Step = .form
    @State private var exportingGiroCode: PNGFileDocument?
    @State private var suggestions: [KnownRecipient] = []
    @FocusState private var nameFocused: Bool
    @FocusState private var amountFocused: Bool
    /// Row of the recipient list chosen with the arrow keys.
    @State private var highlighted: Int?
    /// Row to highlight once the list has changed because one was hidden (#978): the next one
    /// moves into its place. Any other change of the list (typing) clears the highlight.
    @State private var keepHighlight: Int?
    /// Name just taken from the list: typing it must not reopen the list.
    @State private var pickedName: String?
    /// "Empfänger verwalten" (#978).
    @State private var managingRecipients = false
    @State private var dropTargeted = false
    @State private var importNote: String?
    @State private var ownTargetID: Int64?
    /// Measured form and sheet heights: the sheet takes what the form needs (#879).
    @State private var formFit = FormFit()
    /// Opt-in clipboard check (#663): what the banner offers.
    @State private var clipboardOffer: ClipboardPaymentCheck.Offer?
    @AppStorage(ClipboardPaymentCheck.enabledKey) private var clipboardCheck = ClipboardPaymentCheck.enabledDefault
    /// Highest fitted height of the current step: the sheet grows, never shrinks,
    /// so Überweisung ⇄ Umbuchung does not resize it and scroll it on the way back (#881).
    @State private var heightFloor: CGFloat = 0

    private let maxPurpose = 140

    /// The form, then either the review before sending or the GiroCode.
    enum Step { case form, review, giroCode }

    /// Own accounts that can receive a transfer: any kind, but with an IBAN, not archived, not the sender.
    private var ownTargets: [Account] {
        // Tagesgeld books only to own checking accounts (#582).
        if let a = account, a.kind == .savings { return model.internalTransferTargets(from: a) }
        return model.allAccounts.filter {
            !$0.iban.isEmpty && !$0.isArchived && $0.id != model.transferDraft.accountID
        }
    }

    /// Fills name and IBAN from the chosen own account (owner, else account name).
    private func applyOwnTarget(_ id: Int64?) {
        guard let t = ownTargets.first(where: { $0.id == id }) else { return }
        model.transferDraft.name = t.owner.isEmpty ? t.name : t.owner
        model.transferDraft.iban = t.iban
        model.transferDraft.bic = ""
        suggestions = []
    }

    /// "Umbuchung": the recipient is one of the user's own accounts. Kept in the
    /// draft so the batch sheet's switch can open the form in this mode.
    private var ownTransfer: Bool { model.transferDraft.ownTransfer }

    private func setOwnTransfer(_ on: Bool) {
        model.transferDraft.ownTransfer = on
        applyOwnTransfer(on)
    }

    private func applyOwnTransfer(_ on: Bool) {
        if on {
            if !ownTargets.contains(where: { $0.id == ownTargetID }) { ownTargetID = ownTargets.first?.id }
            applyOwnTarget(ownTargetID)
        } else {
            model.transferDraft.name = ""
            model.transferDraft.iban = ""
            model.transferDraft.bic = ""
        }
    }

    /// Savings account as sender: the bank allows only an internal transfer (#582).
    private var internalOnly: Bool { account?.kind == .savings }

    private var account: Account? {
        model.transferFormAccounts.first { $0.id == model.transferDraft.accountID }
    }
    /// The account can send from GiroCrab (FinTS / bunq); otherwise the form
    /// ends with a GiroCode for the bank's app.
    private var canSend: Bool {
        model.canSendTransfer(from: account?.id)
    }
    private var giroCodePayload: String? {
        guard let cents else { return nil }
        let d = model.transferDraft
        return GiroCode.payload(name: d.name, iban: d.iban, bic: bicText, amountCents: cents, purpose: d.purpose)
    }
    private var ibanValid: Bool { validateIban(iban: model.transferDraft.iban) }
    private var bicText: String { model.transferDraft.bic.trimmingCharacters(in: .whitespaces) }
    private var bicValid: Bool { bicText.isEmpty || Self.looksLikeBIC(bicText) }
    private var cents: Int64? {
        guard let c = try? parseAmountToCents(text: model.transferDraft.amountText), c > 0 else { return nil }
        return c
    }
    private var isBunq: Bool { account?.connector == .bunq }
    private var sendsInstant: Bool {
        model.transferDraft.instant && account?.connector?.offersInstantChoice == true
    }
    private var isMagicDemoAccount: Bool {
        guard let id = account?.connectionId,
              let c = model.connections.first(where: { $0.id == id }) else { return false }
        return AppModel.isMagicDemoLogin(c.loginName)
    }
    private var pinStored: Bool {
        guard let id = account?.connectionId else { return false }
        return model.isDemo || (model.connectionStatus(id)?.hasPin ?? false)
    }
    private var canReview: Bool {
        account != nil
            && !model.transferDraft.name.trimmingCharacters(in: .whitespaces).isEmpty
            && ibanValid && bicValid && cents != nil
            && model.transferDraft.purpose.count <= maxPurpose
    }

    /// The height the form needs (#879), at most what the screen offers;
    /// `nil` (fixed 700 pt) until measured or before macOS 15.
    /// The fitted sheet shows the whole form: no scroll bars, also not for the
    /// moment the form has grown and the sheet not yet (#881).
    private var formFitsSheet: Bool {
        #if os(macOS)
        let screen = NSScreen.main?.visibleFrame.height ?? 900
        guard let h = formFit.height(min: 0, max: .infinity) else { return false }
        return h <= screen - 120
        #else
        return false
        #endif
    }

    private var fittedHeight: CGFloat? {
        #if os(macOS)
        let screen = NSScreen.main?.visibleFrame.height ?? 900
        return formFit.height(min: 420, max: screen - 120).map { max($0, heightFloor) }
        #else
        return nil
        #endif
    }

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            header
            // Tagesgeld: only Umbuchung, said in the form; no switch.
            if step == .form && !internalOnly && (model.canBatchTransfer || !ownTargets.isEmpty || ownTransfer) {
                TransferKindPicker(batch: false, offersOwn: !ownTargets.isEmpty || ownTransfer).padding(.bottom, 10)
            }
            if step == .form, let offer = clipboardOffer {
                ClipboardPaymentBanner(offer: offer, apply: { acceptClipboard($0) }, ignore: { clipboardOffer = nil })
                    .padding(.horizontal, 20).padding(.bottom, 10)
            }
            Divider()
            switch step {
            case .form:
                form(draft: $model.transferDraft)
                demoHint($model.transferDraft)
            case .review: sendConfirmation
            case .giroCode: giroCodeView
            }
            if model.transferError != nil || model.transferInstantDeclined != nil {
                VStack(alignment: .leading, spacing: 8) {
                    if let e = model.transferError {
                        Label(e, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(Theme.negative).appFont(.callout)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let reason = model.transferInstantDeclined {
                        instantDeclined(reason)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 20).padding(.bottom, 12)
            }
            Divider()
            buttons.padding(14)
        }
        .macFrame(width: 560, height: fittedHeight == nil ? 700 : nil)
        .frame(height: fittedHeight)
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { formFit.total = $0 }
        // The GiroCode view does not scroll: back to the fixed height.
        .onChange(of: step) { formFit = FormFit(total: formFit.total); heightFloor = 0 }
        .onChange(of: fittedHeight) { _, h in if let h, h > heightFloor { heightFloor = h } }
        .task { // screenshots
            switch model.options.demoSheetName {
            case "transfer-confirm": step = .review
            case "transfer-girocode": step = .giroCode
            case "transfer-recipients": managingRecipients = true
            case "transfer-suggestions": keepHighlight = 0; suggestions = recipients("n")
            default: break
            }
        }
        .task { // the demo offers fictional data and leaves the real clipboard untouched
            if model.options.demoSheetName == "transfer-clipboard" {
                clipboardOffer = ClipboardPaymentCheck.offer(for: .demo)
                return
            }
            let offer = await ClipboardPaymentCheck.filtered(checkClipboard())
            // the pattern detection is async: offer only on a still open, still empty form
            guard step == .form, ClipboardPaymentCheck.isEmpty(model.transferDraft, ownTransfer: ownTransfer) else { return }
            clipboardOffer = offer
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 12).strokeBorder(Theme.accent, style: StrokeStyle(lineWidth: 2, dash: [6]))
                    .background(Theme.accent.opacity(0.06)).allowsHitTesting(false)
            }
        }
        // what was hidden or shown again there applies to an open list at once
        .capturableSheet(isPresented: $managingRecipients, onDismiss: refreshSuggestions) { KnownRecipientsSheet() }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            guard step == .form, let p = providers.first else { return false }
            _ = p.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                Task { @MainActor in await importInvoice(url) }
            }
            return true
        }
        // ⌘V outside a text field (inside one, normal paste keeps working).
#if os(macOS)
        .onPasteCommand(of: [.plainText]) { _ in pasteFromClipboard() }
#endif
        .onAppear {
            if internalOnly && !ownTransfer { setOwnTransfer(true) }
            // Opened as Umbuchung from the batch sheet's switch.
            else if ownTransfer { setOwnTransfer(!ownTargets.isEmpty) }
        }
        // Switched on from outside (the kind switch); switching off is done by
        // whoever switches off, so a template or paste keeps its fields.
        .onChange(of: model.transferDraft.ownTransfer) { _, on in if on { applyOwnTransfer(true) } }
        .onChange(of: model.transferDraft.accountID) { _, _ in
            if internalOnly && !ownTransfer { setOwnTransfer(true); return }
            if !internalOnly && ownTransfer && ownTargets.isEmpty { setOwnTransfer(false); return }
            guard ownTransfer else { return }
            if !ownTargets.contains(where: { $0.id == ownTargetID }) { ownTargetID = ownTargets.first?.id }
            applyOwnTarget(ownTargetID)
        }
        .onChange(of: model.transferDraft.iban) { _, new in
            let f = Self.groupedIBAN(new)
            if f != new { model.transferDraft.iban = f }
        }
        .onChange(of: model.transferDraft.purpose) { _, new in
            if new.count > maxPurpose { model.transferDraft.purpose = String(new.prefix(maxPurpose)) }
        }
        .onChange(of: model.transferDraft.name) { _, new in
            if new != pickedName { pickedName = nil }
            refreshSuggestions()
        }
        .onChange(of: nameFocused) { refreshSuggestions() }
        // here, not on the list: that one only exists while it has rows, so it would miss the
        // list appearing and emptying
        .onChange(of: suggestions.map(\.iban)) { _, new in
            highlighted = keepHighlight.flatMap { new.isEmpty ? nil : min($0, new.count - 1) }
            keepHighlight = nil
        }
    }

    private var title: LocalizedStringKey {
        switch step {
        case .form: "Neue Überweisung"
        case .review: "Überweisung prüfen"
        case .giroCode: "GiroCode scannen"
        }
    }

    /// Title left, the helpers (templates, recipients, paste) as small actions right.
    private var header: some View {
        HStack(spacing: 12) {
            Text(title).appFont(.title2, weight: .semibold)
            Spacer()
            if step == .form {
                TransferTemplatesMenu()
                Button { managingRecipients = true } label: { Label("Empfänger", systemImage: "person.2") }
                    .help("Empfänger verwalten: veraltete IBANs nicht mehr vorschlagen")
                    #if os(iOS)
                    .labelStyle(.iconOnly)
                    #endif
                #if os(macOS)
                Button { pasteFromClipboard() } label: { Label("Einfügen", systemImage: "doc.on.clipboard") }
                    .keyboardShortcut("v", modifiers: [.command, .shift])
                    .help("Erkennt IBAN, Betrag, Empfänger und Zweck (⇧⌘V; ⌘V außerhalb von Textfeldern). Rechnung (PDF, ZUGFeRD, GiroCode) hierher ziehen.")
                #else
                PaymentPasteButton { report(PaymentParser.parseText($0)) }
                    .labelStyle(.iconOnly)
                #endif
            }
        }
        .padding(.horizontal, 20).padding(.vertical, 14)
    }

    // MARK: form

    private func form(draft: Binding<TransferDraft>) -> some View {
        Form {
            Section {
                LabeledContent("Von") {
                    SourceAccountPicker(selection: draft.accountID, accounts: model.transferFormAccounts)
                }
            } footer: {
                if account != nil && !canSend { noDirectTransferNote }
            }

            if internalOnly {
                Section {
                    Label("Vom Tagesgeldkonto sind nur Umbuchungen auf eigene Girokonten möglich.", systemImage: "arrow.left.arrow.right")
                        .appFont(.callout)
                }
            }

            Section {
                if ownTransfer {
                    Picker("Zielkonto", selection: Binding(get: { ownTargetID }, set: { ownTargetID = $0; applyOwnTarget($0) })) {
                        ForEach(ownTargets) { a in
                            Text("\(a.name) · \(formatIBAN(a.iban))").tag(Optional(a.id)).sensitive()
                        }
                    }
                    LabeledContent("Empfänger") {
                        Text(draft.wrappedValue.name).appFont(.body).sensitive()
                    }
                } else {
                    LabeledContent("Empfänger") {
                        TextField("Empfänger", text: draft.name)
                            .transferField()
                            .focused($nameFocused)
                            .onKeyPress(.downArrow) { moveHighlight(1) }
                            .onKeyPress(.upArrow) { moveHighlight(-1) }
                            .onKeyPress(.return) {
                                guard let h = highlighted, suggestions.indices.contains(h) else { return .ignored }
                                pick(suggestions[h])
                                return .handled
                            }
                            .onKeyPress(.escape) {
                                guard !suggestions.isEmpty else { return .ignored }
                                suggestions = []
                                return .handled
                            }
                            .sensitive()
                            .anchorPreference(key: NameFieldBounds.self, value: .bounds) { $0 }
                    }
                    LabeledContent("IBAN") {
                        HStack(spacing: 8) {
                            TextField("IBAN", text: draft.iban, prompt: Text("DE00 0000 0000 0000 0000 00"))
                                .transferField()
                                .appFont(.body, design: .monospaced)
                                .autocorrectionDisabled()
                                .sensitive()
                            validity(draft.wrappedValue.iban.isEmpty ? nil : ibanValid,
                                     valid: "Prüfsumme korrekt", invalid: "IBAN ungültig (Prüfsumme/Länge)")
                        }
                    }
                    LabeledContent("BIC") {
                        HStack(spacing: 8) {
                            TextField("BIC", text: draft.bic, prompt: Text("optional"))
                                .transferField()
                                .appFont(.body, design: .monospaced)
                                .autocorrectionDisabled()
                                // 8 or 11 characters: as wide as the amount (HIG: size fits the input)
                                .frame(maxWidth: 160 * textScale)
                                .sensitive()
                            validity(bicText.isEmpty ? nil : bicValid, valid: "BIC gültig", invalid: "BIC ungültig")
                            Spacer(minLength: 0)
                        }
                    }
                }
            } footer: {
                if let importNote { Text(importNote) }
            }

            Section {
                LabeledContent("Betrag") {
                    HStack(spacing: 6) {
                        TextField("Betrag", text: draft.amountText, prompt: Text(Money.plain(0)))
                            .transferField(alignment: .trailing)
                            .appFont(.body, monospacedDigit: true)
                            .focused($amountFocused)
                            .frame(maxWidth: 160 * textScale)
                            .sensitive(.amounts)
                        Text("€").foregroundStyle(Theme.secondary)
                        Spacer(minLength: 0)
                    }
                }
                LabeledContent("Verwendungszweck") {
                    TextField("Verwendungszweck", text: draft.purpose, prompt: Text("optional"), axis: .vertical)
                        .transferField()
                        .lineLimit(1...3)
                        .sensitive()
                }
                if canSend && account?.connector?.offersInstantChoice == true {
                    // Mutually exclusive choices in one pop-up (HIG): one row
                    // instead of a radio group of three.
                    Picker("Ausführung", selection: executionMode(draft)) {
                        Text("Normale Überweisung").tag(ExecutionMode.regular)
                        Text("Terminüberweisung").tag(ExecutionMode.scheduled)
                        Text("Echtzeitüberweisung (SEPA Instant)").tag(ExecutionMode.instant)
                    }
                    .pickerStyle(.menu)
                    if draft.wrappedValue.executionMode == .scheduled {
                        DatePicker("Ausführungsdatum", selection: draft.executionDate,
                                   in: (Calendar.current.date(byAdding: .day, value: 1, to: Date()) ?? Date())...,
                                   displayedComponents: .date)
                    }
                }
            } footer: {
                paymentFooter(draft.wrappedValue)
            }

            if canSend && !pinStored {
                Section {
                    LabeledContent(isBunq ? "bunq-API-Schlüssel" : "Banking-PIN") {
                        SecureField(isBunq ? "bunq-API-Schlüssel" : "Banking-PIN", text: draft.pin,
                                    prompt: Text("wird nicht gespeichert"))
                            .transferField()
                    }
                }
            }
        }
        .formStyle(.grouped)
        // The list floats over the form below the name field (a row would clip
        // it), so the sheet keeps its size while typing (#881).
        .overlayPreferenceValue(NameFieldBounds.self) { anchor in
            GeometryReader { g in
                if let anchor, !suggestions.isEmpty {
                    let field = g[anchor]
                    suggestionList
                        .frame(width: field.width)
                        .offset(x: field.minX, y: field.maxY + 4)
                }
            }
        }
        .scrollIndicators(formFitsSheet ? .never : .automatic)
        .reportsFormFit($formFit)
    }

    /// Demo PIN under the form, outside it: inside a section it would be a box
    /// in a box (HIG), in a footer macOS makes it narrower than the sections.
    /// Same 20 pt margins as the sections and the error lines below.
    @ViewBuilder private func demoHint(_ draft: Binding<TransferDraft>) -> some View {
        if canSend && (model.isDemo || isMagicDemoAccount) {
            DemoHintBanner(style: .pinAndTan, buttonTitle: "PIN 12345 einfügen") { draft.pin.wrappedValue = "12345" }
                .padding(.horizontal, 20).padding(.bottom, 12)
        }
    }

    /// Check mark or cross after a field that has a value.
    @ViewBuilder private func validity(_ ok: Bool?, valid: LocalizedStringKey, invalid: LocalizedStringKey) -> some View {
        if let ok {
            Image(systemName: ok ? "checkmark.circle.fill" : "xmark.circle.fill")
                .foregroundStyle(ok ? Theme.inflow : Theme.negative)
                .help(ok ? valid : invalid)
                .accessibilityLabel(ok ? valid : invalid)
        }
    }

    /// Not while the bank's refusal is shown below the form: that box says it all.
    private func showsInstantNote(_ d: TransferDraft) -> Bool {
        d.executionMode == .instant && canSend && account?.connector?.offersInstantChoice == true
            && model.transferInstantDeclined == nil
    }

    /// Only what needs attention: an invalid amount, the purpose limit once it
    /// is close, the instant-transfer note.
    @ViewBuilder private func paymentFooter(_ d: TransferDraft) -> some View {
        let amountInvalid = !d.amountText.isEmpty && cents == nil
        let nearLimit = d.purpose.count >= maxPurpose - 20
        if amountInvalid || nearLimit || showsInstantNote(d) {
            VStack(alignment: .leading, spacing: 6) {
                if amountInvalid || nearLimit {
                    HStack {
                        if amountInvalid { Text("Betrag > 0 eingeben").foregroundStyle(Theme.negative) }
                        Spacer()
                        if nearLimit {
                            Text("\(d.purpose.count)/\(maxPurpose)")
                                .monospacedDigit()
                                .foregroundStyle(d.purpose.count >= maxPurpose ? Theme.negative : Theme.tertiary)
                        }
                    }
                }
                if showsInstantNote(d) { instantNote }
            }
        }
    }

    /// Known recipients as a drop-down under the name (#881): one row per IBAN with
    /// its last use, pick with the mouse or ↑/↓ and Return; Esc closes it. The row under
    /// the pointer offers "Nicht mehr vorschlagen" (#978), as does its context menu.
    private var suggestionList: some View {
        VStack(spacing: 0) {
            ForEach(Array(suggestions.enumerated()), id: \.element.iban) { i, r in
                HStack(spacing: 4) {
                    Button { pick(r) } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(alignment: .firstTextBaseline) {
                                Text(r.name).appFont(.body).lineLimit(1).sensitive()
                                Spacer(minLength: 8)
                                Text("zuletzt \(DateText.medium(r.lastUsed))").appFont(.caption)
                                    .foregroundStyle(Theme.tertiary).fixedSize()
                            }
                            Text(formatIBAN(r.iban)).appFont(.caption, design: .monospaced)
                                .foregroundStyle(Theme.secondary).lineLimit(1).sensitive()
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.leading, 8).padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    Button { hide(r) } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.tertiary)
                            .padding(.trailing, 8).padding(.vertical, 4)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Nicht mehr vorschlagen")
                    .opacity(i == highlighted ? 1 : 0)
                    .disabled(i != highlighted)
                    .accessibilityHidden(true)
                }
                .background(RoundedRectangle(cornerRadius: 6).fill(i == highlighted ? Theme.selection : .clear))
                .onHover { if $0 { highlighted = i } }
                .contextMenu {
                    Button("Nicht mehr vorschlagen", systemImage: "eye.slash") { hide(r) }
                }
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(i == highlighted ? .isSelected : [])
                .accessibilityAction { pick(r) }
                .accessibilityAction(named: "Nicht mehr vorschlagen") { hide(r) }
            }
        }
        .padding(4)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.card))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.border))
        .shadow(color: .black.opacity(0.25), radius: 8, y: 4)
    }

    /// Known recipients for what was typed: only once something is typed, one
    /// row per IBAN (the most used spelling), the most recently used first, at most five.
    private func recipients(_ query: String) -> [KnownRecipient] {
        guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
        return model.suggestRecipients(query)
    }

    /// The list for the name as typed: only while the field has the focus and the name was not
    /// just taken from the list.
    private func refreshSuggestions() {
        let name = model.transferDraft.name
        suggestions = nameFocused && name != pickedName ? recipients(name) : []
    }

    /// "Nicht mehr vorschlagen": the IBAN leaves the list at once, the highlight keeps its
    /// place; "Empfänger" shows it again.
    private func hide(_ r: KnownRecipient) {
        let before = suggestions.map(\.iban), place = highlighted
        model.setRecipientHidden(r.iban, true)
        refreshSuggestions()
        if suggestions.map(\.iban) != before { keepHighlight = place }
    }

    private func moveHighlight(_ step: Int) -> KeyPress.Result {
        guard !suggestions.isEmpty else { return .ignored }
        highlighted = min(max((highlighted ?? -1) + step, 0), suggestions.count - 1)
        return .handled
    }

    /// Takes name and IBAN, closes the list and goes on to the amount.
    private func pick(_ r: KnownRecipient) {
        pickedName = r.name
        model.transferDraft.name = r.name
        model.transferDraft.iban = r.iban
        suggestions = []
        highlighted = nil
        amountFocused = true
    }

    /// Why this account ends in a GiroCode instead of a direct transfer.
    @ViewBuilder private var noDirectTransferNote: some View {
        if model.bankingGate(forAccount: account?.id) != nil {
            // Security lock: no direct transfer, the GiroCode needs no bank contact.
            Label("Die Sicherheitssperre verhindert direkte Überweisungen von diesem Konto. Am Ende erhalten Sie einen GiroCode zum Scannen in der App Ihrer Bank.", systemImage: "exclamationmark.shield")
        } else if account?.connector == .comdirect {
            // The comdirect REST API has no payment endpoint; FinTS has.
            Label("Die comdirect-Schnittstelle (REST) kann keine Überweisungen ausführen. Verbinden Sie das Konto zusätzlich per FinTS, um direkt zu überweisen. Sonst erhalten Sie am Ende einen GiroCode zum Scannen in der comdirect-App.", systemImage: "qrcode")
        } else {
            Label("Von diesem Konto kann GiroCrab nicht direkt überweisen. Am Ende erhalten Sie einen GiroCode zum Scannen in der App Ihrer Bank.", systemImage: "qrcode")
        }
    }

    @ViewBuilder private var instantNote: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Kommt in Sekunden an und ist nicht rückholbar. Bietet die Bank sie nicht an, wird nichts gesendet; Sie können sie dann als normale Überweisung senden.")
            if let c = cents, c >= 10_000_000 {
                Label("Viele Banken begrenzen Echtzeitüberweisungen (oft auf 100.000 € oder ein eigenes Limit). Die Bank kann den Auftrag dann ablehnen.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(Theme.warning)
            }
        }
    }

    private func executionMode(_ draft: Binding<TransferDraft>) -> Binding<ExecutionMode> {
        Binding(get: { draft.wrappedValue.executionMode }, set: { draft.wrappedValue.executionMode = $0 })
    }

    // MARK: confirmation

    /// The transfer as EPC QR code for the bank's app: the only way out for
    /// read-only accounts (e.g. C24), on request for sendable ones.
    private var giroCodeView: some View {
        let d = model.transferDraft
        let payload = giroCodePayload
        return VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 16) {
                Group {
                    if let payload, let cg = GiroCode.image(for: payload) {
                        Image(decorative: cg, scale: 1)
                            .interpolation(.none)
                            .resizable()
                            .accessibilityLabel(Text("GiroCode"))
                    } else {
                        Image(systemName: "qrcode").resizable().foregroundStyle(Theme.tertiary)
                    }
                }
                .frame(width: 200, height: 200)
                .sensitive() // the code carries name, IBAN, amount and purpose
                VStack(alignment: .leading, spacing: 8) {
                    amountHeadline
                    summaryRow("An", d.name)
                    summaryRow("IBAN", d.iban)
                    if !bicText.isEmpty { summaryRow("BIC", bicText.uppercased()) }
                    summaryRow("Zweck", d.purpose.isEmpty ? "–" : d.purpose)
                    summaryRow("Von", account.map { "\($0.name) · \(formatIBAN($0.iban))" } ?? "–")
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12).fill(Theme.card))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.border))
            if let payload { giroCodeActions(payload) }
            Label("In der App Ihrer Bank „Fotoüberweisung“ bzw. „QR-Code scannen“ wählen, den Code scannen und dort mit TAN freigeben. GiroCrab sendet nichts.", systemImage: "camera.viewfinder")
                .appFont(.callout).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if canSend {
                Label("Ausführungsdatum und Echtzeitüberweisung wählen Sie in der App Ihrer Bank; ein GiroCode enthält sie nicht.", systemImage: "info.circle")
                    .appFont(.callout).foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        #if os(macOS)
        .fileExporter(isPresented: Binding(get: { exportingGiroCode != nil }, set: { if !$0 { exportingGiroCode = nil } }),
                      document: exportingGiroCode, contentType: .png, defaultFilename: "GiroCode.png") { _ in }
        #endif
    }

    /// Copy / save (macOS) or share (iOS) the code as a picture.
    private func giroCodeActions(_ payload: String) -> some View {
        HStack(spacing: 8) {
            #if os(macOS)
            Button("Bild kopieren", systemImage: "doc.on.doc") {
                if let png = GiroCode.png(for: payload) { BugReport.copyToPasteboard(png) }
            }
            Button("Sichern …", systemImage: "square.and.arrow.down") {
                exportingGiroCode = GiroCode.png(for: payload).map(PNGFileDocument.init(data:))
            }
            #else
            ShareLink(item: GiroCodeImage(payload: payload), preview: SharePreview("GiroCode")) {
                Label("Bild teilen", systemImage: "square.and.arrow.up")
            }
            #endif
        }
        .help("Das Bild enthält IBAN, Empfänger und Betrag.")
        .buttonStyle(.bordered)
        .controlSize(.small)
    }

    /// The amount at the top of the review and the GiroCode step.
    private var amountHeadline: some View {
        Text(Money.format(cents ?? 0))
            .appFont(.largeTitle, weight: .semibold, monospacedDigit: true)
            .sensitive(.amounts)
    }

    /// Read-only review in the same grouped style as the form: the amount first,
    /// then recipient, payment and execution, each as label + value.
    private var sendConfirmation: some View {
        let d = model.transferDraft
        return Form {
            Section {
                amountHeadline
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 6)
            }
            // Same rows and labels as the form, without headings (#881).
            Section {
                reviewRow("Von", account.map { "\($0.name) · \(formatIBAN($0.iban))" } ?? "–")
            }
            Section {
                reviewRow("Empfänger", d.name)
                reviewRow("IBAN", d.iban, monospaced: true)
                if !bicText.isEmpty { reviewRow("BIC", bicText.uppercased(), monospaced: true) }
            }
            Section {
                reviewRow("Verwendungszweck", d.purpose.isEmpty ? "–" : d.purpose)
                reviewRow("Ausführung", sendsInstant
                    ? String(localized: "Sofort (Echtzeitüberweisung)")
                    : d.executionDateISO.map { String(format: String(localized: "Am %@ (Terminüberweisung)"), $0) }
                        ?? String(localized: "Heute (SEPA-Überweisung)"), hides: [])
            } footer: {
                Label("Bitte Empfänger und IBAN genau prüfen. Eine freigegebene Überweisung lässt sich meist nicht mehr zurückholen.", systemImage: "exclamationmark.shield")
            }
        }
        .formStyle(.grouped)
        .reportsFormFit($formFit)
    }

    private func reviewRow(_ label: LocalizedStringKey, _ value: String, monospaced: Bool = false,
                           hides: ReportRedaction = .personal) -> some View {
        LabeledContent(label) {
            Text(value)
                .appFont(.body, design: monospaced ? .monospaced : .default)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
                .sensitive(hides)
        }
    }

    /// Rows of the GiroCode review: all of them are personal.
    private func summaryRow(_ k: LocalizedStringKey, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(k).foregroundStyle(Theme.secondary).frame(width: 90 * textScale, alignment: .leading)
            Text(v).textSelection(.enabled).fixedSize(horizontal: false, vertical: true).sensitive()
            Spacer(minLength: 0)
        }
    }

    /// Instant was not possible and nothing was sent: offer a regular
    /// transfer, which needs its own approval.
    private func instantDeclined(_ reason: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Echtzeitüberweisung nicht ausgeführt", systemImage: "bolt.slash.fill")
                .appFont(.callout, weight: .bold)
            Text(String(format: String(localized: "Es wurde kein Geld gesendet. Grund der Bank: %@"), reason))
                .appFont(.callout).fixedSize(horizontal: false, vertical: true)
            Button("Als normale Überweisung senden") { model.sendAsRegularTransfer() }
                .disabled(model.transferBusy)
            Text("Kommt in der Regel am nächsten Bankarbeitstag an. Sie geben sie erneut frei.")
                .appFont(.caption).foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
    }

    // MARK: buttons

    private var buttons: some View {
        HStack {
            switch step {
            case .giroCode:
                Button("Zurück") { step = .form }
                Spacer()
                Button("Fertig") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            case .review:
                // Back and the approval only: HIG, never Cancel, Back and Done
                // together. Esc goes back to the form; it closes from there.
                Button("Zurück") { step = .form }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(isBunq ? "In der bunq-App freigeben" : "Jetzt mit TAN freigeben") { model.startTransfer() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(model.transferBusy)
            case .form:
                Button("Abbrechen", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if canSend {
                    // nothing is sent, so no PIN needed
                    Button("Als GiroCode anzeigen") { leaveForm(to: .giroCode) }
                        .disabled(!canReview || giroCodePayload == nil)
                }
                let next: Step = canSend ? .review : .giroCode
                Button(next == .review ? "Weiter" : "GiroCode erstellen") { leaveForm(to: next) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(!canReview || (canSend && !pinStored && model.transferDraft.pin.isEmpty)
                              || (!canSend && giroCodePayload == nil))
            }
        }
    }

    private func leaveForm(to next: Step) {
        model.transferError = nil
        step = next
    }

    // MARK: clipboard / invoice drop

    #if os(macOS)
    private func pasteFromClipboard() {
        report(PaymentParser.parseClipboard())
    }
    #endif

    private func importInvoice(_ url: URL) async {
        importNote = String(localized: "Lese Rechnung …")
        report(await PaymentParser.parseFile(url) ?? ParsedPayment())
    }

    private func report(_ p: ParsedPayment, onlyEmpty: Bool = false) {
        guard !p.isEmpty else { importNote = String(localized: "Nichts Erkennbares gefunden"); return }
        model.fillTransferDraft(p, onlyEmpty: onlyEmpty)
        importNote = String(localized: "Felder ausgefüllt – bitte prüfen")
    }

    /// Opt-in (#663): offers the clipboard as a payment when the empty form opens.
    private func checkClipboard() -> ClipboardPaymentCheck.Offer? {
        guard clipboardCheck else { return nil }
        return ClipboardPaymentCheck.check(
            draftEmpty: ClipboardPaymentCheck.isEmpty(model.transferDraft, ownTransfer: ownTransfer),
            lastHandled: &model.clipboardChangeCountHandled)
    }

    private func acceptClipboard(_ p: ParsedPayment) {
        clipboardOffer = nil
        report(p, onlyEmpty: true)
    }

    // MARK: helpers

    /// Upper-case, only A–Z/0–9, groups of four.
    static func groupedIBAN(_ raw: String) -> String {
        let clean = raw.uppercased().filter { $0.isASCII && ($0.isLetter || $0.isNumber) }
        return stride(from: 0, to: clean.count, by: 4).map { i -> String in
            let s = clean.index(clean.startIndex, offsetBy: i)
            let e = clean.index(s, offsetBy: min(4, clean.count - i))
            return String(clean[s..<e])
        }.joined(separator: " ")
    }

    static func looksLikeBIC(_ s: String) -> Bool {
        let u = Array(s.uppercased())
        guard u.count == 8 || u.count == 11 else { return false }
        return u[..<6].allSatisfy { $0.isASCII && $0.isLetter } && u[6...].allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) }
    }
}

/// "Von Konto" of the transfer, standing order and batch forms: the picker
/// draws the selected account's name and IBAN itself (an iOS menu picker),
/// so it is covered as a whole; the label beside it stays readable.
struct SourceAccountPicker: View {
    let selection: Binding<Int64?>
    let accounts: [Account]

    var body: some View {
        Picker("Von Konto", selection: selection) {
            ForEach(accounts) { a in
                Text("\(a.name) · \(formatIBAN(a.iban))").tag(Optional(a.id))
            }
        }
        .labelsHidden()
        .sensitive()
    }
}

private extension View {
    /// A text field of the transfer form. macOS: bezeled, so an empty field
    /// reads as a field (#881); iOS keeps the inline trailing style.
    func transferField(alignment: TextAlignment = .leading) -> some View {
        #if os(macOS)
        labelsHidden().textFieldStyle(.roundedBorder).multilineTextAlignment(alignment)
        #else
        labelsHidden().multilineTextAlignment(.trailing)
        #endif
    }
}

/// Where the recipient name field is, for the suggestion list over the form.
private struct NameFieldBounds: PreferenceKey {
    static let defaultValue: Anchor<CGRect>? = nil
    static func reduce(value: inout Anchor<CGRect>?, nextValue: () -> Anchor<CGRect>?) {
        value = value ?? nextValue()
    }
}
