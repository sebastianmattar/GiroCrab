// test: serial-defaults
import XCTest
@testable import GiroCrabUI

/// `swift test --parallel` runs several processes at once; with one shared
/// `UserDefaults.standard` they overwrote each other's settings, so tests like
/// `testViewChoiceAndCollapseStatePersist` failed at random. Each test process
/// gets its own store, the app keeps `.standard`.
final class AppDefaultsTests: XCTestCase {
    func testAppUsesTheStandardStore() {
        XCTAssertNil(AppDefaults.suiteName(unitTests: false, pid: 4242))
    }

    func testEachTestProcessGetsItsOwnStore() {
        XCTAssertEqual(AppDefaults.suiteName(unitTests: true, pid: 4242), "GiroCrabTests-4242")
        XCTAssertNotEqual(AppDefaults.suiteName(unitTests: true, pid: 1), AppDefaults.suiteName(unitTests: true, pid: 2))
    }

    func testTheTestStoreStaysOutOfThePreferencesFolder() throws {
        let path = try XCTUnwrap(AppDefaults.testStorePath())
        XCTAssertTrue(path.hasPrefix(NSTemporaryDirectory()), path)
        XCTAssertFalse(path.contains("Library/Preferences"), path)
    }

    func testThisTestRunIsIsolated() {
        XCTAssertFalse(AppDefaults.store === UserDefaults.standard)
        AppDefaults.store.set("x", forKey: "AppDefaultsTests.probe")
        XCTAssertNil(UserDefaults.standard.string(forKey: "AppDefaultsTests.probe"))
        AppDefaults.store.removeObject(forKey: "AppDefaultsTests.probe")
    }
}
