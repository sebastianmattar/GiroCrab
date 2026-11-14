#!/usr/bin/env bash
# Build, Developer-ID sign, notarize, staple and verify GiroCrab.app (macOS),
# and optionally archive & upload the iOS build to TestFlight / App Store Connect.
#
# Usage: scripts/notarize.sh [--upload <TAG> [--critical | --critical-below X.Y.Z]
#                             [--rotate-key <account> | --lost-key <account>] | --nightly] [--ios] [--no-sync]
#   --upload <TAG>  create/update GitHub release <TAG> with structured changelog and upload the
#                   signed + notarized macOS disk image (GiroCrab-macOS-arm64.dmg)
#                   vX.Y.Z          stable release
#                   vX.Y.Z-beta.N   beta (GitHub pre-release; bundle version X.Y.ZbN)
#                   RELEASE_NOTE_PREFIX="…" adds a Markdown block above the notes (and the appcast).
#   --critical      mark this stable release as a critical (security) update in the
#                   appcast: users can't skip/postpone it, but still confirm the install
#   --critical-below X.Y.Z
#                   critical only for installs older than X.Y.Z, normal update otherwise
#   --rotate-key <account>
#                   transition release for a new Sparkle key: generate key <account>, ship its
#                   public key, but sign disk image and appcast with the CURRENT key (installed
#                   apps only know that one); afterwards <account> signs. Stable tags only.
#   --lost-key <account>
#                   the signing key is LOST: new key <account>, new public key, new feed.
#                   Installed apps accept it only after Sparkle's 20-day grace period.
#                   See docs/auto-updates.md ("Schlüsselwechsel").
#   --no-sync       with --upload of a stable tag: skip the automatic
#                   scripts/sync-docs-and-website.sh --push (README PR + girocrab.de commit)
#   --nightly       move 'nightly' tag to HEAD, create/update rolling GitHub pre-release and upload the disk image
#                   (bundle version <last v* tag's version>.<UTC timestamp>)
# In-app updates: every run merges its item into the one signed appcast.xml on
# the latest stable release (channels: stable, beta, nightly), see
# scripts/make-appcast.sh and docs/auto-updates.md.
# Banking policy (docs/banking-policy.md): the current signed policy from
# policy/ on main is checked and bundled as floor (fresh installs that never
# reach GitHub trust it for 90 days from GiroCrabBuildDate); an invalid
# signature or nothing published aborts the release. Skipped while the policy
# key is a placeholder.
#   --ios           also bump iOS build number, archive for iOS, and upload to TestFlight
#                   With --ios, MARKETING_VERSION in project.pbxproj is NEVER rewritten (only
#                   CURRENT_PROJECT_VERSION + 1) and the iOS archive is pinned to it, so no new
#                   external beta review is needed. IOS_MARKETING_VERSION=x.y.z, passed
#                   explicitly, writes that version to project.pbxproj and pins iOS to it.
#                   Without --ios a stable release sets MARKETING_VERSION to the release version.
#                   (macOS always gets the release version via GIROCRAB_VERSION.)
# LOCKSTEP RULE: GiroCrab/girocrab.de (the marketing site, checked out next to this
# repo as ../girocrab.de) and the newest icon from
# GiroCrabApp/Xcode/App/Assets.xcassets/AppIcon.appiconset must always be kept in sync
# whenever README.md, the screenshots or a release change (version, download buttons
# docs/assets/download-*.svg, docs/assets/app-icon.png, GiroCrabApp/Assets/girocrab-icon.svg).
# Stable releases warn below if the site does not mention the new version yet.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

UPLOAD_TAG=""
CRITICAL=0
CRITICAL_BELOW=""
NIGHTLY=0
BUILD_IOS=0
ROTATE_TO=""
LOST_TO=""
NO_SYNC=0
while [ $# -gt 0 ]; do
  case "$1" in
    --upload)
      UPLOAD_TAG="${2:-}"
      [ -n "$UPLOAD_TAG" ] || { echo "error: --upload needs a tag (e.g. v0.5.5 or v0.6.0-beta.1)" >&2; exit 2; }
      shift 2 ;;
    --nightly)
      NIGHTLY=1
      shift ;;
    --critical)
      CRITICAL=1
      shift ;;
    --critical-below)
      CRITICAL=1
      CRITICAL_BELOW="${2:-}"
      [[ "$CRITICAL_BELOW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
        || { echo "error: --critical-below needs a version X.Y.Z" >&2; exit 2; }
      shift 2 ;;
    --ios)
      BUILD_IOS=1
      shift ;;
    --no-sync)
      NO_SYNC=1
      shift ;;
    --rotate-key)
      ROTATE_TO="${2:-}"
      [ -n "$ROTATE_TO" ] || { echo "error: --rotate-key needs a new keychain account name" >&2; exit 2; }
      shift 2 ;;
    --lost-key)
      LOST_TO="${2:-}"
      [ -n "$LOST_TO" ] || { echo "error: --lost-key needs a new keychain account name" >&2; exit 2; }
      shift 2 ;;
    -h|--help)
      sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "usage: $0 [--upload <TAG> [--critical | --critical-below X.Y.Z] [--rotate-key <account> | --lost-key <account>] | --nightly] [--ios] [--no-sync] (see --help)" >&2; exit 2 ;;
  esac
done

if [ -n "$UPLOAD_TAG" ] && [ "$NIGHTLY" -eq 1 ]; then
  echo "error: --upload and --nightly cannot be combined" >&2
  exit 2
fi
if [ -n "$ROTATE_TO$LOST_TO" ]; then
  [ -z "$ROTATE_TO" ] || [ -z "$LOST_TO" ] || { echo "error: --rotate-key and --lost-key cannot be combined" >&2; exit 2; }
  # One stable release reaches every channel (beta/nightly users also get stable).
  [[ "$UPLOAD_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { echo "error: --rotate-key/--lost-key only with --upload of a stable tag vX.Y.Z" >&2; exit 2; }
fi
if [ "$CRITICAL" -eq 1 ]; then
  if [[ ! "$UPLOAD_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: --critical only with --upload of a stable tag vX.Y.Z" >&2
    exit 2
  fi
  [ -f scripts/sparkle-public-ed-key.txt ] \
    || { echo "error: --critical needs in-app updates (scripts/sparkle-public-ed-key.txt)" >&2; exit 2; }
  # Users who already skipped this exact version are NOT re-prompted, even if
  # it is critical now (verified with Sparkle 2.10.1). Ship fixes as a new version.
  if gh release view "$UPLOAD_TAG" --json assets -q '.assets[].name' 2>/dev/null | grep -qx appcast.xml; then
    echo "warning: $UPLOAD_TAG was already published with an appcast. Users who skipped it" >&2
    echo "         will not see it again. Prefer releasing the fix as a new version." >&2
  fi
fi

# Helper: Generate structured Markdown release notes between previous v* tag and target ref
generate_release_notes() {
  local target_tag="$1"
  local prev_tag
  prev_tag="$(git tag --list 'v*' --sort=-version:refname | grep -vxF "$target_tag" | head -1 || true)"

  local range="HEAD"
  [ -n "$prev_tag" ] && range="${prev_tag}..HEAD"

  local feats fixes perfs others
  feats="$(git log "$range" --no-merges --pretty=format:'- %s (%h)' --grep='^feat' -i || true)"
  perfs="$(git log "$range" --no-merges --pretty=format:'- %s (%h)' --grep='^perf' -i || true)"
  fixes="$(git log "$range" --no-merges --pretty=format:'- %s (%h)' --grep='^fix' -i || true)"

  # Also ask GitHub API for PR-based release notes (includes @authors and PR links)
  local gh_notes=""
  local repo
  repo="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)"
  if [ -n "$repo" ]; then
    if [ -n "$prev_tag" ]; then
      gh_notes="$(gh api "repos/${repo}/releases/generate-notes" \
        -f tag_name="$target_tag" \
        -f target_commitish="main" \
        -f previous_tag_name="$prev_tag" \
        -q .body 2>/dev/null || true)"
    else
      gh_notes="$(gh api "repos/${repo}/releases/generate-notes" \
        -f tag_name="$target_tag" \
        -f target_commitish="main" \
        -q .body 2>/dev/null || true)"
    fi
  fi

  {
    if [ -n "$gh_notes" ]; then
      echo "$gh_notes"
      echo ""
    fi

    echo "### Commit-Übersicht (${prev_tag:-initial} → ${target_tag})"
    # printf '%s': commit subjects are data – no backslash-escape interpretation.
    if [ -n "$feats" ]; then
      printf '\n#### ✨ Neue Funktionen\n%s\n' "$feats"
    fi
    if [ -n "$perfs" ]; then
      printf '\n#### ⚡ Performance\n%s\n' "$perfs"
    fi
    if [ -n "$fixes" ]; then
      printf '\n#### 🐛 Fehlerbehebungen\n%s\n' "$fixes"
    fi
    echo -e "\n---\n**macOS (Apple Silicon):** Signiert mit Developer ID & von Apple notarisiert (\`GiroCrab-macOS-arm64.dmg\`)."
  }
}

# Sparkle bundle version for a v* tag: vX.Y.Z -> X.Y.Z, vX.Y.Z-beta.N -> X.Y.ZbN.
# ("X.Y.Z-beta.N" itself would compare EQUAL to X.Y.Z in Sparkle.)
bundle_version_for_tag() {
  if [[ "$1" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    echo "${BASH_REMATCH[1]}"
  elif [[ "$1" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-beta\.([0-9]+)$ ]]; then
    echo "${BASH_REMATCH[1]}b${BASH_REMATCH[2]}"
  else
    return 1
  fi
}

# If releasing a version tag (e.g. v0.5.5), the tag is the single source of truth for the
# app version: package-macos-app.sh would otherwise fall back to Cargo.toml, which can
# already be ahead of the tag (v0.5.5 shipped with CFBundleShortVersionString 0.6.0).
CHANNEL=""
if [ -n "$UPLOAD_TAG" ]; then
  BUILD_VER="$(bundle_version_for_tag "$UPLOAD_TAG")" || {
    echo "error: release tag must look like vX.Y.Z or vX.Y.Z-beta.N (got '$UPLOAD_TAG')" >&2
    exit 2
  }
  CLEAN_VER="${UPLOAD_TAG#v}"          # display version, e.g. 0.6.0 or 0.6.0-beta.1
  BASE_VER="${CLEAN_VER%%-*}"
  if [ "$BUILD_VER" = "$BASE_VER" ]; then CHANNEL="stable"; else CHANNEL="beta"; fi
  export GIROCRAB_VERSION="$CLEAN_VER" GIROCRAB_BUILD_VERSION="$BUILD_VER"
  CARGO_VER="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
  # Version consistency: Cargo.toml must match the tag (set ALLOW_VERSION_MISMATCH=1 to
  # override, e.g. for a re-upload of an old tag).
  if [ "$CARGO_VER" != "$BASE_VER" ]; then
    if [ "${ALLOW_VERSION_MISMATCH:-0}" = "1" ]; then
      echo "warning: Cargo.toml version is ${CARGO_VER:-<none>}, release tag is $UPLOAD_TAG;" \
        "the app is built as $CLEAN_VER ($BUILD_VER)" >&2
    else
      echo "error: Cargo.toml version is ${CARGO_VER:-<none>} but the release tag is $UPLOAD_TAG." \
        "Bump [workspace.package] version to $BASE_VER first (or ALLOW_VERSION_MISMATCH=1)." >&2
      exit 2
    fi
  fi
  PBXPROJ="GiroCrabApp/Xcode/GiroCrab.xcodeproj/project.pbxproj"
  # One multiplatform target shares MARKETING_VERSION with iOS. With --ios the pbxproj value
  # is the iOS version: it is only rewritten when IOS_MARKETING_VERSION is passed explicitly
  # (to that value); otherwise it stays untouched. Without --ios it gets the release version.
  if [ "$BUILD_IOS" -eq 1 ]; then PBX_MV="${IOS_MARKETING_VERSION:-}"; else PBX_MV="$CLEAN_VER"; fi
  if [ "$CHANNEL" = "stable" ] && [ -f "$PBXPROJ" ] && [ -n "$PBX_MV" ]; then
    echo "==> Setting MARKETING_VERSION = ${PBX_MV} in project.pbxproj"
    sed -i '' "s/MARKETING_VERSION = [^;]*;/MARKETING_VERSION = ${PBX_MV};/g" "$PBXPROJ"
    # Verify every MARKETING_VERSION now equals the intended version.
    BAD_MV="$(grep -o 'MARKETING_VERSION = [^;]*;' "$PBXPROJ" | grep -vxF "MARKETING_VERSION = ${PBX_MV};" || true)"
    if [ -n "$BAD_MV" ]; then
      echo "error: project.pbxproj still has MARKETING_VERSION != ${PBX_MV}: $BAD_MV" >&2
      exit 2
    fi
  elif [ "$CHANNEL" = "stable" ] && [ "$BUILD_IOS" -eq 1 ]; then
    echo "==> --ios without IOS_MARKETING_VERSION: MARKETING_VERSION in project.pbxproj stays untouched"
  fi
  # Lockstep: the marketing site must already show this version (warning only).
  if [ "$CHANNEL" = "stable" ]; then
    SITE_DIR="${GIROCRAB_SITE_DIR:-$ROOT/../girocrab.de}"
    if [ -f "$SITE_DIR/index.html" ]; then
      grep -q "v${CLEAN_VER}" "$SITE_DIR/index.html" \
        || echo "warning: $SITE_DIR/index.html does not mention v${CLEAN_VER} – update GiroCrab/girocrab.de (icon, screenshots, buttons) in lockstep." >&2
    else
      echo "warning: girocrab.de checkout not found at $SITE_DIR – keep GiroCrab/girocrab.de in lockstep with README/screenshots/release." >&2
    fi
  fi
  # README: keep the "current version" lines in step with the stable tag.
  if [ "$CHANNEL" = "stable" ] && [ -f README.md ]; then
    # The release-notes headings must have been written for this version by hand.
    if ! grep -q "^### ✨ Neu in v${CLEAN_VER}\$" README.md \
      || ! grep -q "^### ✨ What's new in v${CLEAN_VER}\$" README.md; then
      echo "error: README.md has no '### ✨ Neu in v${CLEAN_VER}' / '### ✨ What's new in v${CLEAN_VER}' section." \
        "Update the highlights (DE + EN) for this release first." >&2
      exit 2
    fi
    echo "==> Setting README version to v${CLEAN_VER}"
    sed -i '' -E "s/(Aktuelle Version: \*\*)v[0-9][^*]*(\*\*)/\1v${CLEAN_VER}\2/; s/(Current version: \*\*)v[0-9][^*]*(\*\*)/\1v${CLEAN_VER}\2/" README.md
    if ! grep -q "Aktuelle Version: \*\*v${CLEAN_VER}\*\*" README.md \
      || ! grep -q "Current version: \*\*v${CLEAN_VER}\*\*" README.md; then
      echo "error: README.md lacks 'Aktuelle Version: **v${CLEAN_VER}**' / 'Current version: **v${CLEAN_VER}**'" >&2
      exit 2
    fi
  fi
fi

PROFILE="${NOTARY_PROFILE:-notary-profile}"
IDENTITY="${SIGN_IDENTITY:-$(security find-identity -p codesigning -v \
  | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -1)}"
if [ -z "$IDENTITY" ]; then
  echo "error: no 'Developer ID Application' identity in keychain." >&2
  exit 1
fi
echo "==> Identity: $IDENTITY"

# Outside build/ and dist/: both are wiped while packaging.
POLICY_DIR="$(mktemp -d)"
# Bundled as the floor for a fresh install; published in policy/ on main.
echo "==> Banking policy: fetch + verify (aborts on an invalid signature)"
scripts/banking-policy.sh fetch "$POLICY_DIR"
export GIROCRAB_BANKING_POLICY_DIR="$POLICY_DIR"
POLICY_FILES=()
if [ -f "$POLICY_DIR/banking-policy.json" ]; then
  POLICY_FILES=("$POLICY_DIR/banking-policy.json" "$POLICY_DIR/banking-policy.json.sig")
fi

if [ "$NIGHTLY" -eq 1 ]; then
  CHANNEL="nightly"
  # <last v* tag's bundle version>.<UTC timestamp>: sorts after that tag and
  # before the next beta/stable, so nightly testers also get those.
  LAST_TAG="$(git describe --tags --abbrev=0 --match 'v[0-9]*' HEAD 2>/dev/null)" \
    || { echo "error: --nightly needs a previous v* tag" >&2; exit 2; }
  LAST_BUILD="$(bundle_version_for_tag "$LAST_TAG")" \
    || { echo "error: last tag $LAST_TAG is neither vX.Y.Z nor vX.Y.Z-beta.N" >&2; exit 2; }
  STAMP="$(date -u +%Y%m%d%H%M)"
  export GIROCRAB_BUILD_VERSION="${LAST_BUILD}.${STAMP}"
  export GIROCRAB_VERSION="${LAST_TAG#v}-nightly.${STAMP}"
fi

# Sparkle key rotation / lost key: decide the keys BEFORE packaging – the
# bundle gets the (new) public key from scripts/sparkle-public-ed-key.txt.
if [ -n "$ROTATE_TO" ]; then
  echo "==> Sparkle key rotation to '$ROTATE_TO' (transition release)"
  KEY_EXPORTS="$(scripts/sparkle-key.sh prepare-rotation "$ROTATE_TO")"   # assignment: a failure stops the release
  eval "$KEY_EXPORTS"
elif [ -n "$LOST_TO" ]; then
  echo "==> Sparkle key LOST – switching to '$LOST_TO'"
  KEY_EXPORTS="$(scripts/sparkle-key.sh prepare-lost "$LOST_TO")"   # assignment: a failure stops the release
  eval "$KEY_EXPORTS"
fi

# GiroCrab's own FinTS product registration (docs/fints-registrierung.md): from the
# keychain, compiled in by girocrab-core/build.rs. Never from a file in the repo.
FINTS_ID_EMBEDDED=0
if GIROCRAB_FINTS_PRODUCT_ID="$(scripts/fints-registrierung.sh get 2>/dev/null)" && [ -n "$GIROCRAB_FINTS_PRODUCT_ID" ]; then
  export GIROCRAB_FINTS_PRODUCT_ID
  FINTS_ID_EMBEDDED=1
  echo "==> FinTS product ID: available, will be compiled in (value not shown)"
else
  unset GIROCRAB_FINTS_PRODUCT_ID
  if [ "$CHANNEL" = "stable" ] && [ "${ALLOW_NO_PRODUCT_ID:-0}" != "1" ]; then
    echo "error: no FinTS product ID (scripts/fints-registrierung.sh set). Stable must ship it;" >&2
    echo "       override for a test run only: ALLOW_NO_PRODUCT_ID=1" >&2
    exit 1
  fi
  echo "warning: building WITHOUT GiroCrab's FinTS product ID ($CHANNEL): users must enter their own." >&2
fi

# iOS: bump the build number and make sure the XCFramework exists BEFORE either
# job starts (no race on generated/ or project.pbxproj; package-macos-app.sh
# then finds a current XCFramework and skips its own build).
if [ "$BUILD_IOS" -eq 1 ]; then
  PBXPROJ="GiroCrabApp/Xcode/GiroCrab.xcodeproj/project.pbxproj"
  CURRENT_BUILD="$(sed -n 's/.*CURRENT_PROJECT_VERSION = \([0-9][0-9]*\);.*/\1/p' "$PBXPROJ" | head -1)"
  [ -n "$CURRENT_BUILD" ] || { echo "error: no numeric CURRENT_PROJECT_VERSION in $PBXPROJ" >&2; exit 1; }
  NEXT_BUILD=$((CURRENT_BUILD + 1))
  echo "==> Bumping iOS CURRENT_PROJECT_VERSION: ${CURRENT_BUILD} -> ${NEXT_BUILD}"
  sed -i '' "s/CURRENT_PROJECT_VERSION = ${CURRENT_BUILD};/CURRENT_PROJECT_VERSION = ${NEXT_BUILD};/g" "$PBXPROJ"

  # Release/TestFlight needs macOS + iOS device only: skip the simulator slice.
  if ./build-xcframework.sh --check --strict-env --need-ios 2>/dev/null; then
    echo "==> XCFramework is up to date — skipping rebuild"
  else
    echo "==> Building XCFramework (macOS + iOS device, no simulator)"
    ./build-xcframework.sh --no-sim
  fi
fi

echo "==> Build (xcframework + swift release + bundle) via package-macos-app.sh"
./package-macos-app.sh

rm -rf build; mkdir -p build
cp -R dist/GiroCrab.app build/GiroCrab.app
APP="build/GiroCrab.app"; ZIP="build/GiroCrab-macOS.zip"

echo "==> Signing (hardened runtime, inside-out incl. Sparkle)"
scripts/sign-app.sh "$IDENTITY" "$APP"

# Everything from the notarization submit to the GitHub upload. With --ios it runs in
# the background (Apple's servers take minutes) while the iOS archive builds.
# Body deliberately not indented: it contains multi-line strings.
macos_release_pipeline() {
echo "==> Zip + notarize (app)"
ditto -c -k --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$PROFILE" --wait

echo "==> Staple + Gatekeeper (app)"
xcrun stapler staple "$APP"
spctl --assess -vv --type execute "$APP"

# The release artifact is a Developer-ID-signed, notarized and stapled disk
# image (one artifact for Sparkle, Homebrew and manual download). The app inside
# carries its own stapled ticket, so it also passes Gatekeeper offline after
# being dragged to /Applications.
DMG="build/GiroCrab-macOS-arm64.dmg"
echo "==> Disk image: build, sign, notarize, staple"
scripts/make-dmg.sh "$APP" "$DMG" "$IDENTITY"
xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$DMG"

echo "==> Verify final disk image"
xcrun stapler validate "$DMG"
spctl -a -vvv -t open --context context:primary-signature "$DMG"
codesign -dvvv "$DMG" 2>&1 | grep "^Authority=Developer ID Application" >/dev/null \
  || { echo "error: disk image is not signed with 'Developer ID Application'" >&2; exit 1; }

VERIFY_DIR="$(mktemp -d)"
MOUNT_DIR="$(mktemp -d)"
cleanup_verify() {
  hdiutil detach -quiet "$MOUNT_DIR" 2>/dev/null || true
  rm -rf "$VERIFY_DIR" "$MOUNT_DIR"
}
trap cleanup_verify EXIT
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$MOUNT_DIR" "$DMG"
[ -L "$MOUNT_DIR/Applications" ] || { echo "error: disk image lacks the /Applications link" >&2; exit 1; }
ditto "$MOUNT_DIR/GiroCrab.app" "$VERIFY_DIR/GiroCrab.app"
hdiutil detach -quiet "$MOUNT_DIR"
VAPP="$VERIFY_DIR/GiroCrab.app"

CS_OUT="$(codesign -dvvv "$VAPP" 2>&1)"
echo "$CS_OUT" | grep "^Authority=Developer ID Application" >/dev/null \
  || { echo "error: signature is not 'Developer ID Application'" >&2; exit 1; }
xcrun stapler validate "$VAPP"
spctl -a -vvv -t exec "$VAPP"

if [ -n "$CHANNEL" ]; then
  got_short="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$VAPP/Contents/Info.plist")"
  got_build="$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$VAPP/Contents/Info.plist")"
  if [ "$got_short" != "$GIROCRAB_VERSION" ] || [ "$got_build" != "$GIROCRAB_BUILD_VERSION" ]; then
    echo "error: app version is '$got_short' ($got_build), expected '$GIROCRAB_VERSION'" \
      "($GIROCRAB_BUILD_VERSION); not uploading" >&2
    exit 1
  fi
  echo "app version as expected: $got_short ($got_build, $CHANNEL)"
fi

echo "--- banking policy in the build"
scripts/banking-policy.sh check-app "$VAPP"
if [ ${#POLICY_FILES[@]} -gt 0 ]; then
  cmp -s "$VAPP/Contents/Resources/banking-policy.json" "$POLICY_DIR/banking-policy.json" \
    || { echo "error: bundled banking-policy.json missing or not the verified one" >&2; exit 1; }
  # without it the app gets no 90-day allowance for the bundled policy
  /usr/libexec/PlistBuddy -c "Print :GiroCrabBuildDate" "$VAPP/Contents/Info.plist" >/dev/null 2>&1 \
    || { echo "error: GiroCrabBuildDate missing in Info.plist" >&2; exit 1; }
fi

echo "--- FinTS product ID in the build"
if [ "$FINTS_ID_EMBEDDED" = "1" ]; then
  scripts/fints-registrierung.sh check-app "$VAPP"
else
  echo "none embedded (see warning above)"
fi

echo "--- launch smoke test"
"$VAPP/Contents/MacOS/GiroCrabMac" >/dev/null 2>&1 &
APP_PID=$!
sleep 5
if kill -0 "$APP_PID" 2>/dev/null; then
  kill "$APP_PID" 2>/dev/null || true
  wait "$APP_PID" 2>/dev/null || true
  echo "app started and stayed running"
else
  echo "error: signed app exited within 5s" >&2
  exit 1
fi

mkdir -p dist
cp "$DMG" dist/GiroCrab-macOS-arm64.dmg
ARTIFACT="dist/GiroCrab-macOS-arm64.dmg"
echo "==> Verified: $ARTIFACT"

# --- In-app updates: merge this build into the ONE signed appcast -----------
# Stable: the new feed goes onto the new release (live once it is published as
# "latest"). Beta/nightly: it replaces appcast.xml on the current latest stable
# release. Always strictly after the disk image it points to. No-op until the Sparkle
# public key exists (scripts/sparkle-public-ed-key.txt).
make_feed() {  # <tag hosting the disk image> <notes file>
  [ -f scripts/sparkle-public-ed-key.txt ] || return 1
  local repo
  repo="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
  curl -fsSL -o build/appcast-current.xml \
    "https://github.com/${repo}/releases/latest/download/appcast.xml" \
    || rm -f build/appcast-current.xml
  GIROCRAB_TAG="$1" GIROCRAB_EXISTING_APPCAST="build/appcast-current.xml" \
  GIROCRAB_CRITICAL="$CRITICAL" GIROCRAB_CRITICAL_BELOW="$CRITICAL_BELOW" \
    scripts/make-appcast.sh "$CHANNEL" "$ARTIFACT" "$2" dist/appcast.xml
}
upload_feed_to_latest_stable() {
  local latest
  latest="$(gh release view --json tagName -q .tagName 2>/dev/null || true)"
  if [ -z "$latest" ]; then
    echo "warning: no published stable release yet – appcast not uploaded" >&2
    return 0
  fi
  echo "==> Updating appcast.xml on latest stable release $latest"
  gh release upload "$latest" dist/appcast.xml --clobber
}

if [ -n "$UPLOAD_TAG" ]; then
  NOTES="$(generate_release_notes "$UPLOAD_TAG")"
  if [ "$CRITICAL" -eq 1 ]; then
    NOTES="> ⚠️ **Sicherheitsupdate** – bitte zeitnah installieren.

${NOTES}"
  elif [ "$CHANNEL" = "beta" ]; then
    NOTES="> 🧪 **Beta-Version** – Vorabversion zum Testen.

${NOTES}"
  fi
  # Optional extra text above the notes (also in the appcast), e.g. a schema warning.
  if [ -n "${RELEASE_NOTE_PREFIX:-}" ]; then
    NOTES="${RELEASE_NOTE_PREFIX}

${NOTES}"
  fi
  RELEASE_FLAGS=()
  [ "$CHANNEL" = "beta" ] && RELEASE_FLAGS=(--prerelease --latest=false)

  printf '%s\n' "$NOTES" > build/release-notes.md
  # Do NOT call make_feed inside an `if`: that disables `set -e`, so a failing
  # make-appcast.sh (signing, key mismatch, ...) was silently treated as "no key"
  # and the release went out without appcast.xml (Issue #322, v0.6.2).
  HAVE_FEED=0
  if [ -f scripts/sparkle-public-ed-key.txt ]; then
    rm -f dist/appcast.xml
    make_feed "$UPLOAD_TAG" build/release-notes.md
    [ -s dist/appcast.xml ] || { echo "error: appcast.xml was not generated; not publishing" >&2; exit 1; }
    HAVE_FEED=1
  else echo "==> No appcast (no scripts/sparkle-public-ed-key.txt yet – in-app updates off)"; fi
  if [ -n "$ROTATE_TO" ]; then
    # Before anything is published: new key inside, old key signs.
    scripts/sparkle-key.sh verify-transition "$VAPP" "$ARTIFACT" dist/appcast.xml
  fi

  if ! git rev-parse "$UPLOAD_TAG" >/dev/null 2>&1; then
    echo "==> Creating and pushing git tag $UPLOAD_TAG"
    git tag -a "$UPLOAD_TAG" -m "Release $UPLOAD_TAG"
    git push origin "$UPLOAD_TAG"
  fi
  if gh release view "$UPLOAD_TAG" >/dev/null 2>&1; then
    echo "==> Updating existing release $UPLOAD_TAG"
    gh release upload "$UPLOAD_TAG" "$ARTIFACT" --clobber
    gh release edit "$UPLOAD_TAG" --notes "$NOTES" ${RELEASE_FLAGS[@]+"${RELEASE_FLAGS[@]}"}
  else
    echo "==> Creating GitHub release $UPLOAD_TAG"
    gh release create "$UPLOAD_TAG" "$ARTIFACT" \
      --title "GiroCrab $UPLOAD_TAG" \
      --notes "$NOTES" ${RELEASE_FLAGS[@]+"${RELEASE_FLAGS[@]}"}
  fi
  if [ "$HAVE_FEED" -eq 1 ]; then
    if [ "$CHANNEL" = "stable" ]; then
      gh release upload "$UPLOAD_TAG" dist/appcast.xml --clobber
    else
      upload_feed_to_latest_stable
    fi
  fi
  if [ -n "$ROTATE_TO" ]; then
    scripts/sparkle-key.sh finish-rotation "$SPARKLE_ROTATION_NEW" "$SPARKLE_ROTATION_OLD"
  elif [ -n "$LOST_TO" ]; then
    echo "==> New key '$LOST_TO' is active. Commit scripts/sparkle-public-ed-key.txt and scripts/sparkle-key-account.txt."
  fi
elif [ "$NIGHTLY" -eq 1 ]; then
  SHORT_SHA="$(git rev-parse --short HEAD)"
  BUILD_DATE="$(date -u +%Y-%m-%d)"
  NOTES="$(generate_release_notes "nightly")"
  TITLE="Nightly Build (${BUILD_DATE}, ${SHORT_SHA})"
  printf '%s\n' "$NOTES" > build/release-notes.md

  git tag -f nightly HEAD
  git push origin refs/tags/nightly --force

  if gh release view nightly >/dev/null 2>&1; then
    gh release edit nightly --prerelease --latest=false --title "$TITLE" --notes "$NOTES"
    gh release upload nightly "$ARTIFACT" --clobber
  else
    gh release create nightly "$ARTIFACT" \
      --prerelease --latest=false --title "$TITLE" --notes "$NOTES"
  fi
  # The rolling disk image was just replaced; until the feed below is uploaded, the
  # old nightly item's signature no longer matches and clients reject the
  # download (fails safe).
  export GIROCRAB_SHORT_VERSION="Nightly ${BUILD_DATE} (${SHORT_SHA})"
  # Not inside an `if` (that would disable `set -e` and skip a failing feed silently).
  if [ -f scripts/sparkle-public-ed-key.txt ]; then
    rm -f dist/appcast.xml
    make_feed nightly build/release-notes.md
    [ -s dist/appcast.xml ] || { echo "error: nightly appcast.xml was not generated" >&2; exit 1; }
    upload_feed_to_latest_stable
  fi
fi
}

ios_pipeline() {
  IOS_ARCHIVE="build/GiroCrab-iOS.xcarchive"
  IOS_EXPORT="build/ios-export"
  IOS_PLIST="build/ExportOptions-upload.plist"
  rm -rf "$IOS_ARCHIVE" "$IOS_EXPORT"

  # TestFlight: a changed MARKETING_VERSION triggers a 24 h external beta
  # review, a new build number under an approved version does not. By default
  # the iOS archive is pinned to the (untouched) MARKETING_VERSION of
  # project.pbxproj; IOS_MARKETING_VERSION=x.y.z (explicit) overrides it and was
  # already written to project.pbxproj for stable releases. macOS gets the
  # release version via GIROCRAB_VERSION, independent of the pbxproj.
  IOS_MARKETING_VERSION="${IOS_MARKETING_VERSION:-$(sed -n 's/.*MARKETING_VERSION = \([^;]*\);.*/\1/p' "$PBXPROJ" | head -1)}"
  IOS_VERSION_ARGS=()
  if [ -n "$IOS_MARKETING_VERSION" ]; then
    echo "==> Pinning iOS MARKETING_VERSION = ${IOS_MARKETING_VERSION} (Build ${NEXT_BUILD})"
    IOS_VERSION_ARGS=("MARKETING_VERSION=${IOS_MARKETING_VERSION}")
  fi

  xcodebuild archive \
    -project GiroCrabApp/Xcode/GiroCrab.xcodeproj \
    -scheme GiroCrab \
    -destination "generic/platform=iOS" \
    -archivePath "$IOS_ARCHIVE" \
    -allowProvisioningUpdates \
    ${IOS_VERSION_ARGS[@]+"${IOS_VERSION_ARGS[@]}"}

  cp scripts/ExportOptions.plist "$IOS_PLIST"
  /usr/libexec/PlistBuddy -c "Add :destination string upload" "$IOS_PLIST" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Set :destination upload" "$IOS_PLIST"

  xcodebuild -exportArchive \
    -archivePath "$IOS_ARCHIVE" \
    -exportOptionsPlist "$IOS_PLIST" \
    -exportPath "$IOS_EXPORT" \
    -allowProvisioningUpdates

  echo "==> iOS Build ${NEXT_BUILD} uploaded to TestFlight${IOS_MARKETING_VERSION:+ (MARKETING_VERSION ${IOS_MARKETING_VERSION})}"
}

if [ "$BUILD_IOS" -eq 1 ]; then
  # Notarization + GitHub upload (background) run in parallel with the iOS archive + TestFlight
  # upload (foreground). Neither failure may leave the other half-reported.
  MAC_STATUS_FILE="$(mktemp)"; rm -f "$MAC_STATUS_FILE"
  echo "==> macOS notarization + release runs in the background ([macOS] lines); iOS archive starts now"
  (
    set +e
    ( set -e; macos_release_pipeline ) 2>&1
    echo $? > "$MAC_STATUS_FILE"
  ) | while IFS= read -r line; do printf '[macOS] %s\n' "$line"; done &
  MAC_JOB=$!

  set +e
  ( set -e; ios_pipeline )
  IOS_RC=$?
  echo "==> iOS finished (exit $IOS_RC); waiting for the macOS notarization/release job"
  wait "$MAC_JOB"
  MAC_RC="$(cat "$MAC_STATUS_FILE" 2>/dev/null || echo 1)"; rm -f "$MAC_STATUS_FILE"
  set -e

  if [ "$MAC_RC" -ne 0 ] || [ "$IOS_RC" -ne 0 ]; then
    [ "$MAC_RC" -eq 0 ] || echo "error: macOS notarization/release FAILED (exit $MAC_RC) – see the [macOS] lines above" >&2
    [ "$IOS_RC" -eq 0 ] || echo "error: iOS archive/TestFlight upload FAILED (exit $IOS_RC)" >&2
    echo "note: CURRENT_PROJECT_VERSION in project.pbxproj was already bumped to ${NEXT_BUILD}." >&2
    exit 1
  fi
else
  macos_release_pipeline
fi

if [ "$BUILD_IOS" -eq 1 ]; then
  echo ""
  echo "==> iOS build number was bumped to ${NEXT_BUILD} in the working tree (not committed). Don't forget:"
  echo "    git commit -m \"chore: bump iOS build number to ${NEXT_BUILD}\" -- \"$PBXPROJ\" && git push"
fi

if [ -n "$UPLOAD_TAG" ] && [[ "$UPLOAD_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo ""
  if [ "$NO_SYNC" -eq 1 ]; then
    echo "==> --no-sync: skipping README + girocrab.de sync."
    echo "    Later: ./scripts/sync-docs-and-website.sh --push --version $UPLOAD_TAG"
  else
    echo "==> Syncing README + girocrab.de for $UPLOAD_TAG"
    if ! ./scripts/sync-docs-and-website.sh --push --version "$UPLOAD_TAG"; then
      echo "warning: README/website sync failed; the release itself is done. Retry:" >&2
      echo "    ./scripts/sync-docs-and-website.sh --push --version $UPLOAD_TAG" >&2
    fi
  fi
fi

echo "==> Done"
