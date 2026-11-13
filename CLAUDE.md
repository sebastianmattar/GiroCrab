# GiroCrab – agent efficiency rules

Rust core (`girocrab-core`, UniFFI) + SwiftUI app (`GiroCrabApp`). Root = this dir (`app/`).

## File map (go here first; don't `find`/`grep` to locate code)
- `girocrab-core/src/lib.rs` – crate root, UniFFI exports
- `girocrab-core/src/models.rs` – domain types
- `girocrab-core/src/vault.rs` – encrypted storage
- `girocrab-core/src/schema.rs` – versioned schema migrations (see `docs/ARCHITECTURE.md`)
- `girocrab-core/src/connectors/mod.rs` – bank connector trait/dispatch (impls alongside: `fints/`, `comdirect.rs`, `trade_republic.rs`, …)
- `girocrab-core/src/connectors/` – all connector impls: `fints/`, `comdirect.rs`, `trade_republic.rs`, `enable_banking.rs`, `paypal.rs`, `portal_import.rs` (Amex/Amazon Visa web + CSV/PDF import)
- `girocrab-core/src/merchants.rs` – merchant matching
- `girocrab-core/src/documents.rs` – booking documents / PDF text extraction
- `girocrab-core/src/text.rs` – text helpers (mojibake repair etc.)
- `GiroCrabApp/Sources/GiroCrabUI/AppModel.swift` – app state / core bridge
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
- Data/large tables: `girocrab-core/src/merchant_dict.rs`, icon/merchant SVG/JSON, `feed.json`, `craft.json`, `*.log`, `agent-session.jsonl`.
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

## Workflow
- Branch from `upstream/main`, push to `origin` (fork), PR against `GiroCrab/GiroCrab`. PR title
  and description in German, conventional-commit prefix (`feat:`, `fix:`, `perf:`, `ci:`, `docs:`).
- The local xcframework is whatever branch last ran `./build-xcframework.sh`. After a rebase or
  branch switch with Rust changes, rebuild before trusting `swift build` errors.
- `Localizable.xcstrings` is not sorted: append keys, never re-sort (a sort is a 4k-line diff).

## Lockstep: README, screenshots, releases, website
`GiroCrab/girocrab.de` (clone next to this repo as `../girocrab.de`) and the newest icon from
`GiroCrabApp/Xcode/App/Assets.xcassets/AppIcon.appiconset` must ALWAYS be updated together
whenever `README.md`, `docs/screenshots/**`, `docs/assets/**` or a release changes:
- Icon: `docs/assets/app-icon.png` (256 px, cropped from `icon-1024.png`) and `GiroCrabApp/Assets/girocrab-icon.svg` mirror the appiconset; the website's `img/` icons too. Never reuse the old logo.
- Download buttons `docs/assets/download-macos.svg` / `download-ios.svg` carry the version; the site hero must show the same version and links.
- Marketing screenshots: `--screenshot` hides the demo banners; re-shoot, then copy to `../girocrab.de/img/screenshots/`.
- Never remove from `../girocrab.de/index.html` the support section `#unterstuetzen` ("☕ Projekt unterstützen (Freiwillige Zuwendung)", paypal.me/aprinz) and its `THIRD_PARTY_NOTICES.md` link; keep it above the footer on every sync.
- Social preview: `og:image`/`twitter:image` use the square `og-logo-v2.png` (512×512, from `icon-1024.png`; `twitter:card=summary`). Regenerate it when the icon changes.
- Commit + push both repos in the same session. `scripts/notarize.sh` warns if the site lacks the new version.

## Release checklist (`scripts/notarize.sh --upload vX.Y.Z`)
- Before running: update `../girocrab.de` + icon/buttons/screenshots in lockstep (see above); bump `version` in `Cargo.toml`, and in `README.md` rename `### ✨ Neu in v…` / `### ✨ What's new in v…` to the new version with updated highlights (DE + EN).
- The script aborts if `Cargo.toml` differs from the tag (override: `ALLOW_VERSION_MISMATCH=1`) or if those README headings lack the tag version.
- It sets `MARKETING_VERSION` in `project.pbxproj` and `Aktuelle Version: **v…**` / `Current version: **v…**` in `README.md` (stable tags only), then verifies both. Commit those changes afterwards.

