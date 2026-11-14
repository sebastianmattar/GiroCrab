# GiroCrab – agent efficiency rules

Rust core (`girocrab-core`, UniFFI) + SwiftUI app (`GiroCrabApp`). Root = this dir (`app/`).

## File map (go here first; don't `find`/`grep` to locate code)
- `girocrab-core/src/lib.rs` – crate root, UniFFI exports
- `girocrab-core/src/models.rs` – domain types
- `girocrab-core/src/vault.rs` – encrypted storage: open/key/migrate/snapshot only
- `girocrab-core/src/vault/` – `impl Vault` per feature: `accounts.rs`, `bookings.rs` (list_transactions/paging), `categories.rs`, `rules.rs`, `settings.rs`, `merge.rs`
- `girocrab-core/src/schema.rs` – versioned schema migrations (see `docs/ARCHITECTURE.md`)
- `girocrab-core/src/connectors/` – `mod.rs` (submodule decls + shared HTTP helpers), `types.rs` (`BankConnector`
  trait + DTOs), `catalog.rs` (`descriptor`, `make_connector*`), `sync_session.rs` / `transfer_session.rs` (FFI state
  machines), `persist.rs` (`impl Vault`: `store_fetch`, `begin_sync`/`begin_transfer`), tests in `connectors/tests/`;
  connector impls alongside: `fints/`, `comdirect.rs`, `trade_republic.rs`, `enable_banking.rs`, `paypal.rs`
- `girocrab-core/src/importers/` – one-off file/web-capture importers: `moneymoney_db.rs` (MoneyMoney SQLCipher),
  `moneymoney_import.rs` (CSV/XML/CAMT/MT940 exports), `portal_import.rs` (Amex/Amazon Visa web + CSV/PDF import),
  `amazon_orders.rs`
- `girocrab-core/src/merchants.rs` – merchant matching
- `girocrab-core/src/documents.rs` – booking documents / PDF text extraction
- `girocrab-core/src/text.rs` – text helpers (mojibake repair etc.)
- `girocrab-core/src/report_sanitizer.rs` – scrubs log/diagnostics text for public bug reports (`sanitize_report_text`)
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

Other UI files live in the same `GiroCrabUI/` dir. Ignore `cashcrab-core/` and `CashCrabApp/` (legacy).

## Builds
- **Swift-only change** (only `GiroCrabApp/**/*.swift`): NEVER rebuild `girocrab-core` or run `./build-xcframework.sh`. Run only `swift build -q` (in `GiroCrabApp/`) or `xcodebuild -quiet`.
- Rust change: `cargo check -q -p girocrab-core`; rebuild the xcframework only if the UniFFI API changed.
- Before every push run `cargo fmt --all`: CI fails on `cargo fmt --check` and then skips the Swift job. Enable the hook once: `git config core.hooksPath .githooks`.
- Always pass `-q`/`--quiet` to `cargo check`, `cargo test`, `xcodebuild`. If output is still long, pipe through `tail -n 40`.
- Before pushing a PR (also Swift-only ones; CI runs these on every PR): `cargo fmt --all -- --check` and `cargo clippy -q --workspace --all-targets -- -D warnings`. After pushing, check `gh pr checks`.

## Don't read into context
- Generated bindings: `generated/**/girocrab_core.swift` (6k lines), the xcframework, `target/`, `.build/`.
- Data/large tables: `girocrab-core/data/*.tsv` (BIC/FinTS directories, merchant dictionary; parsed by `src/data_tables.rs`), icon/merchant SVG/JSON, `feed.json`, `craft.json`, `*.log`, `agent-session.jsonl`.
- For any file > 200 lines use `grep -n` or `Read` with `offset`/`limit`; never read it whole.

## Tests
- Swift: `cd GiroCrabApp && swift test -q --filter <Suite>`; whole suite ~30 s. `AppModel` tests
  run against a demo vault: `AppModel(options: LaunchOptions(arguments: ["--demo-seed"], environment: [:]))`,
  pattern in `Tests/GiroCrabUITests/SelectionLoadTests.swift`. Run them before pushing.
- Rust: `cargo test -q -p girocrab-core --lib <module>` for one module; whole run ~50 s.
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
- An edit must either reload synchronously or declare what it invalidates (`rulesStale`,
  `dashboardStale`, `categoryRefreshTask`). A cancelled background load must never be the only
  thing that would have refreshed a view.
- Schema and index changes are a new entry at the end of `STEPS` in `girocrab-core/src/schema.rs`
  (never edit an existing entry). Bump = run `cargo run -q -p girocrab-core --example make_fixture_vault`
  and commit the new fixture (+ the `schema-v{N}.sql` the fixture test writes).
  Counts filter `deleted_ms IS NULL`: indexes they use must cover that column.

## Sensitive UI (bug report screenshots, #301)
"Problem melden" captures the main window (macOS: the window showing `MainSplitView`; iOS: the root view, without
presented sheets) with personal data covered, so every new or changed view inside it must mark what it shows. Sheets,
popovers, other windows and print output are never captured:
- `.sensitive()` on anything personal or financial: amounts, balances, chart axis values and readouts, IBANs, account
  names and numbers, owners, counterparties, merchant names/initials/guessed domains, purposes, references, notes,
  documents and their previews, loan figures. `.sensitive(.labels)` for category, group, budget and security names
  (hidden only on request). Dates, bank/connector names, logos of known brands and error texts stay visible.
- The modifier hides the content and draws an opaque box, so it also covers AppKit/UIKit-backed views (TextEditor,
  PDFView). Never use `.redacted(reason:)` or a plain overlay for this.
- Text that is a parameter (`navigationTitle`, `ContentUnavailableView(description:)`, `.help`, title bars) cannot
  carry the modifier: read `@Environment(\.reportRedaction)` and substitute (see `WindowTitle`, the empty state in
  `BookingStreamView`). AppKit/UIKit-drawn search fields are handled in `BugReport` – new platform text controls
  outside SwiftUI need the same.
- Check a view: `swift run GiroCrabMac --demo-seed --redact --screenshot /tmp/r.png` (`--redact=labels` for labels).
- Text that goes into a public issue (log lines, messages) only via `sanitize_report_text` with the vault's known
  terms; never paste raw log or bank text. New log/diagnostic fields get a test in `report_sanitizer.rs` or
  `BugReportTests`.

## Workflow
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
