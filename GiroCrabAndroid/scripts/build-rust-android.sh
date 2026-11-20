#!/usr/bin/env bash
# Cross-compiles girocrab-core for Android (cargo-ndk) and generates the UniFFI Kotlin bindings.
#
#   GiroCrabAndroid/app/src/main/jniLibs/<abi>/libgirocrab_core.so
#   GiroCrabAndroid/app/src/main/generated/uniffi/girocrab_core/girocrab_core.kt
#
# Both paths are what GiroCrabAndroid/app/build.gradle.kts reads (sourceSets["main"]).
#
# The Kotlin bindings are generated from a *debug host* build of the same crate.
# UniFFI reads its metadata from the ELF symbol table; profile.release sets
# strip = "symbols", so generating from the release library (host or Android)
# fails with "No UniFFI metadata found". The metadata is source-derived, so the
# debug host build produces exactly the bindings that belong to the release .so.
#
# Requires: rustup targets (added automatically), `cargo install cargo-ndk`, and
# an NDK at ANDROID_NDK_HOME / ANDROID_NDK_ROOT / $ANDROID_SDK_ROOT/ndk/<newest>.
#
# Usage:
#   GiroCrabAndroid/scripts/build-rust-android.sh                   # three ABIs, release
#   GiroCrabAndroid/scripts/build-rust-android.sh --debug           # unoptimised
#   GiroCrabAndroid/scripts/build-rust-android.sh --abi arm64-v8a   # repeatable
#   GiroCrabAndroid/scripts/build-rust-android.sh --bindings-only   # only regenerate Kotlin
#   GiroCrabAndroid/scripts/build-rust-android.sh --check           # only verify the outputs
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="$ROOT/GiroCrabAndroid/app/src/main"
JNI_DIR="$APP/jniLibs"
GEN_DIR="$APP/generated"
CRATE=girocrab-core
LIB=girocrab_core
TARGET_DIR="${GIROCRAB_ANDROID_TARGET_DIR:-$ROOT/target}/android"
HOST_DIR="${GIROCRAB_ANDROID_TARGET_DIR:-$ROOT/target}/host"
UNIFFI_BINDGEN="${UNIFFI_BINDGEN:-}"

ALL_ABIS=(arm64-v8a armeabi-v7a x86_64)
PROFILE=release
BUILD_FLAGS=()
ABIS=()
BINDINGS_ONLY=0
CHECK_ONLY=0

while (( $# )); do
  case "$1" in
    --debug) PROFILE=debug ;;
    --abi)
      [[ $# -ge 2 ]] || { echo "--abi needs a value" >&2; exit 2; }
      ABIS+=("$2"); shift ;;
    --bindings-only) BINDINGS_ONLY=1 ;;
    --check) CHECK_ONLY=1 ;;
    -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

[[ "$PROFILE" == release ]] && BUILD_FLAGS+=(--release)
(( ${#ABIS[@]} )) || ABIS=("${ALL_ABIS[@]}")

rust_target_for_abi() {
  case "$1" in
    arm64-v8a) echo aarch64-linux-android ;;
    armeabi-v7a) echo armv7-linux-androideabi ;;
    x86_64) echo x86_64-linux-android ;;
    *) echo "unknown ABI: $1" >&2; return 2 ;;
  esac
}

so_path() { echo "$JNI_DIR/$1/lib$LIB.so"; }
kt_file() { echo "$GEN_DIR/uniffi/$LIB/$LIB.kt"; }

# Verifies the outputs of the ABIs this invocation asked for. A --bindings-only
# run skips the native builds, so it must not require their files.
check_outputs() {
  local rc=0 abi so kt
  if (( ! BINDINGS_ONLY )); then
    for abi in "${ABIS[@]}"; do
      so="$(so_path "$abi")"
      if [[ -f "$so" ]]; then
        printf 'ok      %-12s %s\n' "$abi" "$(du -h "$so" | cut -f1)"
      else
        printf 'missing %-12s %s\n' "$abi" "$so"; rc=1
      fi
    done
  fi
  kt="$(kt_file)"
  if [[ -f "$kt" ]]; then
    printf 'ok      bindings     %s\n' "$kt"
  else
    printf 'missing bindings     %s\n' "$kt"; rc=1
  fi
  return $rc
}

if (( CHECK_ONLY )); then
  check_outputs || { echo "android-rust: run GiroCrabAndroid/scripts/build-rust-android.sh" >&2; exit 1; }
  echo "android-rust: ready"
  exit 0
fi

# --- NDK ---------------------------------------------------------------------
if [[ -z "${ANDROID_NDK_HOME:-}" && -z "${ANDROID_NDK_ROOT:-}" ]]; then
  for sdk in "${ANDROID_SDK_ROOT:-}" "${ANDROID_HOME:-}"; do
    [[ -n "$sdk" && -d "$sdk/ndk" ]] || continue
    export ANDROID_NDK_HOME="$sdk/ndk/$(ls -1 "$sdk/ndk" | sort -V | tail -1)"
    break
  done
fi
NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
if [[ -z "$NDK" || ! -d "$NDK" ]]; then
  echo "error: Android NDK not found — set ANDROID_NDK_HOME" >&2
  exit 1
fi
export ANDROID_NDK_HOME="$NDK"

command -v cargo-ndk >/dev/null || { echo "cargo-ndk missing: cargo install cargo-ndk" >&2; exit 1; }
echo "==> NDK: $NDK"

# --- Rust targets ------------------------------------------------------------
if command -v rustup >/dev/null 2>&1; then
  installed="$(rustup target list --installed)"
  missing=()
  for abi in "${ABIS[@]}"; do
    t="$(rust_target_for_abi "$abi")"
    grep -Fxq "$t" <<<"$installed" || missing+=("$t")
  done
  (( ${#missing[@]} )) && rustup target add "${missing[@]}"
fi

cd "$ROOT"
# The root build.rs would build the xcframework/Swift app; not needed here.
export GIROCRAB_SKIP_SWIFT=1

if (( ! BINDINGS_ONLY )); then
  # Wipe first: a library left over from an earlier all-ABI run would otherwise
  # be packaged next to freshly generated bindings and fail UniFFI's API check.
  rm -rf "$JNI_DIR"
  mkdir -p "$JNI_DIR"
  for abi in "${ABIS[@]}"; do
    echo "==> Building $CRATE ($PROFILE) for $abi"
    cargo ndk -t "$abi" -P 26 -o "$JNI_DIR" build -p "$CRATE" --lib \
      "${BUILD_FLAGS[@]}" --target-dir "$TARGET_DIR"
    [[ -f "$(so_path "$abi")" ]] || { echo "error: $abi produced no $(so_path "$abi")" >&2; exit 1; }
  done
fi

# --- Kotlin bindings ---------------------------------------------------------
echo "==> Building $CRATE and uniffi-bindgen for the host"
cargo build -p "$CRATE" --lib --target-dir "$HOST_DIR"
cargo build -p uniffi-bindgen --bin uniffi-bindgen --release --target-dir "$HOST_DIR"

case "$(uname -s)" in
  Darwin) HOST_SUFFIX=dylib ;;
  *) HOST_SUFFIX=so ;;
esac
HOST_LIB="$HOST_DIR/debug/lib$LIB.$HOST_SUFFIX"
[[ -f "$HOST_LIB" ]] || { echo "error: host build produced no lib$LIB.$HOST_SUFFIX" >&2; exit 1; }

if [[ -z "$UNIFFI_BINDGEN" ]]; then
  UNIFFI_BINDGEN="$HOST_DIR/release/uniffi-bindgen"
fi
# A stale file from an older uniffi version must not survive in Gradle's source set.
rm -rf "$GEN_DIR/uniffi"
mkdir -p "$GEN_DIR"
echo "==> Generating Kotlin bindings"
$UNIFFI_BINDGEN generate --language kotlin --no-format --out-dir "$GEN_DIR" "$HOST_LIB"

echo
check_outputs
