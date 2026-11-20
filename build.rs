//! Builds the native SwiftUI app as part of `cargo build`:
//!   1. ./build-xcframework.sh --macos-only   (Rust core + UniFFI Swift package),
//!      skipped when `--check` finds generated/ up to date
//!   2. swift build --product GiroCrabMac, configured by cargo's profile:
//!      - release: `-c release` in GiroCrabApp/.build, as package-macos-app.sh
//!        builds it. Whole-module optimization compiles GiroCrabUI on mostly one
//!        core, and every edit recompiles all of it.
//!      - dev: `-c debug -Xswiftc -O` in target/swift-dev. Optimized, but
//!        parallel and incremental; `#if DEBUG` code is active. Its own scratch
//!        path, so plain `swift build`/`swift test` (-Onone) and this build
//!        don't recompile everything each time the other one ran.
//!   3. wraps the executable into target/<profile>/GiroCrab.app (ad-hoc signed),
//!      with translations compiled from the String Catalogs (xcstringstool).
//!      One bundle per profile: switching back to a profile cargo already built
//!      does not rerun this script, so a shared bundle would still hold the
//!      other profile's binary.
//!
//! The launcher binary (src/main.rs) then starts that app.
//!
//! Set GIROCRAB_SKIP_SWIFT=1 to skip (e.g. on Linux CI for core-only work).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn clean_env(cmd: &mut Command) {
    // Don't leak the outer cargo's build-script environment into the nested
    // cargo/swift builds (it would confuse their target/profile detection).
    let keep = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "SHELL",
        "DEVELOPER_DIR",
        "RUSTUP_HOME",
        "CARGO_HOME",
        "RUSTUP_TOOLCHAIN",
        "SDKROOT_OVERRIDE",
    ];
    let saved: Vec<(String, String)> = keep
        .iter()
        .filter_map(|k| env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    cmd.env_clear();
    for (k, v) in saved {
        cmd.env(k, v);
    }
    let home = env::var("HOME").unwrap_or_default();
    let path = env::var("PATH").unwrap_or_default();
    cmd.env(
        "PATH",
        format!("{home}/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:{path}"),
    );
    if let Ok(cargo) = env::var("CARGO") {
        cmd.env("CARGO", cargo);
    }
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{what}: could not start: {e}"));
    if !status.success() {
        panic!("{what} failed with {status}");
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    for p in [
        "build-xcframework.sh",
        "girocrab-core/src",
        "girocrab-core/Cargo.toml",
        "GiroCrabApp/Sources",
        "GiroCrabApp/Package.swift",
        "GiroCrabApp/Xcode/App/Assets.xcassets",
        "GiroCrabApp/Xcode/App/Localizable.xcstrings",
        "GiroCrabApp/Xcode/App/InfoPlist.xcstrings",
        "uniffi-bindgen",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }
    println!("cargo:rerun-if-env-changed=GIROCRAB_SKIP_SWIFT");
    // Set by `cargo clippy`; tracked so a later `cargo build` reruns this
    // script instead of reusing the skipped clippy run.
    println!("cargo:rerun-if-env-changed=RUSTC_WORKSPACE_WRAPPER");

    // Cargo reports "debug" or "release" (a custom profile reports the one it
    // inherits from); SwiftPM's configurations have the same names.
    let profile = match env::var("PROFILE").as_deref() {
        Ok("release") => "release",
        _ => "debug",
    };
    let app_dir = root.join("target").join(profile).join("GiroCrab.app");
    let app_bin = app_dir.join("Contents/MacOS/GiroCrab");
    println!("cargo:rustc-env=GIROCRAB_APP_BIN={}", app_bin.display());

    if env::var("GIROCRAB_SKIP_SWIFT").is_ok()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        println!(
            "cargo:warning=skipping the SwiftUI app build (GIROCRAB_SKIP_SWIFT or non-macOS target)"
        );
        return;
    }
    // `cargo clippy --workspace` only lints: the release core build and the
    // Swift app it would trigger take ~90 s after every core edit.
    if env::var("RUSTC_WORKSPACE_WRAPPER").is_ok_and(|w| w.contains("clippy-driver")) {
        return;
    }

    // 1. Rust core -> XCFramework + Swift bindings (separate target dir: no lock contention)
    let script = root.join("build-xcframework.sh");
    let mut check = Command::new("bash");
    check
        .arg(&script)
        .arg("--check")
        .current_dir(&root)
        .stderr(Stdio::null());
    clean_env(&mut check);
    if !check.status().is_ok_and(|s| s.success()) {
        let mut xcf = Command::new("bash");
        xcf.arg(&script).arg("--macos-only").current_dir(&root);
        clean_env(&mut xcf);
        xcf.env("GIROCRAB_TARGET_DIR", root.join("target").join("apple"));
        run(&mut xcf, "build-xcframework.sh --macos-only");
    }

    // 2. SwiftUI app (configurations: see the module docs)
    let pkg = root.join("GiroCrabApp");
    let mut swift = Command::new("xcrun");
    swift.args(["swift", "build", "-c", profile, "--product", "GiroCrabMac"]);
    let scratch = if profile == "release" {
        pkg.join(".build")
    } else {
        swift.args(["-Xswiftc", "-O"]);
        root.join("target").join("swift-dev")
    };
    swift
        .arg("--package-path")
        .arg(&pkg)
        .arg("--scratch-path")
        .arg(&scratch)
        .current_dir(&pkg);
    clean_env(&mut swift);
    run(&mut swift, "swift build (GiroCrabMac)");
    let built = scratch.join(profile).join("GiroCrabMac");
    assert!(
        built.exists(),
        "swift build did not produce {}",
        built.display()
    );

    // 3. .app bundle (gives the window a proper app identity, Dock icon, menu name)
    make_bundle(&app_dir, &built, &scratch);
}

fn make_bundle(app_dir: &Path, exe: &Path, scratch: &Path) {
    let macos = app_dir.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::copy(exe, macos.join("GiroCrab")).expect("copy app binary");
    embed_sparkle(app_dir, exe, scratch);
    make_icns(app_dir);
    let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>GiroCrab</string>
    <key>CFBundleDisplayName</key><string>GiroCrab</string>
    <key>CFBundleIdentifier</key><string>de.girocrab.app</string>
    <key>CFBundleExecutable</key><string>GiroCrab</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>__VERSION__</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleDevelopmentRegion</key><string>de</string>
    <key>CFBundleLocalizations</key><array><string>de</string><string>en</string></array>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.finance</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSFaceIDUsageDescription</key><string>GiroCrab nutzt Face ID / Touch ID, um Ihren verschlüsselten Tresor zu entsperren.</string>
</dict>
</plist>
"#;
    let plist = plist.replace("__VERSION__", env!("CARGO_PKG_VERSION"));
    std::fs::write(app_dir.join("Contents/Info.plist"), plist).unwrap();
    compile_localizations(app_dir);
    // Ad-hoc signature so Keychain + LocalAuthentication accept the bundle.
    let _ = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app_dir)
        .status();
    // Refresh Finder/Dock icon caches for the (re)built bundle.
    let _ = Command::new("touch").arg(app_dir).status();
    let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f")
        .arg(app_dir)
        .status();
}

/// GiroCrabMac links Sparkle.framework dynamically (in-app updates): copy it into
/// Contents/Frameworks and let the binary find it there, like
/// package-macos-app.sh does for release builds. Without it dyld aborts at launch.
fn embed_sparkle(app_dir: &Path, exe: &Path, scratch: &Path) {
    let bin_dir = exe.parent().unwrap();
    let src = [
        bin_dir.join("Sparkle.framework"),
        bin_dir.join("PackageFrameworks/Sparkle.framework"),
        scratch.join(
            "artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework",
        ),
    ]
    .into_iter()
    .find(|p| p.is_dir())
    .unwrap_or_else(|| panic!("Sparkle.framework not found next to {}", bin_dir.display()));

    let frameworks = app_dir.join("Contents/Frameworks");
    let dst = frameworks.join("Sparkle.framework");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&frameworks).unwrap();
    // ditto keeps the framework's symlinks (Versions/Current etc.) intact.
    run(
        Command::new("ditto").arg(&src).arg(&dst),
        "ditto Sparkle.framework",
    );
    // XPC services are only for sandboxed apps; GiroCrab is not sandboxed.
    let _ = std::fs::remove_dir_all(dst.join("Versions/B/XPCServices"));
    let _ = std::fs::remove_file(dst.join("XPCServices"));
    // Removing them breaks Sparkle's own signature: re-sign ad hoc, so the
    // bundle's ad-hoc signature below can seal it.
    run(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&dst),
        "codesign Sparkle.framework",
    );

    // A fresh copy of the binary each build, so the rpath is never present twice.
    run(
        Command::new("install_name_tool")
            .args(["-add_rpath", "@executable_path/../Frameworks"])
            .arg(app_dir.join("Contents/MacOS/GiroCrab")),
        "install_name_tool -add_rpath",
    );
}

/// String Catalogs -> Contents/Resources/<lang>.lproj/{Localizable,InfoPlist}.strings(dict).
/// The SwiftUI code (GiroCrabUI) looks strings up in Bundle.main, i.e. this bundle.
fn compile_localizations(app_dir: &Path) {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let res = app_dir.join("Contents/Resources");
    let _ = std::fs::create_dir_all(&res);
    // Drop translations of a previous build (a removed language must not linger).
    if let Ok(entries) = std::fs::read_dir(&res) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "lproj") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    // InfoPlist: only English; German stays the Info.plist value written above.
    for (catalog, langs) in [
        ("Localizable.xcstrings", &[][..]),
        ("InfoPlist.xcstrings", &["en"][..]),
    ] {
        let src = root.join("GiroCrabApp/Xcode/App").join(catalog);
        let mut cmd = Command::new("xcrun");
        cmd.args(["xcstringstool", "compile"])
            .arg(&src)
            .arg("--output-directory")
            .arg(&res);
        for l in langs {
            cmd.args(["--language", l]);
        }
        clean_env(&mut cmd);
        run(&mut cmd, &format!("xcstringstool compile {catalog}"));
    }
}

/// AppIcon.appiconset (generated from icon.jpeg with sips) -> Contents/Resources/AppIcon.icns
fn make_icns(app_dir: &Path) {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let set = root.join("GiroCrabApp/Xcode/App/Assets.xcassets/AppIcon.appiconset");
    if !set.exists() {
        return;
    }
    let iconset = root.join("target").join("AppIcon.iconset");
    let _ = std::fs::create_dir_all(&iconset);
    for base in [16u32, 32, 128, 256, 512] {
        let _ = std::fs::copy(
            set.join(format!("icon-{base}.png")),
            iconset.join(format!("icon_{base}x{base}.png")),
        );
        let _ = std::fs::copy(
            set.join(format!("icon-{}.png", base * 2)),
            iconset.join(format!("icon_{base}x{base}@2x.png")),
        );
    }
    let res = app_dir.join("Contents/Resources");
    let _ = std::fs::create_dir_all(&res);
    let ok = Command::new("iconutil")
        .arg("-c")
        .arg("icns")
        .arg(&iconset)
        .arg("-o")
        .arg(res.join("AppIcon.icns"))
        .status();
    if !matches!(ok, Ok(s) if s.success()) {
        println!("cargo:warning=iconutil failed – app bundle has no icon");
    }
}
