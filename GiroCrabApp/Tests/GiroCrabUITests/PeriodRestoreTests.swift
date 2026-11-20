// test: serial-defaults (shares UserDefaults.standard across xctest processes)
import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Last chosen period survives a relaunch; „Filter zurücksetzen“ keeps it (#840).
@MainActor
final class PeriodRestoreTests: XCTestCase {
    private let key = AppModel.lastPeriodKey

    override func setUp() { AppDefaults.store.removeObject(forKey: key) }
    override func tearDown() { AppDefaults.store.removeObject(forKey: key) }

    func testChosenPeriodIsRestoredOnLaunch() {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let lastMonth = Period.previousMonth()
        model.interactivePeriod = lastMonth
        XCTAssertEqual(AppModel(options: LaunchOptions(arguments: [], environment: [:])).period, lastMonth)

        model.applyCustomPeriod(from: Date(timeIntervalSinceNow: -86_400 * 10), to: Date())
        XCTAssertEqual(AppModel(options: LaunchOptions(arguments: [], environment: [:])).period, .custom)
    }

    func testWithoutSavedChoiceAndInDemoStartsWithAll() {
        XCTAssertEqual(AppModel(options: LaunchOptions(arguments: [], environment: [:])).period, .all)
        AppDefaults.store.set("months12", forKey: key)
        // demo starts stay reproducible for screenshots
        XCTAssertEqual(AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:])).period, .all)
        AppDefaults.store.set("year:9", forKey: key)
        XCTAssertEqual(AppModel(options: LaunchOptions(arguments: [], environment: [:])).period, .all)
    }

    func testResetClearsListFiltersButKeepsPeriod() {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        model.interactivePeriod = .months12
        let all = model.bookingCountLabel
        XCTAssertFalse(model.hasActiveListFilters)

        model.statusFilter = .checked
        model.directionFilter = .expense
        model.amountFilter = AmountFilter(minCents: 100_000, maxCents: nil)
        model.searchText = "zzz"
        XCTAssertTrue(model.hasActiveListFilters)

        model.resetListFilters()
        XCTAssertFalse(model.hasActiveListFilters)
        XCTAssertEqual(model.period, .months12)
        XCTAssertEqual(model.bookingCountLabel, all)
    }
}
