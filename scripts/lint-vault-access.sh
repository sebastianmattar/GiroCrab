#!/usr/bin/env bash
# Views must not reach into the vault (GitHub #262). Every core call is synchronous
# and holds the vault mutex; called from a view it runs on the main actor and
# freezes the UI on large vaults. Views go through AppModel instead:
#   - reads/writes that may take > 16 ms: `model.vaultOffMain { try $0.… }`
#     (nil when locked or the vault changed) or `requireVaultOffMain` (throws),
#   - jobs of several calls (FNZ import): `model.pinVault()` → `pin.run { … }`,
#   - "is a vault open?": `model.isUnlocked`.
# `vault` itself is used only inside AppModel.swift and its AppModel+*.swift extensions
# (files that contain only `extension AppModel` blocks).
#
#   scripts/lint-vault-access.sh      exits 1 and lists offending lines
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Exempt: AppModel.swift, which owns the vault and VaultPin, and AppModel+<Feature>.swift
# files that hold nothing at top level but imports and `extension AppModel {` blocks.
# A file with any other top-level declaration (a view, a helper type) is checked like
# every other file, whatever its name.
UI=GiroCrabApp/Sources/GiroCrabUI
exempt=("$UI/AppModel.swift")
for f in "$UI"/AppModel+*.swift; do
    [[ -e "$f" ]] || continue
    if ! grep -E '^[^[:space:]/}]' "$f" | grep -qvE '^(import [A-Za-z]+|extension AppModel \{)$'; then
        exempt+=("$f")
    fi
done
exempt_re=$(printf '%s\n' "${exempt[@]}" | sed 's/[+.]/\\&/g' | paste -sd'|' -)

# Any `<expr>.vault` (model.vault, self.vault in a view, …) outside the exempt files.
# Not flagged: comment lines, identifiers that merely start with "vault" (vaultState, …)
# and `.vault"` closing a string ("de.girocrab.vault").
hits=$(grep -rnE '\.vault([^A-Za-z0-9_"]|$)' GiroCrabApp/Sources --include='*.swift' \
    | grep -vE "^($exempt_re):" \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)

if [[ -n "$hits" ]]; then
    echo "Direkter Vault-Zugriff außerhalb von AppModel (siehe scripts/lint-vault-access.sh):" >&2
    echo "$hits" >&2
    exit 1
fi
echo "lint-vault-access: ok"
