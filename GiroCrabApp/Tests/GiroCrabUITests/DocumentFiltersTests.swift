import XCTest
import GiroCrabCore
@testable import GiroCrabUI

final class DocumentFiltersTests: XCTestCase {
    private func doc(_ id: Int64, _ type: String, _ date: String, seen: Bool = true) -> DocumentInfo {
        DocumentInfo(id: id, bookingId: nil, accountId: 1, title: "t\(id)", docType: type, mimeType: "application/pdf", fileName: "f.pdf",
                     sizeBytes: 1, sourceUrl: "", docDate: date, createdAt: 0, bookingLabel: "", accountName: "", seen: seen)
    }

    private var docs: [DocumentInfo] {
        [doc(1, "abrechnung", "2026-03-01"), doc(2, "abrechnung", "2025-12-31"), doc(3, "steuer", "2025-01-15"),
         doc(4, "postbox", "2024-06-01"), doc(5, "sonst", "")]
    }

    func testOnlyExistingTypesAndYearsAreOffered() {
        XCTAssertEqual(DocumentFilters.types(of: docs), ["abrechnung", "steuer", "postbox", "sonst"])
        XCTAssertEqual(DocumentFilters.years(of: docs), [2026, 2025, 2024])
    }

    func testTypeAndYearNarrowTheListTogether() {
        XCTAssertEqual(DocumentFilters.apply(docs, type: "abrechnung", year: nil).map(\.id), [1, 2])
        XCTAssertEqual(DocumentFilters.apply(docs, type: nil, year: 2025).map(\.id), [2, 3])
        XCTAssertEqual(DocumentFilters.apply(docs, type: "abrechnung", year: 2025).map(\.id), [2])
        XCTAssertEqual(DocumentFilters.apply(docs, type: nil, year: nil).count, 5)
    }

    func testUnseenOnlyKeepsTheOpenDocument() {
        let list = [doc(1, "abrechnung", "2026-03-01", seen: false), doc(2, "abrechnung", "2026-02-01"),
                    doc(3, "steuer", "2026-01-01", seen: false)]
        XCTAssertEqual(DocumentFilters.apply(list, type: nil, year: nil, unseenOnly: true).map(\.id), [1, 3])
        // the open (just read) document stays in the list
        XCTAssertEqual(DocumentFilters.apply(list, type: nil, year: nil, unseenOnly: true, keeping: 2).map(\.id), [1, 2, 3])
        XCTAssertEqual(DocumentFilters.apply(list, type: "steuer", year: nil, unseenOnly: true).map(\.id), [3])
        XCTAssertEqual(DocumentFilters.apply(list, type: nil, year: nil).count, 3)
    }
}

/// Bank connection filter of "Belege & PostBox" (#1042).
@MainActor
final class DocumentsConnectionFilterTests: XCTestCase {
    private func model() -> AppModel {
        AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
    }

    func testConnectionFilterNarrowsAccountMenuAndResetsForeignAccount() throws {
        let model = model()
        let withConnection = model.accountsByID.values.filter { $0.connectionId != nil }
        let first = try XCTUnwrap(withConnection.first)
        let cid = try XCTUnwrap(first.connectionId)
        model.selectDocumentsConnection(cid)
        let offered = model.documentsAccountSections.flatMap(\.accounts)
        XCTAssertFalse(offered.isEmpty)
        XCTAssertTrue(offered.allSatisfy { $0.connectionId == cid })

        // an account of the chosen connection stays selected …
        model.documentsAccountFilter = first.id
        model.selectDocumentsConnection(cid)
        XCTAssertEqual(model.documentsAccountFilter, first.id)
        // … one of another connection (or "Ohne Konto") is reset
        if let other = withConnection.first(where: { $0.connectionId != cid }) {
            model.documentsAccountFilter = other.id
            model.selectDocumentsConnection(cid)
            XCTAssertNil(model.documentsAccountFilter)
        }
        model.documentsAccountFilter = AppModel.documentsWithoutAccount
        model.selectDocumentsConnection(cid)
        XCTAssertNil(model.documentsAccountFilter)
    }

    func testWithoutAccountEntryShowsOnlyDocumentsWithoutAccount() {
        let model = model()
        model.selectDocumentsConnection(AppModel.documentsWithoutAccount)
        XCTAssertTrue(model.allDocuments(connectionId: model.documentsConnectionFilter).allSatisfy { $0.accountId == nil })
        // the full account menu is untouched by "Ohne Konto"
        XCTAssertEqual(model.documentsAccountSections.count, model.accountMenuSections.count)
    }

    func testListOnlyHoldsDocumentsOfTheChosenConnection() throws {
        let model = model()
        let cid = try XCTUnwrap(model.accountsByID.values.compactMap(\.connectionId).first)
        let docs = model.allDocuments(connectionId: cid)
        XCTAssertTrue(docs.allSatisfy { d in d.accountId.flatMap { model.accountsByID[$0]?.connectionId } == cid })
    }
}
