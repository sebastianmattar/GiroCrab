# GiroCrab – agent efficiency rules

Rust core (`girocrab-core`, UniFFI) + SwiftUI app (`GiroCrabApp`). Root = this dir (`app/`).

## File map (go here first; don't `find`/`grep` to locate code)
- `girocrab-core/src/lib.rs` – crate root, UniFFI exports
- `girocrab-core/src/models.rs` – domain types
- `girocrab-core/src/vault.rs` – encrypted storage: open/key/migrate/snapshot only
- `girocrab-core/src/vault/` – `impl Vault` per feature: `accounts.rs`, `bookings.rs` (list_transactions/paging), `categories.rs`, `connections.rs` (bank logins, stored PIN), `rules.rs`, `settings.rs`, `merge.rs`
- `girocrab-core/src/schema.rs` – versioned schema migrations (see `docs/ARCHITECTURE.md`)
- `girocrab-core/src/connectors/` – `mod.rs` (submodule decls + shared HTTP helpers), `types.rs` (`BankConnector`
  trait + DTOs), `catalog.rs` (`descriptor`, `make_connector*`), `bank_directory.rs` (search routing + curated PSD2 catalog),
  `sync_session.rs` / `transfer_session.rs` (FFI state machines), `persist.rs` (`impl Vault`: `store_fetch`,
  `begin_sync`/`begin_transfer`), `html.rs` (helpers for connectors that read web pages), tests in `connectors/tests/`; connector impls alongside: `fints/`, `comdirect.rs`,
  `trade_republic.rs`, `enable_banking.rs` (`needs_lean_request` 90d cap for Revolut/Spuerkeess/N26), `paypal.rs`
- `girocrab-core/src/importers/` – one-off file/web-capture importers: `moneymoney_db.rs` (encrypted SQLCipher database import),
  `moneymoney_import.rs` (CSV/XML/CAMT/MT940 exports), `portal_import.rs` (Amex/Amazon Visa web + CSV/PDF import),
  `amazon_orders.rs`
- `girocrab-core/src/tags.rs` – booking tags (#228): schema step, `impl Vault` API, booking tag loading; `tags/merge.rs` (iCloud merge of tags and tag sets), `tags/tests.rs`
- `girocrab-core/src/merchants.rs` – merchant matching
- `girocrab-core/src/recipients.rs` – known recipients of the transfer form (one per IBAN, cached), hidden IBANs + merge (#978)
- `girocrab-core/src/documents.rs` – booking documents / PDF text extraction
- `girocrab-core/src/text.rs` – text helpers (mojibake repair etc.)
- `girocrab-core/src/local_api.rs` + `local_api/server.rs` – read-only local interface for external analysis (#136): queries, scope/token, Unix socket server (`docs/local-api.md`)
- `girocrab-mcp/` – MCP stdio adapter for that socket (own crate, no core dependency outside tests; architecture, bundling, local testing: `docs/local-api-entwicklung.md`)
- `girocrab-core/src/report_sanitizer.rs` – scrubs log/diagnostics text for public bug reports (`sanitize_report_text`)
- `girocrab-core/src/sys.rs` – the only `unsafe` code (libc wrappers); exceptions to `scripts/lint-no-obfuscation.sh` (CI, #948) go in that script.
- `GiroCrabApp/Sources/GiroCrabUI/PrivacyRedaction.swift` – `.sensitive()` modifier; `BugReport.swift` / `BugReportUI.swift` – Hilfe › Problem melden
- `GiroCrabApp/Sources/GiroCrabUI/AppModel.swift` – app state (all stored instance properties; static members stay in their feature file), init, vault lifecycle,
  `vaultOffMain`/`pinVault`; features live in `AppModel+<Feature>.swift` extensions: `Loading` (reloads, booking
  list paging, account markers), `Selection` (sidebar/period/search, pay cycle), `Editing` (categories, rules,
  notes), `Connections` (bank connections, settings, FinTS product ID), `Sync`, `Transfer`, `Documents`,
  `Recurring`, `Logos`, `Printing`; helper types in `AppModelTypes.swift`.
  An `AppModel+*.swift` file holds only `extension AppModel` blocks. Members the feature files share are internal
  but not for views: `scripts/lint-appmodel-internals.sh` (CI) lists them; add one there when you widen a `private`.
- `GiroCrabApp/Sources/GiroCrabUI/RootView.swift` – top-level UI
- `GiroCrabApp/Sources/GiroCrabUI/BookingListView.swift` – booking list
- `GiroCrabApp/Sources/GiroCrabUI/ConnectionsUI.swift` – bank connections UI

Other UI files live in the same `GiroCrabUI/` dir. A view with its own design page (`docs/*_UI.md`) names it in a comment on its type; a new view gets one. Ignore `cashcrab-core/` and `CashCrabApp/` (legacy).

## Builds
- **Swift-only change** (only `GiroCrabApp/**/*.swift`): NEVER rebuild `girocrab-core` or run `./build-xcframework.sh`. Run only `swift build -q` (in `GiroCrabApp/`) or `xcodebuild -quiet`.
- Rust change: `cargo check -q -p girocrab-core`; rebuild the xcframework only if the UniFFI API changed.
- Before every push run `cargo fmt --all`: CI fails on `cargo fmt --check` and then skips the Swift job. Enable the hook once: `git config core.hooksPath .githooks`
  (pre-push runs every cheap CI lint via `scripts/verify-fast.sh --lint-only`, ~10 s; without the flag it adds clippy and the serde check for the changed files, `--tests` the unit tests).
- Always pass `-q`/`--quiet` to `cargo check`, `cargo test`, `xcodebuild`. If output is still long, pipe through `tail -n 40`.
- Before pushing a PR (also Swift-only ones; CI runs these on every PR): `cargo fmt --all -- --check` and `cargo clippy -q --workspace --all-targets -- -D warnings`. After pushing, check `gh pr checks`.
  Under clippy the root `build.rs` skips the xcframework + Swift app build (~8 s instead of ~90 s after a core
  edit); `cargo check/build --workspace` still run it, so set `GIROCRAB_SKIP_SWIFT=1` for core-only checks.
- **CI runs the Linux jobs (`qt-minimum`, `debian`) of `ci.yml` for any change under `girocrab-core/**` (other than Markdown) or to `Localizable.xcstrings`**, not only for Linux files.
  Before pushing such a change: `python3 -I GiroCrabLinux/scripts/i18n-report.py` (every Linux UI text needs English, step
  "English UI texts") and `(cd GiroCrabLinux && cargo fmt -- --check)`. Its clippy/test steps need Qt: leave those to CI.
- **A red check on code the PR does not touch is usually inherited from `main`.** Before debugging it: run the failing step
  locally, check `git diff --quiet origin/main HEAD -- <dir>` (silent = same as main) and `gh run list -R GiroCrab/GiroCrab
  --workflow <name>` (other open PRs red on the same job = main's fault). Report it in the PR instead of "fixing" it there.
  Logs of a run that is still going: `gh api repos/GiroCrab/GiroCrab/actions/jobs/<job id>/logs --allow-escape-sequences`
  (`gh run view --log-failed` waits for the whole run).
- Every `uniffi::Record` / `uniffi::Enum` in `girocrab-core` carries `#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]` right below its `#[derive(…)]` (front ends without UniFFI, e.g. Linux, exchange JSON). `scripts/lint-serde-feature.sh` checks it (CI) and `cargo check -q -p girocrab-core --features serde` must build; run both before pushing Rust changes.

## User-visible texts (no third-party brands)
Nutzersichtbare Texte (README, docs, Website, App-Strings, Release-Notes) nennen keine Marken anderer Banking- oder
Finanzsoftware und enthalten keine Vergleiche mit Fremdprodukten. Funktionen von GiroCrab werden sachlich und belegbar
beschrieben.
User-visible texts (README, docs, website, app strings, release notes) do not name brands of other banking or finance
software and contain no comparisons with other products. GiroCrab's features are described factually and verifiably.
Internal code identifiers (module, function, test and file names) are exempt; `scripts/lint-brand-names.sh` (CI) checks the rest.

## Don't read into context
- Generated bindings: `generated/**/girocrab_core.swift` (6k lines), the xcframework, `target/`, `.build/`.
- Data/large tables: `girocrab-core/data/*.tsv` (BIC/FinTS directories, merchant dictionary; parsed by `src/data_tables.rs`), icon/merchant SVG/JSON, `feed.json`, `craft.json`, `*.log`, `agent-session.jsonl`.
- For any file > 200 lines use `grep -n` or `Read` with `offset`/`limit`; never read it whole.

## Tests
- Swift: `cd GiroCrabApp && swift test -q --filter <Suite>`; whole suite ~30 s. `AppModel` tests
  run against a demo vault: `AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))`,
  pattern in `Tests/GiroCrabUITests/SelectionLoadTests.swift`. Run them before pushing.
- Tests, die `UserDefaults.standard` nutzen, tragen `// test: serial-defaults` (läuft seriell nach der parallelen Phase, `scripts/swift-test-split.sh`). Eigene Suites immer mit `\(UUID().uuidString)` (`scripts/lint-test-defaults.sh`).
- Rust: `cargo test -q -p girocrab-core --lib <module>` for one module; whole `--lib` run ~10 s. It stays
  that fast only because unit tests skip `cipher_memory_security` (`cfg!(test)` in `Vault::apply_key`) and
  `.cargo/config.toml` builds SQLite with `SQLITE_DEFAULT_MEMSTATUS=0`: both make parallel test threads
  wait on each other (147 s with both on, 417 s with only memory security on). Tests that wait on timers use
  `#[tokio::test(start_paused = true)]`, never real sleeps.
- Perf: `COPIES=600 cargo run --release -q -p girocrab-core --example category_perf` times the core
  calls behind a category change on ~31k bookings (reference numbers in the file). Measure before
  claiming a speed-up; `limit` on `list_transactions` does not reduce core work.

## Architecture rules of thumb
- Every core call from Swift is synchronous and holds the vault mutex. Anything over ~16 ms goes
  through `AppModel.offMain` and checks `listGeneration` / vault identity before publishing.
  From UI code use `model.vaultOffMain { try $0.… }` (nil when locked/swapped) or
  `requireVaultOffMain` (multi-step jobs: `pinVault()`); views never touch `model.vault` (`isUnlocked` for the nil check) –
  `scripts/lint-vault-access.sh` enforces this in CI.
- `vault.list_transactions` enriches every row in the filter (merchant, flow, sums); `limit`/`offset`
  only trim the result. Never call it for a count or a sum – add a SQL aggregate.
- **Multi-currency invariant (#569):** Never sum amounts across different currencies into one total. `EUR` is the primary
  aggregate currency; non-EUR currencies are grouped and displayed separately per currency (*„Zusätzlich: …“*).
- **PR size & schema splitting:** Keep each PR to one focused concern (~15–20 files max). Oversized multi-feature PRs are
  automatically put on `hold` by the review gate. If a feature needs a schema migration, land the schema step + core model
  in a small standalone PR first so parallel PRs do not collide on `STEPS` numbers.
- An edit must either reload synchronously or declare what it invalidates (`rulesStale`,
  `dashboardStale`, `categoryRefreshTask`). A cancelled background load must never be the only
  thing that would have refreshed a view.
- Schema and index changes are a new entry at the end of `STEPS` in `girocrab-core/src/schema.rs`
  (never edit an existing entry). Bump = run `cargo run -q -p girocrab-core --example make_fixture_vault`
  and commit the new fixture (+ the `schema-v{N}.sql` the fixture test writes).
  Counts filter `deleted_ms IS NULL`: indexes they use must cover that column.

## Text size (#343, `GiroCrabUI/TextSize.swift`)
macOS has no Dynamic Type; the app scales text itself (Einstellungen › Allgemein › Schriftgröße, ×1/1.15/1.3).
- Text styles: `.appFont(.caption, weight: .semibold, design:, monospacedDigit: true)`, never `.font(.caption…)`
  (`scripts/lint-fonts.sh`, CI). `.font(.system(size:))` only for icons/signets, which must not scale.
- Text inside a macOS sidebar `List` row needs `.appDefaultFont()` on the `Text` itself (the List overrides the scene font).
- Form section headers (macOS grouped `Form`): `Section { … } header: { Text("…").appSectionHeader() }`,
  not `Section("…")` – the scaled scene font drops the header's bold. Not in `List`s or menus (own style).
- Fixed widths/heights that hold text: multiply by `@Environment(\.textScale)` (see sidebar `macColumnWidth`,
  chart axis label widths). Don't add `minimumScaleFactor` to make text fit (it shrinks only some labels):
  allow a second line, or `.fixedSize()` on what must never truncate (amounts) so the name truncates instead.
- Swift Charts: `AxisValueLabel()` / `AxisValueLabel(format:)` keep the system size; use
  `AxisValueLabel { Text(…).appFont(.caption2).fixedSize() }` (without `fixedSize` the label is cut off).
- Stay at system size (acceptable, like Mail/Finder): segmented pickers, toolbar items, menus, alerts, picker values.
- Check visually: `swift build -q` in `GiroCrabApp/`, then
  `.build/debug/GiroCrabMac --screenshot <png> --text-size larger [--demo-sheet …|--demo-settings …]`
  next to `--text-size standard` (`--screenshot` alone always renders standard).
## Sensitive UI (bug report screenshots, #301/#384)
"Problem melden" captures the main window (macOS: the window showing `MainSplitView`; iOS: the root view), the
sheet or popover over it and, on macOS, the tracked windows in front of it (the Settings window and its sheets),
with personal data covered, so every new or changed view in there must mark what it shows.
Other windows (Hilfe, Abruf-Protokoll), alerts and print output are never captured:
- Sheets and popovers are presented with `.capturableSheet` / `.capturablePopover` instead of `.sheet` / `.popover`
  (`GiroCrabUI/ReportDialogs.swift`; `scripts/lint-report-dialogs.sh` fails otherwise). Their content is redacted like
  the main window and drawn into the picture at its place; text fields, text editors and web views (bank/portal logins) are
  covered by the capture itself, on macOS also pop-up menus. An iOS menu-style `Picker`/`Menu` draws its value itself, so mark
  it `.sensitive()`. An alert or a non-capturable presentation leaves the picture without it and the report says so.
  A view that must never be captured (the report itself) carries `// report: not captured (<why>)`.
  macOS closes an open popover when the report window takes the focus, so only a sheet (or a popover over a
  window that keeps the focus) is in the picture there; iOS/iPadOS keep it.
- A macOS window of its own is captured only when tracked: the `Settings` scene carries `.reportCapturable()`,
  so its views get the redaction environment and its window (with its sheets) is drawn over the main window.
  A window that must never be captured (the report itself) carries no tracker or one with role `.report`.
- `.sensitive()` on anything personal: IBANs, account names and numbers, owners, counterparties, merchant
  names/initials/guessed domains, purposes, references, notes, documents and their previews.
  `.sensitive(.amounts)` on money and quantities: amounts, balances, sums, shares, chart axis values and readouts, loan
  figures; text mixing both gets `.sensitive([.personal, .amounts])` (a requested `.personal` redaction hides amounts too,
  `ReportRedaction.effective`). `.sensitive(.labels)` for category, group, budget and security names
  (hidden only on request). Dates, bank/connector names, logos of known brands and error texts stay visible.
- The modifier does not draw the content (opacity 0) and puts a soft, fixed smudge in its place (gradients, no `.blur`),
  so it also covers AppKit/UIKit-backed views (TextEditor, PDFView). Never use `.redacted(reason:)`, `.blur` or a plain
  overlay for this. In a parameter string use `redaction.masking(text, .amounts)` for an amount.
- Text that is a parameter (`navigationTitle`, `ContentUnavailableView(description:)`, `.help`, title bars) cannot
  carry the modifier: read `@Environment(\.reportRedaction)` and substitute (see `WindowTitle`, the empty state in
  `BookingStreamView`). AppKit/UIKit-drawn search fields are handled in `BugReport` – new platform text controls
  outside SwiftUI need the same.
- Check a view: `swift run GiroCrabMac --demo-seed --redact --screenshot /tmp/r.png` (`--redact=labels` for labels)
  or `--privacy` / `--privacy=all` (privacy mode).
  A dialog in the picture: `--demo-seed --demo-sheet report:<sheet>` (e.g. `report:transfer`, `report:category`)
  opens the report over that demo sheet.
- Text that goes into a public issue (log lines, messages) only via `sanitize_report_text` with the vault's known
  terms; never paste raw log or bank text. New log/diagnostic fields get a test in `report_sanitizer.rs` or
  `BugReportTests`.

## Git, AI Attribution & Process Reflection
The PR card (`tools/export-ai-provenance.py`, `docs/ai-transparency.md`) reads only structured signals, so mark AI work like this:
- Every AI-authored commit gets `AI-Orchestrator: <model that orchestrates the session>` and a matching
  `Co-authored-by: <Model> <email>` trailer. Name the model that actually took part, never another one:
  `Google Gemini` / `gemini@google.com` when the maintainer `@dr-prinz` drives the session through Gemini
  (the default for the maintainer's sessions), otherwise the model the contributor names (`ChatGPT`, `Claude`,
  `Cursor`, `GitHub Copilot`, …). A coding agent that writes code adds `AI-Agent: <tool>` or `AI-Model: <model>`.
  Stage 1+2 (framing/orchestration) = `AI-Orchestrator`; stage 3 (code generation) = `Co-authored-by`, `AI-Agent`, `AI-Model`.
- Every AI-authored PR body ends with this section (German is fine; the five labels stay as written):
  ```
  ### 🤖 AI Process Reflection
  - **Human-Steering:** what the human specified or decided (architecture, security, scope)
  - **AI-Agency:** what the AI decided on its own (names, structure, edge cases)
  - **Review-Focus:** where a human reviewer should look first
  - **Security-Check:** what was checked for secrets/personal data (logs, `.sensitive()` redaction, new network/process/file access, dependencies + `THIRD_PARTY_NOTICES.md`); "Nothing relevant: <why>" is valid
  - **Perf-Check:** what was measured or ruled out (`offMain` for calls over ~16 ms, no `list_transactions` for counts/sums, `category_perf` numbers); "No hot path touched: <why>" is valid
  ```
- Optional trailers `Security-Check:` / `Perf-Check:` carry the same content as the bullets; the card shows both in stage 4. Before pushing run the checks in the PR checklist of `CONTRIBUTING.md`.
- This is a self-report, shown on the card as unverified. Never put real financial data, IBANs or secrets in it.
- **Community contributor PRs landed via an `origin` branch** (e.g. to resolve merge conflicts or apply a pre-merge fix
  without pushing to the contributor's fork): ALWAYS preserve the contributor's credit on squash-merge, either by
  - keeping their authorship (`git commit --author "<Name> <email>"`, or `gh pr merge --rebase` when the history is clean), OR
  - adding `Co-authored-by: <Contributor Name> <id+login@users.noreply.github.com>` (copied from the original commit's
    author header, `git log --format='%an <%ae>'`) to the PR body AND the squash commit message, so GitHub counts the
    commit in their contributor graph.

## Workflow
- **Verify locally first, never block on CI.** Targeted `cargo test`, `swift test`, `clippy`, `cargo fmt --all -- --check`
  and the `scripts/lint-*.sh` checks run locally before the push (together well under a minute for a focused change).
  After the PR is open do NOT sit in the session for 10–15 min with `gh pr checks --watch` (and never pipe a `--watch` to `tail`).
  Either enable auto-merge: `gh pr merge <N> --auto --squash --delete-branch` (enabled on the repo), or merge once the fast required check
  (lint/`card`) is green when local verification already covered the full test suite. Look at `gh pr checks <N>` (no `--watch`) only on demand.
- **Merge without waiting for the slow runners:** when the relevant local checks (`cargo test`, `swift test`, `scripts/lint-*.sh`)
  already passed on this Mac and `card` is green, do NOT wait for the cloud `CI / swift` / `CI / rust` runs: merge immediately with
  `gh pr merge <N> --squash --delete-branch --admin`.
- CI is one workflow (`ci.yml`); its `changes` job sets per-job flags from the changed files (PR: against the base, push to main: against the
  previous head, manual run: everything). Linux (`qt-minimum`, `debian`) runs for `GiroCrabLinux/**`, `girocrab-core/**` (not `*.md`) and
  `**/Localizable.xcstrings`; Flatpak for `GiroCrabLinux/**` and `girocrab-core/Cargo.toml`; `windows-core` for the core only; `windows-app` for
  `GiroCrabWindows/**` and the core, its release build + installer for `GiroCrabWindows/**`/`scripts/package-windows.ps1` only; `swift` for
  `GiroCrabApp/**` and the core. The cheap lint steps of `rust` always run. In PRs Debian/Flatpak build only amd64/x86_64 (arm64/aarch64 on main).
- `rust` (fmt, lints, clippy, `cargo test --workspace`) runs on Ubuntu; the macOS-only branches of the local API are tested in `swift`.
  Only `main` writes the Rust cache (`save-if`), PRs restore it: the repository's 10 GB Actions cache is small, branch caches evicted main's.
  `cache-cleanup.yml` deletes a PR's caches (`refs/pull/<N>/merge`) when it is closed or merged. Check the size: `gh api repos/GiroCrab/GiroCrab/actions/cache/usage`.
- Branch from `upstream/main`, push to `origin` (fork), PR against `GiroCrab/GiroCrab`. PR title
  and description in German, conventional-commit prefix (`feat:`, `fix:`, `perf:`, `ci:`, `docs:`).
- The local xcframework is whatever branch last ran `./build-xcframework.sh`. Before trusting
  `swift build` errors, run `./build-xcframework.sh --check` (silent + exit 0 = bindings match the
  core sources; the hooks in `.githooks` run it after checkout/merge/rebase) and rebuild if it warns.
- `Localizable.xcstrings` is sorted by key (CI: `scripts/xcstrings.py check`): never append at the end. After adding
  keys run `scripts/xcstrings.py format` (`GiroCrabApp/Tools/update-localizations.sh` writes the same form).
  Merge driver for conflicts, once per clone: `git config merge.xcstrings.driver 'scripts/xcstrings.py merge %O %A %B'`.

## Lockstep: README, screenshots, releases, website
`GiroCrab/girocrab.de` (clone next to this repo as `../girocrab.de`) and the newest icon from
`GiroCrabApp/Xcode/App/Assets.xcassets/AppIcon.appiconset` must ALWAYS be updated together
whenever `README.md`, `docs/screenshots/**`, `docs/assets/**` or a release changes:
- Icon: `docs/assets/app-icon.png` (256 px, cropped from `icon-1024.png`) and `GiroCrabApp/Assets/girocrab-icon.svg` mirror the appiconset; the website's `img/` icons too. Never reuse the old logo.
- Download buttons `docs/assets/download-macos.svg` / `download-ios.svg` carry the version; the site hero must show the same version and links.
- Marketing screenshots: `--screenshot` hides the demo banners; re-shoot, then copy to `../girocrab.de/img/screenshots/`.
- Never remove from `../girocrab.de/index.html` the support section `#unterstuetzen` ("☕ Projekt unterstützen (Freiwillige Zuwendung)", paypal.me/aprinz) and its `THIRD_PARTY_NOTICES.md` link; keep it above the footer on every sync.
- Social preview: `og:image`/`twitter:image` use the square `og-logo-v2.png` (512×512, from `icon-1024.png`; `twitter:card=summary`). Regenerate it when the icon changes.
- Automated: `scripts/sync-docs-and-website.sh` (`--dry-run` / `--push`) syncs version strings and the `<!-- sync:connectors -->` blocks (from `catalog.rs`) in README + site; runs from `release.yml` (on publish) and `nightly.yml` (needs secret `WEBSITE_DEPLOY_TOKEN`, see `docs/docs-website-sync.md`). Prose, screenshots and "Neu in v…" stay manual.
- Commit + push both repos in the same session. `scripts/notarize.sh` warns if the site lacks the new version.

## Third-party notices (dependencies)
- **Rule:** whenever dependencies in `Cargo.toml` (any workspace crate), `Cargo.lock` or `GiroCrabApp/Package.swift` are added, removed or upgraded (and before every release), `THIRD_PARTY_NOTICES.md` MUST be updated in the same commit/PR (name, version, exact license, repository).
- Linux: whenever `girocrab-core/Cargo.toml` dependencies or versions change, also sync `GiroCrabLinux/Cargo.lock` (`cargo update -w --manifest-path GiroCrabLinux/Cargo.toml`) and `GiroCrabLinux/packaging/flatpak/cargo-sources.json` (`GiroCrabLinux/packaging/flatpak/update-cargo-sources.sh`) so Flatpak CI (`--locked --offline`) stays green.
- Check: `scripts/check-third-party-notices.sh` fails if a direct crate or Swift package is missing from the notices (also runs in CI). Run it before pushing and before `scripts/notarize.sh`.

## Release checklist (`scripts/notarize.sh --upload vX.Y.Z`)
- Before running: update `../girocrab.de` + icon/buttons/screenshots in lockstep (see above); bump `version` in `Cargo.toml`, and in `README.md` rename `### ✨ Neu in v…` / `### ✨ What's new in v…` to the new version with updated highlights (DE + EN).
- The script aborts if `Cargo.toml` differs from the tag (override: `ALLOW_VERSION_MISMATCH=1`) or if those README headings lack the tag version.
- It sets `MARKETING_VERSION` in `project.pbxproj` and `Aktuelle Version: **v…**` / `Current version: **v…**` in `README.md` (stable tags only), then verifies both. Commit those changes afterwards.


## Release / nightly commands (`scripts/notarize.sh`)
- Mac only (nightly on GitHub): `./scripts/notarize.sh --nightly`
- Mac (nightly on GitHub) + iPhone (TestFlight upload incl. automatic build bump): `./scripts/notarize.sh --nightly --ios`
- Official release (Mac on GitHub + iPhone to TestFlight): `./scripts/notarize.sh --upload vX.Y.Z --ios`
- `--ios` increments `CURRENT_PROJECT_VERSION` in `project.pbxproj` by +1 (working tree only, NOT committed: commit it afterwards),
  runs `./build-xcframework.sh`, archives and uploads. `MARKETING_VERSION` is taken from the project unless
  `IOS_MARKETING_VERSION=x.y.z` is set (pins only the iOS archive; a changed version triggers a 24 h TestFlight beta review,
  a new build under an approved version does not). Example: `IOS_MARKETING_VERSION=0.6.1 ./scripts/notarize.sh --nightly --ios`.
- Check the TestFlight version/build before running; a build number must be higher than any already uploaded for that version.

## Data tables
`girocrab-core/data/*.tsv` are embedded at compile time and parsed by `src/data_tables.rs`: `bic_directory.tsv` (BIC),
`fints_directory.tsv` (FinTS endpoints), `merchants.tsv` (merchant dictionary). Never read them whole; `grep -n` a key instead.

## Bank-Konnektoren: End-to-End-Pflicht (kein reiner Core-Merge)
- Ein neuer Bank- oder Depot-Konnektor gilt **nicht** als fertig und darf sein Feature-Issue nicht schließen, solange er nur als Parser in `girocrab-core/src/connectors/` existiert.
- Jeder Konnektor-PR muss zwingend alle vier Ebenen verdrahten:
  1. Core-Konnektor (`girocrab-core/src/connectors/`)
  2. Bankenkatalog / Preset-Registrierung
  3. FFI-Anbindung (`girocrab-core` / UniFFI)
  4. UI-Einrichtung & Tests in `GiroCrabApp`
- Vor jedem PR `bash scripts/lint-connectors.sh` ausführen (und bei Nacharbeit an `finpension` / `paypal_nvp` deren Eintrag aus der Allowlist entfernen).
