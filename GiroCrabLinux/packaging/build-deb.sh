#!/usr/bin/env bash
# Builds a Debian/Ubuntu package of GiroCrab for Linux:
#   GiroCrabLinux/target/debian/girocrab_<version>_<arch>.deb
# Install with `sudo apt install ./girocrab_<version>_<arch>.deb`, which also pulls in Qt 6,
# the QML modules and Kirigami from the distribution (Depends). Recommended extras:
# QtWebEngine (bank consent / web logins, printing), Qt's PDF image plugin (printing, GiroCode in PDFs), QtPdf
# (PDF preview), tesseract (receipt OCR),
# a Secret Service provider (GNOME Keyring, KWallet or KeePassXC) for the vault key.
#
# Needs: Rust (cargo), qt6-base-dev, qt6-declarative-dev (build), dpkg-dev (dpkg-shlibdeps).
# Options: --no-build  package the existing target/release/girocrab
# Env:     DEB_MAINTAINER="Name <mail>" overrides the Maintainer field.
#
# Note: qmlcachegen compiles the QML ahead of time into C++ that uses two Qt-private
# symbols, so dpkg-shlibdeps adds `qt6-declarative-private-abi (= <Qt version>)`. Like
# every Qt application in Debian/Ubuntu, the package must be rebuilt for each
# distribution release (its Qt version); it then installs and runs as usual.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/.." && pwd)"
repo="$(cd "$crate/.." && pwd)"
cd "$crate"

pkg=girocrab
app_id=de.girocrab.GiroCrab
# The version of the GiroCrab workspace (core and Mac app), like build.rs.
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
[ -n "$version" ] || { echo "Keine Version in $repo/Cargo.toml" >&2; exit 1; }
# Every build from git gets its own, increasing version (0.7.0+git202611142247.5372f85), so
# `apt install ./girocrab_….deb` replaces an installed build of the same release. Uncommitted
# changes use the build time. DEB_VERSION overrides it (e.g. DEB_VERSION=0.7.0 for a release).
if [ -n "${DEB_VERSION:-}" ]; then
    version="$DEB_VERSION"
elif git rev-parse --git-dir >/dev/null 2>&1; then
    if git diff --quiet HEAD -- . 2>/dev/null; then
        stamp="$(git log -1 --format=%cd --date=format:%Y%m%d%H%M -- .)"
    else
        stamp="$(date +%Y%m%d%H%M)"
    fi
    version="$version+git$stamp.$(git rev-parse --short HEAD)"
fi
arch="$(dpkg --print-architecture)"

if [[ "${1:-}" != "--no-build" ]]; then
    nice cargo build --release --locked
fi
bin="$crate/target/release/girocrab"
[[ -x "$bin" ]] || { echo "missing $bin – build first" >&2; exit 1; }

out="$crate/target/debian"
stage="$out/$pkg-$version"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" \
         "$stage/usr/share/icons/hicolor/scalable/apps" "$stage/usr/share/metainfo" \
         "$stage/usr/share/doc/$pkg"

install -m 0755 "$bin" "$stage/usr/bin/girocrab"
strip --strip-unneeded "$stage/usr/bin/girocrab"
install -m 0644 "$here/$app_id.desktop" "$stage/usr/share/applications/$app_id.desktop"
install -m 0644 "$here/$app_id.metainfo.xml" "$stage/usr/share/metainfo/$app_id.metainfo.xml"
install -m 0644 "$repo/GiroCrabApp/Assets/girocrab-icon.svg" \
                "$stage/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
# PNG sizes for desktops that ignore SVG icons (optional tool)
if command -v rsvg-convert >/dev/null; then
    for size in 48 64 128 256 512; do
        dir="$stage/usr/share/icons/hicolor/${size}x${size}/apps"
        mkdir -p "$dir"
        rsvg-convert -w "$size" -h "$size" "$repo/GiroCrabApp/Assets/girocrab-icon.svg" -o "$dir/$app_id.png"
    done
fi
install -m 0644 "$repo/THIRD_PARTY_NOTICES.md" "$stage/usr/share/doc/$pkg/THIRD_PARTY_NOTICES.md"
gzip -9n -c "$crate/README.md" > "$stage/usr/share/doc/$pkg/README.md.gz"
cat > "$stage/usr/share/doc/$pkg/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: GiroCrab
Source: https://github.com/GiroCrab/GiroCrab

Files: *
Copyright: GiroCrab contributors
License: GPL-3.0-or-later
 On Debian systems, the full text of the GNU General Public License version 3
 can be found in /usr/share/common-licenses/GPL-3.
 .
 Third-party components (Rust crates, bundled SQLCipher/OpenSSL, Simple Icons)
 are listed with their licences in THIRD_PARTY_NOTICES.md.
EOF

# Shared-library dependencies (Qt 6 libs, libc …) computed from the binary.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$pkg" "$pkg" > "$tmp/debian/control"
shlibs="$(cd "$tmp" && dpkg-shlibdeps -O "$stage/usr/bin/girocrab" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"

# QML modules are loaded at runtime, so dpkg-shlibdeps cannot see them.
qml_deps="qml6-module-qtquick, qml6-module-qtquick-controls, qml6-module-qtquick-templates,
 qml6-module-qtquick-layouts, qml6-module-qtquick-shapes, qml6-module-qtquick-dialogs,
 qml6-module-qtquick-window, qml6-module-qtqml-workerscript, qml6-module-qtqml-models,
 qml6-module-qtcore, qml6-module-qt-labs-qmlmodels, qml6-module-org-kde-kirigami"
depends="$(echo "$shlibs, $qml_deps" | tr -s ' \n' ' ')"

installed_kb="$(du -sk "$stage/usr" | cut -f1)"
cat > "$stage/DEBIAN/control" <<EOF
Package: $pkg
Version: $version
Architecture: $arch
Maintainer: ${DEB_MAINTAINER:-Sebastian <thxgiving@gmail.com>}
Installed-Size: $installed_kb
Section: misc
Priority: optional
Homepage: https://girocrab.de
Depends: $depends
Recommends: qml6-module-qtwebengine, qt6-image-formats-plugin-pdf, qml6-module-qtquick-pdf, gnome-keyring | kwalletmanager | keepassxc, tesseract-ocr, tesseract-ocr-deu, breeze-icon-theme
Description: Verschlüsselte Finanzverwaltung (inoffizieller Linux-Port von GiroCrab)
 GiroCrab ruft Umsätze direkt bei der Bank ab (FinTS/HBCI, PSD2 und weitere
 Schnittstellen), kategorisiert sie mit Regeln und speichert alles in einem
 lokal verschlüsselten Tresor (SQLCipher). Kein Server, kein Tracking.
 .
 Inoffizieller Linux-Port (Qt 6 / QML) auf dem Rust-Kern der Mac-App; nicht
 vom GiroCrab-Projekt herausgegeben. FinTS-Banken brauchen eine eigene,
 kostenlose Produktregistrierung der Deutschen Kreditwirtschaft.
EOF

dpkg-deb --root-owner-group --build -Zxz "$stage" "$out/${pkg}_${version}_${arch}.deb" >/dev/null
echo "$out/${pkg}_${version}_${arch}.deb"
