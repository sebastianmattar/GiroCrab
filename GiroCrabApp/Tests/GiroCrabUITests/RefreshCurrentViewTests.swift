import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// Pull-to-refresh picks the sync that fits the current selection.
@MainActor
final class RefreshCurrentViewTests: XCTestCase {
    private var model: AppModel!
    private var started: [AppModel.RefreshSyncAction] = []

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        started = []
        model.refreshSyncOverride = { [unowned self] in self.started.append($0) }
    }

    override func tearDown() async throws { model = nil }

    func testAllBookingsAndCategoryStartSyncAll() async {
        model.sidebarSelection = .allAccounts
        await model.refreshCurrentView()
        model.sidebarSelection = .uncategorized
        await model.refreshCurrentView()
        XCTAssertEqual(started, [.all, .all])
    }

    func testUserGroupStartsGroupSync() async {
        model.sidebarSelection = .userGroup("g-1")
        await model.refreshCurrentView()
        XCTAssertEqual(started, [.group("g-1")])
    }

    func testSingleAccountSyncsOnlyThatAccount() async throws {
        let account = try XCTUnwrap(model.allAccounts.first { acc in
            acc.connectionId.flatMap { cid in model.connections.first { $0.id == cid } }.map { !model.usesPortal($0) } ?? false
        })
        model.sidebarSelection = .account(account.id)
        await model.refreshCurrentView()
        XCTAssertEqual(started, [.account(account.id)])
    }

    /// A failed "<Konto> abrufen" is retried for the account only (#1124);
    /// "Abrufen" of the login fetches all of it again.
    func testFailedAccountSyncKeepsItsAccountUntilAFullSync() throws {
        let account = try XCTUnwrap(model.allAccounts.first { acc in
            acc.connectionId.flatMap { cid in model.connections.first { $0.id == cid } }.map { !model.usesPortal($0) } ?? false
        })
        let connection = try XCTUnwrap(model.connections.first { $0.id == account.connectionId })
        model.syncAccountID = account.id
        model.syncingConnectionID = connection.id
        model.finishSync(error: NSError(domain: "test", code: 1))
        XCTAssertEqual(model.syncAccountID, account.id)
        model.requestFullSync(connection)
        XCTAssertNil(model.syncAccountID)
    }

    func testAccountWithoutConnectionDoesNotSync() async throws {
        guard let account = model.allAccounts.first(where: { $0.connectionId == nil }) else {
            throw XCTSkip("demo vault has no import-only account")
        }
        model.sidebarSelection = .account(account.id)
        await model.refreshCurrentView()
        XCTAssertTrue(started.isEmpty)
    }

    func testNoDuplicateWhileSyncRuns() async {
        model.sidebarSelection = .allAccounts
        model.syncingConnectionID = 1
        let task = Task { await model.refreshCurrentView() }
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(started.isEmpty)
        model.syncingConnectionID = nil   // the running sync ends: refresh returns
        await task.value
        XCTAssertTrue(started.isEmpty)
    }
}
