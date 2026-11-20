#if os(macOS)
import XCTest
import Foundation

/// Exercise the compiled shipping catalog through Foundation, including interpolation.
final class AccountTotalsLocalizationTests: XCTestCase {
    func testTotalsControlsAndExplanationsHaveEnglishTranslations() throws {
        let package = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let catalog = package.appendingPathComponent("Xcode/App/Localizable.xcstrings")
        let output = FileManager.default.temporaryDirectory.appendingPathComponent("girocrab-localization-\(UUID())")
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: output) }
        let tool = Process()
        tool.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
        tool.arguments = ["xcstringstool", "compile", catalog.path, "--output-directory", output.path, "--language", "en"]
        try tool.run()
        tool.waitUntilExit()
        XCTAssertEqual(tool.terminationStatus, 0)
        let bundle = try XCTUnwrap(Bundle(path: output.appendingPathComponent("en.lproj").path))
        let translations = [
            ("In Gesamtsummen zählen", "Include in overall totals"),
            ("Dieses Konto ist archiviert und zählt nicht zu den Gesamtsummen.", "This account is archived and does not count towards overall totals."),
            ("Gruppe „%@“ ist ausgeschlossen – dieses Konto zählt deshalb trotz eingeschaltetem Kontoschalter nicht zu den Gesamtsummen.", "Group “%@” is excluded, so this account does not count towards overall totals even though its own switch is on."),
            ("Eine Gruppe ist ausgeschlossen – dieses Konto zählt deshalb trotz eingeschaltetem Kontoschalter nicht zu den Gesamtsummen.", "A group is excluded, so this account does not count towards overall totals even though its own switch is on."),
            ("Ausgeschaltet zählen die Konten dieser Gruppe und ihrer Untergruppen nicht zu den übergreifenden Geld- und Vermögenswerten – „Verfügbar“, Einnahmen, Konsum, Investitionen, Cashflow, Vermögen und die Saldoverläufe der Übersicht – z. B. für Geschäfts- oder Kinderkonten.", "When switched off, the accounts in this group and its subgroups are excluded from overall financial figures: available funds, income, consumption, investments, cashflow, wealth and overview balance history, for example for business or children's accounts.")
        ]
        for (key, english) in translations {
            let value = bundle.localizedString(forKey: key, value: nil, table: "Localizable")
            XCTAssertEqual(String(format: value, "Review Group"), String(format: english, "Review Group"), key)
        }
    }
}
#endif
