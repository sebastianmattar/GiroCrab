import XCTest
import GiroCrabCore
@testable import GiroCrabUI

/// PSD2 consent expiry and the scheduled sync rules (Enable Banking, e.g. C24).
final class PSD2ConsentTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_794_355_200) // 2026-11-11 00:00 UTC

    func testParsesConnectorTimestamps() {
        // as written by the connector (chrono to_rfc3339) and as returned by Enable Banking
        XCTAssertNotNil(PSD2Consent.parse("2027-05-09T23:00:00.123456789+00:00"))
        XCTAssertNotNil(PSD2Consent.parse("2027-05-09T23:00:00+00:00"))
        XCTAssertNotNil(PSD2Consent.parse("2027-05-09T23:00:00Z"))
        XCTAssertNil(PSD2Consent.parse(""))
        XCTAssertNil(PSD2Consent.parse(nil))
        XCTAssertNil(PSD2Consent.parse("morgen"))
    }

    func testStages() {
        let day: TimeInterval = 86_400
        XCTAssertEqual(PSD2Consent.stage(until: nil, now: now), .none)
        XCTAssertEqual(PSD2Consent.stage(until: now.addingTimeInterval(170 * day), now: now), .valid)
        XCTAssertEqual(PSD2Consent.stage(until: now.addingTimeInterval(13.5 * day), now: now), .expiringSoon)
        XCTAssertEqual(PSD2Consent.stage(until: now.addingTimeInterval(-1), now: now), .expired)
        // the notification uses its own, shorter window
        XCTAssertEqual(PSD2Consent.stage(until: now.addingTimeInterval(10 * day), now: now, warnDays: 7), .valid)
        XCTAssertEqual(PSD2Consent.daysLeft(until: now.addingTimeInterval(13.5 * day), now: now), 13)
    }

    func testAutoSyncOnlyWithValidConsentAndAfterTheInterval() {
        let consent = now.addingTimeInterval(100 * 86_400)
        let unix = Int64(now.timeIntervalSince1970)
        // never synced -> due
        XCTAssertTrue(PSD2Consent.autoSyncDue(lastSyncUnix: nil, consentEnd: consent, hours: 6, now: now))
        // synced 5 h ago (e.g. manually) -> not yet; 6 h -> due
        XCTAssertFalse(PSD2Consent.autoSyncDue(lastSyncUnix: unix - 5 * 3600, consentEnd: consent, hours: 6, now: now))
        XCTAssertTrue(PSD2Consent.autoSyncDue(lastSyncUnix: unix - 6 * 3600, consentEnd: consent, hours: 6, now: now))
        // off, no consent, expired consent -> never (would open a bank login)
        XCTAssertFalse(PSD2Consent.autoSyncDue(lastSyncUnix: nil, consentEnd: consent, hours: 0, now: now))
        XCTAssertFalse(PSD2Consent.autoSyncDue(lastSyncUnix: nil, consentEnd: nil, hours: 6, now: now))
        XCTAssertFalse(PSD2Consent.autoSyncDue(lastSyncUnix: nil, consentEnd: now.addingTimeInterval(-60), hours: 6, now: now))
    }

    @MainActor
    func testSyncNoticeNamesNewAndRevokedAccounts() {
        let model = AppModel()
        let conn = BankConnection(id: 1, connector: .enableBanking, displayName: "C24", loginName: "", bankCode: "",
                                  sfSymbol: "", lastSyncUnix: nil, allAccountsArchived: false)
        let plain = model.syncNotice(conn, SyncSummary(accountsUpdated: 1, transactionsImported: 5, transactionsCategorized: 2, transactionsSkippedArchived: 0,
                                                       newAccounts: [], revokedAccounts: [], postboxDocuments: nil, warnings: []))
        XCTAssertTrue(plain.contains("C24"))
        XCTAssertFalse(plain.contains("·"))
        let changed = model.syncNotice(conn, SyncSummary(accountsUpdated: 1, transactionsImported: 0, transactionsCategorized: 0, transactionsSkippedArchived: 0,
                                                         newAccounts: ["GLS Giro"], revokedAccounts: ["Pocket", "Gemeinschaftskonto"], postboxDocuments: nil, warnings: []))
        XCTAssertTrue(changed.contains("GLS Giro"))
        XCTAssertTrue(changed.contains("Pocket, Gemeinschaftskonto"))
        // #1083: failed parts point to the log; their texts (account names) stay out of the notice
        let partial = model.syncNotice(conn, SyncSummary(accountsUpdated: 1, transactionsImported: 0, transactionsCategorized: 0, transactionsSkippedArchived: 0,
                                                         newAccounts: [], revokedAccounts: [], postboxDocuments: nil,
                                                         warnings: ["Cash: Umsätze (HKCAZ/camt.052) – TAN verlangt"]))
        XCTAssertTrue(partial.contains("Abruf-Protokoll"), partial)
        XCTAssertFalse(partial.contains("Cash"), partial)
    }
}
