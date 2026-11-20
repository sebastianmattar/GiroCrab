import XCTest
@testable import GiroCrabUI

/// The demo banner must not claim an empty vault when the own vault holds data,
/// and names no bank count that goes stale when the demo changes.
final class DemoBannerTextTests: XCTestCase {
    func testOwnVaultWithDataIsNotCalledEmpty() {
        let text = DemoBanner.message(hasOwnVault: true, mayHoldRealData: true)
        XCTAssertFalse(text.contains("leer"), text)
        XCTAssertTrue(text.contains("unverändert"), text)
    }

    func testEmptyOwnVaultSaysSo() {
        XCTAssertTrue(DemoBanner.message(hasOwnVault: true, mayHoldRealData: false).contains("noch leer"))
    }

    /// #952: the banner says that changes to the throw-away demo vault are reset.
    func testResetNoteMentionsReset() {
        XCTAssertTrue(DemoBanner.resetNote.contains("zurückgesetzt"), DemoBanner.resetNote)
        XCTAssertTrue(DemoBanner.resetNote.contains("Beenden"), DemoBanner.resetNote)
    }

    func testNoBankCount() {
        for own in [true, false] {
            for real in [true, false] {
                let text = DemoBanner.message(hasOwnVault: own, mayHoldRealData: real)
                XCTAssertNil(text.range(of: #"\d+ Banken"#, options: .regularExpression), text)
            }
        }
    }
}
