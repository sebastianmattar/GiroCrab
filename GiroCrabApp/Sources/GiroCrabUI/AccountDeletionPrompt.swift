import SwiftUI
import GiroCrabCore

/// The confirmation before an account is deleted for good, shared by the sidebar context menu
/// and the account settings sheet (`docs/KONTO_EINSTELLUNGEN_UI.md`), so both ways say the same.
///
/// A system confirmation dialog is never part of a bug report picture (CLAUDE.md, "Sensitive UI"),
/// so account name and booking count cannot leak through it and need no `.sensitive()`.
enum AccountDeletionPrompt {
    static func title(_ account: Account) -> Text {
        Text("Konto „\(account.name)“ löschen?")
    }

    static func message(_ account: Account) -> Text {
        Text("Das Konto wird samt \(account.transactionCount) Buchungen, Belegen und Depotpositionen unwiderruflich gelöscht – auch aus Auswertungen und Kategoriesummen. Ist es das letzte Konto seines Bankzugangs, wird auch der Bankzugang mit den gespeicherten Zugangsdaten entfernt.")
    }

    /// The red "Löschen" button of the dialog.
    static func confirmButton(_ action: @escaping () -> Void) -> some View {
        Button("Löschen", role: .destructive, action: action)
    }

    /// The settings sheet offers deleting only while "Das Konto wurde aufgelöst" is on, saved or not.
    static func isOffered(archivedToggle: Bool) -> Bool { archivedToggle }
}
