# Contributing to GiroCrab

Thanks for helping! / Danke für die Mitarbeit! Issues and PRs may be in English or German.

**Privacy first:** never commit or attach real IBANs, account numbers, phone numbers, PINs, API keys,
bank exports (MT940/CAMT/CSV) or real people's names. Use invented data (e.g. the public example IBAN
`DE89 3704 0044 0532 0130 00`, "Max Mustermann").

## Setup

```bash
./build-xcframework.sh                 # only needed when the Rust API changes
cargo test -p girocrab-core -q
cargo run -- --demo-seed               # or GIROCRAB_DEMO_SEED=1
```

`cargo run` builds the app in debug configuration with `-O`: incremental, so a one-file change
rebuilds in seconds. `#if DEBUG` code is active, e.g. the banking policy lives in
`banking-policy-debug.json` instead of the keychain. `cargo run --release` builds it as shipped
(whole-module optimization), which is much slower and recompiles all of the UI on every edit.

Swift-only changes: `cd GiroCrabApp && swift build -q` (no core rebuild needed).

Optional: `nix develop` (flake.nix) sets up the pinned Rust toolchain plus Apple targets and the
native build tools the dependency tree needs (SQLCipher/OpenSSL, AWS-LC). Xcode itself still has to
be installed separately – see [docs/nix.md](docs/nix.md).

Free disk space with `scripts/clean.sh [--deep]`: the default run removes `dist/`, `build/`, stray
temp files, GiroCrab DerivedData, incremental Rust caches and merged local branches while keeping
built dependencies and the xcframework; `--deep` also runs `cargo clean` and removes `GiroCrabApp/.build`.

Database schema changes are a new migration step plus a fixture vault, see
[Schema & migrations](docs/ARCHITECTURE.md#schema--migrations).

## Adding a bank in 3 steps

1. **Copy the template.** `cp girocrab-core/src/connectors/template.rs girocrab-core/src/connectors/<bank>.rs`
   and implement `begin` (login / challenge), `respond` (TAN/poll) and `fetch` (accounts + bookings).
   Amounts are `i64` cents, debits negative; `external_id` must be stable for de-duplication.
2. **Register it.** Add a variant to `ConnectorKind` (`models.rs`), a `BankDescriptor` in
   `descriptor()` and a line in `make_connector()` (both in `connectors/catalog.rs`), plus
   `pub mod <bank>;` in `connectors/mod.rs`. The `BankConnector` trait and its DTOs (`Phase`,
   `Challenge`, `FetchResult`, …) live in `connectors/types.rs`; the sync/transfer state machines are
   in `sync_session.rs` / `transfer_session.rs`, and storing fetched data is `persist.rs`.
   File importers (database, CSV/CAMT/MT940, portal imports) are not connectors: see
   `girocrab-core/src/importers/`.
   For curated PSD2 presets, add the bank to `psd2_catalog()` in `connectors/bank_directory.rs`.
   Add the Swift display name/colour in `GiroCrabUI/Formatting.swift` and run `bash scripts/lint-connectors.sh`.
3. **Test it with fake data.** Unit-test your parser with invented fixtures, then check the UI via
   `GIROCRAB_DEMO_SEED=1`. Run `cargo fmt`, `cargo clippy` and `cargo test -p girocrab-core`.
   Optional: `git config core.hooksPath .githooks` runs `cargo fmt --check` before every push, like CI does,
   and warns after a checkout, pull or rebase when the xcframework in `generated/` no longer matches
   the Rust core (`./build-xcframework.sh --check`).
   Mind the trust model: the hooks live in the working tree, so after a checkout or merge git runs the
   hook scripts (and `build-xcframework.sh`) of the revision you just checked out. Only enable the
   hooks if you would also build that code (a build runs its `build.rs` anyway), and don't check out
   branches you don't trust with hooks enabled; `git -c core.hooksPath=/dev/null checkout <branch>`
   skips them once.

Unsure where to start? Pick one of the open `[Bank]` issues labelled
[`help wanted`](https://github.com/GiroCrab/GiroCrab/issues?q=is%3Aopen+label%3A%22help+wanted%22+%22%5BBank%5D%22),
or open a new `[Bank]` issue first.

## UI strings

The String Catalogs (`GiroCrabApp/Xcode/App/*.xcstrings`) are sorted by key so that parallel PRs
don't collide at the end of the file. Add keys with `GiroCrabApp/Tools/update-localizations.sh` or
by hand, then run `scripts/xcstrings.py format` (CI checks it).
`git config merge.xcstrings.driver 'scripts/xcstrings.py merge %O %A %B'` makes git merge the
catalogs per key instead of per line; the hooks in `.githooks` register it automatically.

## Texts, documentation and trademarks / Texte, Doku und Markennamen

**DE**
- Nutzersichtbare Texte (README, Doku, Website, App-Strings, Release-Notes) nennen keine Marken anderer Banking- oder Finanzsoftware und enthalten keine Vergleiche mit Fremdprodukten. Interne Code-Bezeichner (Modul-, Funktions-, Test- und Dateinamen) sind ausgenommen. `scripts/lint-brand-names.sh` (CI) prüft Doku und String-Kataloge.
- Banken, Broker und Zahlungsdienste, die GiroCrab anbindet, dürfen sachlich genannt werden („Connector für Konten bei der X-Bank“). Nicht erlaubt: Formulierungen, die eine Partnerschaft oder offizielle Unterstützung nahelegen, sowie Logos von Banken in Doku und Website.
- Inoffizielle Schnittstellen werden in Doku und Bank-Tabelle als „inoffiziell“ gekennzeichnet, mit Hinweis auf mögliche Verstöße gegen Nutzungsbedingungen.
- Kein Kopieren aus proprietärer Dokumentation oder fremdem Code. Fremdcode nur mit GPL-kompatibler Lizenz und Quellenangabe (Projekt, Lizenz, Link). Technische Erkenntnisse in eigenen Worten beschreiben.
- Funktionen von GiroCrab sachlich und belegbar beschreiben: keine absoluten Versprechen („verlustfrei“, „alle Banken“), keine Zeitangaben ohne Messgrundlage.

**EN**
- User-visible texts (README, docs, website, app strings, release notes) do not name brands of other banking or finance software and contain no comparisons with other products. Internal code identifiers (module, function, test and file names) are exempt. `scripts/lint-brand-names.sh` (CI) checks the docs and string catalogs.
- Banks, brokers and payment services that GiroCrab connects to may be named factually ("connector for accounts at X Bank"). Do not imply a partnership or official support, and do not use bank logos in docs or on the website.
- Mark unofficial interfaces as "unofficial" in docs and the bank table, noting possible terms-of-service issues.
- Do not copy from proprietary documentation or third-party code. Third-party code only under a GPL-compatible licence with attribution (project, licence, link). Describe technical findings in your own words.
- Describe GiroCrab's features factually and verifiably: no absolute claims ("lossless", "all banks"), no time claims without measurement.

## Adding merchants

Edit `girocrab-core/data/merchants.tsv` (parsed by `src/data_tables.rs`, matching logic in
`src/merchants.rs`). One merchant per line, tab-separated: `key`, `name`, `needles` (`|`-separated),
`kind` (a `MerchantKind` variant), `symbol` (SF Symbol), `color` (`#RRGGBB`), `domain`, and optionally
`isin`, `wkn` for funds. Rows are matched top to bottom, the first hit wins. Needles are matched
against the normalised, lower-case counterparty; short brand names carry spaces (`" dm "`) to avoid
false hits. Validate with `cargo test -q -p girocrab-core --lib data_tables` and `--lib merchants`
(unique keys, lower-case needles, valid colours). Private persons (first name + surname)
automatically get a monogram, never a logo.

## Testing with demo data

`GIROCRAB_DEMO_SEED=1 cargo run` (or `cargo run -- --demo-seed`) opens a throw-away vault with
fictional accounts, invented bookings and demo depot positions (`girocrab-core/src/demo.rs`). Change the data
there only with invented names and amounts.

## Mit KI-Unterstützung beitragen (In 4 Schritten zum PR) / Contributing with AI Agents

Du kannst ein Issue auch mit Claude Code, Cursor oder Codex umsetzen; `CLAUDE.md` erklärt der KI die Architektur-, Privacy- und Performance-Regeln.
You can also tackle an issue with an AI coding tool; `CLAUDE.md` tells it our architecture, privacy and performance rules.

* 👉 [Interaktive Mitbau-Anleitung (In 4 Schritten mit KI zum eigenen PR)](https://girocrab.de/girocrab_mitbau_anleitung.html)
* 🎬 [10s Promo-Animation mit Sound](https://girocrab.de/girocrab_10s_promo_video.html)

Zuerst im Issue Bescheid geben (siehe unten), dann Fork klonen und diesen Prompt in die KI kopieren (`#123` durch deine Issue-Nummer ersetzen):

```text
Lies CLAUDE.md und CONTRIBUTING.md und setze Issue #123 um: Prüfe zuerst die Architektur-, Privacy- (.sensitive()) und Performance-Regeln, schlage einen sauberen Plan vor und implementiere ihn danach inkl. Tests. Führe anschließend cargo test / swift test sowie alle Linter (scripts/lint-*.sh, scripts/xcstrings.py check) aus und starte die App zur Kontrolle im Demo-Modus mit GIROCRAB_DEMO_SEED=1.
```

Sieht es in der Demo-Vorschau gut aus:

```text
Erstelle einen neuen Branch, committe die Änderungen mit den AI-Attribution-Trailern und öffne einen Pull Request für #123 inklusive des "### 🤖 AI Process Reflection"-Blocks (mit Security-Check und Perf-Check).
```

New issue authors get this hint once as a comment (`.github/workflows/issue-ai-invite.yml`).

### 🌟 Community Success Story: Vom anonymisierten Browser-Mitschnitt (HAR) zum eigenen Bank-Connector (#42)

`@zcarsten` hat ohne Rust-Vorkenntnisse einen Connector für seine Bank gebaut – mit einem Browser-Mitschnitt und einer KI:

1. **Mitschnitt:** Bank-Portal im Browser (Chrome/Edge) öffnen, DevTools › *Network* starten, einmal einloggen und Umsätze aufrufen, dann als `.har` exportieren.
2. **Anonymisieren:** Namen, IBANs, Kontostände, Zugangsdaten, Tokens und Cookies in der `.har` durch Platzhalter ersetzen, **bevor** die Datei irgendwo geteilt oder in eine KI geladen wird.
3. **KI bauen lassen:** Claude Code oder Codex bekommt die anonymisierte `.har`, `girocrab-core/src/connectors/template.rs` und `CLAUDE.md` und implementiert den Rust-Connector samt Tests.
4. **Teilen:** Noch nicht bereit für einen Pull Request? Kein Problem – hänge den erzeugten `.patch` (`git diff > bank.patch`) einfach direkt im GitHub-Issue an, wie in #42 / PR #642. Wir helfen beim Rest.

**From an anonymized browser capture (HAR) to your own bank connector (#42).** `@zcarsten` built a connector without prior Rust experience: record the bank portal in the browser DevTools (Network tab, export `.har`), anonymize names, IBANs, balances and credentials in the file *before* sharing it, then let Claude Code / Codex build the connector against `girocrab-core/src/connectors/template.rs` and `CLAUDE.md`. If you don't feel ready for a pull request yet, attach the generated `.patch` / `git diff` to the GitHub issue (as in #42 / PR #642) and we'll take it from there.

## Größere Vorhaben & Vorab-Spezifikation / Larger features & architecture alignment

**DE**
- **Erst abstimmen, dann Code & Tokens einsetzen:** Bei größeren Themen (Plattform-Parität wie Android/Windows/Linux, übergreifende Architektur-Änderungen oder mehrstufige Features) bitte **vor** dem ersten Code-PR ein Architektur-/Tracking-Issue eröffnen: Ist-Stand, vorgeschlagener Stufen-Schnitt, offene Designfragen und geplante Teil-PRs kurz skizzieren. Das spart allen Beteiligten unnötige Review-Schleifen und Agenten-Tokens.
- **Ablageort für Spezifikationen:** Architektur-Vorschläge und Meilenstein-Pläne werden im **GitHub-Issue** abgestimmt; dauerhafte Architektur-Dokumentation gehört unter `docs/`. Bitte keine externen Planungs- oder Agenten-Verzeichnisse (wie `.cursor/`, `.claude/` oder Top-Level-Spec-Frameworks ohne Code) als PR einreichen – unsere Triage-Pipeline ist auf lauffähige, testbare Teil-PRs ausgelegt und hält oder schließt reine Planungs-Bäume automatisch.

**EN**
- **Align before writing code or spending agent tokens:** For larger efforts (platform parity across Android/Windows/Linux, cross-cutting refactors, or multi-stage features), open an architecture/tracking issue **before** sending code PRs: outline the current gap, proposed stages, open design questions, and small PR slices.
- **Where specs live:** Discuss proposals and task breakdowns in **GitHub Issues**; lasting architecture docs belong in `docs/`. Do not open PRs that add standalone planning or agent framework trees without runnable changes – our automated triage pipeline expects small, testable slices and closes tool-specific planning trees automatically.

## Picking up an issue

To avoid two people building the same thing (it has happened, e.g. account groups in #31):

1. **Ask first.** Comment `/assign` (also `/assign-me`, `/take` or "Ich übernehme das") on the issue:
   a workflow assigns it to you if nobody else has it. Please don't start before that, except for tiny
   fixes (typos, one-line bugs).
2. **One issue, one assignee.** If an issue is already assigned, talk to the assignee or a maintainer
   before starting a competing implementation.
3. **One feature per issue.** Split larger requests into separate issues, so each can be assigned
   and merged on its own.
4. **Link your PR** with `Closes #123` (or `Refs #123` if it is only part of it).
5. **Let go if plans change.** If you can't continue, comment `/unassign` (or tell us on the issue) so it can be reassigned.

## Pull requests

* **Keep PRs small and focused (one concern per PR):** Oversized PRs touching dozens of files or bundling multiple features and schema versions are automatically held for splitting. For larger features, split into (1) schema + core logic and (2) UI + tests.
* CI runs `cargo fmt --check`, `cargo clippy`, `cargo test -p girocrab-core` and a Swift build.
* Work through the checklist below. Tick what applies, write "n/a: <why>" for the rest.
* By contributing you agree to license your work under GPL-3.0-or-later.

### Pull-Request-Checkliste: Security, Performance & Qualität / PR Checklist: Security, Performance & Quality

Security and performance are **mandatory** for every PR, with or without AI help. The checklist is
self-reported; the AI Provenance & Transparency Card shows it (see
[AI transparency](docs/ai-transparency.md)) and never blocks a merge, but a missing answer is
marked with ⚠️. / Security und Performance sind **Pflicht** für jeden PR, mit oder ohne KI-Hilfe.
Die Checkliste ist eine Selbstauskunft; die Card zeigt sie an, blockiert nichts und markiert eine
fehlende Angabe mit ⚠️.

#### 🔐 Security & Privacy (Pflicht / mandatory)

- [ ] **Redaction:** every new or changed SwiftUI view in the main window marks personal or financial data with `.sensitive()` (amounts, IBANs, names, purposes, notes, documents) or `.sensitive(.labels)` (category, group, budget, security names). Verify with `swift run GiroCrabMac --demo-seed --redact --screenshot /tmp/r.png` (`--redact=labels` for labels). / Neue oder geänderte Ansichten im Hauptfenster markieren personenbezogene und finanzielle Daten mit `.sensitive()`.
- [ ] **Logs and errors:** text that reaches a log, diagnostics or a public issue goes through `sanitize_report_text` (`report_sanitizer.rs`); new fields get a test. Never paste raw log or bank text.
- [ ] **File imports:** paths from users or archives are checked against traversal and symlinks (`std::fs::symlink_metadata`, see `importers/moneymoney_db.rs`).
- [ ] **Exports:** CSV cells that are not numeric are escaped against formula injection (a leading `=`, `+`, `-` or `@` is neutralised, see `csv_text` in `export/csv.rs`).
- [ ] **Dependencies and licences:** `scripts/check-third-party-notices.sh` passes, and `THIRD_PARTY_NOTICES.md` is updated in the same PR when `Cargo.toml`, `Cargo.lock` or `Package.swift` changed.
- [ ] **Brand neutrality:** `scripts/lint-brand-names.sh` passes.
- [ ] **New access:** new network, process or file access is named in the PR description.
- [ ] **No hidden code:** `unsafe` only in `girocrab-core/src/sys.rs`; `scripts/lint-no-obfuscation.sh` passes (rules in `docs/ARCHITECTURE.md`). An exception is a new entry in that script.

#### ⚡ Performance & Responsiveness (Pflicht / mandatory)

- [ ] **16 ms rule:** every core call from Swift is synchronous and holds the vault mutex. Views never touch `model.vault` (`scripts/lint-vault-access.sh`); use `model.vaultOffMain { … }` or `requireVaultOffMain` / `pinVault()` for anything over ~16 ms and check `listGeneration` before publishing.
- [ ] **Aggregates:** use SQL aggregates, not `list_transactions`, for counts and sums (it enriches every row in the filter). Avoid O(n × m) nested loops over bookings.
- [ ] **Benchmark:** when core queries, filtering, rules or categories are touched, run `COPIES=600 cargo run --release -q -p girocrab-core --example category_perf` (about 31k bookings) and put the timings in the PR. Measure before claiming a speed-up.

#### ♿ UI scaling, platforms & schema integrity

- [ ] **Text size:** text styles use `.appFont(...)` (`scripts/lint-fonts.sh`); check visually with `--screenshot <png> --text-size larger` next to `--text-size standard`.
- [ ] **Linux / serde:** every `uniffi::Record` / `uniffi::Enum` carries the `serde` `cfg_attr`. `scripts/lint-serde-feature.sh` passes and `cargo check -q -p girocrab-core --features serde` builds.
- [ ] **iOS:** with `#if os(macOS)` or other platform-specific APIs, the iOS target still builds (`xcodebuild -quiet`, iOS scheme).
- [ ] **Schema:** a new table or column is a new entry at the end of `STEPS` in `girocrab-core/src/schema.rs`, plus the fixture (`cargo run -q -p girocrab-core --example make_fixture_vault`). Check what `vault/merge.rs` (iCloud merge) does with it. Never edit an existing step.
- [ ] **Strings:** `scripts/xcstrings.py check` passes.
- [ ] **Swift tests and `UserDefaults`:** tests that use `UserDefaults.standard` carry `// test: serial-defaults` (run serially after the parallel phase, `scripts/swift-test-split.sh`); own suites always with `\(UUID().uuidString)` (`scripts/lint-test-defaults.sh`).
- [ ] **Before pushing:** `cargo fmt --all`, `cargo clippy -q --workspace --all-targets -- -D warnings`, `scripts/lint-appmodel-internals.sh`.

#### 🤖 AI Provenance & Process Reflection

PRs written with AI help carry two things (details and examples: [AI transparency](docs/ai-transparency.md)):

1. **Git trailers** on every AI-authored commit:
   * `AI-Orchestrator: <model>` is the model that plans and orchestrates the session, with a matching `Co-authored-by: <Model> <email>`.
   * `AI-Agent: <tool>` or `AI-Model: <model>` is the coding agent that wrote the code.
   * Optional: `Security-Check: <text>` and `Perf-Check: <text>`, with the same content as the bullets below. The card reads both places.
2. **A `### 🤖 AI Process Reflection` block** at the end of the PR body with five bullets: `Human-Steering`, `AI-Agency`, `Review-Focus` and the mandatory **`Security-Check`** and **`Perf-Check`**:
   * **Security-Check:** what you checked for secrets and personal data (logs, `.sensitive()` redaction, new HTTP/process/file access, new or upgraded dependencies, `THIRD_PARTY_NOTICES.md`). "Nothing relevant: <why>" is a valid answer; an empty one is not.
   * **Perf-Check:** what you measured or ruled out (`offMain` for calls over ~16 ms, no `list_transactions` for counts or sums, `category_perf` numbers for changes on the category path). "No hot path touched: <why>" is a valid answer. Naming `category_perf` in the PR body is shown as a hint.

   The card lists both in stage 4 "Verification & Audit" and in the reflection block. PRs that only have the three original bullets still render.
