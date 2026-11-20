# Sicherheitssperre für Bankverbindungen (banking-policy.json)

Ein kritisches Update wird nie erzwungen – weder über Sparkle (`--critical`)
noch im App Store. Für den Ausnahmefall, dass eine **alte Version beim Kontakt
mit der Bank selbst Schaden anrichtet** (Sicherheitslücke in einem Connector,
umgezogener Bankserver, fehlerhafte Anmeldungen, die den Zugang sperren),
kann der Herausgeber eine **signierte Richtliniendatei** veröffentlichen. Ältere
Versionen dürfen dann keine Bank mehr kontaktieren, bis sie aktualisiert sind.

* **Gesperrt:** Kontoabrufe (FinTS, PSD2/Enable Banking, APIs, Portal-Logins),
  Überweisungen und das Anlegen von Bankzugängen – für alle Connectors oder nur
  für einzelne.
* **Weiter möglich:** Tresor öffnen, Buchungen ansehen, suchen, kategorisieren,
  exportieren, drucken; Überweisungen per GiroCode (kein Bankkontakt).
* **Hinweis:** nicht wegklickbares Banner über der Buchungsliste mit
  „Jetzt aktualisieren“ (Sparkle, App Store oder Releases-Seite) und „Details“.
* Die Datei kann vorhandene Funktionen nur **einschränken**. Sie enthält keinen
  Code, schaltet nichts frei, installiert nichts und löscht nichts.

Nur für Ausnahmefälle, nicht für normale Releases.

## Aktivierung

Ausgeliefert wird ein **Platzhalter** in `BankingPolicy.publicKeys`
(`GiroCrabApp/Sources/GiroCrabUI/BankingPolicy.swift`). Solange dort kein
gültiger Schlüssel steht, ist die Funktion **vollständig aus**: keine Anfrage an
GitHub, keine Sperre, in Einstellungen › Sicherheit steht „Nicht aktiv“.

Aktivieren (einmalig, Maintainer, auf dem Release-Mac):

```sh
scripts/banking-policy.sh keygen          # Schlüssel in den Schlüsselbund, öffentlichen Schlüssel eintragen,
                                          # erste leere Policy (sequence 1) nach policy/
```

Danach `BankingPolicy.swift`, `scripts/banking-policy-signing-account.txt` und
`policy/` nach `main` committen, **dann** ein Release bauen. `notarize.sh`
bricht ab, solange unter der festen Adresse noch keine Policy liegt.

Der private Schlüssel liegt nur im Schlüsselbund des Release-Macs (Dienst
„GiroCrab Banking Policy“), nie im Repo, in der CI oder in GitHub Secrets. Eine
verschlüsselte Sicherung gehört offline in den Passwortmanager – getrennt von
Sparkle-Schlüssel und Developer-ID-Zertifikat.

## Dateiformat (Schema 1)

```json
{
  "schema": 1,
  "issued": "2026-11-20T10:00:00Z",
  "sequence": 7,
  "rules": [
    {
      "distribution": ["developer-id", "mac-app-store"],
      "channels": ["stable", "beta", "nightly"],
      "minimumVersion": "0.7.2",
      "connectors": ["fints"],
      "advisory": "https://github.com/GiroCrab/GiroCrab/security/advisories/GHSA-…",
      "updateAvailable": true
    }
  ]
}
```

Daneben `banking-policy.json.sig`: Ed25519-Signatur über die **exakten Bytes**.

| Feld | Bedeutung |
|---|---|
| `distribution` | `developer-id`, `mac-app-store`, `ios`, `linux`, `windows`. Bei den Apple-Builds der Info.plist-Schlüssel `GiroCrabDistribution`; Linux (`banking_policy.rs`) und Windows (`GiroCrabWindows/src/policy.rs`) tragen ihren Wert fest im Build. Unbekannte Werte werden ignoriert. |
| `channels` | optional; `stable`, `beta`, `nightly`. Maßgeblich ist der Kanal des **laufenden Builds** (aus `CFBundleVersion`), nicht der gewählte Update-Kanal. Fehlt das Feld: alle Kanäle. |
| `minimumVersion` | Ältere Builds (Vergleich wie Sparkle) sind gesperrt. Achtung: `0.7.2b1` < `0.7.2` – eine Mindestversion `0.7.2` sperrt auch die Betas von 0.7.2. |
| `connectors` | optional; Connector-Schlüssel aus `ConnectorKind::as_str`. Eine ID trifft, wenn sie gleich ist oder der Schlüssel mit `<ID>:` beginnt: `fints` = alle FinTS-Banken, `fints:50050201` = nur 1822direkt, `enablebanking` = alle PSD2-Banken; weitere: `comdirect`, `paypal`, `traderepublic`, `bunq`, `scalable`, `amazon-orders`, `fnz`, `easybank`, `fin4u` (Alte Leipziger), `volkswagenbank`, `uberspace`, `kontist`, `schwaebischhall` (Schwäbisch Hall). Fehlt das Feld: alle Connectors. |
| `advisory` | optional; nur `https`. Ziel von „Details“. |
| `updateAvailable` | optional, Standard `true`. `false`: „Ein Update ist in Vorbereitung.“, ohne „Jetzt aktualisieren“ (z. B. solange ein App-Store-Fix im Review ist). |
| `sequence` | steigt bei jeder Veröffentlichung. Eine Datei mit kleinerer `sequence` als der zuletzt angenommenen wird ignoriert (Schutz vor dem Zurückspielen alter Dateien). |

Greifen mehrere Regeln, gilt die strengste. Leere `rules` = keine Sperre.

## Verhalten der App

* **Abruf** von `https://raw.githubusercontent.com/GiroCrab/GiroCrab/main/policy/banking-policy.json`
  (+ `.sig`), also einer festen Adresse unabhängig von Releases, beim Start, vor jedem Bankkontakt (höchstens alle 15 Minuten).
  Ohne Cookies, ohne Cache, ohne Nutzerdaten; max. 64 KB, 10 s Zeitlimit.
  GitHub sieht dabei IP-Adresse und Zeitpunkt.
* **Prüfung:** Signatur (einer der einkompilierten Schlüssel) → `schema` →
  `sequence`. Alles andere (Netzwerkfehler, 404, falsche Signatur, ältere
  `sequence`) ändert **nichts** am gespeicherten Stand. Keine Toleranzfrist.
* **Speicherung:** im Schlüsselbund (`de.girocrab.banking-policy`, nicht
  synchronisiert, nur dieses Gerät). Übersteht `brew uninstall --zap`, das
  Löschen von Application Support und eine Neuinstallation.
* **Untergrenze:** Die beim Build aktuelle Policy liegt im Bundle
  (`Contents/Resources/banking-policy.json`); eine höhere `sequence` ersetzt
  den gespeicherten Stand.
* **Ohne Online-Prüfung: 90 Tage ab Build.** Wurde auf diesem Gerät noch nie
  eine Policy online geprüft (frische Installation, GitHub per Firewall oder
  Little Snitch blockiert), gilt die im Bundle mitgelieferte Policy – sie war
  beim Release aktuell – für **90 Tage ab Build-Datum**
  (`GiroCrabBuildDate` in der Info.plist, gesetzt von `package-macos-app.sh`).
  Sperrt sie diesen Build selbst, gilt die Sperre sofort. Danach hält die App
  den Bankkontakt an, bis eine Online-Prüfung gelingt („Sicherheitsprüfung
  nicht möglich“, mit Hinweis auf `raw.githubusercontent.com`). Builds ohne
  Build-Datum oder ohne gebündelte Policy (Xcode, `swift run`) bekommen diese
  Frist nicht. Bestehende Installationen arbeiten bei einer GitHub-Störung mit
  dem gespeicherten Stand weiter.
  Folge: Ein Gerät, das GitHub nie erreicht, bemerkt eine *neue* Sperre
  nicht; ab Tag 91 hält es den Bankkontakt aber ganz an, bis es aktualisiert
  wird oder GitHub erreicht.
* **Zweite Verteidigungslinie:** Die App gibt die Sperre an den Rust-Core
  weiter (`set_banking_block`). `begin_sync`, `begin_transfer` und das Anlegen
  von Verbindungen brechen dort mit `CoreError::BankingBlocked` ab – auch auf
  einem UI-Pfad, der übersehen wurde. Portal-Logins (Amex, Amazon, FNZ) laufen
  im eingebetteten Browser der App, nicht im Core; dort sperrt die App das
  Öffnen des Login-Fensters.
* Die Sperre gilt ohne zeitliche Begrenzung, bis ein Update sie aufhebt: eine
  Version, die die Mindestversion erfüllt, oder eine neuere Datei ohne Sperre.

## Im Ernstfall

```sh
# alle FinTS-Verbindungen in Developer-ID-Builds älter als 0.7.2 sperren
scripts/banking-policy.sh set --distribution developer-id --minimum 0.7.2 \
  --connectors fints --advisory https://github.com/GiroCrab/GiroCrab/security/advisories/GHSA-…

# Sperre aufheben
scripts/banking-policy.sh clear
```

`set` erhöht `sequence`, signiert, prüft die Signatur, committet beide Dateien
in `policy/` und pusht nach `main` (ein Release ist nicht nötig). Voraussetzung:
ausgecheckt ist `main`, gleich `origin/main`. Ist `main` geschützt, bleibt der
Commit lokal; dann als Pull Request einreichen. GitHub liefert die neue Datei
nach spätestens etwa 5 Minuten aus. Jede Sperre ist so ein nachvollziehbarer
Commit. Es verweigert eine
`minimumVersion` über der neuesten veröffentlichten Version des Kanals (Schutz
vor Selbstaussperrung), außer mit `--update-not-available`.

**App Store:** Sperre sofort mit `--update-not-available` setzen → Fix mit
Expedited Review einreichen → nach Freigabe `set` ohne die Option, Mindestversion
auf die neue Version.

## Releases

Releases veröffentlichen die Policy nicht; sie liegt allein in `policy/` auf
`main`. `notarize.sh` nimmt nur den aktuellen Stand mit:

* vor dem Bauen: lädt `banking-policy.json` + `.sig` von der festen Adresse,
  prüft die Signatur und legt sie als Untergrenze ins Bundle;
* nach dem Bauen: prüft, dass Schlüssel und Datei wirklich im Build stecken.
* Ist die Signatur ungültig oder noch nichts veröffentlicht, **bricht das
  Release ab**. Ohne aktiven Schlüssel (Platzhalter) wird der Schritt
  übersprungen.

## Schlüsselwechsel

```sh
scripts/banking-policy.sh rotate-key            # neues Paar, öffentlicher Schlüssel zusätzlich eintragen
# … Release mit beiden Schlüsseln über alle Vertriebswege ausliefern …
scripts/banking-policy.sh clear                 # bzw. set …: signiert jetzt mit dem neuen Schlüssel, sequence + 1000
scripts/banking-policy.sh rotate-key --retire-old   # Folge-Release: alten Schlüssel austragen
```

Bei **gestohlenem** Schlüssel zusätzlich eine Sicherheitsmeldung (GitHub
Advisory) veröffentlichen. Bekannte Lücke: Ältere Apps kennen den neuen
Schlüssel noch nicht; wer zusätzlich Schreibzugriff auf `main` hat, könnte bei
ihnen bis zum Update Sperren setzen oder aufheben.

## Testen

Debug-Builds (`swift run`, nicht Release) lassen sich mit Wegwerf-Schlüsseln und
einem lokalen Server testen. Ein Test-Schlüssel schreibt **nie** in den echten
Schlüsselbund-Eintrag:

| Variable | Wirkung |
|---|---|
| `GIROCRAB_BANKING_POLICY_KEY` | öffentlicher Test-Schlüssel (Base64) statt der einkompilierten |
| `GIROCRAB_BANKING_POLICY_URL` | Policy-Adresse, z. B. `http://127.0.0.1:8765/banking-policy.json` |
| `GIROCRAB_BANKING_POLICY_VERSION` | vorgetäuschte `CFBundleVersion`, z. B. `0.7.1` |
| `GIROCRAB_BANKING_POLICY_STORE` | Datei statt Speicher für den gespeicherten Stand (Neustart testen) |
| `GIROCRAB_BANKING_POLICY_BUILD_DATE` | vorgetäuschtes `GiroCrabBuildDate` (ISO 8601, z. B. `2026-08-01T00:00:00Z`), um die 90-Tage-Frist zu testen; eine gebündelte Policy legt man dafür neben die Debug-Binärdatei (`banking-policy.json` + `.sig`) |

```sh
swiftc -O -o /tmp/bpt scripts/banking-policy-tool/main.swift GiroCrabApp/Sources/GiroCrabUI/BankingVersion.swift
export BANKING_POLICY_KEY_FILE=/tmp/policy-test.key   # Test-Schlüssel in einer Datei statt im Schlüsselbund
/tmp/bpt keygen test                                  # gibt den öffentlichen Schlüssel aus
/tmp/bpt sign test srv/banking-policy.json srv/banking-policy.json.sig
python3 -m http.server 8765 --bind 127.0.0.1 --directory srv
GIROCRAB_BANKING_POLICY_KEY=<öffentlicher Schlüssel> \
GIROCRAB_BANKING_POLICY_URL=http://127.0.0.1:8765/banking-policy.json \
GIROCRAB_BANKING_POLICY_VERSION=0.7.1 swift run GiroCrabMac --demo-seed
```

`scripts/banking-policy.sh` lässt sich ohne GitHub und Schlüsselbund trocken
ausprobieren: `BANKING_POLICY_KEY_DIR`, `BANKING_POLICY_ACCOUNT_FILE`,
`BANKING_POLICY_SWIFT_FILE` (Kopie der Swift-Datei), `BANKING_POLICY_SOURCE_DIR`
(lokaler Ordner statt `policy/` und GitHub; dann wird nie committet),
`BANKING_POLICY_APPCAST` und `BANKING_POLICY_NO_PUSH=1` (siehe Kopf des Skripts).

Nur in einem separaten macOS-Benutzer testen, wenn ein echter Tresor im Spiel
sein könnte.
