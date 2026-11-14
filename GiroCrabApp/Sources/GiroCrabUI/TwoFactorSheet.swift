import SwiftUI
import GiroCrabCore

/// Native 2FA sheet: decoupled push-TAN (spinner + auto polling), code entry,
/// photoTAN image + code, or PSD2 browser redirect.
struct TwoFactorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL
    @State private var code = ""
    @State private var pulse = false
    /// The bank's consent page inside GiroCrab (PSD2 redirect).
    @State private var consent: ConsentBrowser?
    @State private var showManualRedirect = false

    /// The sheet shows the bank's consent page (needs a browser-sized window).
    private var showsConsentPage: Bool {
        consent != nil && model.activeChallenge?.challenge.kind == .redirect
    }

    var body: some View {
        if let active = model.activeChallenge {
            let c = active.challenge
            VStack(spacing: 18) {
                HStack(spacing: 10) {
                    SymbolTile(symbol: active.connection.sfSymbol, tint: active.connection.connector.tint, size: 30)
                    VStack(alignment: .leading, spacing: 0) {
                        Text(active.connection.displayName).appFont(.headline)
                        Text(c.methodName).appFont(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    SyncBatchProgressLabel()
                }

                if model.isDemo || AppModel.isMagicDemoLogin(active.connection.loginName) {
                    DemoHintBanner(style: .pinAndTan, buttonTitle: c.kind == .decoupledPush ? nil : "TAN 123456 einfügen") {
                        code = "123456"
                    }
                }

                switch c.kind {
                case .decoupledPush:
                    Image(systemName: "iphone.radiowaves.left.and.right")
                        .font(.system(size: 56, weight: .light))
                        .foregroundStyle(.tint)
                        .symbolEffect(.variableColor.iterative, options: .repeating, isActive: true)
                        .padding(.top, 6)
                    Text(c.title).appFont(.title3, weight: .bold)
                    Text(c.message).multilineTextAlignment(.center).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    if let s = c.redirectUrl, let url = URL(string: s) {
                        Button { openURL(url) } label: { Label("Im Browser anmelden", systemImage: "safari") }
                            .buttonStyle(.borderedProminent)
                    }
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(active.waiting ? "Warte auf Freigabe …" : "Push-Nachricht gesendet")
                            .appFont(.callout).foregroundStyle(.secondary)
                    }
                case .code, .imageCode:
                    Image(systemName: c.kind == .imageCode ? "qrcode.viewfinder" : "lock.rotation")
                        .font(.system(size: 44, weight: .light))
                        .foregroundStyle(.tint)
                    Text(c.title).appFont(.title3, weight: .bold)
                    if let data = c.image, let img = platformImage(data) {
                        img.resizable().interpolation(.none).scaledToFit().frame(maxWidth: 220, maxHeight: 220)
                            .background(.white)
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                    Text(c.message).multilineTextAlignment(.center).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    TextField("TAN / Code", text: $code)
                        .appFont(.title2, design: .monospaced)
                        .multilineTextAlignment(.center)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 220)
                        #if os(iOS)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        #endif
                        .onSubmit(submit)
                    if active.connection.connector == .tradeRepublic && c.kind == .code {
                        // HIG: belongs to the code field, not to the dialog's
                        // button row (which keeps at most three buttons)
                        Button("Code per SMS anfordern") { model.submitCode("SMS") }
                            #if os(macOS)
                            .buttonStyle(.link)
                            #else
                            .buttonStyle(.plain)
                            #endif
                            .appFont(.callout)
                            .help("Kein Code in der App? Code per SMS anfordern.")
                    }
                case .redirect:
                    if let consent {
                        // Bank login inside GiroCrab; closes itself on the redirect.
                        ConsentWebView(browser: consent)
                            // banks lay their consent pages out for a desktop
                            // browser: a narrower view cuts off the account list
                            .desktopFrame(minWidth: 820, idealWidth: 880, minHeight: 420, idealHeight: 620, maxHeight: .infinity)
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.border))
                        Text("Bei der Bank anmelden und den Zugriff erlauben – das Fenster schließt sich danach von selbst.")
                            .appFont(.callout).foregroundStyle(.secondary)
                    } else {
                        Image(systemName: "building.columns")
                            .font(.system(size: 44, weight: .light))
                            .foregroundStyle(.tint)
                        Text(c.title).appFont(.title3, weight: .bold)
                        Text("Sie melden sich direkt bei Ihrer Bank an und erlauben den Zugriff. GiroCrab übernimmt das Ergebnis automatisch.")
                            .multilineTextAlignment(.center).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        if let s = c.redirectUrl, let url = URL(string: s) {
                            Button { openConsent(url) } label: { Label("Bei der Bank anmelden", systemImage: "lock.shield") }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.large)
                        }
                    }
                    // Fallback for banks that block embedded browsers.
                    DisclosureGroup("Stattdessen im eigenen Browser anmelden", isExpanded: $showManualRedirect) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(c.message).appFont(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            if let s = c.redirectUrl, let url = URL(string: s) {
                                Button { openURL(url) } label: { Label("Zustimmungsseite öffnen", systemImage: "arrow.up.right.square") }
                                    .buttonStyle(.bordered)
                            }
                            TextField("Weitergeleitete Adresse hier einfügen", text: $code)
                                .textFieldStyle(.roundedBorder)
                                .autocorrectionDisabled()
                                .onSubmit(submit)
                        }
                        .padding(.top, 4)
                    }
                    .appFont(.callout)
                }

                if active.busy && c.kind == .redirect {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Freigabe wird übernommen …").appFont(.callout).foregroundStyle(.secondary)
                    }
                }
                if let e = active.error {
                    Text(e).foregroundStyle(.red).appFont(.callout)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                if let s = c.expiresInSeconds {
                    Text("Gültig noch etwa \(s / 60) Min.").appFont(.caption).foregroundStyle(.tertiary)
                }

                SyncPromptButtons(cancel: model.skipCurrentSync) {
                    if c.kind == .code || c.kind == .imageCode || (c.kind == .redirect && showManualRedirect) {
                        Button(action: submit) {
                            Text("Weiter").frame(maxWidth: .infinity)
                        }
                        .keyboardShortcut(.defaultAction)
                        .buttonStyle(.borderedProminent)
                        .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty || active.busy)
                    }
                }
            }
            .padding(24)
            // the sheet takes the height its content needs: never cut off the
            // result line, the error or the buttons. Not with the bank's page:
            // that one shrinks to fit the window instead of pushing the sheet
            // past it.
            .fixedSize(horizontal: false, vertical: !showsConsentPage)
            // the bank's own page needs the room of a browser window
            .macFrame(minWidth: showsConsentPage ? 868 : 400, idealWidth: showsConsentPage ? 928 : 420)
        }
    }

    /// Sends the code / pasted address – once: Return reaches both the field
    /// and the default button, and a second, empty call would cut off the
    /// first. A pasted address stays in the field until the answer is in, so
    /// it can be sent again after an error.
    private func submit() {
        let value = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, model.activeChallenge?.busy != true else { return }
        if model.activeChallenge?.challenge.kind != .redirect { code = "" }
        model.submitCode(value)
    }

    private func openConsent(_ url: URL) {
        let redirect = model.settingEntry("enablebanking.redirect_url")?.value
            .trimmingCharacters(in: .whitespaces)
        consent = ConsentBrowser(
            url: url,
            redirect: (redirect?.isEmpty == false ? redirect : nil) ?? ConsentRedirect.defaultURL
        ) { result in
            consent = nil
            model.submitCode(result.absoluteString)
        }
    }

    private func platformImage(_ data: Data) -> Image? {
        #if os(macOS)
        NSImage(data: data).map { Image(nsImage: $0) }
        #else
        UIImage(data: data).map { Image(uiImage: $0) }
        #endif
    }
}


/// "Bankzugang 2 von 5" while "Alle abrufen" runs over several logins
/// (PIN and TAN sheets), so skipping one is an informed choice.
struct SyncBatchProgressLabel: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let p = model.syncBatchProgress {
            Text("Bankzugang \(p.current) von \(p.total)")
                .appFont(.caption)
                .monospacedDigit()
                .foregroundStyle(Theme.secondary)
        }
    }
}

/// Button row of a sync prompt (PIN, TAN, setup, browser login).
///
/// Single sync: `[Abbrechen] … [Default]`, as everywhere in GiroCrab.
/// "Alle abrufen" with more logins waiting:
/// `[Alle abbrechen] … [Überspringen] [Default]` – HIG: the farther-reaching
/// alternative stands apart on the left (like "Don't Save"); Cancel
/// (Esc = skip this login) sits next to the default button, both one width.
struct SyncPromptButtons<Default: View>: View {
    @Environment(AppModel.self) private var model
    /// Cancel this prompt: skips the login in a batch, otherwise just cancels.
    let cancel: () -> Void
    /// Extra buttons after the leading one (e.g. "Demo-Daten ansehen").
    var leading: AnyView? = nil
    /// The default button; its label should use `.frame(maxWidth: .infinity)`.
    @ViewBuilder let defaultButton: () -> Default

    var body: some View {
        #if os(iOS)
        // iPhone: stack the actions so nothing clips horizontally
        VStack(spacing: 10) {
            defaultButton()
            if model.canSkipInSyncBatch {
                Button(role: .cancel, action: cancel) {
                    Text("Überspringen").frame(maxWidth: .infinity)
                }
                Button("Alle abbrechen") { model.cancelAllSync() }
            } else {
                Button("Abbrechen", role: .cancel, action: cancel)
            }
            if let leading { leading }
        }
        .frame(maxWidth: .infinity)
        .interactiveDismissDisabled(model.syncBatchProgress != nil)
        #else
        HStack {
            if model.canSkipInSyncBatch {
                Button("Alle abbrechen") { model.cancelAllSync() }
                    .help("Den Abruf für alle restlichen Bankzugänge beenden.")
                    .fixedSize()
            } else {
                Button("Abbrechen", role: .cancel, action: cancel)
                    .keyboardShortcut(.cancelAction)
            }
            if let leading { leading }
            Spacer()
            EqualWidthHStack {
                if model.canSkipInSyncBatch {
                    Button(role: .cancel, action: cancel) {
                        Text("Überspringen").frame(maxWidth: .infinity)
                    }
                    .keyboardShortcut(.cancelAction)
                    .help("Diesen Bankzugang auslassen und mit dem nächsten weitermachen.")
                }
                defaultButton()
            }
        }
        // iOS: a swipe-down would bypass `cancel` and stall "Alle abrufen";
        // the preference reaches the sheet from here (PIN, setup, browser login)
        .interactiveDismissDisabled(model.syncBatchProgress != nil)
        #endif
    }
}

/// HIG: buttons side by side in a dialog (Cancel + default) share one width,
/// that of the widest title, with the system spacing between them (pattern
/// from Apple's "Compose custom layouts with SwiftUI", WWDC22). Labels use
/// `.frame(maxWidth: .infinity)` to fill the width.
struct EqualWidthHStack: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard !subviews.isEmpty else { return .zero }
        let m = widest(subviews)
        let gaps = spacing(subviews).reduce(0, +)
        return CGSize(width: m.width * CGFloat(subviews.count) + gaps, height: m.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let m = widest(subviews)
        let gaps = spacing(subviews)
        var x = bounds.minX
        for (i, s) in subviews.enumerated() {
            s.place(at: CGPoint(x: x, y: bounds.midY), anchor: .leading,
                    proposal: ProposedViewSize(width: m.width, height: m.height))
            x += m.width + gaps[i]
        }
    }

    private func widest(_ subviews: Subviews) -> CGSize {
        subviews.map { $0.sizeThatFits(.unspecified) }.reduce(.zero) {
            CGSize(width: max($0.width, $1.width), height: max($0.height, $1.height))
        }
    }

    /// System spacing after each subview (0 after the last).
    private func spacing(_ subviews: Subviews) -> [CGFloat] {
        subviews.indices.map { i in
            i < subviews.count - 1
                ? subviews[i].spacing.distance(to: subviews[i + 1].spacing, along: .horizontal)
                : 0
        }
    }
}
