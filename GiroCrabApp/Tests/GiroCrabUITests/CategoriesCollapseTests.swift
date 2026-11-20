// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
@testable import GiroCrabUI

/// The sidebar's category tree folds away under its header; the state is remembered.
@MainActor
final class CategoriesCollapseTests: XCTestCase {
    override func setUp() async throws {
        AppDefaults.store.removeObject(forKey: "sidebar.categoriesCollapsed")
    }

    override func tearDown() async throws {
        AppDefaults.store.removeObject(forKey: "sidebar.categoriesCollapsed")
    }

    func testCategoriesStartOpenAndCollapsedStateIsRemembered() {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertFalse(model.categoriesCollapsed)
        model.categoriesCollapsed = true
        let again = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertTrue(again.categoriesCollapsed)
    }
}
