import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// "Nicht mehr vorschlagen" in the transfer form and "Empfänger verwalten" (#978).
@MainActor
final class KnownRecipientsTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws { model = nil }

    /// The demo pays "Hausverwaltung Nord" under two spellings to one IBAN: one suggestion.
    func testOneSuggestionPerIBAN() {
        let found = model.suggestRecipients("hausverwaltung")
        XCTAssertEqual(found.map(\.name), ["Hausverwaltung Nord GmbH"])
        XCTAssertEqual(found.first?.timesUsed, 3)
    }

    func testHiddenRecipientLeavesTheSuggestionsUntilShownAgain() async throws {
        let tim = try XCTUnwrap(model.suggestRecipients("hoffmann").first)
        model.setRecipientHidden(tim.iban, true)
        XCTAssertTrue(model.suggestRecipients("hoffmann").isEmpty)

        let read = try await model.knownRecipients("hoffmann")
        let listed = try XCTUnwrap(read)
        XCTAssertEqual(listed.map(\.iban), [tim.iban])
        XCTAssertEqual(listed.first?.hidden, true)

        model.setRecipientHidden(tim.iban, false)
        XCTAssertEqual(model.suggestRecipients("hoffmann").map(\.iban), [tim.iban])
    }
}
