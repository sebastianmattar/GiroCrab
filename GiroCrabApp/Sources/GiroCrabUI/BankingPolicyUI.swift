import SwiftUI
import GiroCrabCore

// MARK: - App model: checks at the bank entry points

extension AppModel {
    /// Runs `action` once the banking policy is fresh (at most one download
    /// per 15 minutes). With the feature off (placeholder key) `action` runs
    /// right away, so nothing changes for builds without a policy key.
    func withFreshBankingPolicy(_ action: @escaping @MainActor () -> Void) {
        let policy = bankingPolicy
        guard policy.isEnabled else { action(); return }
        Task { @MainActor in
            await policy.refresh()
            action()
        }
    }

    /// Connector key of a login (`fints:<BLZ>`, `enablebanking:<country>:<bank>`, …).
    func connectionKey(_ connection: BankConnection) -> String {
        connectionKeys[connection.id] ?? connectorKindKey(kind: connection.connector)
    }

    /// Why this login may not contact its bank (nil = allowed).
    func bankingGate(for connection: BankConnection) -> BankingGate? {
        if Self.isMagicDemoLogin(connection.loginName) { return nil }
        return bankingPolicy.gate(for: connectionKey(connection))
    }

    /// Block for the login of an account (transfers).
    func bankingGate(forAccount accountID: Int64?) -> BankingGate? {
        guard let accountID, let account = allAccounts.first(where: { $0.id == accountID }),
              let connection = connections.first(where: { $0.id == account.connectionId }) else { return nil }
        return bankingGate(for: connection)
    }

    /// Block for a login that is about to be added.
    func bankingGate(forNew kind: ConnectorKind) -> BankingGate? {
        bankingPolicy.gate(for: connectorKindKey(kind: kind))
    }

    func bankingGate(forNewFinTS blz: String) -> BankingGate? {
        bankingPolicy.gate(for: "fints:" + blz.trimmingCharacters(in: .whitespaces))
    }

    /// Shows the reason and returns false if adding is blocked.
    func bankingAllowsAdding(_ gate: BankingGate?) -> Bool {
        guard let gate else { return true }
        showError(gate.title, gate.detail)
        return false
    }

    /// Every bank contact is blocked (e.g. "Alle abrufen" makes no sense).
    var bankingBlocksAll: Bool { bankingPolicy.gate?.all ?? false }
}

// MARK: - "Jetzt aktualisieren"

enum BankingUpdateAction {
    /// App Store ID of GiroCrab, once it is in the (Mac) App Store.
    static let appStoreID: String? = nil
    static let releasesURL = URL(string: "https://github.com/GiroCrab/GiroCrab/releases/latest")!

    /// Developer ID with Sparkle: the update dialog. App Store / iOS: the
    /// store page. Otherwise (forks, builds without updater): the releases
    /// page – the button never does nothing.
    @MainActor
    static func perform(openURL: OpenURLAction) {
        switch BankingDistribution.current {
        case .developerID where AppUpdates.shared.isAvailable:
            AppUpdates.shared.checkForUpdates()
        case .macAppStore:
            openURL(appStoreID.flatMap { URL(string: "macappstore://apps.apple.com/app/id\($0)") } ?? releasesURL)
        case .iOS:
            openURL(appStoreID.flatMap { URL(string: "itms-apps://apps.apple.com/app/id\($0)") } ?? releasesURL)
        default:
            openURL(releasesURL)
        }
    }
}

// MARK: - Banner above the booking list

/// Not dismissable while the block lasts; reading stays possible, so a banner
/// instead of a modal alert (HIG: alerts only for what needs a decision now).
struct BankingPolicyBanner: View {
    @Environment(\.openURL) private var openURL
    @State private var checking = false

    var body: some View {
        VStack(spacing: 0) {
            if let gate = BankingPolicy.shared.gate {
                content(gate)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 8)
                    .frame(maxWidth: 720)
                    .frame(maxWidth: .infinity)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.default, value: BankingPolicy.shared.gate)
        // First check right after launch, so the banner is there before the
        // user tries to sync.
        .task { await BankingPolicy.shared.refresh() }
    }

    private func content(_ gate: BankingGate) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: "exclamationmark.shield.fill")
                .foregroundStyle(.red)
                .appFont(.title3)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 8) {
                Text(gate.title)
                    .appFont(.headline)
                Text(gate.detail)
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    if gate.reason == .checkUnavailable {
                        Button {
                            checking = true
                            Task {
                                await BankingPolicy.shared.refresh(force: true)
                                checking = false
                            }
                        } label: {
                            Text("Erneut prüfen")
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(checking)
                        if checking { ProgressView().controlSize(.small) }
                    } else {
                        // "Ein Update ist in Vorbereitung": no action that cannot succeed yet.
                        if gate.updateAvailable {
                            Button("Jetzt aktualisieren") { BankingUpdateAction.perform(openURL: openURL) }
                                .buttonStyle(.borderedProminent)
                        }
                        if let advisory = gate.advisory {
                            Button("Details") { openURL(advisory) }
                                .help(advisory.absoluteString)
                        }
                    }
                }
                .controlSize(.regular)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.red.opacity(0.08))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.red.opacity(0.35), lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Einstellungen › Sicherheit

/// Open documentation of the feature (App Review Guideline 2.5.19: no hidden
/// functions). Shown in every build; "Sicherheit" exists everywhere, the
/// update pane does not (App Store).
struct BankingPolicySection: View {
    private let docsURL = URL(string: "https://github.com/GiroCrab/GiroCrab/blob/main/docs/banking-policy.md")!

    var body: some View {
        let policy = BankingPolicy.shared
        Section {
            Text("GiroCrab prüft regelmäßig, ob diese Version noch sicher mit Banken kommunizieren darf. Enthält sie eine Sicherheitslücke oder einen schweren Fehler im Bankzugang, sind Abrufe und Überweisungen bis zum Update gesperrt. Ihre Daten bleiben lesbar und exportierbar. Dafür lädt GiroCrab höchstens alle 15 Minuten eine signierte Datei von GitHub; dabei werden keine persönlichen Daten übertragen.")
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            LabeledContent("Status") {
                Text(statusText(policy))
            }
            if policy.isEnabled {
                LabeledContent("Letzte Prüfung") {
                    if let last = policy.lastSuccessfulCheck {
                        Text(last, style: .relative)
                    } else {
                        Text("Noch nicht geprüft")
                    }
                }
                if let failure = policy.lastFailure {
                    LabeledContent("Letzter Fehler") {
                        Text(failure).foregroundStyle(.secondary)
                    }
                }
                if let seq = policy.document?.sequence {
                    LabeledContent("Richtlinie", value: String(localized: "Stand \(seq)"))
                }
            }
            if BankingPolicy.usesDebugOverrides {
                Label("Debug-Build: Adresse, Schlüssel, Version oder Build-Datum sind per Umgebungsvariable überschrieben.", systemImage: "hammer")
                    .appFont(.caption)
                    .foregroundStyle(.orange)
            }
            HStack {
                if policy.isEnabled {
                    Button("Jetzt prüfen") { Task { await policy.refresh(force: true) } }
                }
                Spacer()
                Link("Mehr erfahren", destination: docsURL)
            }
        } header: {
            Text("Sicherheitssperre für Bankverbindungen").appSectionHeader()
        }
    }

    private func statusText(_ policy: BankingPolicy) -> String {
        guard policy.isEnabled else {
            return String(localized: "Nicht aktiv (kein Prüfschlüssel in diesem Build)")
        }
        guard let gate = policy.gate else {
            if policy.reliesOnBundledPolicy, let until = policy.bundledPolicyValidUntil {
                // GitHub not reachable yet: the policy shipped with this version counts
                return String(localized: "Keine Sperre (Stand dieser Version, bis \(until.formatted(date: .abbreviated, time: .omitted)))")
            }
            return policy.lastSuccessfulCheck == nil && !policy.verifiedOnline
                ? String(localized: "Wird geprüft …")
                : String(localized: "Keine Sperre")
        }
        switch gate.reason {
        case .checkUnavailable: return String(localized: "Prüfung nicht möglich")
        case .outdated:
            return gate.all
                ? String(localized: "Alle Bankverbindungen gesperrt")
                : String(localized: "Teilweise gesperrt")
        }
    }
}
