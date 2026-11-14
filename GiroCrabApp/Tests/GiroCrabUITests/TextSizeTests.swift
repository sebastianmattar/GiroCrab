import XCTest
import SwiftUI
@testable import GiroCrabUI

/// App-wide text size (GitHub #343).
final class TextSizeTests: XCTestCase {
    func testStepsAndMenuNavigation() {
        XCTAssertEqual(AppTextSize.allCases.map(\.scale), [1, 1.15, 1.3])
        XCTAssertEqual(AppTextSize.standard.larger, .large)
        XCTAssertEqual(AppTextSize.larger.larger, nil)
        XCTAssertEqual(AppTextSize.large.smaller, .standard)
        XCTAssertEqual(AppTextSize.standard.smaller, nil)
    }

    func testUnknownStoredValueFallsBackToStandard() {
        XCTAssertNil(AppTextSize(rawValue: "huge"))
    }

    func testScalesMacTextStyles() {
        // Standard = the system sizes of NSFont.preferredFont(forTextStyle:)
        XCTAssertEqual(Font.TextStyle.body.macSize(scale: 1), 13)
        XCTAssertEqual(Font.TextStyle.headline.macMetrics.weight, .bold)
        // caption 10 pt (HIG minimum) → 12 → 13 pt
        XCTAssertEqual(AppTextSize.allCases.map { Font.TextStyle.caption.macSize(scale: $0.scale) }, [10, 12, 13])
        XCTAssertEqual(AppTextSize.allCases.map { Font.TextStyle.body.macSize(scale: $0.scale) }, [13, 15, 17])
        XCTAssertEqual(AppTextSize.allCases.map { Font.TextStyle.largeTitle.macSize(scale: $0.scale) }, [26, 30, 34])
    }
}
