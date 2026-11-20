#!/usr/bin/env bash
# One command for the checks CI runs on a change, picked by the files the branch touches.
# Prints a table of every check with its result, for the PR description.
#
#   scripts/verify-fast.sh              lints + compile checks for the changed surface
#   scripts/verify-fast.sh --lint-only  lints only (seconds; used by .githooks/pre-push)
#   scripts/verify-fast.sh --tests      also the unit tests of the changed surface
#   scripts/verify-fast.sh --all        treat every surface as changed
#   VERIFY_BASE=<ref>                   compare with <ref> instead of upstream/main or origin/main
#
# Changed files = commits since the merge base + staged + unstaged + untracked files.
# A check this machine cannot run (Swift on Linux, Qt) is listed as "skipped", never as passed.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

mode=compile; all=false
for a in "$@"; do
  case "$a" in
    --lint-only) mode=lint ;;
    --tests) mode=tests ;;
    --all) all=true ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "verify-fast: unknown option $a" >&2; exit 2 ;;
  esac
done

base="${VERIFY_BASE:-}"
if [ -z "$base" ]; then
  for r in upstream/main origin/main main; do
    git rev-parse -q --verify "$r" >/dev/null && { base=$r; break; }
  done
fi
files=""
if [ "$all" = false ] && [ -n "$base" ] && mb=$(git merge-base "$base" HEAD 2>/dev/null); then
  files=$( { git diff --name-only "$mb"; git ls-files --others --exclude-standard; } | sort -u)
else
  all=true
fi
touched() { [ "$all" = true ] || grep -qE "$1" <<<"$files"; }
# touched_except <dir ERE> <file ERE>: a file under <dir> changed that does not match <file ERE>
touched_except() { [ "$all" = true ] || grep -E "$1" <<<"$files" | grep -qvE "$2"; }
core_touched() { touched_except '^girocrab-core/' '\.md$'; }

rust=false; linux=false; swift=false
# Like the `rust` flag in ci.yml, except that a Python-only change in tools/ needs no clippy run.
core_touched || touched_except '^tools/' '\.py$' \
  || touched '^(girocrab-mcp/|uniffi-bindgen/|src/|build\.rs$|Cargo\.(toml|lock)$|\.cargo/)' && rust=true
core_touched || touched '^GiroCrabLinux/|Localizable\.xcstrings$' && linux=true
core_touched || touched '^GiroCrabApp/' && swift=true

results=(); failed=0
run() { # run <label> <command...>
  local label=$1; shift
  local start=$SECONDS log
  log=$(mktemp)
  if "$@" >"$log" 2>&1; then
    results+=("ok|$label|$((SECONDS - start))s")
  else
    results+=("FAIL|$label|$((SECONDS - start))s"); failed=1
    echo "── $label failed: $*" >&2; tail -n 30 "$log" >&2
  fi
  rm -f "$log"
}
skip() { results+=("skipped|$1|$2"); }

# Lints: every one CI runs in the `rust` job's cheap steps (all together a few seconds).
run "cargo fmt --check" cargo fmt --all -- --check
run "FinTS product ID scan" scripts/fints-registrierung.sh scan
run "third-party notices" scripts/check-third-party-notices.sh
for l in scripts/lint-*.sh; do run "$(basename "$l" .sh)" "$l"; done
run "xcstrings sorted" python3 -I scripts/xcstrings.py check
run "Linux UI texts have English" python3 -I GiroCrabLinux/scripts/i18n-report.py
if touched '^(scripts|tools)/.*\.py$'; then
  run "python tests (scripts, tools)" python3 -m unittest scripts/test_xcstrings.py scripts/test_sync_docs.py tools/test_export_ai_provenance.py
fi
if touched '^scripts/test-.*\.cjs$|portal'; then
  if command -v node >/dev/null; then
    run "portal script tests" node --test scripts/test-fnz-portal.cjs scripts/test-quirion-portal.cjs scripts/test-schwaebisch-hall-portal.cjs scripts/test-advanzia-portal.cjs
  else skip "portal script tests" "node missing"; fi
fi
if [ "$linux" = true ]; then
  run "GiroCrabLinux cargo fmt --check" bash -c 'cd GiroCrabLinux && cargo fmt -- --check'
fi
if [ "$mode" != lint ]; then
  if [ "$rust" = true ]; then
    run "cargo clippy --workspace -D warnings" cargo clippy -q --workspace --all-targets -- -D warnings
    run "core builds with --features serde" env GIROCRAB_SKIP_SWIFT=1 cargo check -q -p girocrab-core --features serde
    run "core library: sources + data tables only" env GIROCRAB_SKIP_SWIFT=1 scripts/lint-no-obfuscation.sh --compiled
    run "core library (serde): sources + data only" env GIROCRAB_SKIP_SWIFT=1 scripts/lint-no-obfuscation.sh --compiled --features serde
    [ "$mode" = tests ] && run "cargo test girocrab-core --lib" env GIROCRAB_SKIP_SWIFT=1 cargo test -q -p girocrab-core --lib
  fi
  [ "$linux" = true ] && skip "GiroCrabLinux clippy/test" "needs Qt: left to CI"
  if [ "$swift" = true ]; then
    if [ "$(uname)" = Darwin ]; then
      run "bindings match core (build-xcframework --check)" ./build-xcframework.sh --check
      run "swift build" bash -c 'cd GiroCrabApp && swift build -q'
      [ "$mode" = tests ] && run "swift tests (parallel + serial-defaults)" scripts/swift-test-split.sh -q
    else
      skip "swift build / tests" "not macOS: left to CI"
    fi
  fi
fi

echo
echo "verify-fast against ${base:-<no base>}$([ "$all" = true ] && echo ' (all surfaces)'): rust=$rust linux=$linux swift=$swift mode=$mode"
printf '| %-7s | %-48s | %-22s |\n' result check "time / reason"
printf '|---------|--------------------------------------------------|------------------------|\n'
for r in "${results[@]}"; do
  IFS='|' read -r s l t <<<"$r"
  printf '| %-7s | %-48s | %-22s |\n' "$s" "$l" "$t"
done
exit $failed
