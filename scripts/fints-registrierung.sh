#!/usr/bin/env bash
# GiroCrab's own FinTS product registration (Deutsche Kreditwirtschaft) for
# Beta/Stable/Nightly builds. The value never lives in the repository: it is
# read from the release Mac's keychain (service "GiroCrab FinTS Product ID")
# or from the environment variable GIROCRAB_FINTS_PRODUCT_ID and compiled in
# by girocrab-core/build.rs (masked, see docs/fints-registrierung.md).
#
#   scripts/fints-registrierung.sh set            store it in the keychain (prompts, input hidden)
#   scripts/fints-registrierung.sh get            print it (for notarize.sh; never run it in a shared log)
#   scripts/fints-registrierung.sh status         "available"/"missing" – never the value
#   scripts/fints-registrierung.sh check-app <GiroCrab.app>
#                                              the ID is really inside the built binary
#                                              (masked) and not readable as plain text
#   scripts/fints-registrierung.sh scan           no ID-looking value committed in the repo
#
# The value is read via environment/keychain only, never passed as an argument,
# and this script never prints it except for `get`.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVICE="${FINTS_PRODUCT_ID_KEYCHAIN_SERVICE:-GiroCrab FinTS Product ID}"
ACCOUNT="release"
die() { echo "error: $*" >&2; exit 1; }

value() {
  if [ -n "${GIROCRAB_FINTS_PRODUCT_ID:-}" ]; then
    printf '%s' "$GIROCRAB_FINTS_PRODUCT_ID"
  else
    security find-generic-password -s "$SERVICE" -a "$ACCOUNT" -w 2>/dev/null || true
  fi
}

case "${1:-}" in
  set)
    printf 'FinTS product ID (input hidden): ' >&2
    IFS= read -rs v; echo >&2
    v="$(printf '%s' "$v" | tr -d '[:space:]')"
    [ -n "$v" ] || die "empty value"
    [ "${#v}" -eq 25 ] || die "the product ID must be exactly 25 characters (got ${#v})"
    security add-generic-password -U -s "$SERVICE" -a "$ACCOUNT" -w "$v" >/dev/null
    echo "stored in keychain service '$SERVICE'"
    ;;
  get)
    v="$(value)"; [ -n "$v" ] || exit 1
    printf '%s' "$v"
    ;;
  status)
    if [ -n "$(value)" ]; then echo "available"; else echo "missing"; exit 1; fi
    ;;
  check-app)
    app="${2:-}"; [ -d "$app" ] || die "check-app: give the path to GiroCrab.app"
    bin="$app/Contents/MacOS/GiroCrabMac"
    if [ ! -f "$bin" ] && [ -f "$app/Info.plist" ]; then
      exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Info.plist" 2>/dev/null || true)"
      [ -n "$exe" ] && [ -f "$app/$exe" ] && bin="$app/$exe"
    fi
    [ -f "$bin" ] || die "no binary at $bin"
    v="$(value)"; [ -n "$v" ] || die "no product ID to check against"
    # The value goes in through the environment, not argv (ps).
    GIROCRAB_FINTS_PRODUCT_ID="$v" python3 -I - "$bin" <<'PY'
import os, sys
v = os.environ["GIROCRAB_FINTS_PRODUCT_ID"].strip().encode()
mask = b"girocrab-fints-product-id-mask"   # keep in sync with girocrab-core/build.rs
masked = bytes(b ^ mask[i % len(mask)] ^ (i & 0xFF) for i, b in enumerate(v))
data = open(sys.argv[1], "rb").read()
if masked not in data:
    sys.exit("error: FinTS product ID is NOT in the build (masked form missing)")
if v in data:
    sys.exit("error: FinTS product ID is readable as plain text in the binary")
print("ok: FinTS product ID embedded (masked), not readable as plain text")
PY
    ;;
  scan)
    # An assignment of a long hex-looking value to the variable anywhere in the repo.
    if git grep -nIE "GIROCRAB_FINTS_PRODUCT_ID[\"']?[[:space:]]*[=:][[:space:]]*[\"']?[0-9A-Fa-f]{16,}" -- . ':!scripts/fints-registrierung.sh' >/dev/null; then
      die "something that looks like a FinTS product ID is committed (file names above are hidden on purpose): git grep GIROCRAB_FINTS_PRODUCT_ID"
    fi
    echo "ok: no committed product ID found"
    ;;
  *)
    sed -n '2,17p' "$0"; exit 2 ;;
esac
