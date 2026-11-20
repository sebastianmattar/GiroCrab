//! GiroCrab for Linux – Qt6/QML front end on the GiroCrab Rust core.
//!
//! The QML views mirror the SwiftUI views of `GiroCrabApp/Sources/GiroCrabUI`
//! file by file (RootView.swift → Main.qml, SidebarView.swift → SidebarView.qml, …).

mod announcements;
mod banking_policy;
mod bridge;
mod bug_report;
mod dispatch;
mod i18n;
mod ocr;
mod qr_scan;
mod sessions;
mod vault_key;
mod web_login;

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;

use cxx_qt_lib::{QQmlApplicationEngine, QQuickStyle, QString, QUrl};

unsafe extern "C" {
    fn girocrab_prepare_qt();
    fn girocrab_create_app(argc: c_int, argv: *mut *mut c_char, version: *const c_char);
    fn girocrab_exec_app() -> c_int;
    fn girocrab_destroy_app();
    fn _exit(code: c_int) -> !;
}

fn main() {
    // SAFETY: plain Qt setters, called once before the application object exists.
    unsafe { girocrab_prepare_qt() };
    // Decrypted QuickLook copies a crash or force quit may have left behind.
    dispatch::remove_quicklook_files();
    // QApplication (created in C++, cpp/prepare.cpp): the print dialog needs
    // Qt Widgets. argv stays alive for the whole process, as Qt requires.
    let argv: &mut Vec<*mut c_char> = Box::leak(Box::new(
        // args_os: a non-UTF-8 argument (e.g. a file name) must not panic
        std::env::args_os()
            .filter_map(|a| CString::new(a.as_bytes()).ok())
            .map(CString::into_raw)
            .chain(std::iter::once(std::ptr::null_mut()))
            .collect(),
    ));
    let argc = (argv.len() - 1) as c_int;
    let version = CString::new(env!("GIROCRAB_VERSION")).unwrap_or_default();
    // SAFETY: argv is leaked (valid for the program's lifetime) and null-terminated.
    unsafe { girocrab_create_app(argc, argv.as_mut_ptr(), version.as_ptr()) };
    // Settings › Allgemein › Sprache: translator, locale and core texts before any QML loads.
    i18n::setup();
    // Fusion follows the KDE/Plasma palette. Set after the QApplication exists:
    // the KDE platform theme it loads would otherwise pick org.kde.desktop, whose
    // native ToolButton background draws the label a second time.
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        QQuickStyle::set_style(&QString::from("Fusion"));
    }
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/GiroCrab/Main.qml"));
    }
    // SAFETY: the application object exists (created above).
    let code = unsafe { girocrab_exec_app() };
    // Normal quit: the decrypted QuickLook copies go too (start-up cleans up after a crash).
    dispatch::remove_quicklook_files();
    bug_report::clear_temp_dir();
    drop(engine);
    // SAFETY: the engine is dropped; nothing uses the application any more.
    unsafe { girocrab_destroy_app() };
    // „App zurücksetzen“: QML `Settings` may have written values back while closing.
    if dispatch::app_was_reset()
        && let Err(e) = dispatch::remove_settings_file()
    {
        eprintln!("GiroCrab: {e}");
    }
    // "Neu starten" (Settings › Allgemein › Sprache): the settings are written, start anew.
    i18n::relaunch_if_requested();
    // A background load (Core.callAsync) may still be inside the core. `exit()` would run
    // the libraries' exit handlers under its feet (SQLCipher's codec, Qt's QML types): the
    // worker then crashed while decrypting a page, and writing the core dump made quitting
    // take seconds. Everything that must be saved is saved by now (QML `Settings` when the
    // engine went, the vault after every write), so end without exit handlers.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` ends the process at once; nothing runs after it.
    unsafe { _exit(code) }
}
