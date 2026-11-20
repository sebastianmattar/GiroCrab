import Security
import XCTest
@testable import GiroCrabUI

/// iCloud Keychain sharing needs the data-protection keychain. A build without
/// the entitlement (the notarized Mac app so far) must not offer a switch that
/// can only fail (#909).
final class KeychainSyncAvailabilityTests: XCTestCase {
    func testMissingEntitlementMeansUnavailable() {
        XCTAssertFalse(VaultKeyManager.iCloudKeychainAvailable(probe: errSecMissingEntitlement))
    }

    func testReachableKeychainMeansAvailable() {
        XCTAssertTrue(VaultKeyManager.iCloudKeychainAvailable(probe: errSecItemNotFound))
        XCTAssertTrue(VaultKeyManager.iCloudKeychainAvailable(probe: errSecSuccess))
    }

    #if os(macOS)
    /// The `swift test` process carries no entitlements, like the notarized Mac app.
    /// Only a write reveals that: a lookup answers "not found" either way.
    func testUnentitledProcessCannotShare() {
        XCTAssertFalse(VaultKeyManager.canShareViaICloudKeychain)
    }
    #endif

    func testUnavailableSwitchExplainsItself() {
        XCTAssertNil(KeychainSyncNote.text(available: true))
        let note = KeychainSyncNote.text(available: false)
        XCTAssertNotNil(note)
        XCTAssertTrue(note?.contains("Tresor-Schlüssel kopieren") == true, note ?? "")
    }
}
