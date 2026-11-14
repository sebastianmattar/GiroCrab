import SwiftUI

// Quiet project news, see Announcements.swift. HIG decisions:
// * Banner, not alert: nothing interrupts, nothing blocks the window.
// * Standard system look (material, secondary text, system symbols); the one
//   difference for "important" is the accent-coloured symbol – no red, red is
//   reserved for the security lock.
// * Always dismissible with a labelled close button; "Mehr erfahren" opens a
//   sheet with the full text, never a second window or the browser unasked.
// * Not shown while a security lock is visible: one message at a time.

private func markdown(_ s: String) -> AttributedString {
    (try? AttributedString(
        markdown: s,
        options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
    )) ?? AttributedString(s)
}

struct AnnouncementBanner: View {
    @State private var center = AnnouncementCenter.shared
    @State private var detail: Announcement?

    var body: some View {
        VStack(spacing: 0) {
            // One message at a time: the security lock always wins.
            if BankingPolicy.shared.gate == nil, let a = center.banner {
                content(a)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 8)
                    .transition(.opacity)
            }
        }
        .animation(.default, value: center.banner?.id)
        .task { await center.refresh() }
        .sheet(item: $detail) { a in
            AnnouncementSheet(announcement: a, language: center.languageCode)
        }
    }

    private func content(_ a: Announcement) -> some View {
        let lang = center.languageCode
        let summary = a.localized(a.summary, language: lang)
        let hasMore = !a.localized(a.body, language: lang).isEmpty || a.link != nil
        return HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: a.severity == .important ? "info.circle.fill" : "info.circle")
                .foregroundStyle(a.severity == .important ? Color.accentColor : Color.secondary)
                .appFont(.title3)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(a.localized(a.title, language: lang)).appFont(.headline)
                if !summary.isEmpty {
                    Text(markdown(summary))
                        .appFont(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if hasMore {
                    Button("Mehr erfahren") { detail = a }
                        .linkButtonStyle()
                        .padding(.top, 2)
                }
            }
            Spacer(minLength: 0)
            Button {
                center.dismiss(a)
            } label: {
                Image(systemName: "xmark")
                    .appFont(.caption, weight: .semibold)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Mitteilung ausblenden")
            .help("Mitteilung ausblenden")
        }
        .padding(12)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

struct AnnouncementSheet: View {
    let announcement: Announcement
    let language: String
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        let body = announcement.localized(announcement.body, language: language)
        let summary = announcement.localized(announcement.summary, language: language)
        VStack(alignment: .leading, spacing: 16) {
            Text(announcement.localized(announcement.title, language: language))
                .appFont(.title2, weight: .semibold)
            ScrollView {
                Text(markdown(body.isEmpty ? summary : body))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
            HStack {
                if let link = announcement.link {
                    Button("Mehr im Web") { openURL(link) }
                }
                Spacer()
                Button("OK") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .desktopFrame(minWidth: 380, idealWidth: 460, minHeight: 220, idealHeight: 320)
        #if os(iOS)
        .presentationDetents([.medium, .large])
        #endif
    }
}

/// Hilfe › Mitteilungen: everything that applies to this build, including what
/// was dismissed, so nobody has to fear having lost something.
struct AnnouncementsListView: View {
    @State private var center = AnnouncementCenter.shared
    @Environment(\.openURL) private var openURL

    var body: some View {
        let lang = center.languageCode
        Group {
            if center.all.isEmpty {
                ContentUnavailableView(
                    "Keine Mitteilungen",
                    systemImage: "tray",
                    description: Text(center.isEnabled
                        ? "Es gibt derzeit nichts Neues vom GiroCrab-Projekt."
                        : "Mitteilungen sind in den Einstellungen ausgeschaltet.")
                )
            } else {
                List(center.all) { a in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(a.localized(a.title, language: lang)).appFont(.headline)
                        let text = a.localized(a.body, language: lang).isEmpty
                            ? a.localized(a.summary, language: lang)
                            : a.localized(a.body, language: lang)
                        Text(markdown(text)).foregroundStyle(.secondary).textSelection(.enabled)
                        if let link = a.link {
                            Button("Mehr im Web") { openURL(link) }.linkButtonStyle()
                        }
                    }
                    .padding(.vertical, 4)
                }
            }
        }
        .desktopFrame(minWidth: 380, minHeight: 280)
        .task { await center.refresh() }
    }
}

private extension View {
    /// `.buttonStyle(.link)` is macOS-only; on iOS use a plain, tinted button.
    @ViewBuilder func linkButtonStyle() -> some View {
        #if os(macOS)
        self.buttonStyle(.link)
        #else
        self.buttonStyle(.plain).foregroundStyle(.tint)
        #endif
    }
}
