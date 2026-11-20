#if os(macOS)
import AppKit
import XCTest
@testable import GiroCrabUI

@MainActor
final class VaultKeyPasteboardTests: XCTestCase {
    private let key = Data(repeating: 7, count: 32).base64EncodedString()

    private func scratchPasteboard() -> NSPasteboard {
        NSPasteboard(name: NSPasteboard.Name("de.girocrab.test.\(UUID().uuidString)"))
    }

    func testKeyIsNeverOfferedAsPlainText() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        VaultKeyManager.copyKeyToPasteboard(key, pasteboard: pb)
        XCTAssertNil(pb.string(forType: .string), "the key must not be readable as text")
        XCTAssertEqual(VaultKeyManager.keyFromPasteboard(pb), key)
    }

    func testKeyIsMarkedConcealedForClipboardManagers() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        VaultKeyManager.copyKeyToPasteboard(key, pasteboard: pb)
        let types = pb.types?.map(\.rawValue) ?? []
        XCTAssertTrue(types.contains("org.nspasteboard.ConcealedType"))
    }

    /// Transient would keep the key off Universal Clipboard, the only way to the other device (#1138).
    func testKeyIsNotMarkedTransient() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        VaultKeyManager.copyKeyToPasteboard(key, pasteboard: pb)
        XCTAssertFalse((pb.types?.map(\.rawValue) ?? []).contains("org.nspasteboard.TransientType"))
    }

    func testPlainTextIsNeverTaken() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        pb.clearContents()
        pb.setString(key, forType: .string)
        XCTAssertNil(VaultKeyManager.keyFromPasteboard(pb), "a hash or token on the clipboard must not pass as the key")
    }

    func testInvalidKeyTypeIsRejected() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        VaultKeyManager.copyKeyToPasteboard("kein Schlüssel", pasteboard: pb)
        XCTAssertNil(VaultKeyManager.keyFromPasteboard(pb))
    }

    /// A copy received via Universal Clipboard has no timer here, so reading it removes it.
    func testReadingTheKeyClearsThePasteboard() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        VaultKeyManager.copyKeyToPasteboard(key, pasteboard: pb)
        XCTAssertEqual(VaultKeyManager.keyFromPasteboard(pb), key)
        XCTAssertNil(pb.data(forType: NSPasteboard.PasteboardType(VaultKeyManager.keyPasteboardType)))
    }

    func testNothingToReadFromAnEmptyPasteboard() {
        let pb = scratchPasteboard()
        defer { pb.releaseGlobally() }
        XCTAssertNil(VaultKeyManager.keyFromPasteboard(pb))
    }
}
#endif
