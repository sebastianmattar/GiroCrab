#!/usr/bin/env bash
# Cross-compiles girocrab-core for Android (cargo-ndk) and generates the UniFFI Kotlin bindings.
# Requires: rustup, `cargo install cargo-ndk`, ANDROID_NDK_HOME. The UniFFI bindgen is the
# workspace crate `uniffi-bindgen` (override with UNIFFI_BINDGEN="<command>").
# Usage: GiroCrabAndroid/scripts/build-rust-android.sh [--debug]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="$ROOT/GiroCrabAndroid/app/src/main"
JNI_LIBS="$APP/jniLibs"
GEN_DIR="$APP/generated"
PROFILE="release"
[[ "${1:-}" == "--debug" ]] && PROFILE="debug"
UNIFFI_BINDGEN="${UNIFFI_BINDGEN:-cargo run -q -p uniffi-bindgen --bin uniffi-bindgen --}"

command -v cargo-ndk >/dev/null || { echo "cargo-ndk missing: cargo install cargo-ndk" >&2; exit 1; }
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to your NDK directory}"

rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android >/dev/null

cd "$ROOT"
# The root build.rs would build the xcframework/Swift app; not needed here.
export GIROCRAB_SKIP_SWIFT=1

rm -rf "$JNI_LIBS" "$GEN_DIR"
mkdir -p "$JNI_LIBS" "$GEN_DIR"

PROFILE_FLAG=()
[[ "$PROFILE" == "release" ]] && PROFILE_FLAG=(--release)

# The release profile strips symbols, and with them the UniFFI metadata the bindgen reads:
# keep the symbol table here and strip the copies in jniLibs after the bindings are generated.
CARGO_PROFILE_RELEASE_STRIP=debuginfo \
  cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -P 26 -o "$JNI_LIBS" \
  build -q -p girocrab-core "${PROFILE_FLAG[@]}"

# Library mode: bindings are derived from the compiled cdylib (no UDL needed).
LIB="$(find target/aarch64-linux-android/$PROFILE -maxdepth 1 -name 'libgirocrab_core.so' | head -n1)"
[[ -n "$LIB" ]] || { echo "libgirocrab_core.so not found" >&2; exit 1; }

# shellcheck disable=SC2086 # UNIFFI_BINDGEN is a command line, word splitting is intended
$UNIFFI_BINDGEN generate --library "$LIB" --language kotlin --out-dir "$GEN_DIR" --no-format

if [[ "$PROFILE" == "release" ]]; then
  STRIP="$(find "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" -name llvm-strip | head -n1)" # a symlink to llvm-objcopy
  [[ -n "$STRIP" ]] || { echo "llvm-strip not found in the NDK" >&2; exit 1; }
  find "$JNI_LIBS" -name '*.so' -exec "$STRIP" --strip-all {} +
fi

echo "jniLibs:  $JNI_LIBS"
echo "bindings: $GEN_DIR"
