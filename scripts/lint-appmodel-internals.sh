#!/usr/bin/env bash
# AppModel is split into AppModel.swift + AppModel+<Feature>.swift. Swift's `private`
# is file-scoped, so members the feature files share had to become internal. They are
# still AppModel's own plumbing: views must not call them or write to them.
#   - INTERNAL: state machines, staleness tokens (`listGeneration`, `syncGeneration`),
#     load helpers, `offMain` (views use `vaultOffMain` / `pinVault`, which check that the
#     vault is still current), `useVault` (the vault switches only via lock/unlock/demo).
#   - READ_ONLY: were `private(set)`; views may read them but only AppModel writes them
#     (their setters keep indexes, UserDefaults and reloads in step).
# Flags `model.X` / `sharedModel.X` / `appModel.X` / `AppModel.X` outside AppModel*.swift,
# and for READ_ONLY only writes (`= += .append(…) [k] =`) and `$model.X` bindings.
# A member that is no longer declared in AppModel*.swift fails the check, so the lists
# stay current: rename or remove it here as well.
#
#   scripts/lint-appmodel-internals.sh      exits 1 and lists offending lines
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

INTERNAL="
activeSession activeTransfer applyAccountGroups booking bookingIndexByID categoryRefreshTask
dashboardStale dashboardTask dropPendingPage endSelectionLoad failedSyncSecret finishSync
finishTransfer freshBooking handle handleTransfer historyRequest listGeneration loadBookings
loadDashboard loadMerchantStats loadMoreTask loadsSelectionInBackground logoCache logoInflight
merchantStatsCache merchantStatsKey merchantStatsTask mirrorRunning offMain periodDidChange
pollTask productIDAlternatives pruneMultiSelection publish quickLookCleanupObserver
rebuildAccountIndex rebuildBookingIndex recurringByKey reloadProductIDState reloadScopeBookings
reloadSelection resetSyncBatch rulesStale runCore scheduleAutoReview schedulePoll
scheduleSearch scopeLoadTask searchTask selectionAccountIDs selectionLoadTask
selectionPlaceholderTask settingsWriteTask settlePendingSelectionLoad startSelectionLoad
statementsCountBefore stickyBooking syncBatchSkipped syncBatchTotal syncGeneration
syncingConnectionIDBeforeError syncQueue trimmedHistory useVault
"
READ_ONLY="
accountMarkers accountsByID autoReviewTask customPeriodFrom customPeriodTo dayGroups
isLoadingMoreBookings isLoadingSelection
"

UI=GiroCrabApp/Sources/GiroCrabUI
model_files=("$UI"/AppModel.swift "$UI"/AppModel+*.swift)
fail=0

# Every listed member must still be declared on AppModel.
for name in $INTERNAL $READ_ONLY; do
    if ! grep -qE "^    ([@A-Za-z()]+ )*(var|let|func) $name([^A-Za-z0-9_]|$)" "${model_files[@]}"; then
        echo "lint-appmodel-internals: '$name' ist in AppModel*.swift nicht mehr deklariert – Liste in $0 anpassen." >&2
        fail=1
    fi
done

join() { echo $1 | tr ' ' '|'; }
internal_re=$(join "$INTERNAL")
read_only_re=$(join "$READ_ONLY")
recv='(model|sharedModel|appModel|AppModel)'
end='([^A-Za-z0-9_]|$)'

others() {
    grep -rnE "$1" GiroCrabApp/Sources --include='*.swift' \
        | grep -vE "^$UI/AppModel(\+[A-Za-z]+)?\.swift:" \
        | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true
}

hits=$(others "(^|[^A-Za-z0-9_])$recv\.($internal_re)$end")
writes=$(others "(^|[^A-Za-z0-9_])$recv\.($read_only_re)([[:space:]]*([-+*/]?=[^=]|\.(append|insert|remove[A-Za-z]*|merge|formUnion|subtract)\()|\[[^]]*\][[:space:]]*=[^=])|\\\$$recv\.($read_only_re)$end")

if [[ -n "$hits" ]]; then
    echo "AppModel-Interna außerhalb von AppModel*.swift (siehe scripts/lint-appmodel-internals.sh):" >&2
    echo "$hits" >&2
    fail=1
fi
if [[ -n "$writes" ]]; then
    echo "Schreibzugriff auf AppModel-Zustand, den nur AppModel setzt (siehe scripts/lint-appmodel-internals.sh):" >&2
    echo "$writes" >&2
    fail=1
fi
[[ $fail -eq 0 ]] || exit 1
echo "lint-appmodel-internals: ok"
