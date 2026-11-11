import SwiftUI
import PDFKit
import UniformTypeIdentifiers
import GiroCrabCore

/// PDF handed to `.fileExporter` (save / export).
struct PDFFileDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.pdf] }
    var data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

/// "Dokumente / Kontoauszüge" of one account: list, FinTS fetch (HKEKA), PDFKit preview, export.
struct AccountDocumentsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let account: Account
    @State private var preview: DocumentInfo?

    var body: some View {
        let docs = model.accountDocuments(account.id)
        let fetching = model.statementsAccountID == account.id
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Dokumente / Kontoauszüge").font(.title3.weight(.semibold))
                    Text(account.name).font(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
                if model.canFetchStatements(account) {
                    Button {
                        model.fetchStatements(for: account)
                        dismiss()
                    } label: {
                        Label("Kontoauszüge abrufen", systemImage: "arrow.down.doc")
                    }
                    .disabled(fetching || model.syncingConnectionID != nil)
                }
                Button("Fertig") { dismiss() }.keyboardShortcut(.defaultAction)
            }
            .padding()
            if model.canFetchStatements(account) {
                AutoStatementsToggle(account: account)
                    .padding(.horizontal)
                    .padding(.bottom, 10)
            }
            Divider()
            if docs.isEmpty {
                ContentUnavailableView(
                    "Keine Dokumente",
                    systemImage: "doc.text",
                    description: Text(model.canFetchStatements(account)
                        ? "Mit „Kontoauszüge abrufen“ lädt GiroCrab die PDF-Kontoauszüge per FinTS (ggf. mit TAN)."
                        : "Importierte oder abgelegte Dokumente dieses Kontos erscheinen hier.")
                )
            } else {
                List(docs) { d in
                    Button { preview = d } label: {
                        HStack(spacing: 10) {
                            Image(systemName: docSymbol(d)).foregroundStyle(Theme.accent).frame(width: 20)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(d.title).lineLimit(1)
                                Text("\(DateText.medium(d.docDate)) · \(ByteCountFormatter.string(fromByteCount: Int64(d.sizeBytes), countStyle: .file))")
                                    .font(.caption2).foregroundStyle(Theme.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(Theme.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 520, minHeight: 440)
        #endif
        .sheet(item: $preview) { d in DocumentPreviewSheet(doc: d) }
    }
}

/// Opt-in: fetch this account's PDF statements during regular syncs (GitHub #57).
struct AutoStatementsToggle: View {
    @Environment(AppModel.self) private var model
    let account: Account
    @State private var on = false

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Toggle("Kontoauszüge automatisch beim Sync abrufen", isOn: $on)
                .onChange(of: on) { _, new in model.setAutoStatements(account, new) }
            Text("Höchstens einmal pro Woche. Je nach Bank ist dafür eine zusätzliche TAN nötig. Ist ein Dokumentenordner eingestellt, landen die PDFs auch dort.")
                .font(.caption)
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onAppear { on = model.autoStatements(account) }
    }
}

/// Native PDFKit preview with save / export.
struct DocumentPreviewSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let doc: DocumentInfo
    @State private var data: Data?
    @State private var exporting = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(doc.title).font(.headline).lineLimit(1)
                Spacer()
                Button { exporting = true } label: { Label("Sichern …", systemImage: "square.and.arrow.down") }
                    .disabled(data == nil)
                Button("Schließen") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .padding()
            Divider()
            if let data {
                DocumentPreview(doc: doc, data: data)
            } else {
                ContentUnavailableView("Keine Vorschau", systemImage: "doc")
            }
        }
        #if os(macOS)
        .frame(minWidth: 640, minHeight: 720)
        #endif
        .task { data = model.documentData(doc) }
        .fileExporter(
            isPresented: $exporting,
            document: PDFFileDocument(data: data ?? Data()),
            contentType: .pdf,
            defaultFilename: doc.fileName.replacingOccurrences(of: ".pdf", with: "")
        ) { result in
            if case .failure(let e) = result { model.errorMessage = e.localizedDescription }
        }
    }
}
