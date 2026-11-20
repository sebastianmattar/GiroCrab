# Android-Parität

Stand der Android-App (`GiroCrabAndroid/`, `de.girocrab.app`) gegenüber dem Rust-Kern
(`girocrab-core/`) — was heute läuft, was fehlt und in welcher Reihenfolge es dazukommt.

Die Seite ist der dauerhafte Ort für diesen Plan; die einzelnen Arbeitsblöcke stehen als
übernehmbare Issues im Tracking-Issue **#1431**. Zahlen und Zitate wurden gegen `01538aa`
erhoben (Android-`versionName` 0.1.0); nach Umbauten im Client bitte nachziehen.

## Kurzfassung

Die App zeigt acht Compose-Oberflächen (sieben Screens plus Buchungsdetail) und ist im Demo-Betrieb
vollständig bedienbar. An den Kern ist sie nicht angebunden: die native Bibliothek fehlt, das
Build-Skript dafür existiert nicht, und die CI baut nur Kotlin.

- Der Kern hat **92 `#[uniffi::export]`-Blöcke** (freie Funktionen und `impl`-Blöcke). Ein Block
  kann mehrere Signaturen umfassen; gezählt sind daraus rund **293 exportierte Funktionen und
  Methoden**. Die Swift-App ruft **154 verschiedene `vault.`-Methoden** auf, Android **zwei** — und
  die erste liefert `null`.
- `GiroCrabAndroid/app/src/main/jniLibs` existiert nicht, ebenso
  `scripts/build-rust-android.sh` (beides in `app/build.gradle.kts:37-39` erwartet).
- `.github/workflows/android.yml` enthält **keinen** Treffer für `cargo`, `ndk` oder `rust`.
- Es gibt weder `AndroidKeyStore` noch `EncryptedSharedPreferences` im Kotlin-Bestand; die App
  legt nur die Tresordatei in `filesDir` ab.

Damit ist der Abstand zum Kern nirgends festgehalten: die Screens lesen ausschließlich den
Demo-Snapshot aus Kotlin.

## Ist-Stand im Einzelnen

**Demo-Betrieb.** `AppViewModel` fragt beim Start `core.openOwnVault()`; bei `null` wird
`core.demoSnapshot()` geladen (`model/AppViewModel.kt:69,86`), der Zustand ist `VaultState.Demo`,
über dem Buchungsstrom steht der Banner „Demo-Daten" (`ui/screens/LockAndOnboarding.kt:196`).
Demodaten kommen aus Kotlin (`model/CoreBridge.kt:92` ff.), sechs Konten über sieben Banken.
Eine Demo-Überweisung ändert nur den Snapshot im Speicher (`model/AppViewModel.kt:139`) und ist
nach einem Neustart weg.

**Native Schicht.** Gradle erwartet `.so`-Dateien unter `app/src/main/jniLibs` und Bindings unter
`app/src/main/generated` (`app/build.gradle.kts:37-39`) für `arm64-v8a`, `armeabi-v7a` und
`x86_64` (`:17`), aufgerufen über JNA 5.15.0 (`:62`). Beides fehlt, deshalb schlägt
`Class.forName("uniffi.girocrab_core.Vault")` fehl (`model/CoreBridge.kt:24`) und
`nativeAvailable` bleibt `false`.

**Tresor.** `DefaultCoreBridge.openOwnVault()` gibt immer `null` zurück, darüber steht
`// TODO(parity)` (`model/CoreBridge.kt:28-29`). Der Sperrbildschirm prüft heute nur die Biometrie
und schaltet keinen Schlüssel frei (`ui/screens/LockAndOnboarding.kt:61-92`).

**Datenabbildung.** `VaultSnapshot` ist das einzige Format der Oberfläche (12 Listen,
`model/Models.kt:209-222`); der Mapper vom Kern dorthin fehlt vollständig. Paging über
`listTransactions` ist nicht angebunden, Analysen werden nicht aus der Saldenhistorie des Kerns
gespeist.

**Schreibpfade.** Keine: es gibt keine Transaktionsgrenzen und keine Fehlerrückmeldung aus dem
Kern.

## Stufenplan

Vier Stufen statt „Android wie macOS" als ein Ziel. Jede Stufe ist für sich benutzbar; Stufe 3 und 4
bekommen eigene Changes, sobald Stufe 2 steht.

**Stufe 1 — native Schicht und Lesepfad.** Build-Skript, NDK/UniFFI in der CI, ein CI-Gate gegen
APKs ohne `.so`, Keystore-umhüllter Tresorschlüssel, `openOwnVault()` über die Bindings, Mapper auf
`VaultSnapshot`, Paging. Ergebnis: die App zeigt den echten Tresor, ausschließlich lesend.

Voraussetzung dafür ist ein Tresor **auf dem Gerät**: Android legt heute keinen an, und das
Android-Backup ist per `allowBackup="false"` aus. Der Import (Tresordatei plus Schlüssel, Quelle:
Desktop-Export) gehört deshalb in Stufe 1 und nicht erst in Stufe 4 — sonst ist der Lesepfad nicht
benutzbar.

**Stufe 2 — Schreibpfade.** Überweisungen über die Kern-Zustandsmaschine statt Snapshot-Mutation,
Kategorien/Regeln/Notizen/Einstellungen über die Kern-API, sichtbare Bank- und TAN-Fehler,
Reload-Disziplin nach jeder Mutation. Ergebnis: die Alpha-Kennzeichnung fällt.

**Stufe 3 — Konten.** Bankzugänge über die bestehenden Konnektoren in `girocrab-core/src/connectors/`
(FinTS, comdirect, Trade Republic, Enable Banking, PayPal, Portal-Importer), alles über denselben
FFI-Rand — kein eigener Android-Code.

**Stufe 4 — Rest.** Import/Export, Dokumente, Daueraufträge, Depot.

Nicht vorgesehen: lokale Schnittstelle (Unix-Socket) und MCP-Adapter. Beides ist heute eine
macOS-Funktion (`docs/local-api.md`: „Nur auf dem Mac") und der MCP-Adapter hängt an genau diesem
Socket; für Android ist beides nicht geplant. Technisch ginge ein `LocalServerSocket`, aber ein
zweiter, abweichender Zugang neben dem Desktop wäre eine eigene Schnittstelle mit eigenem
Berechtigungsmodell — das ist eine Produktentscheidung, keine Grenze der Plattform.

## Entschieden

Die drei offenen Fragen aus dem ersten Entwurf sind beantwortet (Antwort des Maintainers im
geschlossenen PR #1375):

1. **Ablage.** Kein Top-Level-`openspec/`-Baum im Repo. Der Plan steht als diese Seite unter
   `docs/`, die Arbeitsblöcke als Issues (siehe `CONTRIBUTING.md` › „Größere Vorhaben &
   Vorab-Spezifikation").
2. **Release-Zug.** Android bleibt in Stufe 1–2 ein eigenes Alpha-APK-Artefakt; macOS hängt am
   Sparkle-Appcast, iOS an App Store/TestFlight. Das Alpha-Label fällt nach Abschluss von Stufe 2.
3. **Stufe 1 nur lesend.** Ja — erst Build-Pipeline, Keystore und Lesepfad stabil, dann Schreibpfade.

## Design-Entscheidungen

**Bindings: UniFFI + JNA, kein handgeschriebenes JNI.** Ein handgeschriebener JNI-Rand bräuchte
eine Signatur pro exportierter Funktion (heute rund 293), die bei jeder Kern-Änderung mitdriftet.
Gradle zieht JNA bereits (`app/build.gradle.kts:62`), die Klassennamen stehen im Kotlin-Bestand
(`model/CoreBridge.kt:24,53`).
Also `uniffi-bindgen generate --language kotlin` als Build-Schritt. Verworfen: JNI von Hand,
Kotlin-Multiplatform-Ersatz für den Kern (verdoppelt die Geschäftslogik).

**Ein Build-Skript, ein Name.** `app/build.gradle.kts:37` nennt bereits
`scripts/build-rust-android.sh`; das Skript wird angelegt, statt einen zweiten Namen zu erfinden.
Die CI ruft dasselbe Skript auf, damit lokaler und CI-Build nicht auseinanderlaufen.

**Schlüssel: Keystore-umhüllt, Biometrie über `CryptoObject`.** Der 32-Byte-Tresorschlüssel wird mit
einem AES-GCM-Schlüssel aus dem `AndroidKeyStore` umhüllt (`setUserAuthenticationRequired(true)`,
`setInvalidatedByBiometricEnrollment(true)`), der umhüllte Wert liegt in `filesDir`. Entsperrt wird
über `BiometricPrompt` mit `CryptoObject` — ohne Krypto-Freigabe gibt es keinen lesbaren Schlüssel.
Verworfen: `EncryptedSharedPreferences` (abgekündigt, kein Auth-Binding), Klartext in `filesDir`,
Schlüsselableitung aus einem Nutzerpasswort (zweites Geheimnis neben der Gerätesperre).

**Snapshot für die Ansicht, Kern für die Logik.** `VaultSnapshot` bleibt das einzige Format der
Oberfläche; Kategorisierung, Regelanwendung, Saldenhistorie und Suche bleiben im Kern. Sonst
entsteht eine zweite Wahrheit, die zwischen Desktop und Android auseinanderläuft.

## Risiken

- **OpenSSL für Android.** `rusqlite` ist mit `bundled-sqlcipher-vendored-openssl` gebaut
  (`girocrab-core/Cargo.toml:21`). Für Android muss `openssl-src` gegen den NDK übersetzt werden
  (`perl`, NDK-Clang, `CC_<target>`/`AR_<target>`) — der wahrscheinlichste Punkt, an dem der erste
  Build scheitert. Fallback: `bundled-sqlcipher` ohne vendortes OpenSSL hinter einem Feature-Flag,
  Desktop-Build unverändert.
- **R8 und JNA.** `buildTypes.release` hat `isMinifyEnabled = true`. UniFFI-Bindings und
  `com.sun.jna` müssen in `proguard-rules.pro` gehalten werden, sonst bricht nur der Release-Build —
  und erst zur Laufzeit beim ersten `Vault`-Aufruf.
- **APK-Größe.** Drei ABIs plus OpenSSL plus JNA: ohne `abiFilters`-Split wächst die APK deutlich.
  Für die Alpha akzeptabel, für eine Play-Auslieferung ein App Bundle mit Splits.
- **Threading.** Öffnen und Abfragen laufen im Kern über eine tokio-Runtime; der Client muss sie
  hinter `Dispatchers.IO`/`Default` halten und den blockierenden Erstaufruf vom Main-Thread fernhalten.
- **Invalidierung des Keystore-Schlüssels.** Zwei getrennte Auslöser:
  `setInvalidatedByBiometricEnrollment(true)` verwirft den Schlüssel bei **neuer Biometrie**
  (weiterer Finger, neues Gesicht). `setUserAuthenticationRequired(true)` bindet ihn an die
  Gerätesperre: ein **normaler PIN-/Muster-Wechsel mit der alten PIN** überträgt die User-SID und
  lässt den Schlüssel nutzbar; unbrauchbar wird er, wenn die Sperre **ohne die alte PIN** gesetzt
  oder entfernt wird — Werksreset, Entfernen der Bildschirmsperre, administratives *untrusted
  enroll* (AOSP, *Authentication* › Enrollment). Beides ist gewollt, kostet den Nutzer aber den
  Schlüssel. **Offen und vor Stufe 1b zu entscheiden:** Der Desktop-Import deckt
  nur Tresore, deren Schlüssel schon auf dem Desktop liegt — für einen rein auf dem Gerät
  angelegten Tresor braucht es entweder eine beim Einrichten erzeugte Schlüsselkopie, die der
  Nutzer außerhalb der App verwahrt, oder die ausdrückliche Ansage „keine Wiederherstellung". Bis
  dahin darf keine Fehlermeldung einen Import anbieten, den es nicht gibt.

## Aufgaben

Die 23 Aufgaben stehen mit Belegkriterium in vier übernehmbaren Blöcken, dazu drei Abschluss-Punkte:

- **Stufe 1a — native Schicht** (5 Aufgaben): Build-Skript, OpenSSL/SQLCipher, CI-Aufruf, CI-Gate
  gegen APKs ohne `.so`, ProGuard für Release. → Issue #1427
- **Stufe 1b — Tresor und Schlüssel** (6 Aufgaben): `VaultKeyStore`, `BiometricPrompt` mit
  `CryptoObject`, `openOwnVault()`, Import von Tresordatei und Schlüssel, sichtbare Fehlerpfade,
  Sperren im Hintergrund. → Issue #1428
- **Stufe 1c — Datenabbildung** (5 Aufgaben): Mapper für alle 12 Listen, Cursor-Paging, Analyse-Summen
  aus dem Kern, Demo-Pfad nur ohne Tresor. → Issue #1429
- **Stufe 2 — Schreibpfade** (4 Aufgaben): Überweisung über die Kern-Zustandsmaschine, Kategorien/
  Regeln/Notizen/Einstellungen, Bank-/TAN-Fehler, Reload-Disziplin. → Issue #1430
- **Abschluss Stufe 1/2** (3 Punkte, im Tracking-Issue): `SECURITY.md` um Android/Windows ergänzen,
  Alpha-Hinweis im README erst nach Stufe 2 entfernen, diese Seite bei Abschluss von Stufe 2 auf den
  dann aktuellen Stand ziehen (Zahlen, Stufen, Grenzen).

## Belege nachprüfen

```sh
grep -rho '#\[uniffi::export\]' girocrab-core/src | wc -l                                   # 92 Blöcke
grep -rhoE 'vault\.[a-zA-Z]+\(' --include=*.swift GiroCrabApp/Sources | sort -u | wc -l     # 154
grep -rhoE '\bcore\.[a-zA-Z]+\(' --include=*.kt GiroCrabAndroid/app/src/main/java | sort -u
ls GiroCrabAndroid/app/src/main/jniLibs GiroCrabAndroid/scripts/build-rust-android.sh                       # jniLibs fehlt, Skript vorhanden
grep -cE 'cargo|ndk|rust' .github/workflows/android.yml                                     # 0
```

Die 92 sind `#[uniffi::export]`-Blöcke, nicht Funktionen: ein Block auf einem `impl` exportiert
alle seine `pub fn`. Die 293 sind entsprechend `pub fn` innerhalb dieser Blöcke plus exportierte
freie Funktionen — beide Zahlen ändern sich unabhängig.

## Herkunft

Der Plan entstand als OpenSpec-Baum in PR #1375 (geschlossen, weil reine Planungsbäume nicht ins
Repo gehören) und liegt hier in der von den Maintainern gewünschten Form: Prosa unter `docs/`,
Arbeitsblöcke als Issues. Erstellt mit Unterstützung eines KI-Agenten (Hermes, Nous Research) auf
Basis der Antwort des Maintainers; der Ist-Stand wurde aus dem Code gelesen, nicht aus dem Changelog.
