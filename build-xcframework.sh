#!/usr/bin/env bash
# Builds girocrab-core for Apple platforms, generates the UniFFI Swift
# bindings and bundles everything as a local Swift package:
#
#   generated/GiroCrabCorePackage/
#     Package.swift
#     girocrab_coreFFI.xcframework   (static libs + C header + modulemap)
#     Sources/GiroCrabCore/girocrab_core.swift
#
# Usage:
#   ./build-xcframework.sh               # macOS (arm64) + iOS device + iOS simulator
#   ./build-xcframework.sh --macos-only  # quick local build (used by `cargo build`)
#   ./build-xcframework.sh --ios         # force iOS device + simulator slices (also with --macos-only)
#   ./build-xcframework.sh --no-sim      # macOS + iOS device only (skips aarch64-apple-ios-sim;
#                                        # enough for archive/TestFlight, not for the iOS simulator)
#   ./build-xcframework.sh --debug       # unoptimised libraries
#   ./build-xcframework.sh --check       # only compare generated/.stamp with the
#                                        # current core sources; exit 1 if stale
#       --check --need-ios               # also require an iOS device slice
#       --check --strict-env             # also require profile=release and the same
#                                        # GIROCRAB_FINTS_PRODUCT_ID as at build time
#                                        # (used by notarize.sh to skip redundant rebuilds)
#
# Every build writes generated/.stamp with a fingerprint of everything that
# goes into the library and the bindings (girocrab-core sources, uniffi-bindgen,
# Cargo.lock, this script). The git hooks in .githooks run --check after a
# checkout, merge or rebase, so stale bindings show up before `swift build`
# fails with misleading errors.
#
# Requirements: Xcode 15+, rustup with the targets
#   aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
# Toolchain (rustup's cargo/rustc), Mac target and deployment target; shared
# with package-macos-app.sh, which builds the MCP helper the same way.
source "$ROOT/scripts/rust-toolchain.sh"

MACOS_ONLY=0
FORCE_IOS=0
CHECK_ONLY=0
NO_SIM=0
NEED_IOS=0
STRICT_ENV=0
PROFILE=release
for arg in "$@"; do
  case "$arg" in
    --macos-only) MACOS_ONLY=1 ;;
    --ios) FORCE_IOS=1 ;;
    --debug) PROFILE=debug ;;
    --check) CHECK_ONLY=1 ;;
    --no-sim) NO_SIM=1 ;;
    --need-ios) NEED_IOS=1 ;;
    --strict-env) STRICT_ENV=1 ;;
    -h|--help) sed -n '2,31p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

STAMP="$ROOT/generated/.stamp"
# Inputs of the static library and the generated bindings (Cargo.lock is
# hashed separately: cargo may rewrite it during the build).
FINGERPRINT_INPUTS=(girocrab-core/src girocrab-core/Cargo.toml girocrab-core/build.rs
  uniffi-bindgen Cargo.toml build-xcframework.sh)

# sha256 over "content-hash  path" of every input file in the working tree, so
# uncommitted edits count too. Untracked files only if they are not ignored and
# are .rs/.toml (a stray vault.rs.orig must not mark the bindings stale).
sources_fingerprint() {
  local f files=()
  if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    while IFS= read -r -d '' f; do
      if [[ -f "$ROOT/$f" ]]; then files+=("$f"); fi
    done < <(git -C "$ROOT" ls-files -z -c -- "${FINGERPRINT_INPUTS[@]}")
    while IFS= read -r -d '' f; do
      if [[ ( "$f" == *.rs || "$f" == *.toml ) && -f "$ROOT/$f" ]]; then files+=("$f"); fi
    done < <(git -C "$ROOT" ls-files -z -o --exclude-standard -- "${FINGERPRINT_INPUTS[@]}")
  else
    while IFS= read -r -d '' f; do files+=("$f"); done \
      < <(cd "$ROOT" && find "${FINGERPRINT_INPUTS[@]}" -type f -print0 2>/dev/null)
  fi
  (( ${#files[@]} )) || { echo none; return; }
  (cd "$ROOT" && printf '%s\0' "${files[@]}" | LC_ALL=C sort -zu | xargs -0 shasum -a 256) \
    | shasum -a 256 | cut -d' ' -f1
}
lock_fingerprint() { shasum -a 256 < "$ROOT/Cargo.lock" 2>/dev/null | cut -d' ' -f1; }
combine_fingerprint() { printf '%s\n%s\n' "$1" "$2" | shasum -a 256 | cut -d' ' -f1; }

# Hash (never the value) of the FinTS product ID compiled in by build.rs, so a
# library built without it is not mistaken for a release build.
env_fingerprint() {
  if [[ -n "${GIROCRAB_FINTS_PRODUCT_ID:-}" ]]; then
    printf '%s' "$GIROCRAB_FINTS_PRODUCT_ID" | shasum -a 256 | cut -d' ' -f1
  else
    echo none
  fi
}

stamp_value() { sed -n "s/^$1=//p" "$STAMP" 2>/dev/null | head -1; }

if [[ $CHECK_ONLY -eq 1 ]]; then
  if [[ ! -d "$ROOT/generated/GiroCrabCorePackage" ]]; then
    echo "warning: generated/GiroCrabCorePackage is missing – run ./build-xcframework.sh --macos-only" >&2
    exit 1
  fi
  hint="./build-xcframework.sh --macos-only"
  [[ "$(stamp_value slices)" == *ios* ]] && hint="./build-xcframework.sh"
  if [[ ! -f "$STAMP" ]]; then
    echo "warning: generated/.stamp is missing (last build failed, or predates stamps) – run $hint" >&2
    exit 1
  fi
  if [[ "$(stamp_value fingerprint)" != "$(combine_fingerprint "$(sources_fingerprint)" "$(lock_fingerprint)")" ]]; then
    echo "warning: the xcframework / UniFFI bindings in generated/ are stale" >&2
    echo "         (built from $(stamp_value commit); girocrab-core, uniffi-bindgen or Cargo.* changed since)." >&2
    echo "         swift build may fail with misleading errors – rebuild: $hint" >&2
    exit 1
  fi
  # Strict checks (notarize.sh) only; plain --check stays as the hooks know it.
  if [[ $NEED_IOS -eq 1 && "$(stamp_value slices)" != *ios* ]]; then
    echo "note: xcframework has no iOS device slice" >&2
    exit 1
  fi
  if [[ $STRICT_ENV -eq 1 ]]; then
    if [[ "$(stamp_value profile)" != "release" ]]; then
      echo "note: xcframework was not built with the release profile" >&2
      exit 1
    fi
    if [[ "$(stamp_value env)" != "$(env_fingerprint)" ]]; then
      echo "note: xcframework was built with a different GIROCRAB_FINTS_PRODUCT_ID" >&2
      exit 1
    fi
  fi
  exit 0
fi

# Sources as they are when the build starts: an edit made while it runs leaves
# the stamp stale instead of hiding the change. No stamp until the build
# succeeds, so a failed or interrupted build never looks up to date.
SOURCES_FP="$(sources_fingerprint)"
BUILT_FROM="$(git -C "$ROOT" describe --always --dirty 2>/dev/null || echo unknown)"
rm -f "$STAMP"

CRATE=girocrab_core
FFI_MODULE=girocrab_coreFFI
TARGET_DIR="${GIROCRAB_TARGET_DIR:-$ROOT/target/apple}"
OUT="$ROOT/generated/GiroCrabCorePackage"
BUILD_FLAGS=()
[[ "$PROFILE" == "release" ]] && BUILD_FLAGS+=(--release)

export IPHONEOS_DEPLOYMENT_TARGET=17.0

TARGETS=($MAC_TARGET)
if [[ $MACOS_ONLY -eq 0 || $FORCE_IOS -eq 1 ]]; then
  TARGETS+=(aarch64-apple-ios)
  [[ $NO_SIM -eq 1 ]] || TARGETS+=(aarch64-apple-ios-sim)
fi

# OpenSSL's iOS cross configs call `makedepend` (only to write dependency
# files). macOS doesn't ship it, and the XQuartz copy fails on the iOS SDK.
# A no-op stand-in placed first in PATH is enough for a clean build.
# (Kept in a temp dir: OpenSSL's Makefile can't handle spaces in the path.)
SHIM_DIR="$(mktemp -d "${TMPDIR:-/tmp}/girocrab-shim.XXXXXX")"
printf '#!/bin/sh\nexit 0\n' > "$SHIM_DIR/makedepend"
chmod +x "$SHIM_DIR/makedepend"
export PATH="$SHIM_DIR:$PATH"
trap 'rm -rf "$SHIM_DIR"' EXIT

if command -v rustup >/dev/null 2>&1; then
  # Already installed targets need no network access (including offline builds).
  installed_targets="$(rustup target list --installed)"
  missing_targets=()
  for t in "${TARGETS[@]}"; do
    if ! printf '%s\n' "$installed_targets" | /usr/bin/grep -Fxq "$t"; then
      missing_targets+=("$t")
    fi
  done
  if (( ${#missing_targets[@]} )); then
    rustup target add "${missing_targets[@]}"
  fi
fi

echo "==> Building $CRATE ($PROFILE) for: ${TARGETS[*]}"
for t in "${TARGETS[@]}"; do
  # ${arr[@]+"${arr[@]}"}: an empty array (--debug) is "unbound" for bash 3.2 under set -u.
  "$CARGO" build -p girocrab-core --lib ${BUILD_FLAGS[@]+"${BUILD_FLAGS[@]}"} --target "$t" --target-dir "$TARGET_DIR"
done

echo "==> Building uniffi-bindgen-swift"
"$CARGO" build -p uniffi-bindgen --bin uniffi-bindgen-swift --release --target-dir "$TARGET_DIR"
BINDGEN="$TARGET_DIR/release/uniffi-bindgen-swift"
FINGERPRINT="$(combine_fingerprint "$SOURCES_FP" "$(lock_fingerprint)")"

MAC_LIB="$TARGET_DIR/$MAC_TARGET/$PROFILE/lib$CRATE.a"
GEN="$TARGET_DIR/swift-gen"
rm -rf "$GEN"
mkdir -p "$GEN/Headers" "$GEN/Sources"

echo "==> Generating Swift bindings (library mode)"
"$BINDGEN" "$MAC_LIB" "$GEN/Sources" --swift-sources
"$BINDGEN" "$MAC_LIB" "$GEN/Headers" --headers
"$BINDGEN" "$MAC_LIB" "$GEN/Headers" --modulemap \
  --module-name "$FFI_MODULE" --modulemap-filename module.modulemap

echo "==> Creating XCFramework"
# Assembled here, then synced into $OUT (see the rsync below).
STAGE="$TARGET_DIR/GiroCrabCorePackage.staging"
rm -rf "$STAGE"
mkdir -p "$STAGE/Sources/GiroCrabCore"
XCF_ARGS=(-library "$MAC_LIB" -headers "$GEN/Headers")
IOS_LIB="$TARGET_DIR/aarch64-apple-ios/$PROFILE/lib$CRATE.a"
SIM_LIB="$TARGET_DIR/aarch64-apple-ios-sim/$PROFILE/lib$CRATE.a"
INCLUDE_IOS=$(( MACOS_ONLY == 0 || FORCE_IOS == 1 ))
INCLUDE_SIM=$(( INCLUDE_IOS == 1 && NO_SIM == 0 ))
# Fingerprints of the sources the iOS slices in $TARGET_DIR were built from
# (device + simulator, resp. device only).
IOS_FP="$(combine_fingerprint "$FINGERPRINT" "$(env_fingerprint)")"
IOS_STAMP="$TARGET_DIR/ios-slices-$PROFILE.fingerprint"
IOS_DEVICE_STAMP="$TARGET_DIR/ios-device-$PROFILE.fingerprint"
if [[ $INCLUDE_SIM -eq 1 ]]; then
  echo "$IOS_FP" > "$IOS_STAMP"
  echo "$IOS_FP" > "$IOS_DEVICE_STAMP"
elif [[ $INCLUDE_IOS -eq 1 ]]; then
  echo "$IOS_FP" > "$IOS_DEVICE_STAMP"
elif [[ -f "$IOS_LIB" && -f "$SIM_LIB" && "$(cat "$IOS_STAMP" 2>/dev/null)" == "$IOS_FP" ]]; then
  # Keep iOS slices from an earlier full build only when sources AND GIROCRAB_FINTS_PRODUCT_ID match.
  INCLUDE_IOS=1; INCLUDE_SIM=1
  echo "    (keeping up-to-date iOS slices from the last full build)"
elif [[ -f "$IOS_LIB" && "$(cat "$IOS_DEVICE_STAMP" 2>/dev/null)" == "$IOS_FP" ]]; then
  INCLUDE_IOS=1
  echo "    (keeping up-to-date iOS device slice from the last build)"
fi
if [[ $INCLUDE_IOS -eq 1 ]]; then
  XCF_ARGS+=(-library "$IOS_LIB" -headers "$GEN/Headers")
fi
if [[ $INCLUDE_SIM -eq 1 ]]; then
  XCF_ARGS+=(-library "$SIM_LIB" -headers "$GEN/Headers")
fi
xcodebuild -create-xcframework "${XCF_ARGS[@]}" -output "$STAGE/$FFI_MODULE.xcframework" >/dev/null

cp "$GEN/Sources/"*.swift "$STAGE/Sources/GiroCrabCore/"

cat > "$STAGE/Package.swift" <<'SWIFT'
// swift-tools-version:5.10
// GENERATED by build-xcframework.sh – do not edit.
import PackageDescription

let package = Package(
    name: "GiroCrabCorePackage",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [
        .library(name: "GiroCrabCore", targets: ["GiroCrabCore"]),
    ],
    targets: [
        .binaryTarget(name: "girocrab_coreFFI", path: "girocrab_coreFFI.xcframework"),
        .target(
            name: "GiroCrabCore",
            dependencies: ["girocrab_coreFFI"],
            path: "Sources/GiroCrabCore",
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
            ]
        ),
    ]
)
SWIFT

# Copy only files whose content changed and delete those the new package
# lacks. SwiftPM and clang decide what to recompile by modification time:
# rewriting identical bindings or headers would recompile GiroCrabCore and all
# of GiroCrabUI. No -t/-a: rsync would stamp unchanged files with the staging
# time.
rsync -rl --checksum --delete "$STAGE/" "$OUT/"
rm -rf "$STAGE"

SLICES=macos
[[ $INCLUDE_IOS -eq 1 ]] && SLICES=macos,ios-device
[[ $INCLUDE_SIM -eq 1 ]] && SLICES=macos,ios
{
  echo "# Written by build-xcframework.sh; checked by ./build-xcframework.sh --check"
  echo "fingerprint=$FINGERPRINT"
  echo "commit=$BUILT_FROM"
  echo "slices=$SLICES"
  echo "profile=$PROFILE"
  echo "env=$(env_fingerprint)"
} > "$STAMP"

echo "==> Done: $OUT"
