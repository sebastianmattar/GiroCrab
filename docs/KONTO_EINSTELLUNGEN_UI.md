# Konto-Einstellungen (UI-Aufbau)

Diese Seite beschreibt den Dialog „Kontoeinstellungen“ (`AccountSettingsView.swift`), der im Kontextmenü eines Kontos
in der Seitenleiste über **Einstellungen …** geöffnet wird. Er gilt für macOS und iPhone/iPad.

## Grundprinzip

Das Kontextmenü hat einen einzigen Eintrag **Einstellungen …** für alles, was die Stammdaten eines Kontos betrifft.
Er ersetzt die früheren Einträge „Konto bearbeiten …“, „Umbenennen …“, „Symbol & Farbe …“, „Kontoart ändern …“,
„Kontoinhaber …“ und „Startsaldo …“. Im Menü bleiben die Aktionen: Kategorie in der Buchungszeile, Verschieben nach,
Alle Buchungen als gelesen markieren, Neue Buchung, Saldo korrigieren, Kontostand kopieren, Belege anzeigen,
Konto archivieren bzw. löschen.

## Dialog

Ein Sheet (`capturableSheet`), aufgebaut wie der frühere Dialog „Konto bearbeiten“: Titel, ein `Form` mit allen Feldern
untereinander, unten **Abbrechen** und **Sichern**. Es gibt keine Seitenleiste und keine Bereiche. Geändert wird erst mit
Sichern. Einzige Ausnahme: **Symbol & Farbe** gilt sofort, wie bisher im Popover; Abbrechen setzt es nicht zurück.

Die Felder von oben nach unten:

| Feld | Inhalt |
| --- | --- |
| Kontoname | Knopf mit Symbol öffnet das Popover „Symbol & Farbe“, daneben der Name |
| Kontoinhaber | Textfeld |
| Kontoart | Auswahl; nicht bei Offline-Konten, die sind immer Bargeld |
| Startsaldo | Betrag mit Währung; nur wenn der Kontostand aus den Buchungen berechnet wird und das Konto nicht aufgelöst ist. Darunter „Stichtag und Kalibrierung …“ |
| Kontonummer, IBAN, Bank | Je eine Zeile mit Beschriftung links, nur Anzeige, nur wenn vorhanden (kommen von der Bank bzw. vom Import); jeder Wert lässt sich einzeln markieren und kopieren |
| Advanzia-Benutzername | Nur bei Advanzia-Konten, samt Zugangsfeldern |
| Konto-Notiz | Beschriftung in der linken Spalte, Editor rechts. Erscheint unter dem Kontonamen in der Kontoansicht; wird per iCloud mit den anderen Geräten abgeglichen, die neuere Änderung gilt (#980) |
| Das Konto wurde aufgelöst (archivieren) | Archiviert bzw. reaktiviert das Konto. Ist das Toggle an (auch ungesichert), erscheint die Schaltfläche **Konto löschen …** (rote Schrift) unten links, auf Höhe von Abbrechen und Sichern |

- **Das Konto wurde aufgelöst (archivieren)** (`setAccountArchived`) wird beim Speichern als Letztes ausgeführt, weil es alles neu lädt.
  „Konto archivieren …“ im Menü bleibt daneben bestehen.
- **Konto löschen …** (#1436) erscheint nur, solange das Toggle an ist; abgewählt verschwindet sie wieder. Sie sitzt
  in der Knopfzeile und nimmt keine Formularzeile ein: wechselt die Höhe des Sheets beim Umlegen, zeichnet macOS kurz einen
  schwarzen Balken über den oberen Rand. Der Klick öffnet
  die Rückfrage `AccountDeletionPrompt`, dieselbe wie im Kontextmenü der Seitenleiste (Kontoname, Zahl der Buchungen,
  Hinweis auf Belege, Depotpositionen und den Bankzugang beim letzten Konto). Erst **Löschen** löscht, **Abbrechen** ändert
  nichts. Das Löschen wirkt sofort und ohne Sichern; andere ungesicherte Eingaben im Dialog gehen verloren (die Rückfrage sagt „unwiderruflich“, ein eigener Hinweistext unter der Schaltfläche entfällt). Der Kern löscht
  nur gespeichert archivierte Konten, deshalb ruft `AppModel.deleteAccount(_:archiveFirst: true)` intern
  `setAccountArchived(true)` und `deleteAccount` nacheinander auf (das Archivieren ist in der Oberfläche nicht sichtbar;
  schlägt das Löschen fehl, bleibt das Konto archiviert). Danach schließt der Dialog, die Auswahl wechselt auf „Alle Konten“.
  Die Rückfrage ist ein System-Dialog und wird nie in Problemberichten abgebildet, daher braucht sie kein `.sensitive()`.
- **Startsaldo:** Leeres Feld entfernt den Startsaldo, sonst wird der Betrag mit dem bisherigen Stichtag gespeichert.
  „Stichtag und Kalibrierung …“ schließt den Dialog und öffnet das bisherige Startsaldo-Sheet (`OpeningBalanceSheet`)
  mit Stichtag, Kalibrieren und Vorschau; nicht gesicherte Eingaben im Dialog gehen dabei verloren.
- Eigene Felder für Bankleitzahl, BIC, Anschrift des Kontoinhabers und benutzerdefinierte Felder gibt es noch nicht
  (dafür wäre ein neuer Schema-Schritt in `girocrab-core` nötig).

## Regeln für Änderungen

- Neue Stammdaten eines Kontos kommen als Zeile in den Dialog, nicht als neuer Menüeintrag.
- Personenbezogene Werte tragen `.sensitive()`, Beträge `.sensitive(.amounts)` (siehe `CLAUDE.md`, „Sensitive UI“).
- Popover und Sheets laufen über `capturablePopover` / `capturableSheet`.
- Breiten, die Text halten, gehen über `macFrame`.
