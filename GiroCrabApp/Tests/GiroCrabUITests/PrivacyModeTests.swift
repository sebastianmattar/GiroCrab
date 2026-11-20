import XCTest
@testable import GiroCrabUI

/// Privacy mode (Ansicht › Privatmodus ⇧⌘H, GitHub #530): hides amounts
/// (or all personal data) on screen.
final class PrivacyModeTests: XCTestCase {
    func testPersonalRedactionImpliesAmounts() {
        XCTAssertEqual(ReportRedaction.amounts.effective, .amounts)
        XCTAssertEqual(ReportRedaction.personal.effective, [.personal, .amounts])
        XCTAssertEqual(
            ReportRedaction([.personal, .labels]).effective,
            [.personal, .amounts, .labels]
        )
    }

    func testPrivacyModeRedaction() {
        XCTAssertEqual(PrivacyMode.redaction(enabled: false, scope: .amounts), [])
        XCTAssertEqual(PrivacyMode.redaction(enabled: false, scope: .personal), [])
        XCTAssertEqual(PrivacyMode.redaction(enabled: true, scope: .amounts), .amounts)
        XCTAssertEqual(PrivacyMode.redaction(enabled: true, scope: .personal), [.personal, .amounts])
    }

    /// #1007: switching privacy mode on without picking a scope hides names and purposes too.
    func testDefaultScopeHidesPersonalData() {
        XCTAssertEqual(PrivacyMode.defaultScope, .personal)
        let redaction = PrivacyMode.redaction(enabled: true, scope: PrivacyMode.defaultScope)
        XCTAssertTrue(redaction.contains(.personal))
        XCTAssertTrue(redaction.contains(.amounts))
    }

    func testLaunchOptionsPrivacyFlags() {
        XCTAssertTrue(LaunchOptions(arguments: ["app"]).redaction.isEmpty)

        let amountsOnly = LaunchOptions(arguments: ["app", "--privacy"])
        XCTAssertEqual(amountsOnly.redaction, .amounts)

        let allPersonal = LaunchOptions(arguments: ["app", "--privacy=all"])
        XCTAssertEqual(allPersonal.redaction, [.personal, .amounts])
    }
}
