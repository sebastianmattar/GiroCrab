import XCTest
import CoreGraphics
import GiroCrabCore
@testable import GiroCrabUI

/// Problem melden (#301): diagnostics are scrubbed, the issue URL stays short,
/// manual masks are burnt into the picture, and a capture never undoes a newer one.
@MainActor
final class BugReportTests: XCTestCase {
    private func entry(_ message: String, head: String = "2026-11-14 10:00 · FinTS · manual · failed") -> BugReport.LogLine {
        BugReport.LogLine(head: head, message: message)
    }

    private func info(log: [BugReport.LogLine] = []) -> BugReport.Info {
        BugReport.Info(appVersion: "0.6.2 (Build 14)", system: "macOS Version 15.1", architecture: "arm64",
                       language: "de_DE", demo: false, connectors: ["Comdirect (REST)", "Sparkasse (FinTS)"],
                       logLines: log)
    }

    func testHeaderKeepsVersionSystemAndConnectors() {
        let text = BugReport.diagnostics(info(), knownTerms: ["Sparkasse", "Giro"])
        XCTAssertTrue(text.contains("GiroCrab 0.6.2 (Build 14)"), text)
        XCTAssertTrue(text.contains("macOS Version 15.1 · arm64"), text)
        // Known terms only apply to log lines: the header has no user data.
        XCTAssertTrue(text.contains("Sparkasse (FinTS)"), text)
    }

    func testLogLinesLoseIbansNamesAmountsAndEmails() {
        let log = [entry("Konto DE89370400440532013000 von Erika Mustermann: Saldo 1.234,56 €, Kontakt erika@example.de")]
        let text = BugReport.diagnostics(info(log: log), knownTerms: ["Erika Mustermann"])
        for leak in ["DE89", "0532013000", "Erika", "1.234,56", "example.de"] {
            XCTAssertFalse(text.contains(leak), "\(leak) in \(text)")
        }
        XCTAssertTrue(text.contains("FinTS · manual · failed"), text)
    }

    func testAccountNamedLikeItsBankKeepsTheConnectorColumn() {
        let log = [entry("PayPal API: Zugriff verweigert", head: "2026-11-14 10:00 · PayPal · manual · failed"),
                   entry("")]
        let terms = BugReport.publicTermsRemoved(["PayPal", "Erika Mustermann", "erika mustermann", " "], banks: ["paypal"])
        XCTAssertEqual(terms, ["Erika Mustermann"])
        let text = BugReport.diagnostics(info(log: log), knownTerms: terms)
        XCTAssertTrue(text.contains("2026-11-14 10:00 · PayPal · manual · failed · PayPal API: Zugriff verweigert"), text)
        // the head is never sanitized, even with the bank as a term
        XCTAssertTrue(BugReport.diagnostics(info(log: log), knownTerms: ["PayPal"]).contains("· PayPal · manual"))
        XCTAssertTrue(text.hasSuffix("2026-11-14 10:00 · FinTS · manual · failed"), "an empty message keeps its line: \(text)")
    }

    func testIssueURLFitsAndDropsOldestLogLinesFirst() throws {
        // "·" and umlauts take 6 bytes each once percent-encoded
        let log = (0..<400).map { entry("Zeile \($0) · Zeitüberschreitung beim Verbindungsaufbau") }
        let url = BugReport.issueURL(info: info(log: log), knownTerms: [], withScreenshot: true)
        let items = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        let body = try XCTUnwrap(items.first { $0.name == "body" }?.value)
        XCTAssertEqual(url.host, "github.com")
        XCTAssertLessThanOrEqual(url.absoluteString.utf8.count, BugReport.maxURLLength)
        XCTAssertTrue(body.contains("Zeile 0 "), "newest line kept")
        XCTAssertFalse(body.contains("Zeile 399 "), "oldest line dropped")
        XCTAssertTrue(body.contains(BugReport.pasteHint))
        XCTAssertTrue(body.hasSuffix("```"), "code block closed")
    }

    func testPlusSignsSurviveGitHubsFormDecoding() throws {
        let url = BugReport.issueURL(info: info(log: [entry("", head: "2026-11-14 10:00 · FinTS · manual · ok · +12")]), knownTerms: [], withScreenshot: false)
        let query = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.percentEncodedQuery)
        XCTAssertFalse(query.contains("+"), query)
        XCTAssertTrue(query.contains("%2B12"), query)
    }

    func testBackticksInALogLineCannotEndTheCodeBlock() throws {
        let log = [entry("Antwort: ```json {} ``` @maintainer siehe #123")]
        let url = BugReport.issueURL(info: info(log: log), knownTerms: [], withScreenshot: false)
        let body = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "body" }?.value)
        XCTAssertTrue(body.contains("````text\n"), body)
        XCTAssertTrue(body.hasSuffix("\n````"), body)
    }

    func testOnlyCounterpartiesThatCanBeNamesBecomeTerms() {
        let terms = BugReport.counterpartyTerms(["Erika Mustermann", "Mustermann", "Müller", "Meier", "VISA", "Zinsen",
                                                 "Entgelt", "Online", "Gutschrift", "Kontoführung", "Lohn", "Ali",
                                                 "REWE Markt GmbH"])
        XCTAssertEqual(terms, ["Erika Mustermann", "Mustermann", "Müller", "Meier", "REWE Markt GmbH"])
    }

    func testIssueWithoutScreenshotHasNoPasteHint() throws {
        let url = BugReport.issueURL(info: info(), knownTerms: [], withScreenshot: false)
        let body = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "body" }?.value)
        XCTAssertFalse(body.contains(BugReport.pasteHint))
    }

    func testPresanitizedLogGivesTheSameIssue() {
        let log = [entry("Überweisung an Erika Mustermann abgelehnt")]
        let terms = ["Erika Mustermann"]
        let direct = BugReport.issueURL(info: info(log: log), knownTerms: terms, withScreenshot: true)
        let sanitized = BugReport.sanitizedLog(log, knownTerms: terms)
        XCTAssertEqual(sanitized, ["2026-11-14 10:00 · FinTS · manual · failed · Überweisung an [REDACTED] abgelehnt"])
        XCTAssertEqual(BugReport.issueURL(info: info(log: log), sanitizedLog: sanitized, withScreenshot: true), direct)
    }

    func testMasksAreBlackInTheComposedImage() throws {
        let size = 40
        let ctx = try XCTUnwrap(CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
                                          space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: size, height: size))
        let white = try XCTUnwrap(ctx.makeImage())
        // top-left quarter
        let image = try XCTUnwrap(BugReport.compose(white, masks: [CGRect(x: 0, y: 0, width: 0.5, height: 0.5)]))
        XCTAssertEqual(pixel(image, x: 5, y: 5), 0, "masked: black")
        XCTAssertEqual(pixel(image, x: 30, y: 30), 255, "unmasked: white")
        XCTAssertEqual(pixel(image, x: 30, y: 5), 255, "top right stays white")
        XCTAssertNotNil(BugReport.png(image))
    }

    func testSupersededCaptureLeavesTheNewerRedactionInPlace() async {
        let model = AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))
        let first = Task { await BugReport.captureMainWindow(model: model, redaction: .personal, settle: .milliseconds(20)) }
        await Task.yield()
        XCTAssertEqual(model.reportRedaction, .personal)
        let second = Task { await BugReport.captureMainWindow(model: model, redaction: [.personal, .labels], settle: .milliseconds(400)) }
        await Task.yield()
        guard case .superseded = await first.value else { return XCTFail("the older capture must report .superseded") }
        XCTAssertEqual(model.reportRedaction, [.personal, .labels], "the older capture must not reset a newer one")
        guard case .noWindow = await second.value else { return XCTFail("no main window in unit tests") }
        XCTAssertEqual(model.reportRedaction, [], "restored after the last capture")
    }

    /// Red channel of the pixel at (x, y), origin top left.
    private func pixel(_ image: CGImage, x: Int, y: Int) -> UInt8 {
        var data = [UInt8](repeating: 0, count: 4)
        let ctx = CGContext(data: &data, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4,
                            space: CGColorSpace(name: CGColorSpace.sRGB)!,
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        ctx.draw(image, in: CGRect(x: -x, y: -(image.height - 1 - y), width: image.width, height: image.height))
        return data[0]
    }
}
