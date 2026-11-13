import SwiftUI
import UniformTypeIdentifiers
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// The `.pem` file Enable Banking downloads when it generates the key in the
/// browser is named after the application ID (`<uuid>.pem`).
enum EnableBankingKeyFile {
    static func applicationID(fromFileName name: String) -> String? {
        let pattern = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
        guard let r = name.range(of: pattern, options: .regularExpression) else { return nil }
        return String(name[r]).lowercased()
    }

    /// Where enablebanking.com's download lands: start the file dialog there,
    /// not in the folder macOS remembers from the last import.
    static var downloadsFolder: URL? {
        FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
    }

    static func looksLikePrivateKey(_ text: String) -> Bool {
        text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----")
    }

    /// Reads a chosen key file: the PEM text and, if the name carries it, the
    /// application ID. Throws a readable message for anything else.
    static func read(_ url: URL) throws -> (pem: String, applicationID: String?) {
        let ok = url.startAccessingSecurityScopedResource()
        defer { if ok { url.stopAccessingSecurityScopedResource() } }
        let text = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        guard looksLikePrivateKey(text) else {
            throw KeyFileError(message: String(localized: "Das ist kein privater Schlüssel. Bitte die .pem-Datei wählen, die enablebanking.com beim Registrieren heruntergeladen hat."))
        }
        return (text.trimmingCharacters(in: .whitespacesAndNewlines), applicationID(fromFileName: url.lastPathComponent))
    }

    struct KeyFileError: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
}

/// Enable Banking credentials with a short guide, key-file import and a check
/// of the application before the first bank login. Edits `values` in place.
struct EnableBankingSetupSection: View {
    @Environment(AppModel.self) private var model
    @Binding var values: [String: String]
    @State private var importing = false
    /// Result of reading the key file: success, or what went wrong.
    @State private var keyNote: (ok: Bool, text: String)?
    @State private var checking = false
    @State private var check: EnableBankingAppCheck?
    @State private var checkError: String?

    private let idKey = "enablebanking.application_id"
    private let pemKey = "enablebanking.private_key_pem"
    private let redirectKey = "enablebanking.redirect_url"

    private var redirect: String {
        let v = (values[redirectKey] ?? "").trimmingCharacters(in: .whitespaces)
        return v.isEmpty ? ConsentRedirect.defaultURL : v
    }
    private var hasKey: Bool { !(values[pemKey] ?? "").isEmpty || (model.settingEntry(pemKey)?.hasValue ?? false) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            guide
            Divider()
            HStack(spacing: 10) {
                Button { importing = true } label: { Label("Schlüsseldatei (.pem) wählen …", systemImage: "key") }
                if let keyNote {
                    Label(keyNote.text, systemImage: keyNote.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .font(.caption).foregroundStyle(keyNote.ok ? Theme.inflow : Theme.negative)
                        .fixedSize(horizontal: false, vertical: true)
                } else if hasKey {
                    Text("Schlüssel ist gespeichert").font(.caption).foregroundStyle(Theme.secondary)
                }
            }
            LabeledContent("Application-ID") {
                TextField("wird aus dem Dateinamen übernommen", text: binding(idKey))
                    .textFieldStyle(.roundedBorder)
                    .font(.callout.monospaced())
                    .autocorrectionDisabled()
            }
            HStack(spacing: 10) {
                Button {
                    Task { await runCheck() }
                } label: { Label("Anwendung prüfen", systemImage: "checkmark.shield") }
                    .disabled(checking || (values[idKey] ?? "").trimmingCharacters(in: .whitespaces).isEmpty || (values[pemKey] ?? "").isEmpty)
                if checking { ProgressView().controlSize(.small) }
            }
            checkResult
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.data, .text, .plainText]) { result in
            guard case .success(let url) = result else { return }
            do {
                let key = try EnableBankingKeyFile.read(url)
                values[pemKey] = key.pem
                if let id = key.applicationID { values[idKey] = id }
                keyNote = (true, key.applicationID != nil
                    ? String(localized: "Schlüssel und Application-ID übernommen")
                    : String(localized: "Schlüssel übernommen – Application-ID bitte eintragen"))
                check = nil
                checkError = nil
                if key.applicationID != nil { Task { await runCheck() } }
            } catch {
                keyNote = (false, error.localizedDescription)
            }
        }
        .fileDialogDefaultDirectory(EnableBankingKeyFile.downloadsFolder)
        .onAppear {
            if (values[redirectKey] ?? "").isEmpty { values[redirectKey] = ConsentRedirect.defaultURL }
        }
    }

    private var guide: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("So richten Sie Ihre kostenlose Enable-Banking-Anwendung ein:").font(.callout.weight(.medium))
            step(1, "Auf enablebanking.com eine Anwendung registrieren: „Production“ wählen, den Schlüssel im Browser erzeugen lassen und diese Redirect-URL eintragen:")
            HStack(spacing: 6) {
                Text(redirect).font(.caption.monospaced()).textSelection(.enabled)
                Button { copy(redirect) } label: { Image(systemName: "doc.on.doc") }
                    .buttonStyle(.borderless)
                    .help("Redirect-URL kopieren")
            }
            .padding(.leading, 22)
            step(2, "Bei der Anwendung „Activate by linking accounts“ wählen und alle Konten verknüpfen, die GiroCrab sehen soll – nicht verknüpfte Konten liefert die Bank nicht.")
            if !model.psd2LinkHints.isEmpty { importedAccounts }
            step(3, "Hier die heruntergeladene .pem-Datei wählen. GiroCrab übernimmt Schlüssel und Application-ID und prüft die Anwendung.")
            Link("enablebanking.com/cp/applications öffnen", destination: URL(string: "https://enablebanking.com/cp/applications")!)
                .font(.caption)
                .padding(.leading, 22)
        }
    }

    /// The accounts the MoneyMoney import attached to Enable Banking
    /// connections – the ones to link while activating the application.
    private var importedAccounts: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Aus Ihrem MoneyMoney-Import:").font(.caption.weight(.medium))
            ForEach(model.psd2LinkHints, id: \.connectionId) { c in
                Text(c.bankName).font(.caption.weight(.medium)).padding(.top, 2)
                ForEach(Array(c.accounts.enumerated()), id: \.offset) { _, a in
                    Text([a.name, a.iban.isEmpty ? (a.accountNumber.isEmpty ? nil : String(localized: "Konto \(a.accountNumber)")) : formatIBAN(a.iban)]
                        .compactMap { $0 }.joined(separator: " · "))
                        .font(.caption.monospacedDigit()).foregroundStyle(Theme.secondary)
                        .textSelection(.enabled)
                }
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Theme.canvas))
        .padding(.leading, 22)
    }

    private func step(_ n: Int, _ text: LocalizedStringKey) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("\(n).").font(.caption.monospacedDigit().weight(.semibold)).frame(width: 14, alignment: .trailing)
            Text(text).font(.caption).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private var checkResult: some View {
        if let checkError {
            Label(checkError, systemImage: "xmark.octagon.fill")
                .font(.caption).foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        } else if let check {
            if check.problems.isEmpty {
                Label("Anwendung „\(check.name)“ ist bereit (Production, aktiv, Redirect-URL eingetragen).", systemImage: "checkmark.seal.fill")
                    .font(.caption).foregroundStyle(.green)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(check.problems, id: \.self) { p in
                        Label(p, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption).foregroundStyle(.orange)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private func runCheck() async {
        guard model.isUnlocked else { return }
        checking = true
        defer { checking = false }
        do {
            check = try await model.checkEnableBankingApplication(
                applicationId: (values[idKey] ?? "").trimmingCharacters(in: .whitespaces),
                privateKeyPem: values[pemKey] ?? "",
                redirectUrl: redirect)
            checkError = nil
        } catch {
            check = nil
            checkError = describe(error)
        }
    }

    private func binding(_ key: String) -> Binding<String> {
        Binding(get: { values[key] ?? "" }, set: { values[key] = $0 })
    }

    private func copy(_ s: String) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
        #else
        UIPasteboard.general.string = s
        #endif
    }
}
