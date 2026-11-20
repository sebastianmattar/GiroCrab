<p align="center">
  <img src="docs/assets/app-icon.png" alt="GiroCrab – App-Icon" width="112" height="112">
</p>

<h1 align="center">GiroCrab</h1>

<p align="center"><b>🇩🇪 <a href="#-deutsch">Deutsch</a> · 🇬🇧 <a href="#-english">English</a> · 🌐 <a href="https://girocrab.de">girocrab.de</a></b></p>

<h3 align="center">Die freie, native Banking-App für Mac, iPhone &amp; iPad.</h3>

<p align="center">FinTS 3.0 · PSD2 · SEPA · Datenimport — 100&nbsp;% lokal verschlüsselt, kostenlos, Daten bleiben auf deinem Gerät.</p>

<p align="center">
  <a href="https://github.com/GiroCrab/GiroCrab/releases/latest/download/GiroCrab-macOS-arm64.dmg"><img src="docs/assets/download-macos.svg" alt="Für macOS laden – v0.8.7 · Apple Silicon (.dmg)" width="260" height="56"></a>
  &nbsp;
  <a href="https://testflight.apple.com/join/KBmvKVpT"><img src="docs/assets/download-ios.svg" alt="iOS &amp; iPadOS App – kostenlos via Apple TestFlight" width="260" height="56"></a>
</p>

<p align="center"><sub>✓ Offiziell von Apple notarisiert · ✓ Automatische Updates · ✓ Version v0.8.7</sub></p>
<p align="center">
  <a href="https://github.com/GiroCrab/GiroCrab/releases/latest/download/girocrab-windows-x86_64-setup.exe"><img src="docs/assets/download-windows-alpha.svg" alt="Für Windows laden (Alpha – Frühe Vorschau)" width="260" height="56"></a>
  &nbsp;
  <a href="https://github.com/GiroCrab/GiroCrab/releases/latest/download/GiroCrab-Android-alpha.apk"><img src="docs/assets/download-android-alpha.svg" alt="Für Android laden (Alpha – Frühe Vorschau)" width="260" height="56"></a>
</p>
<p align="center"><sub>🧪 <strong>Windows &amp; Android (frühes Alpha-Stadium):</strong> Vorab-Builds zum Ausprobieren (idealerweise im Demo-Modus) – noch nicht auf dem Stand von macOS &amp; iOS.</sub></p>
<p align="center">
  <a href="https://github.com/GiroCrab/GiroCrab/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/GiroCrab/GiroCrab?label=release&color=2ea44f"></a>
  <a href="https://github.com/GiroCrab/GiroCrab/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/GiroCrab/GiroCrab/total?color=007AFF"></a>
  <a href="https://github.com/GiroCrab/GiroCrab/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/GiroCrab/GiroCrab?style=flat"></a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/overview-dark.png">
    <img src="docs/screenshots/overview.png" alt="GiroCrab – Übersicht mit Konten, Salden und Buchungen (fiktive Demo-Daten)" width="900">
  </picture>
</p>

<p align="center"><sub>⭐ Gefällt dir GiroCrab? Ein <a href="https://github.com/GiroCrab/GiroCrab/stargazers">Stern auf GitHub</a> hilft anderen, das Projekt zu finden. · <i>A star helps others find the project.</i></sub></p>

> ### 🛠️ Mitbauen in 4 Schritten · Contribute in 4 steps
> In den ersten 10 Tagen: **310+ GitHub-Sterne**, **580+ erledigte Pull Requests**, **25+ Mitwirkende** – direkt von GitHub.
> Du brauchst **keine Swift- oder Rust-Erfahrung**: Mach aus deinem Funktionswunsch mit **Claude Code, Codex oder Cursor** und der [`CLAUDE.md`](CLAUDE.md) einen eigenen Pull Request.
> 👉 [**Schritt-für-Schritt-Anleitung**](https://girocrab.de/girocrab_mitbau_anleitung.html) · [CONTRIBUTING.md](CONTRIBUTING.md)
>
> *In our first 10 days: 310+ stars, 580+ closed/merged pull requests, 25+ contributors (from GitHub). No Swift or Rust experience needed – turn your own feature wish into a pull request with Claude Code, Codex or Cursor and [`CLAUDE.md`](CLAUDE.md). See the [step-by-step guide](https://girocrab.de/girocrab_mitbau_anleitung.html) and [CONTRIBUTING.md](CONTRIBUTING.md).*

---

## 🇩🇪 Deutsch

**Open-Source-Online-Banking für Mac (Apple Silicon) und iPhone/iPad – lokal, verschlüsselt, Daten bleiben auf deinem Gerät.**
Native SwiftUI-App auf einem Rust-Kern: verschlüsselter **SQLCipher-Banking-Tresor** (AES-256, zufälliger 256-Bit-Schlüssel), Kategorienbaum,
Regeln, Händler-Erkennung, Sparplan-Ansicht, Darlehens-Tilgungsbalken und direkte Verbindungen
zu Ihren Banken per **FinTS 3.0 / HBCI** und **PSD2 (Enable Banking)** – ohne Server, ohne Tracking.

### Für wen ist GiroCrab?

GiroCrab ist eine freie, quelloffene Banking-App (GPLv3) für macOS, Linux (Qt 6 / QML) und – derzeit als Beta über TestFlight – für iPhone und iPad. Alle Daten liegen verschlüsselt auf deinem Gerät. GiroCrab ist ein junges Community-Projekt: Die Bankenabdeckung wächst noch (siehe Bank-Tabelle), und PSD2-Zugänge laufen über einen eigenen, kostenlosen Enable-Banking-Zugang.

### ✨ Neu in v0.8.7

- 🏦 **Neue Banken & Amex-Kontoauszüge (#1168, #1435, #1440, #1451, #1456, #1416):** Native Konnektoren für **Hanseatic Bank** (Kreditkarten, Tages-/Festgeld, SparBrief & Postbox-PDFs), **BHW Bausparkasse** (Bausparverträge, Baudarlehen & Postfach-PDFs) und **Kontist**-Geschäftskonten, automatischer & manueller **PDF-Kontoauszugsabruf für American Express** im Portal-Fenster, **C24, N26 und Bank Norwegian** verifiziert in der Banken-Matrix sowie aktualisierte FinTS-Adresse für **1822direkt**.
- 🧮 **Kontoschalter für Gesamtsummen & Kontoeinstellungen (#1100, #1445, #1455):** Einzelne Konten lassen sich gezielt aus Gesamtsummen, Cashflow und Vermögen ausschließen (mit iCloud-Sync-Stempel; Amazon-Bestellkonten starten ausgeschlossen), aufgelöste Konten direkt in den Kontoeinstellungen löschen und Konten mit negativem Startsaldo ohne gesperrten „Sichern“-Button bearbeiten.
- 💸 **Empfängerliste, Buchungsnotizen & FinTS-Überweisungen (#1129, #1130, #1441, #1088, #1419):** Bekannte Empfänger verwalten/ausblenden, direktes Notiz-Symbol an Buchungen mit Hover-Vorschau, sofortige FinTS-Überweisungen ohne unerlaubtes Ausführungsdatum sowie `HISPAS`-SEPA-Formaterkennung (`pain.001.001.09`).
- 🐧 **Linux-Release-Pakete & Android Native-Build (#1150, #1151, #1458, #1384, #1423, #1443):** EquatePlus-Anmeldung & Beleg-Download, Hintergrund-Sync inkl. sauberem Dialog-Abbruch bei „Alle abrufen“, Privatmodus, Breeze-Icons, direkte `.deb`- & `.flatpak`-Pakete am GitHub-Release sowie CI-geprüfter nativer Rust-/UniFFI-Build unter Android.
- 🛠️ **iPadOS FinTS-Registrierung, Amex-Saldo & Zeitraumwechsel (#1449, #1446, #1444, #1379):** Eingebaute FinTS-Produktregistrierung jetzt fest verifiziert im iOS/iPadOS-TestFlight-Build (Build 24 unter Version `0.7.5`), korrekter `0,00 €`-Saldo bei bezahlter Amex-Abrechnung, synchroner Kopfbereich beim Zeitraumwechsel und erweiterter Code-Integritäts-Linter (`forbid(unsafe_code)`).

### ✨ Neu in v0.8.6

- 🔍 **Natürliche Suche & Beleg-Filter:** Buchungen über natürliche Suchbegriffe finden und gezielt nach Belegen filtern.
- 🏦 **Banken, Echtzeitüberweisung & Import-Korrekturen:** Consorsbank-Archiv, FFB-Postfach-PDFs (#1105) & FFB-Depot-Zusammenführung (#1103), Ford-Money-/Opel-Steuerabgleich (#1102), Ford-Money-Freigabe (#1101), Advanzia-Wiederanlegen (#1115), Trade-Republic-Verrechnungskonto beim Import (#1114), PayPal-Fremdwährungen (#1110) & Enable-Banking-Währungsanzeige (#1034), offene Reservierungen (#1111), ING Business im Bankverzeichnis (#974) sowie Echtzeitüberweisung-Statusrückmeldung `3045` (#1121).
- 🔐 **Sicherheit & FinTS-Registrierung:** In offiziellen Builds wird ausschließlich GiroCrabs offizielle FinTS-Produktregistrierung genutzt (#1092), Connector-Zustände im Speicher geschützt (#1094) und die Schlüsselbund-Prüfung korrigiert (#1084).
- 📱 **iOS / TestFlight – v0.8.6 unter der Haube (Build 22 unter Version 0.7.5):** Damit Updates ohne Review-Wartezeit sofort bei euch landen, erscheint der komplette **v0.8.6-Funktionsumfang** auf iOS direkt als Build 22 unter Version `0.7.5`.

### ✨ Neu in v0.8.5

- 🔐 **Schlüsselbund- & iCloud-Übernahme:** Verbesserte Wiederherstellung und Übernahme bestehender Tresorschlüssel auf macOS und iOS sowie klarerer Hinweis im Demo-Modus.
- 🏦 **Banken & Konten:** Korrekturen bei Enable Banking (u. a. Spuerkeess), überarbeitete Kontoseiten in den Mobile-Apps und stabilere Kategorie-/Konten-Navigation auf Windows.
- 📱 **iOS / TestFlight – v0.8.5 unter der Haube (Build 22 unter Version 0.7.5):** Damit Updates ohne Review-Wartezeit sofort bei euch landen, erscheint der komplette **v0.8.5-Funktionsumfang** auf iOS direkt als Build 22 unter Version `0.7.5`.
- 🧪 **Windows & Android (Alpha-Vorschau):** Erstmals hängen automatische Alpha-Installer für Windows und Android zum Ausprobieren direkt am Release.

### ✨ Neu in v0.8.0
- 🏦 **Integrierte FinTS-Produktregistrierung:** Offizielle FinTS-Produktregistrierungs-ID ist direkt im Release hinterlegt – der manuelle Eintrag entfällt.
- 🛡️ **Automatisches Tresor-Backup & Wiederherstellung:** Vor Migrationen und sensiblen Operationen legt GiroCrab automatisch verschlüsselte Sicherheits-Backups des Tresors an.
- 📈 **EquatePlus, IBKR-Einzelwährungen & C24-Kategorien (#750, #720, #625, #626, #763, #678):** Neuer Connector für EquatePlus (Mitarbeiteraktien & PDF-Belege), automatisches Archivieren abgewählter Konten, native Originalwährungen je IBKR-Depotposition (mit Retry-Logik), C24-Kategorien-Import sowie sprachunabhängige eigene Kategorien und Gruppen.
- ⚡ **Massive Performance-Sprünge (#754, #759, #760):** `ListCache` für Buchungslisten und Händlerhistorie (~20–30× schneller), einmalig kompilierte `LazyLock`-Regex beim Import (~1.000× schneller bei großen Dateien) und gezieltes `apply_rules_to` nach iCloud-Merges.
- 🏷️ **Tags filtern & auswerten, Analysen & Serien (#228, #777, #731, #755, #615, #756, #725, #753, #659):** Cashflow-Karte folgt der Kontoauswahl in der Seitenleiste, wiederkehrende Buchungen trennen bekannte Händler hinter derselben IBAN, und „Geprüft“ wirkt auf die gesamte Mehrfachauswahl.
- 📱 **iOS / TestFlight – v0.8.0 unter der Haube (Build-Nummer zählt!):** Unsere Community-Entwicklung liefert neue Features aktuell schneller aus, als Apples Versions-Review für neue Release-Zweige hinterherkommt. Damit Updates ohne Review-Wartezeit sofort bei euch landen, erscheint der komplette **v0.8.0-Funktionsumfang** (inkl. Tresor-Schema v27 für nahtlosen iCloud-Sync mit macOS v0.8.0) auf iOS direkt als neuer **Build unter Version 0.7.5**.
- 🪟 **Neu & Experimentell – GiroCrab für Windows (`GiroCrabWindows/`):** Erste frühe Windows-Preview auf Basis desselben `girocrab-core`! **Wir suchen Tester & Rust-/Windows-Entwickler** – probiere es aus, melde Feedback in den Issues (`windows`) oder gestalte die Windows-Oberfläche direkt per PR mit!

### ✨ Neu in v0.7.5
- **FinTS-Daueraufträge (#496):** Daueraufträge direkt per FinTS mit TAN-Freigabe anlegen, ändern und löschen (macOS & iOS, inkl. Anlage aus bestehenden Buchungen).
- **C24-Pockets & Gegenkonten (#523):** Eigene Unterkonten/Pockets werden bei Umbuchungen direkt in der Buchungszeile und Detailkarte angezeigt.
- **Kontoansicht & Listen-Komfort (#336, #512, #538):** Konto-Notizen erscheinen direkt im Kopfbereich, der Kategorie-Chip lässt sich pro Konto ausblenden und leere Scrollleisten entfallen.
- **Budgets & Zuordnung (#544, #541, #540):** Monatsbudgets lassen sich unabhängig von Auswertungen einblenden, kategorisierte Einnahmen zählen trotz Umbuchungstext und der Abrufweg-Wechsel erzeugt keine doppelten Konten mehr.

### ✨ Neu in v0.7.0

- 🐧 **Nativer Linux-Port (`GiroCrabLinux/`, #389, #419):** vollständige Qt-6/QML-Desktop-App auf demselben `girocrab-core` – inkl. Secret Service, QtWebEngine, PDF-Belegen, OCR, GiroCode sowie `.deb`- und Flatpak-Paketen.
- 📊 **Grafische Vermögensübersicht (#348, #415):** neue Seitenleisten-Ansicht „Vermögen“ mit Nettovermögen, Donut-Charts, Cash-Verlauf und gruppierten Karten für Vermögenswerte und Verbindlichkeiten; dazu „Neue Umsätze“ mit Kontenkarten für ungeprüfte Buchungen.
- 🏦 **Neue Banken & Verbesserungen bei Brokern:** Consorsbank (FinTS), easybank (Kreditkarte, PDF-Abrechnungen), Sachwerte wie Gold, Immobilien und Fahrzeuge, Scalable-Capital-Rundungs- und Depotübertrags-Fix (#411), FNZ/ebase-Portal mit Wertpapierumsätzen (#314) und Enable-Banking-Setup-Hinweise (#344).
- 🛒 **Amazon-Produktlinks & Inspector-Komfort (#354, #299, #295, #412, #407):** klickbare Artikel-Links (ASIN) im Buchungsdetail, Händlerhistorie getrennt nach „Dieses Konto / Alle Konten“, Kontogruppen-Sortierung bei Belegen und Regeln sowie klare Hinweise bei Splitbuchungen.
- ☁️ **iCloud-Drive-Ordner „GiroCrab“ (#437):** der Ordner ersetzt den Altordner, vorhandene Daten werden kopiert; dazu Enable-Banking-Zugang über den iCloud-Schlüsselbund teilen (#356).
- 🛡️ **Weiteres:** In-App-Fehlerbericht mit automatischer Schwärzung (#301), einstellbare Schriftgröße (#343), App zurücksetzen (#241), Budgets bearbeiten und ausblenden (#430) sowie Sicherheits-Updates für Abhängigkeiten (RUSTSEC-2023-0071, -2026-0387, -2026-0502).

**Highlights aus v0.6.2:**

- 🧮 **Startsaldo für Konten mit berechnetem Saldo (#212, #227):** Ein Startsaldo lässt sich mit optionalem Stichtag festlegen und per „Kalibrieren“ aus dem letzten Kontostand ableiten; der Datenbank-Import übernimmt ihn. Entfernen setzt den Saldo auf die reine Umsatzsumme zurück.
- 🗄️ **Versionierte Schema-Migrationen mit Downgrade-Schutz (#206, #232):** Tresore werden in nummerierten Schritten migriert; ein Tresor aus einer neueren Version wird nicht geöffnet. Diese Version hebt Tresore auf **Schema 10** – vor dem Testen eine Sicherungskopie anlegen, falls v0.6.1 den Tresor wieder öffnen soll.
- 🖱️ **Drag & Drop in der Seitenleiste mit Einfügelinie (#230)**, dazu eindeutige PSD-Bankennamen im FinTS-Verzeichnis (#145) und ein Kopieren-Menü auf iOS (Konto- und Depotpositionen).

**Highlights aus v0.6.1:**

- 📲 **iOS & iPadOS jetzt via Apple TestFlight:** Die iOS-App kann ab sofort direkt per **[TestFlight-Einladung](https://testflight.apple.com/join/KBmvKVpT)** auf iPhone und iPad installiert und automatisch aktualisiert werden – kein eigener Xcode-Build mehr nötig.
- 💿 **macOS-Installer als `.dmg` + Sparkle Auto-Updates:** Offiziell von Apple signiertes und notarisiertes Disk-Image (`GiroCrab-macOS-arm64.dmg`) inklusive integrierter Update-Funktion über Sparkle.
- ⚡ **Kategorie-Zuweisung in `< 1 ms` (#159):** Optimistisches UI-Update und neue SQLite-Covering-Indizes sorgen selbst bei Tresoren mit über 30.000 Buchungen für verzögerungsfreies Kategorisieren.
- ✅ **Buchungen automatisch als geprüft markieren (#176):** Neue Option unter *Einstellungen › Allgemein › Buchungen* – nach wählbarer Verzögerung oder nur manuell.
- 🔴 **macOS Dock-Badge (#173) & farbige Konto-Icons (#175):** Optionale Anzeige der Anzahl ungeprüfter Buchungen im Dock; Konto-Symbole in der Seitenleiste erscheinen in ihrer Kontofarbe. Dazu das gemeinsame **Icon-&-Farbe-Popover** für Konten und Gruppen (#183).
- 🧙 **Neue Assistenten nach Apple HIG:** *Import-Assistent* mit Salden-Abgleich (#181) und *Bank hinzufügen* (#184).
- 🏦 **Enable Banking (PSD2) & Multi-Währungs-Konten (#164):** Finom-Geschäftskonten und Multi-Währungs-Wallets mit geteilter IBAN; Vormerkungen bei comdirect (#186) und bunq (#187).
- 📣 **Projektmitteilungen (#185):** ruhiger, signierter Hinweis-Feed – ohne Release, ohne Tracking.

**Highlights aus v0.4.0 – v0.5.5:**

- 🗄️ **Direkter Datenbank-Import:** eine verschlüsselte `.sqlite`-Datenbank (inkl. Konten, Kategorienbaum, Notizen, Depot-Beständen und PDF-Kontoauszügen) wird **nur lesend** geöffnet und übernommen.
- 📁 **Eigene Kontogruppen & Untergruppen:** frei sortierbar, mit iCloud-Synchronisation und Summenzeile.
- ☁️ **Optionale iCloud-Drive-Synchronisation** zwischen Macs und iOS-Geräten per verschlüsseltem SQLCipher-Snapshot-Merge.
- 🔒 **Händler-Logos nur per Opt-in (#38):** Online-Logos sind standardmäßig **aus**; ohne Zustimmung unter *Einstellungen → Händler-Logos* bleibt alles zu 100 % lokal.

### 📥 Vorhandene Daten übernehmen

GiroCrab kann Umsätze, Kategorien und Notizen aus unterstützten Exportdateien übernehmen. Vor dem Import zeigt eine Vorschau, was erkannt wurde; nicht unterstützte Dateien werden dort gemeldet. Bankzugänge richtest du anschließend separat in GiroCrab ein.

<!-- TODO(Andreas): bitte prüfen: Screenshot des Import-Assistenten ohne Fremdmarke neu aufnehmen (alte Import-Screenshots entfernt) -->

Kategorien (`Haushalt\Lebensmittel`) bleiben als Baum erhalten, Notizen stehen in der Buchungsdetail-Ansicht.
Jeder Umsatz erhält einen Fingerabdruck (Datum, Betrag, IBAN/Name, Verwendungszweck): Ein erneuter Import
oder später per FinTS abgerufene, überlappende Tage werden als Duplikate erkannt und nicht doppelt angelegt – vorhandene Buchungen werden
nur um Kategorie und Notiz ergänzt.

**Komfort-Funktionen im Alltag:**
* **📋 Überweisung aus Zwischenablage (`⌘V` / `⇧⌘V`):** Rechnungstexte oder Bankverbindungen aus der Zwischenablage direkt ins Überweisungsfenster einfügen – Empfänger, prüfsummengültige IBAN, BIC, Betrag und Verwendungszweck werden automatisch erkannt.
* **📄 E-Rechnungen & PDF-Drop:** PDF-Rechnungen einfach auf das Überweisungsfenster ziehen (liest eingebettetes **ZUGFeRD / Factur-X XML**, **EPC-QR-Codes / GiroCode** oder den PDF-Text).
* **✅ Geprüft-Häkchen (`Space`):** Umsätze in der Liste direkt per Leertaste als „geprüft“ markieren.
* **🖱️ Rechtsklick-Workflows:** Mit einem Klick alle Buchungen desselben Empfängers filtern (*„Nach ‚…‘ suchen“*) oder eine *„Neue Überweisung aus Buchung …“* vorausfüllen lassen.

### Funktionen im Überblick

<p align="center"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/overview-dark.png"><img src="docs/screenshots/overview.png" alt="GiroCrab – Übersicht mit fiktiven Demo-Konten" width="900"></picture></p>

<p align="center"><img src="docs/screenshots/workflow-demo.gif" alt="Animierter Rundgang: Übersicht, Alltag &amp; Konsum, Sparpläne, Dunkelmodus" width="900"></p>

*Alle Screenshots zeigen ausschließlich erfundene Demo-Daten (`GIROCRAB_DEMO_SEED=1`).*

- 🔒 **Local-First:** Alle Bankdaten bleiben auf Ihrem Gerät – in einem verschlüsselten Tresor (SQLCipher, AES-256), geschützt durch Schlüsselbund / Touch ID bzw. Face ID. Es gibt keinen Entwickler-Server.
- 📱 **macOS + iOS (iPhone / iPad):** Eine gemeinsame SwiftUI-Codebasis auf dem Rust-Kern. Voraussetzungen: **macOS 14+ (Apple Silicon, `arm64`)** bzw. **iOS 17+**. Für iPhone und iPad steht die App direkt über **[Apple TestFlight](https://testflight.apple.com/join/KBmvKVpT)** bereit (oder kann optional mit Xcode selbst aus dem Quellcode gebaut werden).
- ☁️ **Optionale iCloud-Drive-Synchronisation:** Wer möchte, gleicht Tresor-Daten zwischen Macs und iOS-Geräten über den eigenen iCloud-Drive-Ordner (`iCloud Drive/GiroCrab/`) ab. In die Cloud wandert der Tresor nur als **verschlüsselter SQLCipher-Snapshot** (`wal_checkpoint(TRUNCATE)`); der Abgleich erfolgt deterministisch per `ATTACH DATABASE`-Merge. Eine **laufende SQLite-Datenbank mit WAL-Dateien wird nie direkt im Cloud-Speicher geöffnet.** Die Synchronisation ist **standardmäßig ausgeschaltet** und wird erst in den Einstellungen aktiviert; danach läuft der Abgleich beim Entsperren des Tresors. Der Ordner `iCloud Drive/GiroCrab/` wird erst angelegt, wenn Sie die Synchronisation aktivieren. 🔑 **Schlüssel:** Der Tresor-Schlüssel liegt **nicht** im iCloud-Ordner (die frühere Datei `tresor.key` aus v0.4.0 wird beim nächsten Abgleich automatisch gelöscht). Auf Wunsch teilt GiroCrab ihn über den **iCloud-Schlüsselbund** (`kSecAttrSynchronizable`); das ist standardmäßig ausgeschaltet und wird in den Einstellungen aktiviert. Alternativ kopieren Sie den Schlüssel auf dem ersten Gerät („Tresor-Schlüssel kopieren“, Zwischenablage wird nach 60 s geleert) und fügen ihn einmalig auf dem zweiten Gerät ein („Schlüssel einfügen …“). ⚠️ Wer bereits v0.4.0 mit aktivem Sync genutzt hat: Die alte `tresor.key` kann noch in iCloud-Versionen/Papierkorb liegen; eine Schlüsselrotation ist für ein späteres Update geplant.
- 🗄️ **Datenbank-Import:** lesender Import einer verschlüsselten `.sqlite`-Datei inkl. Konten, Kategorien, Hierarchie und Buchungshistorie (siehe „Vorhandene Daten übernehmen“).
- 🏦 **FinTS 3.0 / HBCI-Client & PSD2 für macOS und iOS:** FinTS 3.0 / HBCI (CAMT.052 & MT940 mit intelligenter Latin-1/UTF-8-Erkennung), **bunq** (persönlicher API-Schlüssel), PayPal, Trade-Republic-WebSocket, comdirect-REST sowie eingebettete Web-Session-/CSV-Importe für Amex & Amazon Visa.
- 💸 **SEPA-Überweisung & Echtzeitüberweisung via FinTS 3.0 / HKCCS** inkl. Verification of Payee (VoP), inkl. PushTAN, SecureGo plus & ChipTAN/photoTAN: IBAN-Prüfung (Mod-97), SEPA-konformer Zeichensatz, Empfängervorschläge aus bisherigen Buchungen und sofort vorgemerkte Buchung.
- 📑 **Sammelüberweisung via FinTS 3.0 / HKCCM:** mehrere Zahlungen von einem Konto in einem Auftrag mit einer TAN, inkl. CSV-Import; wahlweise als Sammelbuchung oder Einzelbuchungen. Verfügbar, wenn die Bank Sammelaufträge per FinTS anbietet; nicht als Echtzeitüberweisung.
- 🗓️ **Seite „Geplant“ mit Daueraufträgen:** Terminüberweisungen und per FinTS (HKCDB) abgerufene Daueraufträge erscheinen mit ihren nächsten Ausführungsterminen und lassen sich direkt per FinTS mit TAN-Freigabe anlegen, ändern und löschen (#496).
- 🧾 **SEPA-Lastschriften & Mandate:** Mandate und Gläubiger-ID verwalten, Einzüge als pain.008-Datei erzeugen (Basis, COR1, Firmen) und – sofern die Bank es per FinTS anbietet – mit TAN einreichen; Rücklastschriften werden erkannt und das Mandat zum Prüfen markiert.
- 📤 **Export:** Buchungen u. a. als CAMT.053-Kontoauszug (jeweils ein Konto und ein Zeitraum, mit Anfangs- und Endsaldo).
- 🏠 **Intelligente Darlehens-Verfolgung:** Tilgungsfortschritt in Prozent und automatische Verknüpfung der monatlichen „Zins & Tilgung“-Raten vom Girokonto.
- 🏷️ **Händler-Erkennung & Logos (Opt-in):** Händler und ETFs (REWE, EnBW, DB, iShares) werden lokal erkannt; Privatpersonen (z. B. „Max Mustermann“) erhalten bewusst nur neutrale Initialen. Händler-Logos werden **standardmäßig nicht** aus dem Netz geladen – das passiert erst nach Ihrer Zustimmung in den Einstellungen.
- ✏️ **Notizen schnell bearbeiten:** Doppelklick oder Return auf die Notiz in der Buchungsdetail-Ansicht.
- 🌙 **macOS Dark Mode:** folgt automatisch dem System-Erscheinungsbild (Hell/Dunkel).
- 🌐 **Lokalisierung DE/EN:** Die App ist auf Deutsch und Englisch verfügbar und folgt der Systemsprache.

- 🧾 **Rechnung scannen:** ZUGFeRD / Factur-X, GiroCode / EPC-QR und PDF-Text füllen die Überweisung vor.
- 🌍 **Banken & Dienste auf dem Mac:** Sparkasse, Volksbank / Atruvia, DKB, ING, comdirect, Consorsbank, Trade Republic, Scalable Capital, American Express, C24, Revolut, Finom, bunq und PayPal – per FinTS, Enable Banking (PSD2) oder direktem Konnektor (siehe Tabelle unten).

| Darlehen mit Tilgungsbalken | Inspektor mit Händler-Logo |
|---|---|
| <img src="docs/screenshots/loan-progress.png" alt="Immobilien-Darlehen, 27 % getilgt"> | <img src="docs/screenshots/analytics-or-inspector.png" alt="Inspektor mit EnBW-Buchung"> |

| SEPA-Überweisung mit IBAN-Prüfung |
|---|
| <img src="docs/screenshots/sepa-transfer.png" alt="Neue Überweisung an Stadtwerke München, IBAN geprüft" width="560"> |

| iOS – Übersicht | iOS – Buchungen |
|---|---|
| <img src="docs/screenshots/ios-overview.png" alt="GiroCrab auf dem iPhone – Übersicht" width="300"> | <img src="docs/screenshots/ios-transactions.png" alt="GiroCrab auf dem iPhone – Buchungen" width="300"> |

### Architektur

<p>
  <a href="https://github.com/GiroCrab/GiroCrab/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/GiroCrab/GiroCrab/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue">
  <img alt="Rust" src="https://img.shields.io/badge/core-Rust-orange">
  <img alt="Swift" src="https://img.shields.io/badge/app-SwiftUI-F05138">
  <img alt="Platforms: macOS 14+, iOS 17+, Apple Silicon" src="https://img.shields.io/badge/macOS%2014%2B%20%C2%B7%20iOS%2017%2B%20%C2%B7%20Apple%20Silicon-lightgrey">
  <img alt="PRs welcome" src="https://img.shields.io/badge/PRs-welcome-brightgreen">
</p>

```
┌──────────────────────────┐   UniFFI    ┌────────────────────────────────────┐
│ GiroCrabApp (SwiftUI)    │ ◄─────────► │ girocrab-core (Rust)               │
│ macOS 14+ (arm64) · iOS  │  Swift-API  │  vault.rs      SQLCipher-Tresor    │
│ 17+ · Face/Touch ID      │             │  rules.rs      Kategorien-Regeln   │
└──────────────────────────┘             │  merchants.rs  Händler-Erkennung   │
   ▲ optional: iCloud Drive              │  connectors/   FinTS · REST · PSD2 │
   │ (verschlüsselter Snapshot)          └──────────────┬─────────────────────┘
                                                        │ direkt, mit Ihren Zugangsdaten
                                                        ▼
                                                  Ihre Bank(en)
```

Kurz: `girocrab-core` (Rust, per UniFFI) enthält Tresor, Regeln, Händler-Erkennung und Bank-Konnektoren; `GiroCrabApp` (SwiftUI) ist die native Oberfläche für macOS und iOS. **Mitwirken:** siehe [CONTRIBUTING.md](CONTRIBUTING.md) – bitte lassen Sie sich ein Issue **vor Arbeitsbeginn zuweisen** (Ausnahme: Kleinstkorrekturen).

### Unterstützte Banken & Schnittstellen

<!-- sync:connectors:start -->
| Bank | Schnittstelle | Umsätze | Salden | Depot | Depot-Aktivitäten | Überweisung | Dauerauftrag | PDF/Postfach | Status |
|---|---|---|---|---|---|---|---|---|---|
| Advanzia Bank | Web-Portal | ✓ | ✓ | – | – | – | – | – | 🧪 Test gesucht [#177](https://github.com/GiroCrab/GiroCrab/issues/177) |
| Alte Leipziger (Bausparen) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | ✅ Verifiziert |
| Amazon Bestellungen | Web-Portal | ✓ | – | – | – | – | – | – | 🧪 Test gesucht |
| BHW Bausparkasse | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht |
| easybank (ehem. Barclays) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht |
| EquatePlus | Web-Portal | – | ✓ | ✓ | – | – | – | – | 🧪 Test gesucht |
| FFB (FIL Fondsbank) | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | 🧪 Test gesucht |
| FNZ Bank | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ✅ Verifiziert |
| Ford Money | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht |
| Hanseatic Bank | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht |
| Opel Direktbank | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht |
| quirion | Web-Portal | ✓ | ✓ | ✓ | – | – | – | ✓ | ✅ Verifiziert |
| Schwäbisch Hall (Bausparen) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Test gesucht [#290](https://github.com/GiroCrab/GiroCrab/issues/290) |
| Trade Republic | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ⚠️ Mit Einschränkungen |
| Uberspace | Web-Portal | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| Volkswagen Bank | Web-Portal | ✓ | ✓ | – | – | – | – | – | ⚠️ Mit Einschränkungen |
| 1822direkt | FinTS 3.0 | ✓ | ✓ | ✓ | – | – | – | ✓ | ✅ Verifiziert |
| Consorsbank | FinTS 3.0 | ✓ | ✓ | ✓ | – | – | – | – | ✅ Verifiziert |
| FinTS-Bank (BLZ + URL) | FinTS 3.0 | ✓ | ✓ | ✓ | – | ✓ | – | ✓ | ✅ Verifiziert |
| ING | FinTS 3.0 | ✓ | ✓ | – | – | – | – | – | 🧪 Test gesucht [#313](https://github.com/GiroCrab/GiroCrab/issues/313) |
| Miles & More Kreditkarte (Deutsche Bank) | FinTS 3.0 | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| Volksbank Kassel Göttingen | FinTS 3.0 | ✓ | ✓ | ✓ | – | ✓ | – | ✓ | ✅ Verifiziert |
| bunq | REST-API | ✓ | ✓ | – | – | ✓ | – | ✓ | ✅ Verifiziert |
| comdirect | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ✅ Verifiziert |
| Interactive Brokers (IBKR) | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | – | 🧪 Test gesucht |
| Kontist | REST-API | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| PayPal | REST-API | ✓ | ✓ | – | – | – | – | – | ⚠️ Mit Einschränkungen [#156](https://github.com/GiroCrab/GiroCrab/issues/156) |
| Scalable Capital | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | – | 🧪 Test gesucht [#45](https://github.com/GiroCrab/GiroCrab/issues/45) |
| Amazon Visa | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| American Express | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| Bank Norwegian | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| C24 | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| ING Business | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | 🧪 Test gesucht [#721](https://github.com/GiroCrab/GiroCrab/issues/721) |
| N26 | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| PayPal (DE) | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verifiziert |
| Revolut | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | 🧪 Test gesucht [#289](https://github.com/GiroCrab/GiroCrab/issues/289) |
| Weitere Banken per PSD2 (Enable Banking) | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ⚠️ Mit Einschränkungen |
| Finpension | Datei-Import | ✓ | ✓ | ✓ | – | – | – | – | 🧪 Test gesucht [#300](https://github.com/GiroCrab/GiroCrab/issues/300) |

Weitere deutsche Banken mit FinTS 3.0 (PIN/TAN) lassen sich über BLZ und FinTS-URL anbinden (2.011 Institute im FinTS-Verzeichnis); europäische PSD2-Banken über Enable Banking mit einer eigenen, kostenlosen Anwendung.

<sub>✅ mit echten Konten getestet · 🧪 implementiert, Rückmeldungen aus der Praxis willkommen (Issue verlinkt) · ⚠️ bekannte Einschränkungen · ✓ umgesetzt · – nicht umgesetzt. Bei FinTS hängen Depot und PDF-Kontoauszüge vom Angebot der jeweiligen Bank ab. Depot: Positionen; Depot-Aktivitäten: Käufe, Verkäufe und Ausschüttungen als eigene Einträge. Anlageklasse und Instrument ermittelt GiroCrab für jedes Depot selbst, einige Konnektoren übernehmen sie von der Bank.</sub>
<!-- sync:connectors:end -->

quirion: Einrichtung, technische Details und geprüfter Umfang stehen in [docs/quirion-connector.md](docs/quirion-connector.md).

FNZ Bank / ebase / finvesto: Einrichtung, Konto- und Depotumsätze, Fondspositionen, PDF-Archiv und Einschränkungen stehen in [docs/fnz-connector.md](docs/fnz-connector.md).

Eine neue Bank anzubinden dauert meist unter 100 Zeilen: siehe
[`connectors/template.rs`](girocrab-core/src/connectors/template.rs) und [CONTRIBUTING.md](CONTRIBUTING.md).

### Schnellstart

Voraussetzungen: **macOS 14+ auf Apple Silicon (`arm64`)**, Xcode 15+, [rustup](https://rustup.rs). iOS-Geräte benötigen **iOS 17+**.

```bash
./build-xcframework.sh          # Rust-Kern + Swift-Bindings (macOS / XCFramework)
cargo run -- --demo-seed        # App mit erfundenen Demo-Daten starten
# oder: GIROCRAB_DEMO_SEED=1 cargo run
cargo test -p girocrab-core     # Tests
```

Die Demo enthält fiktive Konten und erfundene Buchungen – keine echten Daten.

*Intel-Mac?* Der Quellcode-Build funktioniert auch dort mit Swift/Xcode; nur die fertigen Release-Binaries gibt es ausschließlich für Apple Silicon. Für `x86_64`: `rustup target add x86_64-apple-darwin`, in `build-xcframework.sh` das Ziel `aarch64-apple-darwin` durch `x86_64-apple-darwin` ersetzen (`TARGETS`, `MAC_LIB`), dann `./build-xcframework.sh --no-sim` und `swift build` in `GiroCrabApp/`.

**iOS (iPhone / iPad):** Am einfachsten per [TestFlight](https://testflight.apple.com/join/KBmvKVpT). Die App wird (noch) nicht über den App Store verteilt; wer selbst bauen möchte:
`GiroCrabApp/Xcode/GiroCrab.xcodeproj` in Xcode öffnen, Team (persönliche Apple-ID) unter *Signing & Capabilities* wählen und auf das Gerät installieren. Mit einer persönlichen Apple-ID signierte Apps laufen nur eine begrenzte Zeit und müssen danach neu installiert werden.

### 📦 Download (macOS)

Das fertige Disk-Image [`GiroCrab-macOS-arm64.dmg`](https://github.com/GiroCrab/GiroCrab/releases/latest/download/GiroCrab-macOS-arm64.dmg) (Apple Silicon, macOS 14+) gibt es unter [GitHub Releases](../../releases) – ohne Rust oder Xcode.
Aktuelle Version: **v0.8.7**. DMG öffnen und `GiroCrab.app` nach `/Programme` ziehen – die App ist **von Apple notarisiert** und aktualisiert sich per Sparkle selbst.
**iPhone / iPad:** [TestFlight-Beta beitreten](https://testflight.apple.com/join/KBmvKVpT).

**Intel-Macs:** Die offiziellen Binaries sind nur für Apple Silicon (`arm64`). Auf Intel-Macs lässt sich GiroCrab mit Swift/Xcode selbst aus dem Quellcode bauen (siehe [Schnellstart](#schnellstart)).

### ⚖️ Rechtlicher Hinweis / Haftungsausschluss (ZAG / PSD2)

GiroCrab ist ein **kostenloses, nicht-kommerzielles Open-Source-Werkzeug zur ausschließlich eigenen Nutzung** (selbst gehostete Software).
Es ist **kein Zahlungsauslösedienst und kein Kontoinformationsdienst im Sinne des ZAG bzw. der Richtlinie (EU) 2015/2366 (PSD2)**
und wird auch nicht als solcher angeboten.

- **Rein lokal:** Alle Verbindungen laufen ausschließlich direkt zwischen **Ihrem Gerät** und **Ihrer Bank** – mit **Ihren** Zugangsdaten,
  **Ihrer** FinTS-Produktregistrierung und **Ihren** API-Schlüsseln. Die Entwickler betreiben **keinen** Server; Konto-, Buchungs- oder
  Zugangsdaten gelangen **zu keinem Zeitpunkt** an die Entwickler. Die optionale iCloud-Synchronisation nutzt ausschließlich Ihren
  eigenen iCloud-Drive-Speicher (verschlüsselter Snapshot; den Schlüssel teilen Sie per Zwischenablage oder auf Wunsch über den iCloud-Schlüsselbund, siehe oben).
- **Nutzung auf eigenes Risiko, ohne Gewährleistung** (siehe GPLv3). Das gilt insbesondere für **SEPA-Überweisungen** – prüfen Sie
  Empfänger, IBAN und Betrag vor jeder Freigabe selbst – sowie für **inoffizielle Schnittstellen** (z. B. Trade Republic), die
  jederzeit ausfallen können und gegen Nutzungsbedingungen des Anbieters verstoßen können.
- Wer GiroCrab **für Dritte** betreibt, braucht ggf. eine BaFin-Erlaubnis – das ist nicht der vorgesehene Einsatzzweck.
- Keine Anlageberatung; ISIN/WKN-Angaben sind Referenzdaten ohne Gewähr. Marken gehören ihren Inhabern und dienen nur der Wiedererkennung.

<sub>Genannte Marken von Banken und Zahlungsdiensten gehören ihren jeweiligen Inhabern. GiroCrab ist unabhängig von diesen Anbietern.</sub>

---

## 🇬🇧 English

**Open-source online banking for Mac (Apple Silicon) and iPhone/iPad – local-first, encrypted, your data stays on your device.**
A native SwiftUI app on a Rust core: encrypted **SQLCipher banking vault** (AES-256, random 256-bit key), category tree, rules, merchant
recognition, savings-plan view, loan repayment progress and direct connections to your banks via
**FinTS 3.0 / HBCI** and **PSD2 (Enable Banking)** – no server, no tracking. Money is always `i64` cents, never floats.

### Who is GiroCrab for?

GiroCrab is a free, open-source banking app (GPLv3) for macOS, Linux (Qt 6 / QML) and – currently in beta via TestFlight – for iPhone and iPad. All data stays encrypted on your device. GiroCrab is a young community project: bank coverage is still growing (see the bank table), and PSD2 access works through your own free Enable Banking account.

### ✨ What's new in v0.8.7

- 🏦 **New Banks & Amex PDF Statements (#1168, #1435, #1440, #1451, #1456, #1416):** Native connectors for **Hanseatic Bank** (credit cards, savings & Postbox PDFs), **BHW Bausparkasse** (home savings, loans & mailbox PDFs), and **Kontist** business accounts, automatic & on-demand **American Express PDF statement downloads** in the portal sheet, verified **C24, N26, and Bank Norwegian** matrix entries, plus updated FinTS endpoint for **1822direkt**.
- 🧮 **Per-Account Totals Switch & Account Settings (#1100, #1445, #1455):** Exclude individual accounts from overall totals, cashflow, and net worth (synced across devices; Amazon Orders accounts start excluded by default), delete archived accounts directly from Account Settings, and save accounts with negative opening balances cleanly.
- 💸 **Recipient Management, Booking Notes & FinTS Transfers (#1129, #1130, #1441, #1088, #1419):** Manage/hide saved transfer recipients, jump straight to booking notes via the new list indicator, send immediate FinTS transfers without rejected execution dates, and detect `pain.001.001.09` from `HISPAS`.
- 🐧 **Linux Release Packages & Android Native Layer (#1150, #1151, #1458, #1384, #1423, #1443):** EquatePlus sign-in & PDFs, auto-sync with clean dialog cancellation during "Retrieve All", full privacy redaction, Breeze fallback icons, prebuilt `.deb` & `.flatpak` release assets, and CI-verified multi-ABI Rust/UniFFI builds on Android.
- 🛠️ **iPadOS FinTS Registration, Amex €0 Balance & Period Sync (#1449, #1446, #1444, #1379):** Verified built-in FinTS product registration inside the iOS/iPadOS TestFlight archive (Build 24 under version `0.7.5`), accurate `€0.00` Amex balance after paid statements, aligned headers during period switches, and workspace-wide `forbid(unsafe_code)` obfuscation checks.

### ✨ What's new in v0.8.6

- 🔍 **Natural Search & Document Filters:** Find bookings using natural-language queries and filter directly by attached documents.
- 🏦 **Bank, Instant Transfer & Import Fixes:** Consorsbank archive support, FFB mailbox PDFs (#1105) & FFB depot twin folding (#1103), Ford Money / Opel tax-line matching (#1102), Ford Money approval polling (#1101), Advanzia account re-add fix (#1115), Trade Republic clearing account adoption (#1114), PayPal foreign-currency bookings (#1110) & Enable Banking currency display (#1034), pending reservation retention (#1111), ING Business in bank search (#974), and instant transfer `3045` handover handling (#1121).
- 🔐 **Security & FinTS Registration:** Official releases always use GiroCrab's built-in FinTS product registration (#1092), harden in-memory connector state (#1094), and fix read-only keychain checks (#1084).
- 📱 **iOS / TestFlight – Full v0.8.6 under the hood (Build 22 under version 0.7.5):** To deliver updates without Apple review delays, the complete **v0.8.6 feature set** ships on iOS as Build 22 under version `0.7.5`.

### ✨ What's new in v0.8.5

- 🔐 **Keychain & iCloud Key Adoption:** Improved recovery and adoption of existing vault keys on macOS and iOS, plus clearer demo-mode persistence hints.
- 🏦 **Banks & Accounts:** Enable Banking fixes (including Spuerkeess), refined mobile account views, and improved category/account navigation on Windows.
- 📱 **iOS / TestFlight – Full v0.8.5 under the hood (Build 22 under version 0.7.5):** To deliver updates without Apple review delays, the complete **v0.8.5 feature set** ships on iOS as Build 22 under version `0.7.5`.
- 🧪 **Windows & Android (Alpha Preview):** Experimental alpha builds for Windows and Android are now attached directly to the release for early testing.

### ✨ What's new in v0.8.0
- 🏦 **Built-in FinTS Product Registration:** Official FinTS product registration ID is now bundled out of the box in release builds – no manual setup required.
- 🛡️ **Automatic Vault Backup & Restore:** GiroCrab now automatically creates encrypted safety backups of your vault before migrations and critical operations.
- 📈 **EquatePlus, IBKR Native Currencies & C24 Categories (#750, #720, #625, #626, #763, #678):** New EquatePlus connector (employee equity plans & PDF statements), automatic archiving of deselected accounts, native currency display per IBKR depot position (plus retry backoff), C24 category import, and custom categories/groups preserved across language changes.
- ⚡ **Major Performance Gains (#754, #759, #760):** `ListCache` for booking lists and merchant history (~20–30× faster), pre-compiled `LazyLock` regexes across importers (~1,000× faster on large imports), and targeted `apply_rules_to` after iCloud merges.
- 🏷️ **Tag Filtering & Analytics, Recurring & Multi-Select (#228, #777, #731, #755, #615, #756, #725, #753, #659):** Cashflow card follows sidebar account selection, recurring detection separates known merchants sharing an IBAN, and “Reviewed” applies to the full multi-selection.
- 📱 **iOS / TestFlight – Full v0.8.0 under the hood (watch the Build number!):** Community development is currently moving faster than Apple's review cycle for new marketing version trains. To deliver updates without review delays, the complete **v0.8.0 feature set** (including vault schema v27 for seamless iCloud sync with macOS v0.8.0) ships on iOS immediately as a new **Build under version 0.7.5**.
- 🪟 **New & Experimental – GiroCrab for Windows (`GiroCrabWindows/`):** Very early Windows preview powered by the exact same `girocrab-core`! **Looking for testers & Rust/Windows contributors** – take it for a spin, share feedback via issues (`windows`), or jump in with PRs!

### ✨ What's new in v0.7.5
- **FinTS Standing Orders (#496):** Create, edit, and delete standing orders via FinTS with TAN approval on macOS and iOS (including creating from existing bookings).
- **C24 Pockets & Counter-Accounts (#523):** Own sub-accounts/pockets are shown directly in the booking row and detail card for internal transfers.
- **Account Header & List Polish (#336, #512, #538):** Account notes appear directly in the header, the category chip can be toggled per account, and unnecessary scrollbars are hidden.
- **Budgets & Classification (#544, #541, #540):** Monthly budgets work independently of analytics, categorized income counts even with transfer wording, and switching fetch methods no longer duplicates accounts.

### ✨ What's new in v0.7.0

- 🐧 **Native Linux port (`GiroCrabLinux/`, #389, #419):** a full Qt 6/QML desktop app on the same `girocrab-core` – including Secret Service, QtWebEngine, PDF receipts, OCR, GiroCode, and `.deb` and Flatpak packages.
- 📊 **Graphical net-worth overview (#348, #415):** new sidebar view “Net worth” with net worth, donut charts, cash history and grouped cards for assets and liabilities; plus “New transactions” with account cards for unreviewed bookings.
- 🏦 **New banks & broker improvements:** Consorsbank (FinTS), easybank (credit card, PDF statements), tangible assets such as gold, real estate and vehicles, Scalable Capital rounding and depot-transfer fix (#411), FNZ/ebase portal with security transactions (#314) and Enable Banking setup hints (#344).
- 🛒 **Amazon product links & inspector comfort (#354, #299, #295, #412, #407):** clickable item links (ASIN) in the booking detail, merchant history split into “This account / All accounts”, account-group ordering for receipts and rules, and clear hints for split bookings.
- ☁️ **iCloud Drive folder “GiroCrab” (#437):** it replaces the legacy folder, existing data is copied; also sharing the Enable Banking application via iCloud Keychain (#356).
- 🛡️ **More:** in-app bug report with automatic redaction (#301), adjustable text size (#343), reset app (#241), edit and hide budgets (#430), plus dependency security updates (RUSTSEC-2023-0071, -2026-0387, -2026-0502).

**Highlights from v0.6.2:**

- 🧮 **Opening balance for accounts with a computed balance (#212, #227):** set an opening balance with an optional cut-off date and derive it from the latest statement balance with “Calibrate”; the database import picks it up. Removing it falls back to the plain sum of the bookings.
- 🗄️ **Versioned schema migrations with downgrade protection (#206, #232):** vaults migrate in numbered steps; a vault written by a newer version is refused. This version upgrades vaults to **schema 10** – make a backup copy first if you may want to open the vault in v0.6.1 again.
- 🖱️ **Drag & drop in the sidebar with an insertion line (#230)**, unambiguous PSD bank names in the FinTS directory (#145) and a copy menu on iOS (account and depot positions).

**Highlights from v0.6.1:**

- 📲 **iOS & iPadOS via Apple TestFlight:** install and auto-update the iOS app on iPhone and iPad through the **[TestFlight invitation](https://testflight.apple.com/join/KBmvKVpT)** – no Xcode build needed any more.
- 💿 **macOS installer as `.dmg` + Sparkle auto-updates:** a disk image signed and notarized by Apple (`GiroCrab-macOS-arm64.dmg`) with built-in updates via Sparkle.
- ⚡ **Category assignment in `< 1 ms` (#159):** optimistic UI update and new SQLite covering indexes keep categorising instant even with 30,000+ bookings.
- ✅ **Mark bookings as reviewed automatically (#176):** new option under *Settings › General › Bookings* – after a configurable delay or manual only.
- 🔴 **macOS Dock badge (#173) & coloured account icons (#175):** optional count of unchecked bookings in the Dock; sidebar account symbols show their account colour. Plus the shared **icon & colour popover** for accounts and groups (#183).
- 🧙 **New Apple-HIG wizards:** *Import assistant* with balance reconciliation (#181) and *Add bank* (#184).
- 🏦 **Enable Banking (PSD2) & multi-currency (#164):** Finom business accounts and multi-currency wallets sharing one IBAN; pending bookings for comdirect (#186) and bunq (#187).
- 📣 **Project notices (#185):** a calm, signed notice feed – no release needed, no tracking.

**Highlights v0.4.0 – v0.5.5:**

- 🗄️ **Direct database import:** an encrypted `.sqlite` database (accounts, category tree, notes, depot holdings, PDF statements) is opened **read-only**.
- 📁 **Custom account groups & subgroups:** freely sortable, synced via iCloud, with a totals row.
- ☁️ **Optional iCloud Drive sync** between Macs and iOS devices via an encrypted SQLCipher snapshot merge.
- 🔒 **Merchant logos opt-in only (#38):** online logos are **off** by default; without opt-in everything stays 100 % local.

### 📥 Importing existing data

GiroCrab can import transactions, categories and notes from supported export files. A preview shows what was recognised before anything is imported; unsupported files are reported there. Bank connections are set up separately in GiroCrab afterwards.

<!-- TODO(Andreas): please check: re-shoot a screenshot of the import assistant without any third-party brand (old import screenshots removed) -->

Hierarchical categories (`Household\Groceries`) are kept as a tree, notes show in the booking detail view.
Every booking gets a fingerprint (date, amount, IBAN/name, purpose), so a repeated import or overlapping days
fetched later via FinTS are recognised as duplicates and not created twice – existing bookings are only enriched with category and note.
**Everyday convenience features:**
* **📋 Transfer from clipboard (`⌘V` / `⇧⌘V`):** Paste invoice text or bank details from the clipboard straight into the transfer sheet — recipient, checksum-verified IBAN, BIC, amount and purpose are filled in automatically.
* **📄 E-Invoice & PDF Drop:** Drag & drop a PDF invoice onto the transfer sheet (parses embedded **ZUGFeRD / Factur-X XML**, **EPC QR / GiroCode**, or PDF/OCR text).
* **✅ Checked toggle (`Space`):** Mark bookings as checked directly via the Spacebar.
* **🖱️ Context menu actions:** Right-click any booking to filter all transactions by that counterparty or pre-fill a new transfer from the booking.
### Features & Screenshots

<p align="center"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/overview-dark.png"><img src="docs/screenshots/overview.png" alt="GiroCrab – overview with fictional demo accounts" width="900"></picture></p>

<p align="center"><img src="docs/screenshots/workflow-demo.gif" alt="Animated tour: overview, everyday spending, savings plans, dark mode" width="900"></p>

*All screenshots show purely fictional demo data (`GIROCRAB_DEMO_SEED=1`).*

- 🔒 **Local-first:** all banking data lives on your device in an encrypted vault (SQLCipher, AES-256) protected by the Keychain / Touch ID or Face ID. There is no developer server.
- 📱 **macOS + iOS (iPhone / iPad):** one SwiftUI codebase on the shared Rust core. Requires **macOS 14+ (Apple Silicon, `arm64`)** or **iOS 17+**. On iPhone and iPad the app is available via **[Apple TestFlight](https://testflight.apple.com/join/KBmvKVpT)** (or optionally built from source with Xcode).
- ☁️ **Optional iCloud Drive sync:** if you want, sync vault data between Macs and iOS devices through your own iCloud Drive folder (`iCloud Drive/GiroCrab/`). The vault goes to the cloud only as an **encrypted SQLCipher snapshot** (`wal_checkpoint(TRUNCATE)`), and devices are reconciled with a deterministic `ATTACH DATABASE` merge. **A live SQLite database with WAL files is never opened directly in cloud storage.** Sync is **off by default** and has to be enabled in Settings; once on, it runs when the vault is unlocked. The folder `iCloud Drive/GiroCrab/` is only created once you enable sync. 🔑 **Key handling:** the vault key is **not** stored in the iCloud folder (the `tresor.key` file from v0.4.0 is deleted automatically on the next sync). If you want, GiroCrab shares it through **iCloud Keychain** (`kSecAttrSynchronizable`); this is off by default and is turned on in Settings. Alternatively, copy the key on the first device (“Copy vault key”, clipboard is cleared after 60 s) and paste it once on the second device (“Paste key …”). ⚠️ If you used v0.4.0 with sync enabled, the old `tresor.key` may still exist in iCloud versions/trash; key rotation is planned for a later release.
- 🗄️ **Database import:** read-only import of an encrypted `.sqlite` file incl. accounts, categories, hierarchy and booking history (see “Importing existing data”).
- 🏦 **FinTS 3.0 / HBCI client & PSD2 for macOS and iOS:** FinTS 3.0 / HBCI (CAMT.052 & MT940 with smart Latin-1/UTF-8 decoding), **bunq** (personal API key), PayPal, Trade Republic WebSocket, comdirect REST, and embedded web-session/CSV import for Amex & Amazon Visa.
- 💸 **Native SEPA credit transfers via FinTS 3.0 / HKCCS** incl. PushTAN, SecureGo plus & ChipTAN/photoTAN: IBAN checksum validation (Mod-97), SEPA-compliant character set, recipient suggestions from past bookings and an instant pending booking.
- 📑 **Batch transfers via FinTS 3.0 / HKCCM:** several payments from one account in a single order with one TAN, incl. CSV import; booked as one batch entry or as individual entries. Available where the bank offers batch orders via FinTS; not as instant transfers.
- 🗓️ **"Planned" page with standing orders:** scheduled transfers and the bank's standing orders fetched via FinTS (HKCDB) are shown with their next execution dates and can be created, edited and deleted via FinTS with TAN approval (#496).
- 🧾 **SEPA direct debits & mandates:** manage mandates and the creditor ID, create collections as pain.008 files (Core, COR1, B2B) and – where the bank offers it via FinTS – submit them with a TAN; returned debits are detected and the mandate is flagged for review.
- 📤 **Export:** bookings among others as a CAMT.053 statement (one account and one period at a time, with opening and closing balance).
- 🏠 **Smart loan tracking:** automatic repayment progress (%) and cross-account linking of the monthly "Zins & Tilgung" instalments from your checking account.
- 🏷️ **Merchant recognition & logos (opt-in):** German merchants & ETFs (REWE, EnBW, DB, iShares) are recognized locally, while private individuals (e.g. "Max Mustermann") get plain monogram initials by design. Merchant logos are **not** fetched online by default – only after you opt in under Settings.
- ✏️ **Quick note editing:** double-click or press Return on the note in the booking detail view.
- 🌙 **macOS Dark Mode:** automatically follows the system appearance (light/dark).
- 🌐 **German & English localization:** the app ships in German and English and follows the system language.

- 🧾 **Scan invoices:** ZUGFeRD / Factur-X, GiroCode / EPC-QR and PDF text pre-fill the transfer.
- 🌍 **Banks & services on the Mac:** Sparkasse, Volksbank / Atruvia, DKB, ING, comdirect, Consorsbank, Trade Republic, Scalable Capital, American Express, C24, Revolut, Finom, bunq and PayPal – via FinTS, Enable Banking (PSD2) or a direct connector (see the table below).

| Loan with repayment bar | Inspector with merchant logo |
|---|---|
| <img src="docs/screenshots/loan-progress.png" alt="Immobilien-Darlehen, 27% repaid"> | <img src="docs/screenshots/analytics-or-inspector.png" alt="Inspector showing an EnBW booking"> |

| SEPA transfer with IBAN check |
|---|
| <img src="docs/screenshots/sepa-transfer.png" alt="New transfer to Stadtwerke München, IBAN validated" width="560"> |

| iOS – Overview | iOS – Transactions |
|---|---|
| <img src="docs/screenshots/ios-overview.png" alt="GiroCrab on iPhone – overview" width="300"> | <img src="docs/screenshots/ios-transactions.png" alt="GiroCrab on iPhone – transactions" width="300"> |

### Architecture

<p>
  <a href="https://github.com/GiroCrab/GiroCrab/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/GiroCrab/GiroCrab/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue">
  <img alt="Rust" src="https://img.shields.io/badge/core-Rust-orange">
  <img alt="Swift" src="https://img.shields.io/badge/app-SwiftUI-F05138">
  <img alt="Platforms: macOS 14+, iOS 17+, Apple Silicon" src="https://img.shields.io/badge/macOS%2014%2B%20%C2%B7%20iOS%2017%2B%20%C2%B7%20Apple%20Silicon-lightgrey">
  <img alt="PRs welcome" src="https://img.shields.io/badge/PRs-welcome-brightgreen">
</p>

See the diagram above: the SwiftUI app (`GiroCrabApp`, macOS and iOS) talks to `girocrab-core` (Rust) through UniFFI; the core owns the
encrypted vault, categorisation rules, merchant resolver and the bank connectors, and talks to
your bank directly. Optional iCloud sync exchanges an encrypted snapshot via your own iCloud Drive.

**Contributing:** see [CONTRIBUTING.md](CONTRIBUTING.md) – please ask to be **assigned to an issue before you start** (tiny fixes excepted).

### Supported Banks & Interfaces

<!-- sync:connectors:start -->
| Bank | Interface | Transactions | Balances | Securities | Securities activity | Transfers | Standing orders | PDF/post box | Status |
|---|---|---|---|---|---|---|---|---|---|
| Advanzia Bank | Web-Portal | ✓ | ✓ | – | – | – | – | – | 🧪 Testing wanted [#177](https://github.com/GiroCrab/GiroCrab/issues/177) |
| Alte Leipziger (Bausparen) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | ✅ Verified |
| Amazon orders | Web-Portal | ✓ | – | – | – | – | – | – | 🧪 Testing wanted |
| BHW Bausparkasse | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted |
| easybank (ehem. Barclays) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted |
| EquatePlus | Web-Portal | – | ✓ | ✓ | – | – | – | – | 🧪 Testing wanted |
| FFB (FIL Fondsbank) | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | 🧪 Testing wanted |
| FNZ Bank | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ✅ Verified |
| Ford Money | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted |
| Hanseatic Bank | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted |
| Opel Direktbank | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted |
| quirion | Web-Portal | ✓ | ✓ | ✓ | – | – | – | ✓ | ✅ Verified |
| Schwäbisch Hall (Bausparen) | Web-Portal | ✓ | ✓ | – | – | – | – | ✓ | 🧪 Testing wanted [#290](https://github.com/GiroCrab/GiroCrab/issues/290) |
| Trade Republic | Web-Portal | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ⚠️ Limited |
| Uberspace | Web-Portal | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| Volkswagen Bank | Web-Portal | ✓ | ✓ | – | – | – | – | – | ⚠️ Limited |
| 1822direkt | FinTS 3.0 | ✓ | ✓ | ✓ | – | – | – | ✓ | ✅ Verified |
| Consorsbank | FinTS 3.0 | ✓ | ✓ | ✓ | – | – | – | – | ✅ Verified |
| FinTS bank (bank code + URL) | FinTS 3.0 | ✓ | ✓ | ✓ | – | ✓ | – | ✓ | ✅ Verified |
| ING | FinTS 3.0 | ✓ | ✓ | – | – | – | – | – | 🧪 Testing wanted [#313](https://github.com/GiroCrab/GiroCrab/issues/313) |
| Miles & More Kreditkarte (Deutsche Bank) | FinTS 3.0 | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| Volksbank Kassel Göttingen | FinTS 3.0 | ✓ | ✓ | ✓ | – | ✓ | – | ✓ | ✅ Verified |
| bunq | REST-API | ✓ | ✓ | – | – | ✓ | – | ✓ | ✅ Verified |
| comdirect | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | ✓ | ✅ Verified |
| Interactive Brokers (IBKR) | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | – | 🧪 Testing wanted |
| Kontist | REST-API | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| PayPal | REST-API | ✓ | ✓ | – | – | – | – | – | ⚠️ Limited [#156](https://github.com/GiroCrab/GiroCrab/issues/156) |
| Scalable Capital | REST-API | ✓ | ✓ | ✓ | ✓ | – | – | – | 🧪 Testing wanted [#45](https://github.com/GiroCrab/GiroCrab/issues/45) |
| Amazon Visa | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| American Express | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| Bank Norwegian | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| C24 | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| ING Business | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | 🧪 Testing wanted [#721](https://github.com/GiroCrab/GiroCrab/issues/721) |
| More banks via PSD2 (Enable Banking) | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ⚠️ Limited |
| N26 | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| PayPal (DE) | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | ✅ Verified |
| Revolut | Enable Banking (PSD2) | ✓ | ✓ | – | – | – | – | – | 🧪 Testing wanted [#289](https://github.com/GiroCrab/GiroCrab/issues/289) |
| Finpension | Datei-Import | ✓ | ✓ | ✓ | – | – | – | – | 🧪 Testing wanted [#300](https://github.com/GiroCrab/GiroCrab/issues/300) |

Further German banks with FinTS 3.0 (PIN/TAN) can be connected via bank code and FinTS URL (2,011 institutions in the FinTS directory); European PSD2 banks via Enable Banking with your own free application.

<sub>✅ tested with real accounts · 🧪 implemented, feedback from real accounts welcome (issue linked) · ⚠️ known limitations · ✓ implemented · – not implemented. For FinTS, securities and PDF statements depend on what the individual bank offers. Securities: positions; securities activity: buys, sells and distributions as entries of their own. GiroCrab works out asset class and instrument for every depot itself; some connectors take them over from the bank.</sub>
<!-- sync:connectors:end -->

Adding a bank usually takes under 100 lines – start from
[`connectors/template.rs`](girocrab-core/src/connectors/template.rs) and read [CONTRIBUTING.md](CONTRIBUTING.md).

### Download (macOS)

The pre-built disk image [`GiroCrab-macOS-arm64.dmg`](https://github.com/GiroCrab/GiroCrab/releases/latest/download/GiroCrab-macOS-arm64.dmg) (Apple Silicon, macOS 14+) is available under
[GitHub Releases](../../releases) – no Rust or Xcode required. Current version: **v0.8.7**.

> 🧪 **Windows & Android (Early Alpha):** Experimental preview installers for [Windows (`x86_64-setup.exe`)](https://github.com/GiroCrab/GiroCrab/releases/latest/download/girocrab-windows-x86_64-setup.exe) and [Android (`.apk`)](https://github.com/GiroCrab/GiroCrab/releases/latest/download/GiroCrab-Android-alpha.apk) are available for early testing (best tried in demo mode). Open the DMG and drag `GiroCrab.app` to `/Applications` – the app is **notarized by Apple** and updates itself via Sparkle.
**iPhone / iPad:** [join the TestFlight beta](https://testflight.apple.com/join/KBmvKVpT).

**Intel Macs:** the official binaries are Apple Silicon (`arm64`) only. On Intel Macs you can build GiroCrab yourself from source with Swift/Xcode (see [Quickstart](#quickstart)).

### Quickstart

Requirements: **macOS 14+ on Apple Silicon (`arm64`)**, Xcode 15+, [rustup](https://rustup.rs). iOS devices need **iOS 17+**.

```bash
./build-xcframework.sh          # Rust core + Swift bindings (macOS / XCFramework)
cargo run -- --demo-seed        # launch the app with fictional demo data
# or: GIROCRAB_DEMO_SEED=1 cargo run
cargo test -p girocrab-core     # run the tests
```

The demo ships fictional accounts and invented bookings – no real data.

*Intel Mac?* Building from source with Swift/Xcode works there too; only the pre-built release binaries are Apple Silicon-only. For `x86_64`: run `rustup target add x86_64-apple-darwin`, replace `aarch64-apple-darwin` with `x86_64-apple-darwin` in `build-xcframework.sh` (`TARGETS`, `MAC_LIB`), then `./build-xcframework.sh --no-sim` and `swift build` in `GiroCrabApp/`.

**iOS (iPhone / iPad):** easiest via [TestFlight](https://testflight.apple.com/join/KBmvKVpT). The app is not (yet) in the App Store; to build it yourself:
open `GiroCrabApp/Xcode/GiroCrab.xcodeproj` in Xcode, choose your team (personal Apple ID) under *Signing & Capabilities* and install on your device. Apps signed with a personal Apple ID expire after a limited time and have to be reinstalled.

### ⚖️ Legal notice / Disclaimer (PSD2 / ZAG)

GiroCrab is a **free, non-commercial open-source tool for your own personal use only** (self-hosted software).
It is **not a payment initiation service and not an account information service within the meaning of the German ZAG / PSD2
(Directive (EU) 2015/2366)** and is not offered as one.

- **Strictly local:** all connections run directly between **your device** and **your bank**, using **your** credentials,
  **your** FinTS product registration and **your** API keys. The authors operate **no** server, and no account, booking or
  credential data **ever** reaches the authors. Optional iCloud sync uses only your own iCloud Drive storage (encrypted snapshot; you share the key via the clipboard or, if you turn it on, through iCloud Keychain, see above).
- **Use at your own risk, without warranty** (see GPLv3). This applies in particular to **SEPA transfers** – always verify
  recipient, IBAN and amount yourself before authorising – and to **unofficial interfaces** (e.g. Trade Republic), which can
  break at any time and may violate the provider's terms of service.
- Operating GiroCrab **for third parties** may require BaFin authorisation and is not the intended use.
- Not investment advice; ISIN/WKN data is reference information without guarantee. Trademarks belong to their owners and are used for identification only.

<sub>Names of banks and payment services are trademarks of their respective owners. GiroCrab is independent of these providers.</sub>

---

## FinTS-Produktregistrierung / FinTS product registration

**Deutsch:** Offiziell signierte Releases enthalten GiroCrabs eigene, bei der Deutschen Kreditwirtschaft registrierte
Produkt-ID (nur im FinTS-Segment HKVVB an die Bank, nie im Quellcode). Selbst gebaute Open-Source-Binaries enthalten
sie **nicht**: Dort verwendet die App ausschließlich eine ID, die Sie selbst eintragen, oder beim Entwickeln per
`GIROCRAB_FINTS_PRODUCT_ID_OVERRIDE=<ID>` setzen (nur Debug-Builds). Eigene Builds mit eingebauter ID entstehen, wenn beim
Build `GIROCRAB_FINTS_PRODUCT_ID` gesetzt ist (siehe `docs/fints-registrierung.md`).

**English:** Officially signed releases include GiroCrab's own product ID registered with the Deutsche Kreditwirtschaft
(sent to the bank only in the FinTS HKVVB segment, never in source code). Self-built open-source binaries do **not**
include it: they use only an ID you enter yourself, or, for development, one set via
`GIROCRAB_FINTS_PRODUCT_ID_OVERRIDE=<ID>` (debug builds only). To build your own binary with an embedded ID, set
`GIROCRAB_FINTS_PRODUCT_ID` at build time (see `docs/fints-registrierung.md`).

---

## Security / Sicherheit

SQLCipher vault (AES-256 with a per-page HMAC-SHA512, random 256-bit key). On Apple devices the key stays in the Keychain
(`ThisDeviceOnly`) unless you turn on key sync via iCloud Keychain; Linux and Windows keep it in the system keyring /
Credential Manager. TANs live only in memory (`zeroize`); a bank PIN is
kept only if you choose to save it, then inside the encrypted vault (and so also in its encrypted iCloud sync file and
backups). No telemetry. Merchant logos are **opt-in** (off by default, Settings → Merchant logos); without it, no network
request is made for logos and everything stays 100 % local. **Never attach real bank data or credentials to issues.**

DE: Händler-Logos sind **standardmäßig deaktiviert** (Opt-in in den Einstellungen) – ohne Zustimmung werden keine Logos aus dem Netz geladen, alles bleibt zu 100 % lokal.

Sachwerte (gold): only if gold items exist, GiroCrab fetches the daily `XAU/EUR` price from `api.frankfurter.dev` (at most one request a day; the request contains no personal data, quantities or values). Without gold items, no such request is made.

DE: Nur wenn Gold-Sachwerte existieren, ruft GiroCrab den Tageskurs `XAU/EUR` von `api.frankfurter.dev` ab (höchstens eine Anfrage pro Tag; die Anfrage enthält keine persönlichen Daten, Mengen oder Werte). Ohne Gold-Sachwerte findet keine solche Anfrage statt.

**Security lock for bank connections:** if a version turns out to endanger bank access (e.g. a vulnerability in a
connector), the publisher can publish a signed `banking-policy.json`; older versions then stop contacting banks until
they are updated – reading and exporting keep working. Once activated, the app downloads this small signed file from
GitHub at most every 15 minutes (no personal data). Details: [docs/banking-policy.md](docs/banking-policy.md).

DE: **Sicherheitssperre für Bankverbindungen:** Enthält eine Version eine Sicherheitslücke oder einen schweren Fehler im Bankzugang, kann der Herausgeber eine signierte `banking-policy.json` veröffentlichen. Ältere Versionen kontaktieren dann keine Bank mehr, bis sie aktualisiert sind; Lesen und Exportieren bleiben möglich. Sobald aktiviert, lädt die App diese kleine Datei höchstens alle 15 Minuten von GitHub (ohne persönliche Daten). Details: [docs/banking-policy.md](docs/banking-policy.md).

## 🤖 KI-Transparenz / AI Transparency

DE: GiroCrab entsteht in hybrider Mensch-KI-Entwicklung und ist Living Lab des Forschungsprojekts *„AI-enabled Transparency of AI Use in Software Creation Processes“*. Jeder PR bekommt automatisch eine **AI Provenance & Transparency Card**. Echte Finanzdaten kommen nie in Prompts.

EN: Every PR automatically gets an AI provenance card. Real financial data never goes into prompts.

→ **[docs/ai-transparency.md](docs/ai-transparency.md)**

---

## 🏦 Wie funktioniert der Bankabgleich? (FAQ & Technik)

### 1. Wie richte ich den Abgleich mit meinen Banken ein?
1. **Bankverbindung hinterlegen:** Klicke unten in der linken Seitenleiste (oder über die Befehlspalette `⌘K`) auf **„Bankverbindungen“** (*Connections*) → **„Bankverbindung hinzufügen“**.
   - **Klassische Banken (Sparkassen, Volksbanken, DKB, ING, Comdirect, Consorsbank, Deutsche Bank etc.):** Wähle **FinTS / HBCI** und melde dich direkt mit deiner BLZ/BIC sowie deiner Online-Banking-Kennung und PIN an (keine Registrierung bei Drittanbietern nötig).
   - **Neobroker & Direkt-Konnektoren (Trade Republic, Scalable Capital, bunq, PayPal, FNZ, quirion etc.):** Wähle den jeweiligen Direkt-Konnektor aus und melde dich mit deinen regulären Zugangsdaten an.
   - **PSD2-Banken über Enable Banking (N26, C24, Revolut, American Express, Amazon Visa, Trade Republic ohne ständige 2FA etc.):** Für den offiziellen PSD2-/XS2A-Abruf benötigst du einmalig einen kostenlosen persönlichen API-Schlüssel im *Restricted Mode* von Enable Banking (`https://localhost/girocrab/callback`). 👉 **[Interaktive Schritt-für-Schritt-Anleitung (3 Min.)](https://girocrab.de/girocrab_psd2_anleitung.html)** · **[Markdown-Guide](docs/enable-banking-setup.md)**
2. **2FA bestätigen:** Bestätige die Freigabe (Push-TAN / App-Freigabe / PhotoTAN) auf deinem Smartphone. GiroCrab legt alle gefundenen Konten automatisch an.
3. **Umsätze abrufen (Rundruf):** Klicke oben in der Symbolleiste auf **Synchronisieren (`↻`)** oder drücke **`⌘R`**. GiroCrab ruft neue Umsätze und Salden ab, wendet deine Kategorien-Regeln an und verhindert über deterministische Buchungs-Fingerprints zuverlässig Duplikate (auch nach einem vorherigen CSV-/CAMT-Import).

### 2. Läuft der Abgleich technisch über einen Drittanbieter-Service oder über Webscraping?
GiroCrab betreibt **kein eigenes Cloud-Backend** und nutzt kein Headless-Browser-Scraping:
- **FinTS 3.0 & Direkt-Konnektoren (100 % Point-to-Point):** Dein Gerät spricht direkt mit dem Server deiner Bank – ohne zwischengeschalteten Dienst.
- **PSD2 / XS2A (Bring-Your-Own-Key über Enable Banking):** Weil Banken für die PSD2-Schnittstelle gesetzlich ein qualifiziertes eIDAS-Zertifikat (AISP-Zulassung) verlangen, das nicht in einer Open-Source-App ausgeliefert werden kann, nutzt GiroCrab hierfür optional deinen eigenen, kostenlosen Personal-Zugang bei Enable Banking ([interaktive Einrichtungs-Anleitung](https://girocrab.de/girocrab_psd2_anleitung.html)):

* **Natives FinTS 3.0 / HBCI:** Klassische Banken werden direkt aus dem Rust-Kern (`girocrab-core`) über die offiziellen FinTS-3.0-HTTPS-Endpunkte angesprochen (inkl. starker Kundenauthentifizierung nach TAN-Prozess 996/997, Decoupled-TAN, PushTAN & PhotoTAN sowie Abruf von MT940 / CAMT.052 / CAMT.053 und SEPA-Überweisungen via HKCCS).
* **Direkte Mobile-/Web-API-Clients in Rust:** Für Banken und Broker ohne FinTS-Schnittstelle (wie Trade Republic oder Scalable Capital) nutzt GiroCrab keine fragilen Browser-Scraper, sondern spricht über `reqwest` + `rustls` direkt die nativen JSON-, GraphQL- und WebSocket-Endpunkte der Banken an (z. B. lokales ECDSA-P256-Device-Pairing und signierte Requests bei Trade Republic).
* **Zero-Knowledge Datenhaltung & iCloud-Sync (macOS & iOS):** Alle Zugangsdaten, Session-Tokens und Umsätze liegen ausschließlich lokal in deinem verschlüsselten SQLCipher-Tresor (AES-256; geschützt durch Touch ID / Face ID). Über den optionalen **Zero-Knowledge iCloud-Sync** lassen sich Tresore zwischen Mac und iPhone/iPad konfliktfrei auf Buchungs-Fingerprint-Ebene zusammenführen, ohne dass Klartextdaten jemals dein Gerät verlassen.


## 🤝 Contributors / Mitwirkende

Danke an alle, die GiroCrab voranbringen! / Thanks to everyone who helps move GiroCrab forward!

- **[@24unix](https://github.com/24unix)** — Direct database import (#11), search-expression rules (#60), Dark Mode (#2), DE/EN localisation (#4), key-unlock UX hint (#6), privacy opt-in for online merchant logos (#38), iCloud sync architecture (#10), Dock badge for unchecked bookings (#173), auto-review delay (#176), account/group icon & colour popover (#183) and Enable Banking business banks (#164).
- **[@jacboy](https://github.com/jacboy)** — User-defined account groups with conflict-free iCloud sync & UI (#39, #40), SEPA Instant transfers with explicit fallback & OutcomeUnknown safety (#29), Verification of Payee / VoP (#30), bunq personal API key connector (#12), Consorsbank FinTS preset & product ID masking (#28), inline note editing (#41), the Apple-HIG wizards for the import assistant (#181) and Add bank (#184), comdirect/bunq pending bookings (#186, #187), the project-notice feed (#185), the banking-policy security lock (#127) and release-integrity monitoring (#129).
- **[@nochsoeiner](https://github.com/nochsoeiner)** — FinTS securities holdings via HKWPD / MT535 (#23), interactive balance chart with hover readout & click-to-jump (#20), period-aware KPI tiles (#19), account & category markers in the booking stream (#26), and DKB FinTS URL preset fix (#25).
- **[@sebastianmattar](https://github.com/sebastianmattar)** — Depot balances & securities positions import (#51) Trade Republic 8-digit bank code recognition (#52), category assignment without waiting time (#159), paged booking list (#141), logo loading off the main thread (#151) and Swift tests in CI (#163).
- **[@martinhering](https://github.com/martinhering)** — Scalable Capital connector supporting Baader Bank and Scalable broker GraphQL with 2FA (#54).
- **[@andre68723](https://github.com/andre68723)** — FNZ Bank balances, depot values and PDF inbox (#104), smooth booking scope selection (#63) and a reducible main window height on macOS (#62).
- **[@HansWurst90](https://github.com/HansWurst90)** — Build script cleanup (#14) and testing & feedback on data and depot imports.

## License / Lizenz

GPL-3.0-or-later – see [LICENSE](LICENSE).

---

## ☕ Projekt unterstützen (Freiwillige Zuwendung)

**GiroCrab** ist ein freies Open-Source-Projekt. Wenn dir die App im Alltag hilft und du die Weiterentwicklung mit einer kleinen **freiwilligen Zuwendung** (ohne Gegenleistung) für die Kaffeekasse unterstützen möchtest:

[![PayPal](https://img.shields.io/badge/PayPal-Freiwillige%20Zuwendung-00457C?style=for-the-badge&logo=paypal&logoColor=white)](https://paypal.me/aprinz)

*English:* If you enjoy using GiroCrab and would like to support its development with a **voluntary contribution / tip** (no consideration expected): **[paypal.me/aprinz](https://paypal.me/aprinz)**

---

📄 **Open-Source-Lizenzen / Third-Party Notices:** Eine Übersicht aller im Rust-Kern verwendeten Open-Source-Bibliotheken und ihrer Lizenzen befindet sich in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
