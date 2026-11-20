// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
@testable import GiroCrabUI

/// Which sidebar categories are open is remembered across launches (#633).
@MainActor
final class CategoryTreeExpansionTests: XCTestCase {
    override func setUp() async throws {
        clearStoredState()
    }

    override func tearDown() async throws {
        clearStoredState()
    }

    private func clearStoredState() {
        for demo in [false, true] {
            AppDefaults.store.removeObject(forKey: AppModel.expandedCategoriesKey(demo: demo))
        }
    }

    private func demoModel() -> AppModel {
        AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
    }

    func testExpandedCategoriesAreRemembered() throws {
        let model = demoModel()
        XCTAssertTrue(model.expandedCategories.isEmpty, "a fresh install starts folded")
        let parent = try XCTUnwrap(model.categoryTree.first { $0.children != nil }, "demo vault has sub-categories")
        model.expandedCategories.insert(parent.id)

        let again = demoModel()
        XCTAssertEqual(again.expandedCategories, [parent.id])

        again.expandedCategories.remove(parent.id)
        XCTAssertTrue(demoModel().expandedCategories.isEmpty)
    }

    /// Moving into a sub-category also opens the main category above it.
    func testMoveOpensEveryParentAbove() throws {
        let model = demoModel()
        let tree = model.categoryTree
        let main = try XCTUnwrap(tree.first { $0.children != nil })
        let target = try XCTUnwrap(main.children?.first)
        let moved = try XCTUnwrap(tree.last { $0.id != main.id }?.children?.first)

        model.moveCategory(moved.id, to: target.id)
        XCTAssertEqual(model.expandedCategories, [main.id, target.id])
    }

    /// Category ids overlap between the demo and the own vault: each keeps its own set.
    func testDemoAndOwnVaultKeepSeparateSets() {
        let model = demoModel()
        model.expandedCategories = [1]
        model.demoActive = false
        XCTAssertTrue(model.expandedCategories.isEmpty)
        model.expandedCategories = [2]
        model.demoActive = true
        XCTAssertEqual(model.expandedCategories, [1])
        XCTAssertEqual(AppModel.storedExpandedCategories(demo: false), [2])
    }
}
