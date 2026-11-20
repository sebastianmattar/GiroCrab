import XCTest
@testable import GiroCrabUI

/// Amount fields are prefilled with `Money.plain` and read back with `Money.parseCents`.
final class AmountInputTests: XCTestCase {
    func testPlainParsesBackIncludingNegativeAmounts() {
        // a negative Startsaldo ("−42,50") made the field invalid and greyed out "Sichern"
        for cents: Int64 in [0, 4_250, -4_250, 12_345_678, -12_345_678] {
            XCTAssertEqual(Money.parseCents(Money.plain(cents)), cents, Money.plain(cents))
        }
    }

    func testTypographicMinusAndEnDashCountAsMinus() {
        XCTAssertEqual(Money.parseCents("−42,50"), -4_250)
        XCTAssertEqual(Money.parseCents("–42,50"), -4_250)
        XCTAssertEqual(Money.parseCents("-42,50 €"), -4_250)
    }
}
