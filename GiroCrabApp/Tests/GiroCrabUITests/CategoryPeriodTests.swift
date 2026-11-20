// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Per-category default period (#425).
@MainActor
final class CategoryPeriodTests: XCTestCase {
    private let key = "period.byCategory"

    override func setUp() { AppDefaults.store.removeObject(forKey: key) }
    override func tearDown() { AppDefaults.store.removeObject(forKey: key) }

    func testStorageKeyRoundTrip() {
        let now = Date()
        let menu = Period.menu(now: now).flatMap { $0 } + [.days(7)]
        for p in menu {
            let k = try? XCTUnwrap(p.storageKey(now: now))
            XCTAssertEqual(k.flatMap { Period(storageKey: $0, now: now) }, p)
        }
        XCTAssertNil(Period.custom.storageKey())
        XCTAssertNil(Period(storageKey: "year:9"))
        XCTAssertNil(Period(storageKey: "bogus"))
    }

    func testCategoryKeepsItsOwnPeriod() throws {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let ids = model.categories.map(\.id)
        let (a, b) = (try XCTUnwrap(ids.first), try XCTUnwrap(ids.dropFirst().first))
        let year = Period.menu()[4][0]

        model.sidebarSelection = .category(a)
        model.interactivePeriod = year
        model.sidebarSelection = .category(b)
        model.interactivePeriod = .all
        model.sidebarSelection = .category(a)
        XCTAssertEqual(model.period, year)
        model.sidebarSelection = .category(b)
        XCTAssertEqual(model.period, .all)
        // Without a saved choice the current period stays.
        model.sidebarSelection = .allAccounts
        model.interactivePeriod = .months12
        XCTAssertEqual(model.period, .months12)
    }
}
