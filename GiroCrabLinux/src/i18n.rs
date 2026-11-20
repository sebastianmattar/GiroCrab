//! UI language (LanguagePreference.swift): German source texts, English from the Mac app's
//! string catalog plus `i18n/en-linux.json` (build.rs → OUT_DIR/en.json).
//!
//! QML texts go through `qsTr`, which cpp/translator.cpp answers from [`girocrab_tr`];
//! Rust texts that reach the UI go through [`tr`]. The choice is read once at start-up
//! ([`setup`]) and a change takes effect after a restart, like on the Mac.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// German → English, Qt placeholders (`%1`, `%2` …).
const EN_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/en.json"));

static ENGLISH: AtomicBool = AtomicBool::new(false);
static RELAUNCH: AtomicBool = AtomicBool::new(false);

static QUESTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

unsafe extern "C" {
    fn girocrab_language_choice() -> *const c_char;
    fn girocrab_set_language_choice(choice: *const c_char);
    fn girocrab_system_language() -> *const c_char;
    fn girocrab_apply_language(code: *const c_char);
    fn girocrab_language_question() -> *const c_char;
    fn girocrab_acknowledge_system_language();
}

fn english_map() -> &'static HashMap<String, CString> {
    static MAP: OnceLock<HashMap<String, CString>> = OnceLock::new();
    MAP.get_or_init(|| {
        let map: HashMap<String, String> =
            serde_json::from_str(EN_JSON).expect("en.json (written by build.rs)");
        map.into_iter()
            .filter_map(|(de, en)| CString::new(en).ok().map(|en| (de, en)))
            .collect()
    })
}

/// The English text of a German source text (catalog or en-linux.json).
pub fn english(german: &str) -> Option<&'static str> {
    english_map().get(german).and_then(|en| en.to_str().ok())
}

/// A Rust text shown in the UI, in the running language (`%1` … filled by the caller).
pub fn tr(german: &str) -> String {
    if ENGLISH.load(Ordering::Relaxed)
        && let Some(en) = english(german)
    {
        return en.to_string();
    }
    german.to_string()
}

/// cpp/translator.cpp: English for `german` (UTF-8), null if there is none. The pointer
/// stays valid for the whole process.
#[unsafe(no_mangle)]
pub extern "C" fn girocrab_tr(german: *const c_char) -> *const c_char {
    if german.is_null() {
        return std::ptr::null();
    }
    // SAFETY: Qt passes a null-terminated UTF-8 source text.
    let german = unsafe { CStr::from_ptr(german) };
    german
        .to_str()
        .ok()
        .and_then(|g| english_map().get(g))
        .map_or(std::ptr::null(), |en| en.as_ptr())
}

fn c_str(p: *const c_char) -> String {
    // SAFETY: the C++ functions return static null-terminated literals.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// "system", "de" or "en".
pub fn choice() -> String {
    // SAFETY: reads QSettings; the application object exists.
    c_str(unsafe { girocrab_language_choice() })
}

/// The language the app uses when following the system ("de" or "en").
pub fn system_language() -> String {
    // SAFETY: reads QLocale::system(); no preconditions.
    c_str(unsafe { girocrab_system_language() })
}

/// The language of this run ("de" or "en").
pub fn running() -> &'static str {
    if ENGLISH.load(Ordering::Relaxed) {
        "en"
    } else {
        "de"
    }
}

/// Settings › Allgemein › Sprache: stores the choice (takes effect after a restart).
pub fn set_choice(choice: &str) -> Result<(), String> {
    let c = CString::new(choice).map_err(|e| e.to_string())?;
    // SAFETY: c is a valid C string for the duration of the call.
    unsafe { girocrab_set_language_choice(c.as_ptr()) };
    Ok(())
}

/// Called once after the application object exists and before the QML engine loads:
/// QML translator, default locale and the core's language follow the stored choice.
pub fn setup() {
    // LanguagePreference.startupQuestion, decided once per process like RootView on the Mac.
    // SAFETY: plain query of QSettings / QLocale; the application object exists.
    let question = c_str(unsafe { girocrab_language_question() });
    let _ = QUESTION.set(question);
    let code = match choice().as_str() {
        "system" => system_language(),
        chosen => chosen.to_string(),
    };
    ENGLISH.store(code == "en", Ordering::Relaxed);
    let c = CString::new(code.as_str()).unwrap_or_default();
    // SAFETY: the application object exists; c lives for the call.
    unsafe { girocrab_apply_language(c.as_ptr()) };
    girocrab_core::i18n::set_language(code);
}

/// What to ask at start-up: "none", "choose" or "changed:<code>" (see `setup`).
pub fn question() -> &'static str {
    QUESTION.get().map(String::as_str).unwrap_or("none")
}

/// "Deutsch beibehalten" / language chosen on the start screen: asked once per change.
pub fn acknowledge_system_language() {
    // SAFETY: plain QSettings write on the GUI thread.
    unsafe { girocrab_acknowledge_system_language() };
}

/// "Neu starten" (also after „App zurücksetzen“): QML quits, `main` starts a new instance once the old one is gone.
pub fn request_relaunch() {
    RELAUNCH.store(true, Ordering::Relaxed);
}

/// Starts the new instance after "Neu starten" (with the same arguments). In the Flatpak
/// sandbox through the Flatpak portal: a child of this process would end with its sandbox.
pub fn relaunch_if_requested() {
    if !RELAUNCH.load(Ordering::Relaxed) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // After „App zurücksetzen“ the new instance says so once (Mac: toast after the reset).
    const RESET_FLAG: &str = "--app-was-reset";
    let mut args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|a| a != RESET_FLAG)
        .collect();
    if crate::dispatch::app_was_reset() {
        args.push(RESET_FLAG.into());
    }
    let result = if std::path::Path::new("/.flatpak-info").exists() {
        std::process::Command::new("flatpak-spawn")
            .arg(exe)
            .args(args)
            .spawn()
    } else {
        std::process::Command::new(exe).args(args).spawn()
    };
    if let Err(e) = result {
        eprintln!("GiroCrab: Neustart fehlgeschlagen: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::english;

    #[test]
    fn catalog_and_linux_texts_have_english() {
        // From Localizable.xcstrings (placeholders converted) and from en-linux.json.
        assert_eq!(english("Letzte 12 Monate"), Some("Last 12 Months"));
        assert_eq!(english("Speichern && anwenden"), Some("Save && Apply"));
        assert_eq!(english("Tresor-Schlüssel"), Some("Vault Key"));
        assert!(english("%1 – Amazon-Konto").is_some_and(|en| en.contains("%1")));
    }
}
