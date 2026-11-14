#!/usr/bin/env bash
# Text styles go through `.appFont(…)` (GitHub #343). macOS has no Dynamic Type,
# so the app scales its text styles itself (Einstellungen › Allgemein ›
# Schriftgröße); a plain `.font(.caption)` would stay at the system size.
#   .font(.caption.weight(.semibold))  →  .appFont(.caption, weight: .semibold)
#   .font(.body.monospacedDigit())     →  .appFont(.body, monospacedDigit: true)
# Fixed sizes (`.font(.system(size:))`) are left alone: icons and signets.
#
#   scripts/lint-fonts.sh      exits 1 and lists offending lines
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

styles='largeTitle|title|title2|title3|headline|subheadline|body|callout|footnote|caption|caption2'
hits=$(grep -rnE "\.font\(\.($styles)\b|\bFont\.($styles)\b" GiroCrabApp/Sources --include='*.swift' \
    | grep -v '^GiroCrabApp/Sources/GiroCrabUI/TextSize\.swift:' \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)

if [[ -n "$hits" ]]; then
    echo "Text style without .appFont (does not follow the text size setting); use e.g. .appFont(.caption, weight: .semibold), see scripts/lint-fonts.sh:" >&2
    echo "$hits" >&2
    exit 1
fi
echo "lint-fonts: ok"
