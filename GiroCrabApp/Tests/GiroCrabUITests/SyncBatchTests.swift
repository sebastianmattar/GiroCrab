import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// "Alle abrufen": cancelling one login's prompt skips it instead of ending
/// the whole batch (demo vault: every login asks for a TAN).
@MainActor
final class SyncBatchTests: XCTestCase {
    private var model: AppModel!

    override func setUp() async throws {
        model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        XCTAssertEqual(model.vaultState, .open)
    }

    override func tearDown() async throws {
        model.cancelAllSync()
        model = nil
    }

    /// The logins "Alle abrufen" works through, in order.
    private var batch: [BankConnection] {
        model.connections.filter { !model.usesPortal($0) && !$0.allAccountsArchived }
    }

    private func waitUntil(_ what: String, timeout: Double = 5,
                           file: StaticString = #filePath, line: UInt = #line,
                           _ condition: () -> Bool) async throws {
        let end = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > end { XCTFail("Timed out waiting for \(what)", file: file, line: line); return }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    private func waitForChallenge(of c: BankConnection, file: StaticString = #filePath, line: UInt = #line) async throws {
        try await waitUntil("TAN prompt of \(c.displayName)", file: file, line: line) {
            model.activeChallenge?.connection.id == c.id
        }
    }

    func testSkippingGoesOnWithTheNextLoginAndNamesTheSkippedOnes() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()

        try await waitForChallenge(of: logins[0])
        XCTAssertEqual(model.syncBatchProgress?.current, 1)
        XCTAssertEqual(model.syncBatchProgress?.total, logins.count)
        XCTAssertTrue(model.canSkipInSyncBatch)
        model.skipCurrentSync()

        try await waitForChallenge(of: logins[1])
        XCTAssertEqual(model.syncBatchProgress?.current, 2)
        model.skipCurrentSync()

        for (i, c) in logins.enumerated().dropFirst(2) {
            try await waitForChallenge(of: c)
            XCTAssertEqual(model.syncBatchProgress?.current, i + 1)
            XCTAssertEqual(model.canSkipInSyncBatch, i < logins.count - 1)
            model.skipCurrentSync()   // on the last one: "Abbrechen" ends the batch
        }

        try await waitUntil("closing notice") { model.toast?.contains(logins[0].displayName) == true }
        for c in logins { XCTAssertTrue(model.toast?.contains(c.displayName) == true, c.displayName) }
        XCTAssertNil(model.syncBatchProgress)
        XCTAssertFalse(model.canSkipInSyncBatch)
        XCTAssertNil(model.syncingConnectionID)
    }

    func testCancelAllEndsTheBatch() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 2, "demo vault needs at least two logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])

        model.cancelAllSync()
        XCTAssertNil(model.activeChallenge)
        XCTAssertNil(model.syncBatchProgress)
        XCTAssertFalse(model.canSkipInSyncBatch)

        // no further login is asked for
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertNil(model.activeChallenge)
        XCTAssertNil(model.pendingLogin)
        XCTAssertNil(model.setupRequest)
    }

    /// A failed login ends only itself (#1041): its alert shows right away, and the
    /// batch goes on with the next login only once the alert is closed ("Überspringen").
    func testAFailedLoginWaitsForTheAlertAndThenGoesOn() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.Network(message: "Zeitüberschreitung"))

        XCTAssertTrue(model.errorTitle?.contains(logins[0].displayName) == true)
        XCTAssertEqual(model.errorMessage?.contains("Zeitüberschreitung"), true)
        XCTAssertNotNil(model.errorRetry)
        XCTAssertEqual(model.errorCloseTitle, "Überspringen")
        XCTAssertLessThanOrEqual(model.errorAlternatives.count + (model.errorRetry == nil ? 1 : 2), 3, "macOS shows at most three buttons")
        XCTAssertNotNil(model.syncBatchProgress, "the batch is kept")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertNil(model.activeChallenge, "nothing goes on while the alert is open")

        model.errorMessage = nil   // "Überspringen"
        try await waitForChallenge(of: logins[1])
        XCTAssertEqual(model.syncBatchProgress?.current, 2)
    }

    /// "Eigene Registrierung eintragen …" opens Settings: the waiting batch ends
    /// instead of stalling with a queue nobody works through.
    func testEnteringTheOwnRegistrationEndsTheWaitingBatch() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.Network(message: "x"))
        model.errorOnClose = nil
        model.errorMessage = nil
        model.enterOwnProductID()
        XCTAssertNil(model.syncBatchProgress)
        XCTAssertTrue(model.syncQueue.isEmpty)
    }

    /// "Erneut versuchen" in the error alert fetches the failed login again, and the
    /// batch goes on after it.
    func testRetryInTheErrorAlertGoesOnWithTheBatch() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.Network(message: "x"))
        let retry = try XCTUnwrap(model.errorRetry)
        model.errorOnClose = nil   // what the button does before retrying
        model.errorMessage = nil
        retry()

        try await waitForChallenge(of: logins[0])
        XCTAssertEqual(model.syncBatchProgress?.current, 1, "the same login again")
        model.skipCurrentSync()
        try await waitForChallenge(of: logins[1])
        XCTAssertEqual(model.syncBatchProgress?.current, 2)
    }

    /// A missing setting opens the setup form and keeps the batch: cancelling the
    /// form skips this login, completing it fetches the login and goes on.
    func testSetupFormKeepsTheBatch() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.MissingSetting(key: "x"))
        XCTAssertEqual(model.setupRequest?.connection.id, logins[0].id)
        XCTAssertNotNil(model.syncBatchProgress, "the batch waits for the form")
        model.cancelSetupRequest()
        try await waitForChallenge(of: logins[1])

        model.finishSync(error: CoreError.MissingSetting(key: "x"))
        let request = try XCTUnwrap(model.setupRequest)
        model.completeSetup(request, values: [:])
        try await waitForChallenge(of: logins[1])
        XCTAssertEqual(model.syncBatchProgress?.current, 2, "the same login again")
        model.skipCurrentSync()
        try await waitForChallenge(of: logins[2])
    }

    /// "Alle abbrechen" in the error alert ends the batch.
    func testCancelAllInTheErrorAlertEndsTheBatch() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.Network(message: "x"))
        let cancelAll = try XCTUnwrap(model.errorAlternatives.last)
        model.errorOnClose = nil
        model.errorMessage = nil
        cancelAll.action()
        XCTAssertNil(model.syncBatchProgress)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertNil(model.activeChallenge)
    }

    /// A login that needs the browser (Trade Republic) keeps the batch: cancelling
    /// the browser window skips it and goes on with the next login (#1041).
    func testCancellingTheBrowserLoginGoesOnWithTheNextLogin() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        model.requestSyncAll()
        try await waitForChallenge(of: logins[0])
        model.finishSync(error: CoreError.BrowserLoginRequired(message: "Bitte im Browser anmelden"))
        XCTAssertEqual(model.webLoginConnection?.id, logins[0].id)
        XCTAssertNotNil(model.syncBatchProgress, "the batch waits for the browser login")

        model.cancelWebLogin()
        try await waitForChallenge(of: logins[1])
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(model.syncBatchProgress?.current, 2)
    }

    /// Trade Republic with an expired session and no stored PIN (#1128): the core asks
    /// for the PIN, so the PIN sheet opens (with its browser login) instead of an alert
    /// whose "Erneut versuchen" fails the same way. In a batch it waits for the sheet.
    func testExpiredTradeRepublicSessionWithoutStoredPinOpensThePinSheet() async throws {
        let logins = batch
        let i = try XCTUnwrap(logins.dropLast().firstIndex { $0.connector == .tradeRepublic },
                              "demo vault needs a Trade Republic login that is not the last one")
        XCTAssertFalse(model.connectionStatus(logins[i].id)?.hasPin ?? false)
        model.requestSyncAll()
        for l in logins[..<i] {
            try await waitForChallenge(of: l)
            model.skipCurrentSync()
        }
        try await waitForChallenge(of: logins[i])
        model.failedSyncSecret = ""
        model.finishSync(error: CoreError.Authentication(message: "Sitzung abgelaufen"))
        XCTAssertEqual(model.pendingLogin?.connection.id, logins[i].id)
        XCTAssertNil(model.errorMessage)
        XCTAssertNotNil(model.syncBatchProgress, "the batch waits for the PIN sheet")

        model.cancelPendingLogin()
        try await waitForChallenge(of: logins[i + 1])
        XCTAssertNil(model.errorMessage)
    }

    /// A PIN typed in the sheet (or a stored one) that Trade Republic refuses stays an error.
    func testRefusedTradeRepublicPinStaysAnError() async throws {
        let tr = try XCTUnwrap(model.connections.first { $0.connector == .tradeRepublic })
        model.syncingConnectionID = tr.id
        model.failedSyncSecret = "1234"
        model.finishSync(error: CoreError.Authentication(message: "PIN falsch"))
        XCTAssertNil(model.pendingLogin)
        XCTAssertNotNil(model.errorMessage)
        model.errorMessage = nil

        model.setConnectionPin(tr.id, "1234")
        defer { model.setConnectionPin(tr.id, nil) }
        model.syncingConnectionID = tr.id
        model.finishSync(error: CoreError.Authentication(message: "Sitzung abgelaufen"))
        XCTAssertNil(model.pendingLogin)
        XCTAssertNotNil(model.errorMessage)
    }

    /// Errors answered with a prompt are no failure of the batch: the prompt decides.
    func testErrorsWithAPromptAreNotCountedAsFailures() {
        XCTAssertTrue(AppModel.errorOpensPrompt(CoreError.BrowserLoginRequired(message: "")))
        XCTAssertTrue(AppModel.errorOpensPrompt(CoreError.MissingSetting(key: "iban")))
        XCTAssertFalse(AppModel.errorOpensPrompt(CoreError.Network(message: "")))
        XCTAssertFalse(AppModel.errorOpensPrompt(CoreError.Bank(code: "9050", message: "")))
    }

    /// "Weiter" and right after it "Überspringen": the bank's late answer for
    /// the skipped login must neither skip another login nor end the batch.
    func testLateAnswerOfASkippedLoginIsDropped() async throws {
        let logins = batch
        try XCTSkipIf(logins.count < 3, "demo vault needs at least three logins")
        let codeLogin = try XCTUnwrap(logins.dropLast().firstIndex { $0.connector == .tradeRepublic },
                                      "demo vault needs a code login that is not the last one")
        model.requestSyncAll()
        for c in logins[..<codeLogin] {
            try await waitForChallenge(of: c)
            model.skipCurrentSync()
        }
        try await waitForChallenge(of: logins[codeLogin])

        model.submitCode("1234")
        // let the code reach the bank, then skip while its answer is on the way
        try await waitUntil("code sent") { model.activeChallenge?.busy == true }
        await Task.yield()
        model.skipCurrentSync()

        let next = logins[codeLogin + 1]
        try await waitForChallenge(of: next)
        try await Task.sleep(for: .milliseconds(500))   // room for a late answer
        XCTAssertEqual(model.activeChallenge?.connection.id, next.id)
        XCTAssertEqual(model.syncBatchProgress?.current, codeLogin + 2)
        XCTAssertNil(model.errorMessage)
    }
}
