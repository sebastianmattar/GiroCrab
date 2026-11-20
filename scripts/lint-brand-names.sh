#!/usr/bin/env bash
# User-visible texts name no brands of other banking/finance software and contain no
# comparisons with other products (see CLAUDE.md › "User-visible texts").
# Checked: README.md, CONTRIBUTING.md, CLAUDE.md, docs/, the String Catalogs, the Linux English catalog
# (every qsTr text of the Linux app has an entry there or in the String Catalog) and the Windows Slint UI.
# Not checked: source code (internal identifiers such as module, function or test names).
# Names of banks, brokers and payment services we connect to are fine.
#
#   scripts/lint-brand-names.sh      exits 1 and lists offending lines
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

brands='money ?money|outbank|banking4|\bbank ?x\b|pecunia|starmoney|hibiscus|finanzguru|moneywiz|ifinance|quicken|subsembly'
hits=$(grep -rniEI "$brands" README.md CONTRIBUTING.md CLAUDE.md docs GiroCrabApp/Xcode/App/*.xcstrings \
    GiroCrabLinux/i18n GiroCrabWindows/ui 2>/dev/null \
    | grep -v '^docs/promo/' || true)
# Internal source file names in developer docs (rule: identifiers are not renamed).
hits=$(printf '%s\n' "$hits" | grep -viE 'moneymoney_(db|import)\.rs|moneymoney-db:|^docs/moneymoney-umsteiger-und-faq\.md:' | grep -v '^$' || true)

if [[ -n "$hits" ]]; then
    echo "Third-party software brand in a user-visible text; describe the feature neutrally (CLAUDE.md › User-visible texts):" >&2
    echo "$hits" >&2
    exit 1
fi
echo "lint-brand-names: ok"
