# Sicherheit / Security

[Deutsch](#deutsch) · [English](#english)

## Deutsch

### Sicherheitslücken melden

Bitte keine öffentlichen Issues für Schwachstellen. Nutze auf GitHub „Security › Report a vulnerability“
(privater Security Advisory) im Repository `GiroCrab/GiroCrab`. Bitte keine echten Finanzdaten, IBANs oder
Zugangsdaten mitschicken.

### Architektur im Überblick

GiroCrab ist eine lokale Anwendung: Ein Rust-Kern (`girocrab-core`) hält alle Daten, die Oberflächen
(SwiftUI, Qt, …) zeigen sie nur an. Es gibt kein GiroCrab-Konto und keinen GiroCrab-Server, der Finanzdaten sieht.

| Bereich | Maßnahme |
|---|---|
| Daten in Ruhe | Der Tresor ist eine **SQLCipher**-Datenbank (verschlüsselt, mit Schlüsselableitung aus dem Tresorschlüssel). Speicherschutz (`cipher_memory_security`) ist in Produktions-Builds aktiv. |
| Schlüssel und Zugangsdaten | Der Tresorschlüssel und Bank-Zugangsdaten liegen im **Schlüsselbund** (macOS/iOS Keychain), im **Secret Service** (Linux) bzw. im **Windows Credential Manager** (Windows), nicht in Dateien oder Einstellungen (der Android-Alpha-Build öffnet noch keinen eigenen Tresor). |
| Geheimnisse im Speicher | PINs, Passwörter, Tokens und Schlüssel werden in `Zeroizing`-Containern gehalten und beim Freigeben überschrieben. |
| Netzwerk | Verbindungen zu Banken und Portalen nutzen **striktes TLS** (Zertifikatsprüfung, kein Abschalten per Option). Weiterleitungen werden nur **innerhalb desselben Hosts** gefolgt, damit Zugangsdaten und Tokens nie an Dritte gehen. |
| Oberfläche | Die Ansicht kann mit `.sensitive()` Personenbezogenes, Beträge und Bezeichnungen verdecken (Datenschutzmodus, Fehlerbericht-Screenshots). Texte für öffentliche Fehlerberichte laufen durch `sanitize_report_text`. |
| Lokale Schnittstelle | Die optionale lesende Schnittstelle (`docs/local-api.md`) ist ein Unix-Socket mit Token und Geltungsbereich, nur lokal erreichbar. |

### Local-only-Garantie

- Buchungen, Konten, Kategorien, Notizen und Dokumente verlassen das Gerät nicht, außer auf dein ausdrückliches
  Zutun (Abruf bei deiner Bank, Export, iCloud-Abgleich deines eigenen Kontos).
- Keine Telemetrie, keine Analyse-Dienste, keine Werbe-Kennungen.
- Netzwerkverkehr entsteht nur zu den Endpunkten, die du selbst einrichtest (Bank, Zahlungsdienst, Portal).
- Fehlerberichte werden erst nach deiner Prüfung und bereinigt veröffentlicht.

### Bedrohungsmodell

**Geschützt gegen**
- Verlust oder Diebstahl des Geräts bzw. Kopie der Tresordatei und von Backups (SQLCipher, Schlüssel im Schlüsselbund).
- Mitlesen oder Umleiten des Netzwerkverkehrs (striktes TLS, Same-Host-Weiterleitungen).
- Versehentliches Offenlegen in Screenshots, Logs und Fehlerberichten (`.sensitive()`, Sanitizer).
- Reste von Geheimnissen im Arbeitsspeicher oder in Swap/Core-Dumps (Zeroizing, SQLCipher-Speicherschutz).

**Nicht geschützt gegen (außerhalb des Modells)**
- Ein bereits kompromittiertes, entsperrtes System (Malware mit Zugriff auf Prozessspeicher oder Bildschirm).
- Eine kompromittierte Bank, ein kompromittiertes Portal oder gefälschte Zertifizierungsstellen im Systemspeicher.
- Physischer Zugriff auf ein entsperrtes Gerät, schwache Geräte-Passwörter.
- Eine vom Nutzer freigegebene lokale Schnittstelle: Wer das Token hat, liest im vergebenen Umfang.

### Abhängigkeiten

Eingesetzte Bibliotheken und Lizenzen: `THIRD_PARTY_NOTICES.md`.

---

## English

### Reporting a vulnerability

Please do not open public issues for vulnerabilities. Use GitHub “Security › Report a vulnerability” (private
security advisory) in `GiroCrab/GiroCrab`. Do not include real financial data, IBANs or credentials.

### Architecture overview

GiroCrab is a local application: a Rust core (`girocrab-core`) holds all data; the UIs (SwiftUI, Qt, …) only
display it. There is no GiroCrab account and no GiroCrab server that sees financial data.

| Area | Measure |
|---|---|
| Data at rest | The vault is a **SQLCipher** database (encrypted, key derived from the vault key). Memory security (`cipher_memory_security`) is on in production builds. |
| Keys and credentials | The vault key and bank credentials are stored in the **Keychain** (macOS/iOS), **Secret Service** (Linux), or **Windows Credential Manager** (Windows), never in plain files or settings (the Android alpha build does not open an own vault yet). |
| Secrets in memory | PINs, passwords, tokens and keys are held in `Zeroizing` containers and overwritten on drop. |
| Network | Bank and portal connections use **strict TLS** (certificate validation, no opt-out). Redirects are followed **only within the same host**, so credentials and tokens never reach third parties. |
| UI | `.sensitive()` can hide personal data, amounts and labels (privacy mode, bug-report screenshots). Text for public bug reports passes through `sanitize_report_text`. |
| Local interface | The optional read-only interface (`docs/local-api.md`) is a Unix socket with token and scope, reachable locally only. |

### Local-only guarantee

- Transactions, accounts, categories, notes and documents do not leave the device unless you explicitly act
  (fetching from your bank, export, iCloud sync of your own account).
- No telemetry, no analytics services, no advertising identifiers.
- Network traffic only goes to endpoints you set up yourself (bank, payment provider, portal).
- Bug reports are published only after your review and sanitised.

### Threat model

**Protected against**
- Device loss or theft, or a copy of the vault file or backups (SQLCipher, key in the keychain).
- Eavesdropping on or redirecting network traffic (strict TLS, same-host redirects).
- Accidental disclosure in screenshots, logs and bug reports (`.sensitive()`, sanitizer).
- Leftover secrets in memory or swap/core dumps (Zeroizing, SQLCipher memory security).

**Out of scope**
- An already compromised, unlocked system (malware with access to process memory or the screen).
- A compromised bank or portal, or rogue certificate authorities in the system trust store.
- Physical access to an unlocked device, weak device passwords.
- A local interface the user enabled: whoever holds the token reads within the granted scope.

### Dependencies

Libraries and licenses: `THIRD_PARTY_NOTICES.md`.
