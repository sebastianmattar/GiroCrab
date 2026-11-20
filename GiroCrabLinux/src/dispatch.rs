//! Name → core call. Each `Vault` method is one line in `vault_methods!`;
//! argument types come from the method signature, names are the Rust
//! parameter names (snake_case), missing arguments are `null` (→ `None`).

use std::sync::{Arc, Mutex};

use girocrab_core::Vault;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

type R = Result<Value, String>;

unsafe extern "C" {
    fn girocrab_grab_window(path: *const std::os::raw::c_char) -> bool;
    fn girocrab_print_pdf(
        path: *const std::os::raw::c_char,
        job_name: *const std::os::raw::c_char,
    ) -> i32;
    fn girocrab_print_pdf_supported() -> bool;
    fn girocrab_grab_window_titled(
        path: *const std::os::raw::c_char,
        title: *const std::os::raw::c_char,
    ) -> bool;
    fn girocrab_copy_secret(text: *const std::os::raw::c_char);
    fn girocrab_clipboard_concealed() -> bool;
    fn girocrab_compose_report(
        base: *const std::os::raw::c_char,
        masks: *const f64,
        count: usize,
        out: *const std::os::raw::c_char,
    ) -> bool;
    fn girocrab_copy_image(path: *const std::os::raw::c_char) -> bool;
    fn girocrab_show_in_file_manager(path: *const std::os::raw::c_char) -> bool;
}

/// The vault on screen (demo or personal) – `AppModel.vault`. `None` = locked.
static VAULT: Mutex<Option<Arc<Vault>>> = Mutex::new(None);
/// The unlocked personal vault – `AppModel.ownVault` (stays open while the demo is shown).
static OWN: Mutex<Option<Arc<Vault>>> = Mutex::new(None);

/// `AppModel.vaultURL`: ~/.local/share/GiroCrab/tresor.girocrab (XDG data dir);
/// the keychain bypass uses a separate file, so the real vault is never opened
/// with the throw-away key.
pub fn vault_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share")
        })
        .join("GiroCrab");
    let _ = std::fs::create_dir_all(&base);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    }
    base.join(if crate::vault_key::keychain_bypassed() {
        "tresor-dev.girocrab"
    } else {
        "tresor.girocrab"
    })
}

/// QSettings of the app (UserDefaults): `~/.config/GiroCrab/GiroCrab.conf` (XDG config dir).
pub fn settings_file() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("GiroCrab/GiroCrab.conf")
}

/// Deletes `path`; a missing file is fine.
fn remove_if_present(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// „App zurücksetzen“ ran in this process: `main` deletes the settings once more after
/// the QML engine is gone (QML `Settings` write pending values back on destruction)
/// and the new instance starts with `--app-was-reset`.
static APP_RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn app_was_reset() -> bool {
    APP_RESET.load(std::sync::atomic::Ordering::SeqCst)
}

/// Removes the settings file (and the lock file QSettings may leave next to it).
pub fn remove_settings_file() -> Result<(), String> {
    let conf = settings_file();
    remove_if_present(&conf)?;
    remove_if_present(&std::path::PathBuf::from(format!(
        "{}.lock",
        conf.display()
    )))
}

/// AppModel+Reset.swift / VaultResources.swift (#241): deletes the vault file with its
/// SQLite side files, the decrypted QuickLook copies, the settings and – last, since an
/// existing file is unreadable without it – the key in the Secret Service. A failed file
/// deletion stops before anything else is touched, so vault and key stay together.
/// Not deleted (as on the Mac): `banking-policy.json` (anti-rollback anchor of the
/// banking lock), documents exported by the user. Enable Banking credentials live in the
/// vault on Linux and go with it.
fn reset_app() -> R {
    // Tests must never reach the real home directory or keyring.
    #[cfg(test)]
    assert!(
        crate::vault_key::keychain_bypassed()
            && std::env::var_os("XDG_DATA_HOME").is_some()
            && std::env::var_os("XDG_CONFIG_HOME").is_some(),
        "reset_app in tests only with GIROCRAB_NO_KEYCHAIN and temporary XDG dirs"
    );
    crate::sessions::cancel_all();
    *VAULT.lock().unwrap() = None;
    OWN.lock().unwrap().take();
    remove_quicklook_files();
    let vault = vault_path();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        remove_if_present(&std::path::PathBuf::from(format!(
            "{}{suffix}",
            vault.display()
        )))?;
    }
    remove_settings_file()?;
    crate::vault_key::destroy()?;
    APP_RESET.store(true, std::sync::atomic::Ordering::SeqCst);
    ok(())
}

fn file_size(p: &std::path::Path) -> i64 {
    std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(-1)
}

/// `AppModel.unlock()` (core part): key from the keyring, open, starter categories.
fn unlock_own(a: &Value) -> R {
    let path = vault_path();
    let allow_create: bool =
        arg::<Option<bool>>(a, "allow_create")?.unwrap_or(false) && file_size(&path) <= 0;
    let u = crate::vault_key::unlock(allow_create)?;
    let v = Vault::open(path.display().to_string(), u.key.to_vec()).map_err(|e| e.to_string())?;
    v.seed_starter_categories().map_err(|e| e.to_string())?;
    let accounts = v.account_count().map_err(|e| e.to_string())?;
    let connections = v.list_bank_connections().map_err(|e| e.to_string())?.len();
    *OWN.lock().unwrap() = Some(v);
    ok(
        json!({ "account_count": accounts, "has_connections": connections > 0, "created_key": u.created }),
    )
}

/// Settings › Sicherheit: restore / adopt a key (rekeys the open vault, then stores it).
fn import_key(a: &Value) -> R {
    let text: String = arg(a, "text")?;
    let key = crate::vault_key::parse_key(&text)?;
    let Some(own) = OWN.lock().unwrap().clone() else {
        return restore_key(&key);
    };
    // Open vault whose key went missing from the keyring meanwhile: without the
    // previous key there is nothing to roll back to, so no rekey – the pasted
    // key must be the one that opens the vault, and is then just stored.
    let Ok(previous) = crate::vault_key::unlock(false) else {
        let _ = own.checkpoint();
        return restore_key(&key);
    };
    own.rekey(key.to_vec()).map_err(|e| e.to_string())?;
    if let Err(e) = crate::vault_key::store(&key) {
        // keep vault and keyring consistent: both on the previous key again
        let _ = own.rekey(previous.key.to_vec());
        return Err(e);
    }
    ok(())
}

/// Locked vault whose key is missing from (or wrong in) the keyring: the key
/// must open the existing vault file before it is stored. Never creates a vault.
fn restore_key(key: &[u8]) -> R {
    let path = vault_path();
    if file_size(&path) <= 0 {
        return Err(crate::i18n::tr(
            "Es gibt noch keinen Tresor, zu dem der Schlüssel passen könnte.",
        ));
    }
    let v = Vault::open(path.display().to_string(), key.to_vec()).map_err(|e| match e {
        girocrab_core::CoreError::WrongKey => {
            crate::i18n::tr("Der Schlüssel passt nicht zu diesem Tresor.")
        }
        e => e.to_string(),
    })?;
    drop(v);
    crate::vault_key::store(key)?;
    ok(())
}

/// Newest request per load channel (`AppModel.loadOffMain`): a background batch
/// whose token is no longer the newest stops before its next call, so fast
/// clicking does not queue one full load per click behind the vault mutex.
static LOAD_TOKENS: Mutex<Option<std::collections::HashMap<String, i64>>> = Mutex::new(None);

fn supersede(channel: String, token: i64) {
    LOAD_TOKENS
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(channel, token);
}

fn is_current_load(channel: &str, token: i64) -> bool {
    LOAD_TOKENS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(channel))
        .is_none_or(|t| *t == token)
}

#[derive(serde::Deserialize)]
struct BatchCall {
    method: String,
    #[serde(default)]
    args: Value,
}

/// Several calls one after the other on one worker thread (`Core.callAsync("batch", …)`):
/// `{ "results": [...] }` in call order, the first error ends the batch, and
/// `{ "cancelled": true }` once a newer request of the same channel superseded it.
fn batch(a: &Value) -> R {
    let channel: Option<String> = arg(a, "channel")?;
    let token: i64 = arg::<Option<i64>>(a, "token")?.unwrap_or(0);
    let calls: Vec<BatchCall> = arg(a, "calls")?;
    let mut results = Vec::with_capacity(calls.len());
    for c in &calls {
        if channel
            .as_deref()
            .is_some_and(|ch| !is_current_load(ch, token))
        {
            return ok(json!({ "cancelled": true }));
        }
        results.push(call(&c.method, &c.args)?);
    }
    ok(json!({ "results": results }))
}

/// AccountSelectionList.save (#346): takes `self: Arc<Vault>` (a held-back fetch is
/// imported right away), so not part of `vault_methods!`.
fn set_connection_account_choices(v: &Arc<Vault>, a: &Value) -> R {
    v.clone()
        .set_connection_account_choices(arg(a, "connection_id")?, arg(a, "enabled_refs")?)
        .map_err(|e| e.to_string())?;
    ok(())
}

/// Sammelüberweisung / Dauerauftrag / Lastschrift: `self: Arc<Self>`, each returns
/// an `Arc<TransferSession>` that goes into the session registry like "begin_transfer".
fn begin_payment_session(v: &Arc<Vault>, method: &str, a: &Value) -> R {
    let secret: String = arg(a, "secret")?;
    let demo: bool = arg(a, "demo")?;
    let v = v.clone();
    let t = match method {
        "begin_batch_transfer" => v.begin_batch_transfer(arg(a, "request")?, secret, demo),
        "begin_standing_order" => v.begin_standing_order(arg(a, "request")?, secret, demo),
        _ => v.begin_direct_debit(arg(a, "request")?, secret, demo),
    }
    .map_err(|e| e.to_string())?;
    ok(crate::sessions::register(
        crate::sessions::Session::Transfer(t),
    ))
}

/// The unlocked own vault (`AppModel.ownVault`), independent of the demo in front.
fn own_vault() -> Result<Arc<Vault>, String> {
    OWN.lock()
        .unwrap()
        .clone()
        .ok_or_else(|| crate::i18n::tr("Tresor ist gesperrt"))
}

/// A path from a QML file dialog (`file://…` URL or plain path).
fn local_path(path: String) -> String {
    path.strip_prefix("file://").unwrap_or(&path).to_string()
}

/// RAH-10 key file: `path` (file dialog) or `bytes`; at most 1 MB.
fn keyfile_bytes(a: &Value) -> Result<Vec<u8>, String> {
    let Some(path) = arg::<Option<String>>(a, "path")? else {
        return arg(a, "bytes");
    };
    let path = local_path(path);
    let size = file_size(std::path::Path::new(&path));
    if !(0..=1_048_576).contains(&size) {
        return Err(crate::i18n::tr("Die Datei ist zu groß oder nicht lesbar."));
    }
    std::fs::read(&path).map_err(|e| format!("{path}: {e}"))
}

/// Text file chosen in a file dialog (Enable Banking .pem key); at most 1 MB.
fn read_text_file(a: &Value) -> R {
    let path: String = arg(a, "path")?;
    let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
    let size = file_size(std::path::Path::new(&path));
    if !(0..=1_048_576).contains(&size) {
        return Err(crate::i18n::tr("Die Datei ist zu groß oder nicht lesbar."));
    }
    ok(std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
}

/// Largest payment list the batch import reads (AppModel+Batch.swift › batchCsvMaxBytes).
const BATCH_CSV_MAX_BYTES: i64 = 2_000_000;

/// AppModel+Batch.swift › importBatchCSV (file part): the chosen or dropped CSV as UTF-8,
/// else Windows-1252 (spreadsheet exports), parsed by the core. Refuses big files unread.
fn read_batch_csv(a: &Value) -> R {
    let path = local_path(arg(a, "path")?);
    let size = file_size(std::path::Path::new(&path));
    if size > BATCH_CSV_MAX_BYTES {
        return Err(crate::i18n::tr("Die Datei ist zu groß (höchstens 2 MB)."));
    }
    let bytes = std::fs::read(&path)
        .ok()
        .filter(|b| b.len() as i64 <= BATCH_CSV_MAX_BYTES)
        .ok_or_else(|| crate::i18n::tr("Die Datei konnte nicht gelesen werden."))?;
    ok(girocrab_core::batch_csv::parse_batch_csv(decode_text(
        bytes,
    )))
}

/// AppModel+Mandates.swift › importMandateCSV (file part): the chosen or dropped member
/// list as UTF-8, else Windows-1252, parsed by the core. Same size limit as the batch list.
fn read_mandate_csv(a: &Value) -> R {
    let path = local_path(arg(a, "path")?);
    let size = file_size(std::path::Path::new(&path));
    if size > BATCH_CSV_MAX_BYTES {
        return Err(crate::i18n::tr("Die Datei ist zu groß (höchstens 2 MB)."));
    }
    let bytes = std::fs::read(&path)
        .ok()
        .filter(|b| b.len() as i64 <= BATCH_CSV_MAX_BYTES)
        .ok_or_else(|| crate::i18n::tr("Die Datei konnte nicht gelesen werden."))?;
    ok(girocrab_core::mandates::parse_mandate_csv(decode_text(
        bytes,
    )))
}

/// UTF-8, else Windows-1252 (`String(data:encoding: .utf8) ?? .windowsCP1252`).
fn decode_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| {
        const HIGH: [char; 32] = [
            '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
            '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
            '\u{9d}', 'ž', 'Ÿ',
        ];
        e.into_bytes()
            .into_iter()
            .map(|b| match b {
                0x80..=0x9f => HIGH[(b - 0x80) as usize],
                _ => char::from(b),
            })
            .collect()
    })
}

/// CSV export of the Abruf-Protokoll (file dialog → path).
fn write_text_file(a: &Value) -> R {
    let path: String = arg(a, "path")?;
    let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
    let text: String = arg(a, "text")?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    ok(())
}

/// VaultBackup.suggestedPassword (VaultBackupUI.swift): three groups of six from 32
/// characters without look-alikes (l/1, o/0), e.g. "kawxe7-hupdyq-bomtiz" – about 90 bits
/// from the kernel's CSPRNG. 256 is a multiple of 32, so `byte % 32` is unbiased.
fn suggested_backup_password() -> Result<String, String> {
    use std::io::Read;
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = zeroize::Zeroizing::new([0u8; 18]);
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes[..]))
        .map_err(|e| e.to_string())?;
    Ok(bytes
        .chunks(6)
        .map(|g| {
            g.iter()
                .map(|b| ALPHABET[usize::from(b % 32)] as char)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-"))
}

pub fn call_json(method: &str, args: &str) -> String {
    let a: Value = if args.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                return json!({ "err": format!("bad arguments for {method}: {e}") }).to_string();
            }
        }
    };
    // A core panic must neither unwind into C++ (Core.call) nor end a worker
    // thread without its `finished` signal (Core.callAsync): it becomes an error.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(method, &a)))
        .unwrap_or_else(|p| {
            let what = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(crate::i18n::tr("Interner Fehler in %1: %2")
                .replace("%1", method)
                .replace("%2", &what))
        });
    match result {
        Ok(v) => json!({ "ok": v }).to_string(),
        Err(e) => json!({ "err": e }).to_string(),
    }
}

fn arg<T: DeserializeOwned>(a: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(a.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("argument `{name}`: {e}"))
}

fn ok<T: serde::Serialize>(v: T) -> R {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn vault() -> Result<Arc<Vault>, String> {
    VAULT
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| crate::i18n::tr("Tresor ist gesperrt"))
}

fn call(method: &str, a: &Value) -> R {
    match method {
        "core_version" => ok(girocrab_core::core_version()),
        // --- Settings › Allgemein › Sprache (LanguagePreference.swift) ---
        // HelpView.swift › HelpCatalog, read by build.rs: topics (de/en) and shortcut labels.
        "help_content" => {
            serde_json::from_str::<Value>(include_str!(concat!(env!("OUT_DIR"), "/help.json")))
                .map_err(|e| e.to_string())
        }
        "language_state" => ok(json!({
            "choice": crate::i18n::choice(),
            "system": crate::i18n::system_language(),
            "running": crate::i18n::running(),
            "question": crate::i18n::question(),
        })),
        "acknowledge_system_language" => {
            crate::i18n::acknowledge_system_language();
            ok(())
        }
        "set_language_choice" => ok(crate::i18n::set_choice(&arg::<String>(a, "choice")?)?),
        "request_relaunch" => {
            crate::i18n::request_relaunch();
            ok(())
        }
        "fints_builtin_product_id_available" => {
            ok(girocrab_core::fints_builtin_product_id_available())
        }
        "account_color_palette" => ok(girocrab_core::user_groups::account_color_palette()),
        // --- account groups / AccountCopyMenu ---
        "bic_for_iban" => ok(girocrab_core::connectors::fints_directory::bic_for_iban(
            arg(a, "iban")?,
        )),
        "parse_amount_to_cents" => {
            ok(girocrab_core::parse_amount_to_cents(arg(a, "text")?).map_err(|e| e.to_string())?)
        }
        "open_demo" => open_demo(),
        "connector_kind_key" => ok(girocrab_core::banking_gate::connector_kind_key(arg(
            a, "kind",
        )?)),
        "banking_policy_refresh" => ok(crate::banking_policy::refresh(
            arg::<Option<bool>>(a, "force")?.unwrap_or(false),
        )),
        // --- Mitteilungen (Announcements.swift → src/announcements.rs) ---
        "announcements_load" => ok(crate::announcements::load(
            &arg::<Option<String>>(a, "json")?.unwrap_or_default(),
            &arg::<Option<String>>(a, "signature")?.unwrap_or_default(),
        )),
        // network (at most every 6 h, only when switched on): called with Core.callAsync
        "announcements_refresh" => ok(crate::announcements::refresh(
            arg::<Option<bool>>(a, "enabled")?.unwrap_or(false),
            arg::<Option<bool>>(a, "force")?.unwrap_or(false),
            arg(a, "last_check_ms")?,
        )),
        "grab_window" => {
            let path =
                std::ffi::CString::new(arg::<String>(a, "path")?).map_err(|e| e.to_string())?;
            let title = arg::<Option<String>>(a, "title")?.unwrap_or_default();
            if title.is_empty() {
                // SAFETY: called through Core.call on the GUI thread; the path outlives the call.
                return ok(unsafe { girocrab_grab_window(path.as_ptr()) });
            }
            let title = std::ffi::CString::new(title).map_err(|e| e.to_string())?;
            // SAFETY: as above; both strings outlive the call.
            ok(unsafe { girocrab_grab_window_titled(path.as_ptr(), title.as_ptr()) })
        }
        // --- Hilfe › Problem melden (BugReport.swift, #301) ---
        "sanitize_report_text" => ok(girocrab_core::report_sanitizer::sanitize_report_text(
            arg(a, "text")?,
            arg(a, "known_terms")?,
        )),
        "bug_report_prepare" => {
            let v = VAULT.lock().unwrap().clone();
            ok(crate::bug_report::prepare(
                v.as_deref(),
                arg(a, "include_log")?,
                arg(a, "demo")?,
            ))
        }
        "bug_report_issue_url" => ok(crate::bug_report::issue_url(
            &arg(a, "info")?,
            &arg::<Vec<String>>(a, "log")?,
            arg(a, "with_screenshot")?,
        )),
        "bug_report_temp_dir" => ok(crate::bug_report::temp_dir()?.display().to_string()),
        "bug_report_clear" => {
            crate::bug_report::clear_temp_dir();
            ok(())
        }
        "bug_report_compose" => {
            // BugReport.compose: black boxes over unit rectangles, saved as PNG.
            let base =
                std::ffi::CString::new(arg::<String>(a, "base")?).map_err(|e| e.to_string())?;
            let out =
                std::ffi::CString::new(arg::<String>(a, "path")?).map_err(|e| e.to_string())?;
            let masks: Vec<[f64; 4]> = arg(a, "masks")?;
            let flat: Vec<f64> = masks.concat();
            // SAFETY: GUI thread (Core.call); strings and the slice outlive the call.
            ok(unsafe {
                girocrab_compose_report(base.as_ptr(), flat.as_ptr(), masks.len(), out.as_ptr())
            })
        }
        "copy_image" => {
            let path =
                std::ffi::CString::new(arg::<String>(a, "path")?).map_err(|e| e.to_string())?;
            // SAFETY: Core.call runs on the GUI thread (QClipboard); the string outlives the call.
            ok(unsafe { girocrab_copy_image(path.as_ptr()) })
        }
        // --- settings / recurring ---
        "copy_secret" => {
            let text =
                std::ffi::CString::new(arg::<String>(a, "text")?).map_err(|e| e.to_string())?;
            // SAFETY: Core.call runs on the GUI thread (QClipboard); the string outlives the call.
            unsafe { girocrab_copy_secret(text.as_ptr()) };
            ok(())
        }
        // ClipboardPaymentCheck (#663): is the clipboard marked secret by a password manager?
        "clipboard_concealed" => {
            // SAFETY: Core.call runs on the GUI thread (QClipboard); no pointers are passed.
            ok(unsafe { girocrab_clipboard_concealed() })
        }
        "read_text_file" => read_text_file(a),
        "write_text_file" => write_text_file(a),
        "check_enable_banking_application" => {
            let v = vault()?;
            let r = crate::sessions::RUNTIME.block_on(v.check_enable_banking_application(
                arg(a, "application_id")?,
                arg(a, "private_key_pem")?,
                arg(a, "redirect_url")?,
            ));
            ok(r.map_err(|e| e.to_string())?)
        }
        "vault_info" => {
            let p = vault_path();
            ok(
                json!({ "path": p.display().to_string(), "size": file_size(&p),
                       "keychain_bypassed": crate::vault_key::keychain_bypassed(),
                       "demo_seed": crate::vault_key::demo_seed_requested() }),
            )
        }
        "unlock_own" => unlock_own(a),
        "use_own" => {
            let own = OWN
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| crate::i18n::tr("Der eigene Tresor ist nicht geöffnet."))?;
            // preview copies are named by document id: none may survive into another vault
            remove_quicklook_files();
            *VAULT.lock().unwrap() = Some(own);
            ok(())
        }
        "has_own_vault" => ok(OWN.lock().unwrap().is_some()),
        "export_key" => ok(crate::vault_key::export_base64()?),
        "import_key" => import_key(a),
        "lock" => {
            // endBankDialogsForLock: no bank dialog keeps running or writing into the vault.
            crate::sessions::cancel_all();
            remove_quicklook_files();
            if let Some(v) = OWN.lock().unwrap().take() {
                let _ = v.checkpoint();
            }
            *VAULT.lock().unwrap() = None;
            ok(())
        }
        "is_unlocked" => ok(VAULT.lock().unwrap().is_some()),
        // --- background loads (AppModel.loadOffMain) ---
        "supersede" => {
            supersede(arg(a, "channel")?, arg(a, "token")?);
            ok(())
        }
        "batch" => batch(a),
        "reset_app" => reset_app(),
        "set_connection_account_choices" => set_connection_account_choices(&vault()?, a),
        // catalog / directory (free functions of the core)
        "bank_catalog" => ok(girocrab_core::bank_catalog()),
        "fints_presets" => ok(girocrab_core::connectors::fints_presets::fints_presets()),
        "search_banks" => ok(girocrab_core::connectors::bank_directory::search_banks(
            arg(a, "query")?,
            arg::<Option<_>>(a, "live_psd2")?.unwrap_or_default(),
            arg::<Option<u32>>(a, "per_group_limit")?.unwrap_or(12),
        )),
        "lookup_fints_bank_by_blz" => ok(
            girocrab_core::connectors::fints_directory::lookup_fints_bank_by_blz(arg(a, "blz")?),
        ),
        "blz_from_bank_input" => {
            ok(girocrab_core::connectors::fints_directory::blz_from_bank_input(arg(a, "input")?))
        }
        "normalize_phone_number" => {
            ok(girocrab_core::connectors::trade_republic::normalize_phone_number(arg(a, "input")?))
        }
        "is_magic_demo_login" => ok(girocrab_core::demo::is_magic_demo_login(&arg::<String>(
            a, "login",
        )?)),
        // bank dialogs
        "begin_sync" => {
            let v = vault()?;
            let s = v
                .begin_sync(arg(a, "connection_id")?, arg(a, "secret")?, arg(a, "demo")?)
                .map_err(|e| core_error_json(&e))?;
            ok(crate::sessions::register(crate::sessions::Session::Sync(s)))
        }
        // Automatischer Abruf (AutoSync.qml, PSD2Consent.swift › autoSyncTick, #546): the same
        // SyncSession without anything that needs a new approval (no comdirect PostBox, #838).
        "begin_quiet_sync" => {
            let v = vault()?;
            let s = v
                .begin_quiet_sync(arg(a, "connection_id")?, arg(a, "secret")?, arg(a, "demo")?)
                .map_err(|e| core_error_json(&e))?;
            ok(crate::sessions::register(crate::sessions::Session::Sync(s)))
        }
        "fetch_account_statements" => {
            let v = vault()?;
            let s = v
                .fetch_account_statements(arg(a, "account_id")?, arg(a, "secret")?, arg(a, "demo")?)
                .map_err(|e| core_error_json(&e))?;
            ok(crate::sessions::register(crate::sessions::Session::Sync(s)))
        }
        "fetch_comdirect_postbox" => {
            let v = vault()?;
            let s = v
                .fetch_comdirect_postbox(
                    arg(a, "connection_id")?,
                    arg(a, "secret")?,
                    arg(a, "demo")?,
                )
                .map_err(|e| core_error_json(&e))?;
            ok(crate::sessions::register(crate::sessions::Session::Sync(s)))
        }
        "begin_transfer" => {
            let v = vault()?;
            let t = v
                .begin_transfer(arg(a, "request")?, arg(a, "secret")?, arg(a, "demo")?)
                .map_err(|e| e.to_string())?;
            ok(crate::sessions::register(
                crate::sessions::Session::Transfer(t),
            ))
        }
        // Sammelüberweisung, Dauerauftrag, Lastschrift-Einreichung (AppModel+Batch,
        // +StandingOrders, +DirectDebitSubmit): the same TransferSession as "begin_transfer",
        // so the TAN/PIN flow of AppModel.qml drives them through session_start/…
        "begin_batch_transfer" | "begin_standing_order" | "begin_direct_debit" => {
            begin_payment_session(&vault()?, method, a)
        }
        // --- Sammelüberweisung / Lastschriften: CSV import (free functions) ---
        "parse_batch_csv" => ok(girocrab_core::batch_csv::parse_batch_csv(arg(a, "text")?)),
        "read_batch_csv" => read_batch_csv(a),
        "read_mandate_csv" => read_mandate_csv(a),
        "parse_mandate_csv" => ok(girocrab_core::mandates::parse_mandate_csv(arg(a, "text")?)),
        // --- Portal: offene Vormerkungen (PortalUI); `self: Arc<Self>` ---
        "portal_open_holds" => ok(vault()?
            .portal_open_holds(arg(a, "connection_id")?, arg(a, "json_bodies")?)
            .map_err(|e| e.to_string())?),
        "discard_portal_holds" => ok(vault()?
            .discard_portal_holds(arg(a, "connection_id")?, arg(a, "ids")?)
            .map_err(|e| e.to_string())?),
        "portal_schema_dump" => ok(girocrab_core::importers::portal_import::portal_schema_dump(
            arg(a, "json_bodies")?,
            arg(a, "dom_rows_json")?,
        )),
        // --- FinTS mit Schlüsseldatei (Rah10AddForm, AppModel+Rah) ---
        // The key file comes as `path` (file dialog) or as `bytes` (JSON array).
        "inspect_rah10_keyfile" => ok(
            girocrab_core::connectors::fints::bank_keys::inspect_rah10_keyfile(
                keyfile_bytes(a)?,
                arg(a, "password")?,
            )
            .map_err(|e| e.to_string())?,
        ),
        "rah10_bank_key_candidates" => {
            // anonymous first-key request over the network: called with Core.callAsync
            let v = vault()?;
            ok(crate::sessions::RUNTIME
                .block_on(v.rah10_bank_key_candidates(arg(a, "blz")?))
                .map_err(|e| e.to_string())?)
        }
        "add_rah10_connection" => ok(vault()?
            .add_rah10_connection(
                keyfile_bytes(a)?,
                arg(a, "password")?,
                arg(a, "display_name")?,
            )
            .map_err(|e| e.to_string())?),
        "add_rah10_connection_with_bank_keys" => ok(vault()?
            .add_rah10_connection_with_bank_keys(
                keyfile_bytes(a)?,
                arg(a, "password")?,
                arg(a, "display_name")?,
                arg::<Option<_>>(a, "bank_keys")?.unwrap_or_default(),
                arg::<Option<_>>(a, "expected_fingerprints")?.unwrap_or_default(),
            )
            .map_err(|e| e.to_string())?),
        // --- Tresor sichern / wiederherstellen (VaultBackupUI, AppModel+Backup) ---
        // Always the own vault (`AppModel.ownVault`), also while the demo is shown.
        "write_backup" => {
            let own = own_vault()?;
            own.write_backup(local_path(arg(a, "dest_path")?), arg(a, "password")?)
                .map_err(|e| e.to_string())?;
            ok(())
        }
        "restore_backup" => {
            let Some(own) = OWN.lock().unwrap().clone() else {
                return Err(crate::i18n::tr(
                    "Bitte zuerst Ihren eigenen Tresor öffnen, dann die Sicherung wiederherstellen.",
                ));
            };
            own.restore_backup(local_path(arg(a, "backup_path")?), arg(a, "password")?)
                .map_err(|e| e.to_string())?;
            remove_quicklook_files();
            ok(())
        }
        "suggested_backup_password" => ok(suggested_backup_password()?),
        // --- import / export ---
        "preview_moneymoney_file" => {
            let (data, file_name) = read_import_file(a)?;
            let p = vault()?
                .preview_moneymoney_import(data, file_name, arg(a, "target_account_id")?)
                .map_err(|e| core_error_json(&e))?;
            ok(p)
        }
        "import_moneymoney_file" => {
            let (data, file_name) = read_import_file(a)?;
            let r = vault()?
                .import_moneymoney_data(
                    data,
                    file_name,
                    arg(a, "target_account_id")?,
                    arg(a, "preserve_categories")?,
                    arg(a, "preserve_notes")?,
                )
                .map_err(|e| core_error_json(&e))?;
            ok(r)
        }
        "preview_moneymoney_database" => {
            let p = vault()?
                .preview_moneymoney_database(
                    arg(a, "path")?,
                    arg(a, "password")?,
                    arg(a, "statements_dir")?,
                )
                .map_err(|e| core_error_json(&e))?;
            ok(p)
        }
        "import_moneymoney_database" => import_moneymoney_database(a),
        "moneymoney_import_progress" => ok(MM_PROGRESS.lock().unwrap().clone()),
        "cancel_moneymoney_import" => {
            MM_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
            ok(())
        }
        "export_bookings_csv_file" => {
            let csv = vault()?
                .export_bookings_csv(arg(a, "filter")?)
                .map_err(|e| e.to_string())?;
            std::fs::write(arg::<String>(a, "path")?, csv).map_err(|e| e.to_string())?;
            ok(())
        }
        // --- Umsatz-Export (ExportSheet.swift): CSV / CAMT.053 from the core, files written here ---
        "export_csv" => ok(girocrab_core::export::export_csv(
            arg(a, "rows")?,
            arg(a, "columns")?,
            arg(a, "balance_lines")?,
        )),
        "save_export_file" => save_export_file(a),
        "print_pdf" => print_pdf(a),
        "print_web_assets" => print_web_assets(),
        "psd2_catalog" => ok(girocrab_core::connectors::bank_directory::psd2_catalog()),
        // documents: files are read / written here, bytes never travel through JSON
        "attach_file_to_booking" => {
            let path: String = arg(a, "path")?;
            let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let name = file_name_of(&path);
            ok(vault()?
                .attach_document_to_booking(arg(a, "booking_id")?, name, bytes)
                .map_err(|e| e.to_string())?)
        }
        "auto_match_file" => {
            let path: String = arg(a, "path")?;
            let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            // photos: OCR text as the hint (Vision on the Mac); before the vault is touched
            let hint = crate::ocr::extract(&bytes);
            ok(vault()?
                .auto_match_document(file_name_of(&path), bytes, hint)
                .map_err(|e| e.to_string())?)
        }
        "document_temp_file" => document_temp_file(arg(a, "doc_id")?, arg(a, "mime_type")?),
        // „Im Dateimanager anzeigen“ (AppModel.showInFinder): false = no file manager answered.
        // May wait for the bus to start one: called with Core.callAsync.
        "show_in_file_manager" => {
            let path =
                std::ffi::CString::new(arg::<String>(a, "path")?).map_err(|e| e.to_string())?;
            // SAFETY: a blocking D-Bus call on its own connection; the string outlives it.
            ok(unsafe { girocrab_show_in_file_manager(path.as_ptr()) })
        }
        "session_start" | "session_submit" | "session_poll" | "session_confirm_payee" => {
            session_step(method, a)
        }
        // AppModel.cancelTransferSession: for a transfer the bank's answer comes back as
        // `{"outcome": CancelOutcome}`. Anything but `Cancelled` keeps the session
        // registered, so "Status erneut prüfen" can call session_cancel again (the core
        // re-checks the bank before it resends anything).
        "session_cancel" => {
            let id: u64 = arg(a, "session")?;
            let outcome = match crate::sessions::get(id) {
                Ok(s) => crate::sessions::RUNTIME.block_on(async {
                    match s {
                        crate::sessions::Session::Sync(s) => {
                            s.cancel().await;
                            None
                        }
                        crate::sessions::Session::Transfer(t) => Some(t.cancel().await),
                    }
                }),
                Err(_) => None,
            };
            let settled = !matches!(
                outcome,
                Some(ref o) if *o != girocrab_core::connectors::CancelOutcome::Cancelled
            );
            if settled {
                crate::sessions::remove(id);
            }
            ok(json!({ "outcome": outcome }))
        }
        // --- transfer ---
        "validate_iban" => ok(girocrab_core::connectors::validate_iban(arg(a, "iban")?)),
        "parse_payment_text" => ok(girocrab_core::payment_input::parse_payment_text(arg(
            a, "text",
        )?)),
        "parse_epc_payload" => ok(girocrab_core::payment_input::parse_epc_payload(arg(
            a, "payload",
        )?)),
        "parse_amount_loose" => ok(girocrab_core::payment_input::parse_amount_loose(arg(
            a, "text",
        )?)),
        "girocode_payload" => ok(girocrab_core::payment_input::girocode_payload(
            arg(a, "name")?,
            arg(a, "iban")?,
            arg(a, "bic")?,
            arg(a, "amount_cents")?,
            arg(a, "purpose")?,
        )),
        "girocode_matrix" => ok(girocrab_core::payment_input::girocode_matrix(arg(
            a, "payload",
        )?)),
        "read_invoice" => read_invoice(a),
        // --- Sachwerte --- (gold prices over the network: called with Core.callAsync)
        "asset_weight_presets" => ok(girocrab_core::assets::asset_weight_presets()),
        "refresh_asset_prices" => {
            let v = vault()?;
            let force: bool = arg(a, "force")?;
            ok(crate::sessions::RUNTIME
                .block_on(v.refresh_asset_prices(force))
                .map_err(|e| e.to_string())?)
        }
        // --- Händler-Logos (AppModel+Logos.swift, #33) --- network: called with Core.callAsync
        "merchant_logo" => merchant_logo(&*vault()?, a),
        // --- portal logins --- (src/web_login.rs: browser cookies, Amex/Amazon/FNZ/quirion/Schwäbisch Hall/EquatePlus imports)
        m if crate::web_login::handles(m) => crate::web_login::call(m, a, vault),
        _ => {
            let v = vault()?;
            dispatch_vault(&v, method, a)
                .unwrap_or_else(|| Err(format!("unknown core method `{method}`")))
        }
    }
}

/// Network favicon requests started by `merchant_logo` (tests: none while logos are off).
static LOGO_FETCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// AppModel+Logos.swift › logo(for:): the logo cached in the vault for the first candidate
/// domain that has one, else the favicon of the selected provider (network, worker thread).
/// Nothing – not even a cached logo, and no request – while online logos are off (#422).
/// Answer: `{ url: "data:…;base64,…", pixels }` (decodeLogo: `pixels` = largest width) or null.
fn merchant_logo(v: &Vault, a: &Value) -> R {
    let candidates: Vec<String> = arg(a, "candidates")?;
    let e = |e: girocrab_core::CoreError| e.to_string();
    if candidates.is_empty() || v.online_logo_provider().map_err(e)? == "off" {
        return ok(Value::Null);
    }
    for d in &candidates {
        if let Some(bytes) = v.get_merchant_logo_png(d.clone()).map_err(e)? {
            return ok(logo_payload(&bytes));
        }
    }
    LOGO_FETCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let found = crate::sessions::RUNTIME
        .block_on(v.fetch_merchant_logo(candidates))
        .map_err(e)?;
    ok(found.map(|l| logo_payload(&l.bytes)).unwrap_or(Value::Null))
}

fn png_width(b: &[u8]) -> Option<u32> {
    (b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n"))
        .then(|| u32::from_be_bytes([b[16], b[17], b[18], b[19]]))
}

/// AppModel.decodeLogo: an ICO keeps only its largest image (Qt would draw the first one);
/// `pixels` is that image's width, the "sharp logo" threshold of MerchantSignet.
fn logo_payload(bytes: &[u8]) -> Value {
    let (data, mime, pixels): (Vec<u8>, &str, u32) = if let Some(w) = png_width(bytes) {
        (bytes.to_vec(), "image/png", w)
    } else if let Some((dir, entry, w)) = largest_ico_entry(bytes) {
        match png_width(entry) {
            Some(pw) => (entry.to_vec(), "image/png", pw),
            None => {
                // a one-image ICO around the largest bitmap
                let mut ico = vec![0, 0, 1, 0, 1, 0];
                ico.extend_from_slice(&dir[..8]);
                ico.extend_from_slice(&(entry.len() as u32).to_le_bytes());
                ico.extend_from_slice(&22u32.to_le_bytes());
                ico.extend_from_slice(entry);
                (ico, "image/x-icon", w)
            }
        }
    } else if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        (bytes.to_vec(), "image/gif", w)
    } else {
        // JPEG, WebP, …: Qt reads them by content; size unknown = a small logo
        (bytes.to_vec(), "application/octet-stream", 0)
    };
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    json!({ "url": format!("data:{mime};base64,{b64}"), "pixels": pixels })
}

/// The largest image of an ICO file: its directory entry, its bytes and its width
/// (0 in the directory = 256).
fn largest_ico_entry(bytes: &[u8]) -> Option<(&[u8], &[u8], u32)> {
    if bytes.len() < 6 || bytes[..4] != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut best: Option<(&[u8], &[u8], u32)> = None;
    for i in 0..count {
        let Some(dir) = bytes.get(6 + 16 * i..6 + 16 * (i + 1)) else {
            break;
        };
        let size = u32::from_le_bytes([dir[8], dir[9], dir[10], dir[11]]) as usize;
        let offset = u32::from_le_bytes([dir[12], dir[13], dir[14], dir[15]]) as usize;
        let Some(data) = offset
            .checked_add(size)
            .and_then(|end| bytes.get(offset..end))
        else {
            continue;
        };
        let w = match png_width(data) {
            Some(pw) => pw,
            None if dir[0] == 0 => 256,
            None => dir[0] as u32,
        };
        if best.is_none_or(|(_, _, bw)| w > bw) {
            best = Some((dir, data, w));
        }
    }
    best
}

/// A MoneyMoney export file (`path`) as bytes plus its file name, which the
/// importer uses for the account name.
fn read_import_file(a: &Value) -> Result<(Vec<u8>, String), String> {
    let path: String = arg(a, "path")?;
    let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok((data, name))
}

/// Progress of the running MoneyMoney database import (`ImportProgressRelay`
/// in Swift): written by the importing worker thread, polled by the view.
static MM_PROGRESS: Mutex<
    Option<girocrab_core::importers::moneymoney_db::MoneyMoneyDbImportProgress>,
> = Mutex::new(None);
/// "Import abbrechen": the next progress callback tells the core to stop.
static MM_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct ImportProgressRelay;

impl girocrab_core::importers::moneymoney_db::MoneyMoneyImportObserver for ImportProgressRelay {
    fn progress(
        &self,
        progress: girocrab_core::importers::moneymoney_db::MoneyMoneyDbImportProgress,
    ) -> bool {
        *MM_PROGRESS.lock().unwrap() = Some(progress);
        !MM_CANCEL.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// `importMoneymoneyDatabaseWithProgress` – run through `Core.callAsync`.
fn import_moneymoney_database(a: &Value) -> R {
    let v = vault()?;
    MM_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
    *MM_PROGRESS.lock().unwrap() = None;
    let r = v
        .import_moneymoney_database_with_progress(
            arg(a, "path")?,
            arg(a, "password")?,
            arg(a, "options")?,
            Arc::new(ImportProgressRelay),
        )
        .map_err(|e| core_error_json(&e))?;
    ok(r)
}

mod print_assets {
    include!(concat!(env!("OUT_DIR"), "/print_web_assets.rs"));
}

/// Stylesheet and paginator script of the Mac printout (PrintReport.swift, read by
/// build.rs) plus whether a rendered PDF can go to a printer (Qt's PDF image plugin).
fn print_web_assets() -> R {
    // SAFETY: plain query, no arguments.
    let pdf = unsafe { girocrab_print_pdf_supported() };
    Ok(serde_json::json!({
        "css": print_assets::PRINT_CSS,
        "paginator": print_assets::PRINT_PAGINATOR,
        "pdf_supported": pdf,
    }))
}

/// AppModel+Printing.swift › presentExportFile: writes an export (`text`, or the PDF
/// WebPrinter.qml rendered at `from_path`, which is removed afterwards). With `path`
/// (save dialog, which already asked about replacing) exactly there; otherwise into
/// `dir` as `filename`, or the first free `Name-2.ext`, `Name-3.ext`, … so an export
/// never replaces an existing file without asking; a `dir` that is gone falls back to
/// `fallback_dir` (Downloads, created if needed). Returns the path written. `discard`
/// (save dialog cancelled) only removes the rendered PDF.
fn save_export_file(a: &Value) -> R {
    use std::io::Write;
    let text = arg::<Option<String>>(a, "text")?;
    let from = arg::<Option<String>>(a, "from_path")?;
    if arg::<Option<bool>>(a, "discard")?.unwrap_or(false) {
        if let Some(p) = from {
            let _ = std::fs::remove_file(p);
        }
        return ok(());
    }
    let bytes = match (&text, &from) {
        (Some(t), _) => t.clone().into_bytes(),
        (None, Some(p)) => std::fs::read(p).map_err(|e| format!("{p}: {e}"))?,
        (None, None) => return Err("argument `text` or `from_path` missing".into()),
    };
    let written = if let Some(path) = arg::<Option<String>>(a, "path")? {
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        path
    } else {
        let mut dir = std::path::PathBuf::from(arg::<String>(a, "dir")?);
        if !dir.is_dir()
            && let Some(fallback) = arg::<Option<String>>(a, "fallback_dir")?
        {
            dir = fallback.into();
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let filename: String = arg(a, "filename")?;
        let (stem, ext) = match filename.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (filename.clone(), String::new()),
        };
        let mut n = 1;
        loop {
            let name = if n == 1 {
                filename.clone()
            } else {
                format!("{stem}-{n}{ext}")
            };
            let path = dir.join(&name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    f.write_all(&bytes).map_err(|e| e.to_string())?;
                    break path.display().to_string();
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    };
    if let Some(p) = from {
        let _ = std::fs::remove_file(p);
    }
    ok(written)
}

/// Prints a PDF that WebPrinter.qml rendered from the Mac markup: system print dialog,
/// "In Datei drucken" copies the PDF, a printer gets the pages at its resolution.
/// "printed" / "cancelled". The file is removed afterwards.
fn print_pdf(a: &Value) -> R {
    let path: String = arg(a, "path")?;
    let job = arg::<Option<String>>(a, "job_name")?.unwrap_or_default();
    let (p, j) = (
        std::ffi::CString::new(path.clone()).map_err(|e| e.to_string())?,
        std::ffi::CString::new(job).map_err(|e| e.to_string())?,
    );
    // SAFETY: called through Core.call on the GUI thread; the strings outlive the call.
    let r = unsafe { girocrab_print_pdf(p.as_ptr(), j.as_ptr()) };
    let _ = std::fs::remove_file(&path);
    match r {
        1 => ok("printed"),
        0 => ok("cancelled"),
        _ => Err(crate::i18n::tr(
            "Der Druckauftrag konnte nicht erstellt werden.",
        )),
    }
}

/// One step of a bank dialog; a finished or failed dialog leaves the registry.
fn session_step(method: &str, a: &Value) -> R {
    use crate::sessions::{RUNTIME, Session};
    let id: u64 = arg(a, "session")?;
    let s = crate::sessions::get(id)?;
    let code: Option<String> = arg(a, "code")?;
    let result = RUNTIME.block_on(async {
        match (&s, method) {
            (Session::Sync(s), "session_start") => {
                s.start().await.map(|x| serde_json::to_value(x).unwrap())
            }
            (Session::Sync(s), "session_submit") => s
                .submit_code(code.unwrap_or_default())
                .await
                .map(|x| serde_json::to_value(x).unwrap()),
            (Session::Sync(_), "session_confirm_payee") => {
                Err(girocrab_core::CoreError::SessionClosed)
            }
            (Session::Sync(s), _) => s.poll().await.map(|x| serde_json::to_value(x).unwrap()),
            (Session::Transfer(t), "session_start") => {
                t.start().await.map(|x| serde_json::to_value(x).unwrap())
            }
            (Session::Transfer(t), "session_submit") => t
                .submit_code(code.unwrap_or_default())
                .await
                .map(|x| serde_json::to_value(x).unwrap()),
            (Session::Transfer(t), "session_confirm_payee") => t
                .confirm_payee()
                .await
                .map(|x| serde_json::to_value(x).unwrap()),
            (Session::Transfer(t), _) => t.poll().await.map(|x| serde_json::to_value(x).unwrap()),
        }
    });
    match result {
        Ok(step) => {
            let last = ["Finished", "Done", "InstantDeclined", "OutcomeUnknown"];
            if last.iter().any(|k| step.get(k).is_some()) {
                crate::sessions::remove(id);
            }
            ok(crate::sessions::with_image_url(step))
        }
        Err(e) => {
            crate::sessions::remove(id);
            Err(core_error_json(&e))
        }
    }
}

/// Errors keep their CoreError variant (`{"kind": "Bank", "code": …}`) so the model
/// can react like the Swift app (MissingSetting → setup sheet, BrowserLoginRequired, …).
fn core_error_json(e: &girocrab_core::CoreError) -> String {
    let mut v = serde_json::to_value(format!("{e:?}")).unwrap_or_default();
    if let Value::String(dbg) = &v {
        let kind = dbg
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_string();
        v = json!({ "kind": kind, "message": e.to_string(), "debug": dbg });
    }
    format!("\u{1}{v}")
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// QuickLook copy (`AppModel.quickLookURL`): a private, owner-only file in
/// the folder of `prepared_quicklook_dir`. Only the few documents on screen
/// keep a copy (the preview column and the receipt card in the inspector show
/// two at once): asking again for a shown document reuses its file, a new one
/// removes all but the newest `QUICKLOOK_KEEP - 1` others. All go on lock / quit.
fn document_temp_file(doc_id: i64, mime: String) -> R {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let bytes = vault()?
        .get_document_bytes(doc_id)
        .map_err(|e| e.to_string())?;
    let dir = prepared_quicklook_dir().ok_or_else(|| {
        crate::i18n::tr("Der Ordner für die Vorschau ist nicht sicher und wurde nicht verwendet.")
    })?;
    let ext = match mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/heic" => "heic",
        _ => "pdf",
    };
    let name = format!("GiroCrab-Beleg-{doc_id}.{ext}");
    let path = dir.join(&name);
    // a view that shows this document already reads this file: replacing it
    // under the viewer breaks its page decoding
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        // shown again = newest: the clean-up below keeps it while two other documents open
        if let Ok(f) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc_o_nofollow())
            .open(&path)
        {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return ok(path.display().to_string());
    }
    // few decrypted copies at a time, newest kept (viewers that still show an
    // older one keep their open file handle)
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| {
                let modified = e.metadata().and_then(|m| m.modified()).ok();
                (modified, e.path())
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (_, old) in files.into_iter().skip(QUICKLOOK_KEEP - 1) {
            let _ = std::fs::remove_file(old);
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc_o_nofollow())
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(&bytes).map_err(|e| e.to_string())?;
    ok(path.display().to_string())
}

/// Decrypted preview copies kept at once (see `document_temp_file`).
const QUICKLOOK_KEEP: usize = 3;

const fn libc_o_nofollow() -> i32 {
    0o400000 // O_NOFOLLOW on Linux
}

unsafe extern "C" {
    fn getuid() -> u32;
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { getuid() }
}

/// $XDG_RUNTIME_DIR/girocrab-quicklook; without a runtime dir a per-user
/// folder in the temp dir (/tmp/girocrab-quicklook-<uid>).
fn quicklook_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(run) => run.join("girocrab-quicklook"),
        None => std::env::temp_dir().join(format!("girocrab-quicklook-{}", current_uid())),
    }
}

/// `AppModel.preparedQuickLookDirectory`: creates the folder owner-only and
/// refuses a symlink or a folder of another user in its place.
fn prepared_quicklook_dir() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let dir = quicklook_dir();
    match std::fs::symlink_metadata(&dir) {
        Ok(m) => {
            if !m.file_type().is_dir() || m.uid() != current_uid() {
                return None;
            }
            if m.mode() & 0o077 != 0 {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
            }
            Some(dir)
        }
        Err(_) => {
            std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
            Some(dir)
        }
    }
}

/// Deletes every decrypted QuickLook copy (lock, quit, next launch). A folder
/// that is not ours (symlink, other user) is left alone.
pub fn remove_quicklook_files() {
    use std::os::unix::fs::MetadataExt;
    let dir = quicklook_dir();
    if let Ok(m) = std::fs::symlink_metadata(&dir)
        && m.file_type().is_dir()
        && m.uid() == current_uid()
    {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `AppModel.makeDemoVault`: throw-away vault in the temp dir with fixed key.
fn open_demo() -> R {
    remove_quicklook_files();
    let path = std::env::temp_dir().join("GiroCrab-Demo.girocrab");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let key: Vec<u8> = (0..32u32).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect();
    let v = Vault::open(path.display().to_string(), key).map_err(|e| e.to_string())?;
    v.seed_demo_data().map_err(|e| e.to_string())?;
    *VAULT.lock().unwrap() = Some(v);
    ok(())
}

fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Largest invoice file read at all, and the most one PDF stream may inflate to
/// (a ZUGFeRD XML is a few kB; this stops a zip bomb).
const MAX_INVOICE_FILE: u64 = 32 * 1024 * 1024;
const MAX_INFLATED_STREAM: usize = 8 * 1024 * 1024;

/// XML bytes as text: UTF-8, or ISO-8859-1 when the bytes are not valid UTF-8
/// (XMLParser in Swift honours the declared encoding).
fn xml_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// PaymentParser.swift › embeddedInvoiceXML: scans PDF streams (inflating
/// Flate ones) for a CrossIndustryInvoice (ZUGFeRD / Factur-X) document.
fn embedded_invoice_xml(pdf: &[u8]) -> Option<String> {
    const MARKER: &[u8] = b"CrossIndustryInvoice";
    let mut cursor = 0;
    while let Some(s) = find_bytes(pdf, b"stream", cursor) {
        let body_start = s + b"stream".len();
        let e = find_bytes(pdf, b"endstream", body_start)?;
        cursor = e + b"endstream".len();
        let mut body = &pdf[body_start..e];
        if body.first() == Some(&0x0D) {
            body = &body[1..];
        }
        if body.first() == Some(&0x0A) {
            body = &body[1..];
        }
        if find_bytes(body, MARKER, 0).is_some() {
            return Some(xml_text(body));
        }
        if body.len() > 8
            && body[0] == 0x78
            && let Ok(inflated) =
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(body, MAX_INFLATED_STREAM)
            && find_bytes(&inflated, MARKER, 0).is_some()
        {
            return Some(xml_text(&inflated));
        }
    }
    None
}

/// PaymentParser.parseFile (file part): a dropped invoice as ZUGFeRD XML, the
/// GiroCode payload of an image or PDF (`epc`, src/qr_scan.rs) and the text layer
/// of a PDF or the OCR text of a photo (used when neither is usable); QML parses
/// the fields.
fn read_invoice(a: &Value) -> R {
    let path: String = arg(a, "path")?;
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_INVOICE_FILE {
        return Err(crate::i18n::tr("Die Datei ist zu groß."));
    }
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    let is_pdf = data.starts_with(b"%PDF");
    let xml = if is_pdf {
        embedded_invoice_xml(&data)
    } else if find_bytes(&data[..data.len().min(4096)], b"CrossIndustryInvoice", 0).is_some() {
        Some(xml_text(&data))
    } else {
        None
    };
    // PaymentParser.parseFile: embedded XML → EPC QR → text layer. The XML is read in QML
    // (parseInvoiceXML), so the GiroCode is looked for on every image or PDF.
    let epc = if is_pdf || xml.is_none() {
        crate::qr_scan::epc_payload(&data)
    } else {
        None
    };
    // a usable GiroCode (parseEPC: "BCD" and at least 7 lines) makes the text unnecessary
    let text = if epc.as_ref().is_some_and(|p| p.lines().count() >= 7) {
        String::new()
    } else if is_pdf {
        girocrab_core::documents::extract_pdf_text(&data)
    } else if xml.is_none() {
        // a photo of an invoice: OCR text (ReceiptText.extract), when tesseract is there
        crate::ocr::extract(&data).unwrap_or_default()
    } else {
        String::new()
    };
    ok(json!({ "xml": xml, "epc": epc, "text": text }))
}

/// Vault methods, plus the ones whose errors the sheet shows differently.
fn dispatch_vault(v: &Vault, method: &str, a: &Value) -> Option<R> {
    match method {
        // Umsatz-Export: CAMT.053 straight from the vault (#483). ExportSheet.swift shows
        // a refusal (`InvalidInput`: loan/depot, gap in the marked bookings) as its plain
        // message, without the „Ungültige Eingabe:“ prefix of other errors.
        "camt053_export" => Some(camt053_export(v, a)),
        // The Linux cashflow card is the overview: an overall money figure
        // (#337) — only the effective total set contributes, like the Windows
        // overview card. Not in the macro below because it carries no
        // `respect_in_total` from QML.
        "cashflow_by_period" => Some(cashflow_by_period_totals(v, a)),
        _ => dispatch_vault_methods(v, method, a),
    }
}

fn cashflow_by_period_totals(v: &Vault, a: &Value) -> R {
    ok(v.cashflow_by_period(
        arg(a, "granularity")?,
        arg(a, "count")?,
        arg::<Option<Vec<i64>>>(a, "account_ids")?,
        true,
    )
    .map_err(|e| e.to_string())?)
}

fn camt053_export(v: &Vault, a: &Value) -> R {
    ok(v.camt053_export(arg(a, "request")?).map_err(|e| match e {
        girocrab_core::CoreError::InvalidInput { message } => message,
        e => e.to_string(),
    })?)
}

macro_rules! vault_methods {
    ($($name:ident ( $($p:ident),* ))*) => {
        fn dispatch_vault_methods(v: &Vault, method: &str, _a: &Value) -> Option<R> {
            match method {
                $( stringify!($name) => Some((|| -> R {
                    ok(v.$name($(arg(_a, stringify!($p))?),*).map_err(|e| e.to_string())?)
                })()), )*
                _ => None,
            }
        }
    };
}

vault_methods! {
    // vault.rs / demo.rs
    is_empty() account_count() has_account_history() checkpoint()
    // accounts
    list_bank_connections() list_accounts() list_account_groups()
    connection_key(connection_id) add_bank_connection(connector, display_name, login_name)
    add_fints_connection(blz, url, display_name, login_name, tan_medium)
    add_enable_banking_connection(aspsp, country, display_name, login_name)
    delete_bank_connection(connection_id) delete_account(account_id)
    update_bank_connection(connection_id, display_name, login_name)
    set_connection_pin(connection_id, pin) connection_status(connection_id)
    clear_connection_session(connection_id) set_connection_session(connection_id, cookies)
    switch_comdirect_access(connection_id)
    // Enable Banking application per login (#824)
    connection_enable_banking_app(connection_id) connection_enable_banking_key(connection_id)
    list_connection_enable_banking_apps()
    set_connection_enable_banking_app(connection_id, application_id, private_key_pem)
    set_comdirect_postbox(connection_id, enabled) comdirect_postbox_enabled(connection_id)
    set_document_account(doc_id, account_id)
    fints_product_id_mode() set_fints_product_id_mode(mode) fints_product_id_source()
    list_holdings(account_id) balance_history(account_ids, days)
    rename_account(account_id, name)
    // --- depot / loans / dashboard ---
    set_account_initial_balance(account_id, cents) investment_dashboard(months)
    // --- Vermögen (WealthView) ---
    list_security_transactions(account_id)
    list_tangible_account_ids() list_all_asset_items()
    // --- Sachwerte (TangibleAssetsUI) ---
    create_tangible_account(name) list_asset_items(account_id)
    save_asset_item(account_id, item_id, setup) delete_asset_item(item_id)
    set_asset_quantity(item_id, date, quantity) delete_asset_quantity(item_id, date)
    set_asset_value(item_id, date, value_cents) delete_asset_value(item_id, date)
    sell_asset(item_id, date, count, price_cents) set_asset_price(symbol, date, price_cents)
    asset_price_on(symbol, date) asset_unit_value_on(item_id, date) asset_item_history(item_id)
    // --- loans / opening balance ---
    list_manual_loans() list_loan_links() create_manual_loan(setup)
    update_manual_loan(account_id, setup) preview_manual_loan(account_id, setup, limit)
    set_booking_loan(transaction_id, loan_account_id)
    set_account_opening_balance(account_id, cents, date)
    preview_account_opening_balance(account_id, date)
    // --- documents ---
    list_documents_for_booking(booking_id) list_all_documents(filter)
    link_document(doc_id, booking_id) delete_document(doc_id)
    export_document(doc_id, dest_path) export_all_documents(dest_dir)
    auto_statements_enabled(account_id) set_auto_statements(account_id, enabled)
    // user groups
    list_user_groups()
    // --- sidebar drag & drop ---
    move_account_to_group(account_id, group_uuid, index) set_user_group_parent(uuid, parent_uuid)
    reorder_user_groups(uuids) reorder_accounts_in_group(group_uuid, account_ids)
    // --- account groups ---
    create_user_group(name, sf_symbol) update_user_group(uuid, name, sf_symbol, include_in_total)
    delete_user_group(uuid) merge_user_groups(from_uuid, into_uuid) set_user_group_color(uuid, color_hex)
    set_account_archived(account_id, archived) set_account_color(account_id, color_hex)
    set_account_symbol(account_id, sf_symbol) reset_account_symbol(account_id)
    set_account_kind(account_id, kind) set_account_owner(account_id, owner)
    set_account_note(account_id, note)
    // categories
    list_categories(date_from, date_to)
    create_category(name, parent_id, sf_symbol, color_hex)
    // bookings
    count_transactions(filter) list_transactions(filter)
    set_transaction_category(transaction_id, category_id)
    set_transaction_checked(transaction_id, checked)
    set_checked_for_ids(ids, checked)
    update_transaction_note(transaction_id, note)
    update_transaction_text(transaction_id, counterparty, purpose)
    set_booking_salary_flag(booking_id, flag) salary_cycle(account_ids)
    mark_booking_recurring(booking_id, interval, custom_dates)
    ignore_recurring(series_key) reset_recurring(series_key)
    booking_date(booking_id)
    // insights / planning / recurring
    merchant_stats(merchant_key) merchant_history(merchant_key, account_id)
    selection_flow(account_ids, date_from, date_to)
    list_category_budget_progress(year, month)
    list_recurring(account_ids) recurring_month(account_ids, year, month)
    // settings
    list_settings() get_setting(key) set_setting(key, value)
    // --- categorizer / rules ---
    list_rules() save_rule(rule) delete_rule(rule_id) preview_rule(rule, limit)
    apply_rules(only_uncategorized)
    update_category(id, name, parent_id, sf_symbol, color_hex) delete_category(id)
    set_category_budget(category_id, budget_cents)
    list_transaction_splits(transaction_id) set_transaction_splits(transaction_id, splits)
    // --- import / export ---
    export_bookings_csv(filter)
    // --- transfer ---
    suggest_recipients(query, limit) list_known_recipients(query) set_recipient_hidden(iban, hidden)
    list_transfer_templates() save_transfer_template(template) delete_transfer_template(id)
    // --- settings / recurring ---
    list_ignored_recurring(account_ids)
    list_sync_log(filter) clear_sync_log()
    // --- Problem melden (#301) ---
    counterparty_names(limit)
    // --- main window / editing ---
    set_checked_for_filter(filter, checked) recurring_snapshot(account_ids, year, month)
    log_sync(connection_id, source, outcome, message, imported)
    // --- account selection per login (AccountSelectionView.swift, #346) ---
    connection_account_choices(connection_id) pending_account_selection_connections()
    seed_demo_account_selection()
    // --- Seitenleiste: Zähler und gefilterte Kategorien (AppModel+Loading/Selection) ---
    count_planned_payments() count_active_mandates()
    list_categories_filtered(date_from, date_to, account_ids, checked, direction)
    category_history_by_unit(category_id, account_ids, date_from, date_to, key_length)
    // --- Geplante Zahlungen (AppModel+Planned, PlannedPaymentsView) ---
    list_planned_payments(from, to, account_ids) planned_payment_totals(from, to, account_ids)
    // --- Daueraufträge (AppModel+StandingOrders, StandingOrderSheet) ---
    list_standing_orders(account_ids) standing_order_limits(account_id, action)
    // --- Lastschriften / Mandate (AppModel+Mandates, DirectDebitsView, MandateInspector) ---
    list_mandates(account_id) save_mandate(id, input) delete_mandate(id)
    import_mandate_rows(account_id, rows) import_pain008(account_id, xml)
    export_mandates_csv(account_id)
    creditor_id(account_id) set_creditor_id(account_id, creditor_id) suggested_creditor_id()
    build_collection_files(account_id, items, collection_date)
    mark_collected(mandate_ids, due_date)
    mandate_bookings(mandate_id) open_mandate_returns(account_id)
    acknowledge_mandate_returns(mandate_id)
    // --- Tags (AppModel+Tags, TagsUI) ---
    list_tags() create_tag(name, color_hex)
    add_tag_to_bookings(booking_ids, tag_id) remove_tag_from_bookings(booking_ids, tag_id)
    rename_tag(id, name) set_tag_color(id, color_hex) reorder_tags(ids) delete_tag(id)
    merge_tag_into(source, target) set_booking_tags(booking_id, tag_ids)
    // --- Tag-Filter und Auswertung (AppModel+Tags, TagFilterUI, TagManagerView, #778) ---
    tag_totals(filter) tag_account_counts(tag_id)
    // --- Anlageklassen (AssetClassificationUI, DepotMetrics) ---
    classified_assets(account_id)
    set_asset_class_override(security_key, value) set_instrument_override(security_key, value)
    // --- Händler-Logos (AppModel+Logos, MerchantLogoSettingsSection); logos: `merchant_logo` ---
    online_logo_provider() clear_logo_cache() set_merchant_domain(merchant_key, domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the process-wide vault slots.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Sets (`Some`) or removes (`None`) an environment variable for a test that holds
    /// `SERIAL`.
    fn set_env(key: &str, value: Option<impl AsRef<std::ffi::OsStr>>) {
        // SAFETY: test-only; every test that changes the environment holds `SERIAL`.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    fn call_ok(method: &str, args: Value) -> Value {
        let r: Value = serde_json::from_str(&call_json(method, &args.to_string())).unwrap();
        assert!(r.get("err").is_none(), "{method}: {r}");
        r["ok"].clone()
    }

    /// Preview copies (`document_temp_file`): a shown document asked for again
    /// counts as newest, so opening two others keeps it (#941 review); a vault
    /// switch removes every copy.
    #[test]
    fn quicklook_copies_keep_the_shown_document() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let run = std::env::temp_dir().join(format!("girocrab-ql-{}", std::process::id()));
        std::fs::create_dir_all(&run).unwrap();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        set_env("XDG_RUNTIME_DIR", Some(&run));
        call_ok("open_demo", json!({}));
        let docs = call_ok(
            "list_all_documents",
            json!({ "filter": { "account_id": null,
            "doc_type": null, "search": null, "unlinked_only": false } }),
        );
        let docs = docs.as_array().unwrap();
        assert!(docs.len() >= 4, "demo has documents");
        let temp = |i: usize| {
            let d = &docs[i];
            let p = call_ok(
                "document_temp_file",
                json!({ "doc_id": d["id"], "mime_type": d["mime_type"] }),
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::path::PathBuf::from(p.as_str().unwrap())
        };
        let shown = temp(0);
        let older = temp(1);
        assert_eq!(temp(0), shown, "a shown document reuses its copy");
        let third = temp(2);
        let fourth = temp(3);
        assert!(shown.exists(), "the shown document survives two others");
        assert!(!older.exists(), "the oldest copy goes");
        assert!(third.exists() && fourth.exists());
        call_ok("open_demo", json!({}));
        assert!(
            !shown.exists() && !fourth.exists(),
            "a vault switch removes every copy"
        );
        set_env("XDG_RUNTIME_DIR", saved);
        let _ = std::fs::remove_dir_all(run);
    }

    /// Automatischer Abruf (#546, AutoSync.qml): `begin_quiet_sync` registers a SyncSession in
    /// the registry that `session_start` drives like a manual one (demo bank, no network).
    #[test]
    fn quiet_sync_session_round_trip() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        call_ok("open_demo", json!({}));
        let conns = call_ok("list_bank_connections", json!({}));
        let conn = conns
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["connector"] == "FinTsGeneric")
            .or_else(|| conns.as_array().unwrap().first())
            .expect("the demo has bank connections")
            .clone();
        let id = call_ok(
            "begin_quiet_sync",
            json!({ "connection_id": conn["id"], "secret": "", "demo": true }),
        );
        let id = id.as_u64().expect("a session handle");
        assert!(crate::sessions::get(id).is_ok());
        let step = call_ok("session_start", json!({ "session": id }));
        assert!(
            ["NeedsChallenge", "StillWaiting", "Finished"]
                .iter()
                .any(|k| step.get(k).is_some()),
            "{step}"
        );
        if step.get("Finished").is_none() {
            call_ok("session_cancel", json!({ "session": id }));
        }
        assert!(
            crate::sessions::get(id).is_err(),
            "the session left the registry"
        );
        let missing: Value = serde_json::from_str(&call_json(
            "begin_quiet_sync",
            &json!({ "connection_id": -1, "secret": "", "demo": true }).to_string(),
        ))
        .unwrap();
        assert!(
            missing.get("err").is_some(),
            "unknown connection is refused"
        );
    }

    /// Own vault lifecycle with the keychain bypass: create, use, lock, reopen,
    /// demo in front of the unlocked own vault.
    #[test]
    fn own_vault_lifecycle() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("girocrab-test-{}", std::process::id()));
        set_env("XDG_DATA_HOME", Some(&dir));
        set_env("GIROCRAB_NO_KEYCHAIN", Some("1"));
        assert_eq!(call_ok("vault_info", json!({}))["size"], json!(-1));
        let r = call_ok("unlock_own", json!({ "allow_create": true }));
        assert_eq!(r["account_count"], json!(0));
        call_ok("use_own", json!({}));
        let cats = call_ok("list_categories", json!({}));
        assert!(cats.as_array().unwrap().len() > 3, "starter categories");
        call_ok("open_demo", json!({}));
        assert!(
            call_ok("account_count", json!({})).as_u64().unwrap() > 0,
            "demo in front"
        );
        assert_eq!(call_ok("has_own_vault", json!({})), json!(true));
        call_ok("lock", json!({}));
        assert_eq!(call_ok("is_unlocked", json!({})), json!(false));
        assert!(call_ok("vault_info", json!({}))["size"].as_i64().unwrap() > 0);
        // reopening an existing file never creates a key
        call_ok("unlock_own", json!({ "allow_create": true }));
        call_ok("use_own", json!({}));
        call_ok("lock", json!({}));
        // Settings › Sicherheit: restoring a key while locked checks it against the file.
        let wrong: Value = serde_json::from_str(&call_json(
            "import_key",
            &json!({ "text": "11".repeat(32) }).to_string(),
        ))
        .unwrap();
        assert!(wrong.get("err").is_some(), "wrong key must be refused");
        call_ok("import_key", json!({ "text": "42".repeat(32) }));
        // settings / recurring calls on the demo vault
        call_ok("open_demo", json!({}));
        assert!(call_ok("fints_product_id_mode", json!({})).is_string());
        assert!(call_ok("list_ignored_recurring", json!({ "account_ids": [] })).is_array());
        assert!(call_ok("list_known_recipients", json!({ "query": "" })).is_array());
        let filter =
            json!({ "connection_id": null, "only_errors": false, "search": null, "limit": 0 });
        let before = call_ok("list_sync_log", json!({ "filter": filter }))
            .as_array()
            .unwrap()
            .len();
        call_ok(
            "log_sync",
            json!({ "connection_id": null, "source": "Manual", "outcome": "Error", "message": "Test", "imported": 0 }),
        );
        let after = call_ok("list_sync_log", json!({ "filter": filter }));
        assert_eq!(after.as_array().unwrap().len(), before + 1);
        call_ok("clear_sync_log", json!({}));
        let pem = dir.join("key.pem");
        call_ok(
            "write_text_file",
            json!({ "path": pem.display().to_string(), "text": "-----BEGIN PRIVATE KEY-----" }),
        );
        assert_eq!(
            call_ok(
                "read_text_file",
                json!({ "path": format!("file://{}", pem.display()) })
            ),
            json!("-----BEGIN PRIVATE KEY-----")
        );
        call_ok("lock", json!({}));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Händler-Logos (#33): while the provider is "off" (the default) `merchant_logo` answers
    /// null without any network request; the domain override and the cache calls round-trip.
    #[test]
    fn merchant_logos_stay_offline_while_off() {
        use std::sync::atomic::Ordering;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        call_ok("open_demo", json!({}));
        call_ok(
            "set_setting",
            json!({ "key": "merchants.online_logos", "value": "off" }),
        );
        assert_eq!(call_ok("online_logo_provider", json!({})), json!("off"));
        let before = LOGO_FETCHES.load(Ordering::SeqCst);
        let logo = call_ok(
            "merchant_logo",
            json!({ "candidates": ["lidl.de", "lidl.com"] }),
        );
        assert_eq!(logo, Value::Null);
        assert_eq!(
            LOGO_FETCHES.load(Ordering::SeqCst),
            before,
            "no fetch while off"
        );
        assert!(call_ok("clear_logo_cache", json!({})).is_u64());
        let key = "test-merchant-logo";
        assert_eq!(
            call_ok(
                "set_merchant_domain",
                json!({ "merchant_key": key, "domain": "https://www.EnBW.com/privat" })
            ),
            json!("enbw.com")
        );
        assert_eq!(
            call_ok(
                "set_merchant_domain",
                json!({ "merchant_key": key, "domain": null })
            ),
            Value::Null
        );
        // switched on: a fetch starts (an invalid domain is refused before the network)
        call_ok(
            "set_setting",
            json!({ "key": "merchants.online_logos", "value": "duckduckgo" }),
        );
        let logo = call_ok("merchant_logo", json!({ "candidates": ["not a domain!"] }));
        assert_eq!(logo, Value::Null);
        assert_eq!(LOGO_FETCHES.load(Ordering::SeqCst), before + 1);
        assert_eq!(
            call_ok("merchant_logo", json!({ "candidates": [] })),
            Value::Null
        );
        call_ok(
            "set_setting",
            json!({ "key": "merchants.online_logos", "value": "off" }),
        );
    }

    /// AppModel.decodeLogo: PNG width, the largest image of an ICO, unknown formats.
    #[test]
    fn logo_payload_picks_largest_image() {
        let png = |w: u32| {
            let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&[8, 6, 0, 0, 0]);
            b
        };
        let p = logo_payload(&png(64));
        assert_eq!(p["pixels"], json!(64));
        assert!(
            p["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        // ICO: a 16 px bitmap and an embedded 48 px PNG → the PNG alone
        let bmp = vec![0u8; 40];
        let big = png(48);
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        let entry = |w: u8, len: usize, off: usize| {
            let mut e = vec![w, w, 0, 0, 1, 0, 32, 0];
            e.extend_from_slice(&(len as u32).to_le_bytes());
            e.extend_from_slice(&(off as u32).to_le_bytes());
            e
        };
        ico.extend(entry(16, bmp.len(), 38));
        ico.extend(entry(48, big.len(), 38 + bmp.len()));
        ico.extend(&bmp);
        ico.extend(&big);
        let p = logo_payload(&ico);
        assert_eq!(p["pixels"], json!(48));
        assert!(p["url"].as_str().unwrap().starts_with("data:image/png;"));
        // bitmaps only: a one-image ICO of the largest
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        ico.extend(entry(16, 40, 38));
        ico.extend(entry(32, 40, 78));
        ico.extend(vec![0u8; 80]);
        let p = logo_payload(&ico);
        assert_eq!(p["pixels"], json!(32));
        assert!(p["url"].as_str().unwrap().starts_with("data:image/x-icon;"));
        assert_eq!(logo_payload(b"\xff\xd8\xff")["pixels"], json!(0));
    }

    /// AppModel.loadOffMain: a batch answers in call order, stops at the first error and
    /// does nothing more once a newer request of its channel superseded it.
    #[test]
    fn batch_in_order_until_superseded() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        call_ok("open_demo", json!({}));
        let calls =
            json!([{ "method": "account_count" }, { "method": "core_version", "args": {} }]);
        let r = call_ok(
            "batch",
            json!({ "channel": "test", "token": 1, "calls": calls }),
        );
        assert!(r["results"][0].as_u64().unwrap() > 0);
        assert_eq!(r["results"][1], json!(girocrab_core::core_version()));
        call_ok("supersede", json!({ "channel": "test", "token": 2 }));
        let stale = call_ok(
            "batch",
            json!({ "channel": "test", "token": 1, "calls": calls }),
        );
        assert_eq!(stale, json!({ "cancelled": true }));
        let fresh = call_ok(
            "batch",
            json!({ "channel": "test", "token": 2, "calls": calls }),
        );
        assert_eq!(fresh["results"].as_array().unwrap().len(), 2);
        // without a channel nothing can supersede it
        let plain = call_ok("batch", json!({ "calls": calls }));
        assert_eq!(plain["results"].as_array().unwrap().len(), 2);
        let bad: Value = serde_json::from_str(&call_json(
            "batch",
            &json!({ "calls": [{ "method": "no_such_method" }, { "method": "account_count" }] })
                .to_string(),
        ))
        .unwrap();
        assert!(
            bad.get("err").is_some(),
            "the first error ends the batch: {bad}"
        );
        call_ok("lock", json!({}));
    }

    /// „App zurücksetzen“ (#241) with the keychain bypass and temporary XDG dirs only:
    /// vault, side files and settings go, the banking policy stays, the next start is a
    /// first start (no vault file, a new one can be created).
    #[test]
    fn app_reset_deletes_vault_and_settings() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("girocrab-reset-{}", std::process::id()));
        let (data, config) = (dir.join("data"), dir.join("config"));
        std::fs::create_dir_all(config.join("GiroCrab")).unwrap();
        set_env("XDG_DATA_HOME", Some(&data));
        set_env("XDG_CONFIG_HOME", Some(&config));
        set_env("GIROCRAB_NO_KEYCHAIN", Some("1"));
        assert!(vault_path().starts_with(&data) && settings_file().starts_with(&config));
        call_ok("unlock_own", json!({ "allow_create": true }));
        call_ok("use_own", json!({}));
        call_ok("checkpoint", json!({}));
        let conf = settings_file();
        std::fs::write(&conf, "[General]\nhasRealData=true\n").unwrap();
        let policy = config.join("GiroCrab/banking-policy.json");
        std::fs::write(&policy, "{}").unwrap();
        let side = std::path::PathBuf::from(format!("{}-journal", vault_path().display()));
        std::fs::write(&side, "x").unwrap();
        assert!(call_ok("vault_info", json!({}))["size"].as_i64().unwrap() > 0);

        call_ok("reset_app", json!({}));
        assert!(app_was_reset());
        assert_eq!(call_ok("vault_info", json!({}))["size"], json!(-1));
        assert!(
            !side.exists() && !conf.exists(),
            "side files and settings deleted"
        );
        assert!(policy.exists(), "banking policy anchor survives");
        assert_eq!(call_ok("has_own_vault", json!({})), json!(false));
        assert_eq!(call_ok("is_unlocked", json!({})), json!(false));
        // a second reset (nothing left) is no error
        call_ok("reset_app", json!({}));
        // first start again: a new, empty vault
        let r = call_ok("unlock_own", json!({ "allow_create": true }));
        assert_eq!(r["account_count"], json!(0));
        call_ok("lock", json!({}));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Einstellungen › Sicherheit › „Tresor sichern …“ / „Tresor wiederherstellen …“ on the
    /// own vault (keychain bypass, temporary XDG dir), plus the suggested password.
    #[test]
    fn backup_roundtrip_and_suggested_password() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("girocrab-backup-pw-{}", std::process::id()));
        let saved: Vec<_> = ["XDG_DATA_HOME", "GIROCRAB_NO_KEYCHAIN"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        set_env("XDG_DATA_HOME", Some(dir.join("data")));
        set_env("GIROCRAB_NO_KEYCHAIN", Some("1"));
        let p = call_ok("suggested_backup_password", json!({}));
        let p = p.as_str().unwrap();
        assert_eq!(p.len(), 20);
        assert!(p.split('-').all(|g| {
            g.len() == 6
                && g.bytes()
                    .all(|b| b"abcdefghijkmnpqrstuvwxyz23456789".contains(&b))
        }));
        assert_ne!(
            p,
            call_ok("suggested_backup_password", json!({}))
                .as_str()
                .unwrap()
        );

        call_ok("unlock_own", json!({ "allow_create": true }));
        call_ok("use_own", json!({}));
        let rules = call_ok("list_rules", json!({})).as_array().unwrap().len();
        let file = dir.join("GiroCrab-Sicherung 2026-11-16.girocrab-backup");
        call_ok(
            "write_backup",
            json!({ "dest_path": file.display().to_string(), "password": "geheim123" }),
        );
        assert!(file.exists());
        let wrong: Value = serde_json::from_str(&call_json(
            "restore_backup",
            &json!({ "backup_path": file.display().to_string(), "password": "falsch123" })
                .to_string(),
        ))
        .unwrap();
        assert!(wrong.get("err").is_some(), "wrong password must be refused");
        call_ok(
            "restore_backup",
            json!({ "backup_path": file.display().to_string(), "password": "geheim123" }),
        );
        call_ok("use_own", json!({}));
        assert_eq!(
            call_ok("list_rules", json!({})).as_array().unwrap().len(),
            rules
        );
        call_ok("lock", json!({}));
        let _ = std::fs::remove_dir_all(dir);
        for (k, v) in saved {
            set_env(k, v);
        }
    }

    /// The categorizer / rules / split arms with the JSON the QML sends.
    #[test]
    fn rules_categories_splits_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("girocrab-rules-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let cats = call("list_categories", json!({}));
        let cat = cats
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["depth"] == json!(1))
            .unwrap()
            .clone();
        let rule = json!({ "id": 0, "name": "Test", "category_id": cat["id"], "field": "Any",
            "match_kind": "Keyword", "pattern": "REWE", "case_sensitive": false, "min_amount_cents": null,
            "max_amount_cents": 0, "account_ids": [], "priority": 999, "enabled": true, "match_count": 0 });
        let b = call("preview_rule", json!({ "rule": rule, "limit": 200 }))[0].clone();
        let id = call("save_rule", json!({ "rule": rule }));
        call("apply_rules", json!({ "only_uncategorized": false }));
        call("delete_rule", json!({ "rule_id": id }));
        call(
            "set_category_budget",
            json!({ "category_id": cat["id"], "budget_cents": 12000 }),
        );
        call(
            "update_category",
            json!({ "id": cat["id"], "name": "Umbenannt", "parent_id": null,
            "sf_symbol": "tag", "color_hex": "#2563EB" }),
        );
        let amount = b["amount_cents"].as_i64().unwrap();
        call(
            "set_transaction_splits",
            json!({ "transaction_id": b["id"], "splits": [
            { "category_id": cat["id"], "amount_cents": amount - amount / 2, "note": "" },
            { "category_id": null, "amount_cents": amount / 2, "note": "x" }] }),
        );
        assert_eq!(
            call(
                "list_transaction_splits",
                json!({ "transaction_id": b["id"] })
            )
            .as_array()
            .unwrap()
            .len(),
            2
        );
        call("delete_category", json!({ "id": cat["id"] }));
        assert_eq!(
            call_json("parse_amount_to_cents", r#"{"text":"1.234,56"}"#),
            r#"{"ok":123456}"#
        );
        drop(v);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    /// Händler-Historie (#299): the merchant's stats over all accounts and in one account.
    #[test]
    fn merchant_history() {
        let path =
            std::env::temp_dir().join(format!("girocrab-merchant-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let filter = json!({ "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null,
            "limit": 1, "offset": 0, "scope": "All", "merchant_key": null, "direction": "All",
            "checked": null, "whole_days": true });
        let b = call("list_transactions", json!({ "filter": filter }))["transactions"][0].clone();
        let key = b["merchant"]["key"].clone();
        let h = call(
            "merchant_history",
            json!({ "merchant_key": key, "account_id": b["account_id"] }),
        );
        let (all, own) = (h["all"].clone(), h["account"].clone());
        assert_eq!(own["key"], all["key"]);
        assert!(own["count"].as_u64().unwrap() >= 1);
        assert!(own["count"].as_u64().unwrap() <= all["count"].as_u64().unwrap());
        drop(v);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    /// MoneyMoney export file → preview → import, CSV export of the result,
    /// and a database that cannot be read reports its error.
    #[test]
    fn moneymoney_file_import_and_csv_export() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("girocrab-mm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Girokonto.csv");
        std::fs::write(
            &file,
            "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\";\"Notiz\"\n\
             \"15.03.2012\";\"15.03.2012\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf\";\"DE02120300000000202051\";\"BYLADEM1001\";\"-1.234,56\";\"EUR\";\"Wocheneinkauf\"\n\
             \"01.04.2012\";\"01.04.2012\";\"Steuern\";\"Finanzamt\";\"Erstattung\";\"\";\"\";\"2.000,00\";\"EUR\";\"\"\n",
        )
        .unwrap();
        let path = file.display().to_string();
        call_ok("open_demo", json!({}));
        let p = call_ok("preview_moneymoney_file", json!({ "path": path }));
        assert_eq!(p["total_rows"], json!(2));
        assert_eq!(p["new_bookings"], json!(2));
        assert_eq!(p["notes_count"], json!(1));
        let r = call_ok(
            "import_moneymoney_file",
            json!({ "path": path, "target_account_id": null, "preserve_categories": true, "preserve_notes": true }),
        );
        assert_eq!(r["imported"], json!(2));
        assert_eq!(r["account_created"], json!(true));
        let out = dir.join("GiroCrab-Buchungen.csv");
        let filter = json!({ "account_ids": [r["account_id"]], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null, "limit": 0, "offset": 0,
            "scope": "All", "merchant_key": null, "direction": "All", "checked": null, "whole_days": true });
        call_ok(
            "export_bookings_csv_file",
            json!({ "filter": filter, "path": out.display().to_string() }),
        );
        let csv = std::fs::read_to_string(&out).unwrap();
        assert!(csv.starts_with("\u{FEFF}Datum;Konto;"), "{csv}");
        assert!(csv.contains("Finanzamt") && csv.contains("REWE"), "{csv}");
        // not a MoneyMoney database: an error, not a panic
        let r: Value = serde_json::from_str(&call_json(
            "preview_moneymoney_database",
            &json!({ "path": path, "password": "x", "statements_dir": null }).to_string(),
        ))
        .unwrap();
        assert!(r["err"].is_string(), "{r}");
        assert_eq!(
            call_ok("moneymoney_import_progress", json!({})),
            Value::Null
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The loan editor and Startsaldo sheets with the JSON the QML sends.
    #[test]
    fn loans_and_opening_balance_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("girocrab-loans-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let setup = json!({ "name": "Testkredit", "original_cents": null, "anchor_date": "2026-01-31",
                            "anchor_balance_cents": 500000, "interest_rate_ppm": 49900, "match_expr": "" });
        let preview = call(
            "preview_manual_loan",
            json!({ "account_id": null, "setup": setup, "limit": 5 }),
        );
        assert_eq!(preview["status"]["remaining_cents"], json!(500000));
        let id = call("create_manual_loan", json!({ "setup": setup }));
        let loans = call("list_manual_loans", json!({}));
        assert!(
            loans
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["account_id"] == id)
        );
        call(
            "update_manual_loan",
            json!({ "account_id": id, "setup": setup }),
        );
        assert!(call("list_loan_links", json!({})).is_array());

        let accounts = call("list_accounts", json!({}));
        let derived = accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["balance_derived"] == json!(true))
            .unwrap()["id"]
            .clone();
        let p = call(
            "preview_account_opening_balance",
            json!({ "account_id": derived, "date": "" }),
        );
        assert!(p["counted"].as_u64().is_some());
        call(
            "set_account_opening_balance",
            json!({ "account_id": derived, "cents": 1000, "date": "2026-01-01" }),
        );
        call(
            "set_account_opening_balance",
            json!({ "account_id": derived, "cents": null, "date": "" }),
        );
        drop(v);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn transfer_helpers() {
        assert_eq!(
            call_ok(
                "validate_iban",
                json!({ "iban": "DE89 3704 0044 0532 0130 00" })
            ),
            json!(true)
        );
        assert_eq!(
            call_ok("parse_amount_to_cents", json!({ "text": "1.234,56" })),
            json!(123456)
        );
        // payment_input (#663): the wiring of each core call; the rules are tested in the core
        let p = call_ok(
            "parse_payment_text",
            json!({ "text": "Empfänger: Max Müller\nIBAN DE89 3704 0044 0532 0130 00\nBetrag: 1.234,56 EUR" }),
        );
        assert_eq!(p["name"], json!("Max Müller"), "{p}");
        assert_eq!(p["amount_cents"], json!(123456), "{p}");
        assert_eq!(p["bic"], Value::Null, "{p}");
        let payload = call_ok(
            "girocode_payload",
            json!({ "name": "Test", "iban": "DE89 3704 0044 0532 0130 00", "bic": "",
                    "amount_cents": 100, "purpose": "x" }),
        );
        assert_eq!(
            payload,
            json!("BCD\n002\n1\nSCT\n\nTest\nDE89370400440532013000\nEUR1.00\n\n\nx")
        );
        let epc = call_ok("parse_epc_payload", json!({ "payload": payload }));
        assert_eq!(epc["amount_cents"], json!(100), "{epc}");
        assert_eq!(
            call_ok("parse_amount_loose", json!({ "text": "1,234.56" })),
            json!(123456)
        );
        let m = call_ok("girocode_matrix", json!({ "payload": payload }));
        let w = m["width"].as_u64().unwrap() as usize;
        let modules = m["modules"].as_array().unwrap();
        assert_eq!(modules.len(), w * w);
        // … and back: that GiroCode as a dropped image (PGM, 4 px per module, quiet
        // zone included) comes out of read_invoice as `epc`, without OCR text
        let side = w * 4;
        let mut pgm = format!("P5\n{side} {side}\n255\n").into_bytes();
        for y in 0..side {
            for x in 0..side {
                let dark = modules[(y / 4) * w + x / 4] == json!(true);
                pgm.push(if dark { 0 } else { 255 });
            }
        }
        let img = std::env::temp_dir().join(format!("girocrab-qr-{}.pgm", std::process::id()));
        std::fs::write(&img, &pgm).unwrap();
        let r = call_ok("read_invoice", json!({ "path": img.display().to_string() }));
        let _ = std::fs::remove_file(&img);
        assert_eq!(
            r["epc"],
            json!("BCD\n002\n1\nSCT\n\nTest\nDE89370400440532013000\nEUR1.00\n\n\nx"),
            "{r}"
        );
        assert_eq!(r["text"], json!(""), "{r}");
        // ZUGFeRD XML in a Flate stream of a PDF
        let xml =
            b"<rsm:CrossIndustryInvoice><ram:IBANID>DE89</ram:IBANID></rsm:CrossIndustryInvoice>";
        let z = miniz_oxide::deflate::compress_to_vec_zlib(xml, 6);
        let mut pdf = b"%PDF-1.7\n1 0 obj\nstream\n".to_vec();
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\n");
        assert!(embedded_invoice_xml(&pdf).unwrap().contains("IBANID"));
    }

    /// Main window calls: recurring snapshot, "Alle als gelesen markieren", BIC lookup.
    #[test]
    fn main_window_calls() {
        let path =
            std::env::temp_dir().join(format!("girocrab-main-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![9; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let snap = call(
            "recurring_snapshot",
            json!({ "account_ids": [], "year": 2026, "month": 11 }),
        );
        assert!(snap["series"].is_array() && snap["ignored"].is_array());
        assert!(snap["month"]["items"].is_array());
        let filter = json!({ "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null,
            "limit": 0, "offset": 0, "scope": "All", "merchant_key": null, "direction": "All",
            "checked": false, "whole_days": false });
        let ids = call(
            "set_checked_for_filter",
            json!({ "filter": filter, "checked": true }),
        );
        assert!(!ids.as_array().unwrap().is_empty());
        let again = call(
            "set_checked_for_filter",
            json!({ "filter": filter, "checked": true }),
        );
        assert!(again.as_array().unwrap().is_empty());
        // Sidebar › account menu: Kontoart ändern …, Kontoinhaber …
        let id = call("list_accounts", json!({}))[0]["id"].clone();
        call(
            "set_account_kind",
            json!({ "account_id": id, "kind": "Savings" }),
        );
        call(
            "set_account_owner",
            json!({ "account_id": id, "owner": "  Erika Muster " }),
        );
        let acc = call("list_accounts", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .cloned()
            .unwrap();
        assert_eq!(acc["kind"], "Savings");
        assert_eq!(acc["owner"], "Erika Muster");
        assert_eq!(
            call_ok("bic_for_iban", json!({ "iban": "DE89370400440532013000" })),
            json!("COBADEFF")
        );
        // Kontoauswahl (#346): a second login reaching accounts the first one delivers
        let second = call("seed_demo_account_selection", json!({}));
        assert_eq!(
            call("pending_account_selection_connections", json!({})),
            json!([second])
        );
        let choices = call(
            "connection_account_choices",
            json!({ "connection_id": second }),
        );
        let rows = choices.as_array().unwrap();
        assert!(
            rows.iter()
                .any(|c| c["pending"] == true && c["enabled"] == false)
        );
        assert!(
            rows.iter()
                .all(|c| c["account_ref"].is_string() && c["existing_connection"].is_string())
        );
        let keep: Vec<Value> = rows
            .iter()
            .filter(|c| c["enabled"] == true)
            .map(|c| c["account_ref"].clone())
            .collect();
        set_connection_account_choices(
            &v,
            &json!({ "connection_id": second, "enabled_refs": keep }),
        )
        .unwrap();
        assert_eq!(
            call("pending_account_selection_connections", json!({})),
            json!([])
        );
        let after = call(
            "connection_account_choices",
            json!({ "connection_id": second }),
        );
        assert!(
            after
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["pending"] == false)
        );
        assert!(
            set_connection_account_choices(
                &v,
                &json!({ "connection_id": second, "enabled_refs": ["unknown"] }),
            )
            .is_err()
        );
        let _ = std::fs::remove_file(path);
    }

    /// Vermögen (WealthView.qml): Sachwerte accounts and items, depot transactions,
    /// with the JSON field names the QML reads.
    #[test]
    fn wealth_calls() {
        let path =
            std::env::temp_dir().join(format!("girocrab-wealth-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![5; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let tangible = call("list_tangible_account_ids", json!({}));
        let tangible = tangible.as_array().unwrap();
        assert!(!tangible.is_empty(), "demo has a Sachwerte account");
        let items = call("list_all_asset_items", json!({}));
        let items = items.as_array().unwrap();
        assert!(!items.is_empty());
        for it in items {
            assert!(tangible.contains(&it["account_id"]));
            assert!(it["setup"]["name"].is_string() && it["setup"]["price_source"].is_string());
            assert!(it["holding"]["value_cents"].is_i64() && it["holding"]["quantity"].is_f64());
            assert!(it["sold"].is_boolean());
        }
        let accounts = call("list_accounts", json!({}));
        let depot = accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "Brokerage" && !tangible.contains(&a["id"]))
            .expect("demo depot")["id"]
            .clone();
        let txs = call("list_security_transactions", json!({ "account_id": depot }));
        for t in txs.as_array().unwrap() {
            assert!(t["direction"].is_string() && t["booking_date"].is_string());
            assert!(t["fees_cents"].is_i64() && t["taxes_cents"].is_i64());
        }
        let _ = std::fs::remove_file(path);
    }

    /// Sachwerte (AssetEditorSheet.qml, AssetActionSheet.qml): the JSON the sheets send.
    #[test]
    fn tangible_asset_calls() {
        let path =
            std::env::temp_dir().join(format!("girocrab-assets-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![6; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let presets = call_ok("asset_weight_presets", json!({}));
        assert!(presets[0]["label"].is_string() && presets[0]["grams"].is_f64());
        let account = call("create_tangible_account", json!({ "name": "" }));
        assert!(
            call("list_tangible_account_ids", json!({}))
                .as_array()
                .unwrap()
                .contains(&account)
        );
        let setup = json!({ "name": "Bild", "price_source": "manual", "weight_grams": 0.0,
            "purity_permille": 1000.0, "interpolation": "Linear", "purchase_date": "2024-01-10",
            "quantity": 2.0, "purchase_price_cents": 200000, "start_value_cents": 100000,
            "note": "Atelier", "symbol": "photo.artframe" });
        let item = call(
            "save_asset_item",
            json!({ "account_id": account, "item_id": null, "setup": setup }),
        );
        call(
            "set_asset_value",
            json!({ "item_id": item, "date": "2025-01-10", "value_cents": 150000 }),
        );
        call(
            "set_asset_quantity",
            json!({ "item_id": item, "date": "2025-06-01", "quantity": 3.0 }),
        );
        assert_eq!(
            call(
                "asset_unit_value_on",
                json!({ "item_id": item, "date": "2025-01-10" })
            ),
            json!(150000)
        );
        assert!(
            call("asset_item_history", json!({ "item_id": item }))[0]["balance_cents"].is_i64()
        );
        let price = call(
            "asset_price_on",
            json!({ "symbol": "XAU", "date": "2025-01-10" }),
        );
        assert!(price.is_null() || price["value_cents"].is_i64());
        call(
            "sell_asset",
            json!({ "item_id": item, "date": "2025-07-01", "count": 3.0, "price_cents": 160000 }),
        );
        let items = call("list_asset_items", json!({ "account_id": account }));
        let it = &items[0];
        assert_eq!(it["setup"]["interpolation"], "Linear");
        assert_eq!(it["sold"], json!(true));
        assert!(it["values"].as_array().unwrap().len() >= 2);
        assert!(it["quantities"].as_array().unwrap().len() >= 3);
        call(
            "delete_asset_quantity",
            json!({ "item_id": item, "date": "2025-07-01" }),
        );
        call(
            "delete_asset_value",
            json!({ "item_id": item, "date": "2025-07-01" }),
        );
        call("delete_asset_item", json!({ "item_id": item }));
        assert!(
            call("list_asset_items", json!({ "account_id": account }))
                .as_array()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    /// Umsatz-Export: CSV and CAMT.053 from demo bookings, saved without replacing a file.
    #[test]
    fn export_calls() {
        let path =
            std::env::temp_dir().join(format!("girocrab-export-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let accounts = call("list_accounts", json!({}));
        let account = accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "Checking")
            .unwrap()
            .clone();
        let filter = json!({ "account_ids": [account["id"]], "category_id": null,
            "include_subcategories": true, "uncategorized_only": false, "search": null,
            "date_from": null, "date_to": null, "limit": 0, "offset": 0, "scope": "All",
            "merchant_key": null, "direction": "All", "checked": null, "whole_days": true });
        let bookings =
            call("list_transactions", json!({ "filter": filter }))["transactions"].clone();
        let first = &bookings.as_array().unwrap()[0];
        let rows = json!([{ "date": first["booking_date"], "account_name": account["name"],
            "counterparty": "=SUMME(A1)", "purpose": "Miete; März", "category": "Wohnen",
            "note": "", "amount_cents": -123456, "currency": "EUR", "day_balance_cents": 50 }]);
        let csv = call_ok(
            "export_csv",
            json!({ "rows": rows, "columns": { "category": true, "note": false, "daily_balance": true },
                    "balance_lines": [] }),
        );
        let csv = csv.as_str().unwrap();
        assert!(csv.starts_with(
            "\u{FEFF}Datum;Konto;Name;Verwendungszweck;Kategorie;Betrag;Währung;Tagessaldo\r\n"
        ));
        assert!(csv.contains(";'=SUMME(A1);\"Miete; März\";Wohnen;-1234,56;EUR;0,50\r\n"));
        // CAMT.053 straight from the vault: account and period only (#483)
        let camt = call(
            "camt053_export",
            json!({ "request": { "account_id": account["id"], "date_from": null, "date_to": null,
                    "booking_ids": null, "version": "V08", "created_at": "2026-11-15T12:00:00+01:00" } }),
        );
        let camt = camt.as_str().unwrap();
        assert!(camt.contains("urn:iso:std:iso:20022:tech:xsd:camt.053.001.08"));
        assert!(camt.contains("<Cd>OPBD</Cd>") && camt.contains("<Cd>CLBD</Cd>"));
        // A period in the older version, as BookingExport.qml › camtRequest sends it
        let days: Vec<String> = bookings
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["booking_date"].as_str().unwrap().to_string())
            .collect();
        let (from, to) = (days.iter().min().unwrap(), days.iter().max().unwrap());
        let camt = call(
            "camt053_export",
            json!({ "request": { "account_id": account["id"], "date_from": from, "date_to": to,
                    "booking_ids": null, "version": "V02", "created_at": "2026-11-15T11:00:00Z" } }),
        );
        let camt = camt.as_str().unwrap();
        assert!(camt.contains("urn:iso:std:iso:20022:tech:xsd:camt.053.001.02"));
        assert!(camt.contains(&format!("<Dt>{to}</Dt>")));
        // Marked bookings („Auswahl“): every booking of the newest day is a gapless marking
        let newest: Vec<i64> = bookings
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["booking_date"].as_str() == Some(to.as_str()))
            .map(|b| b["id"].as_i64().unwrap())
            .collect();
        let camt = call(
            "camt053_export",
            json!({ "request": { "account_id": account["id"], "date_from": null, "date_to": null,
                    "booking_ids": newest, "version": "V08", "created_at": "2026-11-15T11:00:00Z" } }),
        );
        assert_eq!(
            camt.as_str().unwrap().matches("<Ntry>").count(),
            newest.len()
        );
        // A marking with a gap and a loan/depot are refused with the core's plain
        // message (ExportSheet.swift: `catch CoreError.InvalidInput(message)`).
        let ids: Vec<i64> = bookings
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].as_i64().unwrap())
            .collect();
        let (oldest, newest_id) = (ids[ids.len() - 1], ids[0]);
        let gap = dispatch_vault(
            &v,
            "camt053_export",
            &json!({ "request": { "account_id": account["id"], "date_from": null, "date_to": null,
                     "booking_ids": [oldest, newest_id], "version": "V08",
                     "created_at": "2026-11-15T11:00:00Z" } }),
        )
        .unwrap()
        .unwrap_err();
        assert!(gap.starts_with("CAMT.053 "), "{gap}");
        if let Some(depot) = accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "Brokerage" || a["kind"] == "Loan")
        {
            let refused = dispatch_vault(
                &v,
                "camt053_export",
                &json!({ "request": { "account_id": depot["id"], "date_from": null, "date_to": null,
                         "booking_ids": null, "version": "V08", "created_at": "2026-11-15T11:00:00Z" } }),
            )
            .unwrap()
            .unwrap_err();
            assert!(refused.starts_with("CAMT.053 "), "{refused}");
        }
        let dir = std::env::temp_dir().join(format!("girocrab-export-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let save = |a: Value| call_ok("save_export_file", a).as_str().unwrap().to_string();
        let args = json!({ "text": "a", "dir": dir, "filename": "Umsaetze-Giro.csv" });
        assert!(save(args.clone()).ends_with("/Umsaetze-Giro.csv"));
        assert!(save(args).ends_with("/Umsaetze-Giro-2.csv"));
        let tmp = dir.join("render.pdf");
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        let target = dir.join("Umsaetze-Giro.pdf");
        let written = save(json!({ "from_path": tmp, "path": target }));
        assert_eq!(std::fs::read(&written).unwrap(), b"%PDF-1.4");
        assert!(!tmp.exists());
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        call_ok(
            "save_export_file",
            json!({ "from_path": tmp, "discard": true }),
        );
        assert!(!tmp.exists());
        let moved = save(json!({ "text": "b", "dir": dir.join("gone"),
            "fallback_dir": dir.join("Downloads"), "filename": "x.xml" }));
        assert!(moved.ends_with("/Downloads/x.xml"));
        assert_eq!(
            std::fs::read_to_string(dir.join("Umsaetze-Giro.csv")).unwrap(),
            "a"
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }

    /// Amazon orders (#333, #349, #354): the booking JSON carries what
    /// AmazonOrderRef.qml reads – the purpose "<items> – Bestellung <code>" (items
    /// joined by "; "), the refund purpose, the booking texts and `item_refs` with
    /// one ASIN per item. No network: the capture JSON is invented.
    #[test]
    fn amazon_order_bookings_carry_items_and_asins() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        call_ok("open_demo", json!({}));
        let conn = call_ok(
            "add_bank_connection",
            json!({ "connector": "AmazonOrders", "display_name": "Amazon Bestellungen", "login_name": "" }),
        );
        let orders = json!({ "orders": [{
            "code": "302-1234567-7654321", "date": "12. März 2025",
            "items": [{ "title": "USB-Kabel", "qty": 2, "asin": "B08XYZ1234" },
                      { "title": "Buch" },
                      { "title": "Lampe", "asin": "3453146883" }],
            "rows": [{ "label": "Gesamtsumme:", "value": "45,98 €", "bold": true },
                     { "label": "Erstattung gesamt", "value": "10,00 €" }] }] });
        call_ok(
            "import_amazon_orders",
            json!({ "connection_id": conn, "orders_json": orders.to_string() }),
        );
        let filter = json!({ "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": "302-1234567-7654321", "date_from": null,
            "date_to": null, "limit": 0, "offset": 0, "scope": "All", "merchant_key": null,
            "direction": "All", "checked": null, "whole_days": true });
        let list =
            call_ok("list_transactions", json!({ "filter": filter }))["transactions"].clone();
        let list = list.as_array().unwrap();
        let order = list
            .iter()
            .find(|b| b["amount_cents"] == json!(-4598))
            .expect("order booking");
        assert_eq!(
            order["purpose"],
            json!("2× USB-Kabel; Buch; Lampe – Bestellung 302-1234567-7654321")
        );
        assert_eq!(order["booking_text"], json!("Amazon-Bestellung"));
        assert_eq!(order["item_refs"], json!("B08XYZ1234,,3453146883"));
        let refund = list
            .iter()
            .find(|b| b["amount_cents"] == json!(1000))
            .expect("refund booking");
        assert_eq!(
            refund["purpose"],
            json!("Erstattung für Bestellung 302-1234567-7654321")
        );
        assert_eq!(refund["booking_text"], json!("Amazon-Erstattung"));
        assert_eq!(refund["account_id"], order["account_id"]);
        call_ok("lock", json!({}));
    }

    /// Problem melden (#301): the sanitizer through the bridge, the vault's payee names and
    /// the issue URL. No network: the URL is only built, never opened.
    #[test]
    fn bug_report_calls() {
        let text = call_ok(
            "sanitize_report_text",
            json!({ "text": "Überweisung an Erika Mustermann (DE89370400440532013000) über 1.234,56 € abgelehnt\nZeile 2",
                    "known_terms": ["Erika Mustermann"] }),
        );
        let text = text.as_str().unwrap();
        for leak in ["Erika", "DE89", "0532013000", "1.234,56"] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
        assert!(
            text.contains("[REDACTED]") && text.ends_with("\nZeile 2"),
            "{text}"
        );

        let path =
            std::env::temp_dir().join(format!("girocrab-report-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let names = dispatch_vault(&v, "counterparty_names", &json!({ "limit": 3 }))
            .unwrap()
            .unwrap();
        let names = names.as_array().unwrap();
        assert!(!names.is_empty() && names.len() <= 3, "{names:?}");
        let terms = crate::bug_report::known_terms(
            &v,
            &names
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            &girocrab_core::bank_catalog(),
        )
        .unwrap();
        assert!(!terms.is_empty());
        let prepared = crate::bug_report::prepare(Some(&v), true, true);
        assert_eq!(prepared["log_unavailable"], json!(false));
        assert!(prepared["text"].as_str().unwrap().starts_with("GiroCrab "));
        let url = call_ok(
            "bug_report_issue_url",
            json!({ "info": prepared["info"], "log": ["2026-11-14 10:00 · FinTS · manual · error · +1"],
                    "with_screenshot": true }),
        );
        let url = url.as_str().unwrap();
        assert!(url.starts_with("https://github.com/GiroCrab/GiroCrab/issues/new?title="));
        assert!(url.contains("%2B1") && !url.contains('+'), "{url}");
        drop(v);
        let _ = std::fs::remove_file(path);
    }

    /// Calls of the features since v0.7.0 (Geplant, Daueraufträge, Lastschriften, Tags,
    /// Anlageklassen, Seitenleisten-Zähler) on the seeded demo vault.
    #[test]
    fn feature_calls_since_v070() {
        let path =
            std::env::temp_dir().join(format!("girocrab-feat-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![9; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        // Seitenleiste: counters and the filtered category tree
        assert!(call("count_planned_payments", json!({})).is_u64());
        assert!(call("count_active_mandates", json!({})).as_u64().unwrap() > 0);
        let cats = call(
            "list_categories_filtered",
            json!({ "date_from": null, "date_to": null, "account_ids": [], "checked": null,
                    "direction": "All" }),
        );
        let cat = cats.as_array().unwrap()[0]["id"].clone();
        assert!(
            call(
                "category_history_by_unit",
                json!({ "category_id": cat, "account_ids": [], "date_from": null, "date_to": null,
                    "key_length": 7 }),
            )
            .is_array()
        );
        // Geplant
        let range = json!({ "from": null, "to": null, "account_ids": [] });
        assert!(call("list_planned_payments", range.clone()).is_array());
        assert!(call("planned_payment_totals", range)["count"].is_u64());
        // Daueraufträge
        let orders = call("list_standing_orders", json!({ "account_ids": [] }));
        assert!(
            !orders.as_array().unwrap().is_empty(),
            "demo standing orders"
        );
        let acc = orders[0]["account_id"].clone();
        let limits = call(
            "standing_order_limits",
            json!({ "account_id": acc, "action": "Create" }),
        );
        assert!(limits.is_null() || limits.is_object());
        // Lastschriften / Mandate
        let mandates = call("list_mandates", json!({ "account_id": null }));
        let first = mandates[0].clone();
        let giro = first["account_id"].clone();
        assert_eq!(
            call("creditor_id", json!({ "account_id": giro })),
            json!("DE98ZZZ09999999999")
        );
        assert_eq!(
            call("suggested_creditor_id", json!({})),
            json!("DE98ZZZ09999999999")
        );
        assert!(call("mandate_bookings", json!({ "mandate_id": first["id"] })).is_array());
        assert!(call("open_mandate_returns", json!({ "account_id": null })).is_array());
        let csv = call("export_mandates_csv", json!({ "account_id": giro }));
        assert!(csv.as_str().unwrap().contains("M-2024-001"));
        let parsed = call_ok("parse_mandate_csv", json!({ "text": csv }));
        assert_eq!(
            parsed["rows"].as_array().unwrap().len(),
            mandates.as_array().unwrap().len()
        );
        let saved = call(
            "save_mandate",
            json!({ "id": null, "input": { "account_id": giro, "debtor_name": "Testverein",
                "debtor_iban": "DE02120300000000202051", "debtor_bic": "", "reference": "WP0-1",
                "signed_date": "2025-01-02", "scheme": "Core", "one_off": false,
                "default_amount_cents": 500, "default_purpose": "Beitrag", "note": "",
                "active": true } }),
        );
        assert_eq!(saved["reference"], "WP0-1");
        call("delete_mandate", json!({ "id": saved["id"] }));
        // Tags: create, assign, list, clear, rename, delete
        let booking = call(
            "list_transactions",
            json!({ "filter": {
            "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null,
            "limit": 1, "offset": 0, "scope": "All", "merchant_key": null, "direction": "All",
            "checked": null, "whole_days": false, "tag_ids": [], "untagged_only": false } }),
        )["transactions"][0]["id"]
            .clone();
        let tag = call(
            "create_tag",
            json!({ "name": "Testlauf", "color_hex": "#FF9500" }),
        );
        let changed = call(
            "add_tag_to_bookings",
            json!({ "booking_ids": [booking], "tag_id": tag["id"] }),
        );
        assert_eq!(changed, json!([booking]));
        let listed = call("list_tags", json!({}));
        let row = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == tag["id"])
            .cloned()
            .unwrap();
        assert_eq!(
            (row["name"].clone(), row["booking_count"].clone()),
            (json!("Testlauf"), json!(1))
        );
        call(
            "set_booking_tags",
            json!({ "booking_id": booking, "tag_ids": [] }),
        );
        call("rename_tag", json!({ "id": tag["id"], "name": "Reise" }));
        let listed = call("list_tags", json!({}));
        let row = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == tag["id"])
            .unwrap();
        assert_eq!(
            (row["name"].clone(), row["booking_count"].clone()),
            (json!("Reise"), json!(0))
        );
        call("delete_tag", json!({ "id": tag["id"] }));
        // Anlageklassen
        for a in call("list_accounts", json!({})).as_array().unwrap() {
            assert!(call("classified_assets", json!({ "account_id": a["id"] })).is_array());
        }
        // Sammelüberweisung: CSV import (free function)
        let batch = call_ok(
            "parse_batch_csv",
            json!({ "text": "Name;IBAN;Betrag;Verwendungszweck\nMia Beispiel;DE89370400440532013000;12,50;Test\n" }),
        );
        assert_eq!(batch["rows"].as_array().unwrap().len(), 1, "{batch}");
        // … from a file in Windows-1252 (spreadsheet export): „ü“ survives
        let csv = std::env::temp_dir().join(format!("girocrab-batch-{}.csv", std::process::id()));
        std::fs::write(
            &csv,
            b"Name;IBAN;Betrag;Verwendungszweck\nJ\xfcrgen M\x80ller;DE89370400440532013000;12,50;Beitrag\n",
        )
        .unwrap();
        let read = call_ok(
            "read_batch_csv",
            json!({ "path": format!("file://{}", csv.display()) }),
        );
        assert_eq!(read["rows"][0]["name"], json!("Jürgen M€ller"), "{read}");
        let _ = std::fs::remove_file(&csv);
        // Lastschriften: member list from a file in Windows-1252
        let csv =
            std::env::temp_dir().join(format!("girocrab-mandates-{}.csv", std::process::id()));
        std::fs::write(
            &csv,
            b"Name;IBAN;Mandatsreferenz;Datum\nJ\xfcrgen Beispiel;DE89370400440532013000;M-1;01.02.2025\n",
        )
        .unwrap();
        let read = call_ok(
            "read_mandate_csv",
            json!({ "path": format!("file://{}", csv.display()) }),
        );
        assert_eq!(
            read["rows"][0]["debtor_name"],
            json!("Jürgen Beispiel"),
            "{read}"
        );
        let _ = std::fs::remove_file(&csv);
        drop(v);
        let _ = std::fs::remove_file(path);
    }

    /// Calls that use the vault slots: backup of the own vault (write, change, restore)
    /// and a batch transfer session in the registry.
    #[test]
    fn backup_round_trip_and_payment_sessions() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("girocrab-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("own.girocrab").display().to_string(), vec![7; 32]).unwrap();
        v.seed_demo_data().unwrap();
        *VAULT.lock().unwrap() = Some(v.clone());
        *OWN.lock().unwrap() = Some(v.clone());
        let backup = dir.join("sicherung.girocrab");
        call_ok(
            "write_backup",
            json!({ "dest_path": format!("file://{}", backup.display()), "password": "geheim1234" }),
        );
        assert!(backup.exists());
        let tag = call_ok(
            "create_tag",
            json!({ "name": "Nach Sicherung", "color_hex": "" }),
        );
        let wrong: Value = serde_json::from_str(&call_json(
            "restore_backup",
            &json!({ "backup_path": backup.display().to_string(), "password": "falsch" })
                .to_string(),
        ))
        .unwrap();
        assert!(wrong.get("err").is_some(), "wrong password must be refused");
        call_ok(
            "restore_backup",
            json!({ "backup_path": backup.display().to_string(), "password": "geheim1234" }),
        );
        let tags = call_ok("list_tags", json!({}));
        assert!(
            !tags
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == tag["id"]),
            "restored vault is the backed-up one"
        );
        // Sammelüberweisung on the demo FinTS account: a TransferSession in the registry
        let mut session = None;
        for a in call_ok("list_accounts", json!({})).as_array().unwrap() {
            let r: Value = serde_json::from_str(&call_json(
                "begin_batch_transfer",
                &json!({ "request": { "account_id": a["id"], "batch_booking": true,
                    "execution_date": null, "payments": [{ "name": "Mia Beispiel",
                    "iban": "DE89370400440532013000", "bic": null, "amount_cents": 1250,
                    "purpose": "Test" }] }, "secret": "", "demo": true })
                .to_string(),
            ))
            .unwrap();
            if let Some(id) = r["ok"].as_u64() {
                session = Some(id);
                break;
            }
        }
        let id = session.expect("a demo account accepts a batch transfer");
        assert!(crate::sessions::get(id).is_ok());
        crate::sessions::remove(id);
        *VAULT.lock().unwrap() = None;
        *OWN.lock().unwrap() = None;
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Tags verwalten (#228, TagManager.qml): colour, order, rename, merge, delete with the
    /// argument names the QML sends.
    #[test]
    fn tag_management_calls() {
        let path =
            std::env::temp_dir().join(format!("girocrab-tags-{}.girocrab", std::process::id()));
        let v = Vault::open(path.display().to_string(), vec![5; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let row = |id: &Value| {
            call("list_tags", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == *id)
                .cloned()
        };
        let bookings = call(
            "list_transactions",
            json!({ "filter": {
            "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null,
            "limit": 2, "offset": 0, "scope": "All", "merchant_key": null, "direction": "All",
            "checked": null, "whole_days": false, "tag_ids": [], "untagged_only": false } }),
        )["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].clone())
            .collect::<Vec<_>>();
        let a = call(
            "create_tag",
            json!({ "name": "Verwaltet A", "color_hex": "" }),
        );
        let b = call(
            "create_tag",
            json!({ "name": "Verwaltet B", "color_hex": "" }),
        );
        call(
            "add_tag_to_bookings",
            json!({ "booking_ids": [bookings[0]], "tag_id": a["id"] }),
        );
        call(
            "add_tag_to_bookings",
            json!({ "booking_ids": [bookings[1]], "tag_id": b["id"] }),
        );
        call(
            "set_tag_color",
            json!({ "id": a["id"], "color_hex": "#16A085" }),
        );
        assert_eq!(row(&a["id"]).unwrap()["color_hex"], json!("#16A085"));
        // reorder: B first
        let mut ids: Vec<Value> = call("list_tags", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].clone())
            .filter(|id| *id != b["id"])
            .collect();
        ids.insert(0, b["id"].clone());
        call("reorder_tags", json!({ "ids": ids }));
        assert_eq!(call("list_tags", json!({}))[0]["id"], b["id"]);
        // a case-only rename is allowed
        call(
            "rename_tag",
            json!({ "id": a["id"], "name": "verwaltet a" }),
        );
        assert_eq!(row(&a["id"]).unwrap()["name"], json!("verwaltet a"));
        // merge B into A: A carries both bookings, B is gone
        call(
            "merge_tag_into",
            json!({ "source": b["id"], "target": a["id"] }),
        );
        assert!(row(&b["id"]).is_none());
        assert_eq!(row(&a["id"]).unwrap()["booking_count"], json!(2));
        call("delete_tag", json!({ "id": a["id"] }));
        assert!(row(&a["id"]).is_none());
        drop(v);
        let _ = std::fs::remove_file(&path);
    }

    /// Tag-Filter und Auswertung (#778, TagFilter.qml / TagManagerSheet.qml): the list filter's
    /// `tag_ids` (AND) and `untagged_only`, `tag_totals` per tag plus „Ohne Tag“, and
    /// `tag_account_counts`, whose own rows add up to the tag's booking count.
    #[test]
    fn tag_filter_calls() {
        let path = std::env::temp_dir().join(format!(
            "girocrab-tagfilter-{}.girocrab",
            std::process::id()
        ));
        let v = Vault::open(path.display().to_string(), vec![6; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let call = |m: &str, a: Value| {
            dispatch_vault(&v, m, &a)
                .unwrap()
                .unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        let filter = |tag_ids: Value, untagged: bool| {
            json!({
            "account_ids": [], "category_id": null, "include_subcategories": true,
            "uncategorized_only": false, "search": null, "date_from": null, "date_to": null,
            "limit": 0, "offset": 0, "scope": "All", "merchant_key": null, "direction": "All",
            "checked": null, "whole_days": false, "tag_ids": tag_ids, "untagged_only": untagged })
        };
        let count = |f: Value| {
            call("list_transactions", json!({ "filter": f }))["transactions"]
                .as_array()
                .unwrap()
                .len() as u64
        };
        let tags = call("list_tags", json!({}));
        let used: Vec<Value> = tags
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["booking_count"].as_u64().unwrap() > 0)
            .cloned()
            .collect();
        assert!(used.len() >= 2, "demo has tagged bookings");
        let first = &used[0];
        let all = count(filter(json!([]), false));
        let tagged = count(filter(json!([first["id"]]), false));
        assert_eq!(json!(tagged), first["booking_count"]);
        let untagged = count(filter(json!([]), true));
        assert!(untagged > 0 && untagged < all);
        // several tags: only bookings carrying all of them
        let both = count(filter(json!([used[0]["id"], used[1]["id"]]), false));
        assert!(both <= tagged);
        // totals ignore the filter's own tag choice
        let totals = call(
            "tag_totals",
            json!({ "filter": filter(json!([first["id"]]), true) }),
        );
        let rows = totals.as_array().unwrap();
        let of = |id: &Value| rows.iter().find(|r| r["tag_id"] == *id).cloned();
        let row = of(&first["id"]).unwrap();
        assert_eq!(row["booking_count"], first["booking_count"]);
        assert!(row["sum_cents"].is_i64());
        assert_eq!(of(&Value::Null).unwrap()["booking_count"], json!(untagged));
        // per account: own rows add up to the tag's count, linked loans come extra
        let accounts = call("tag_account_counts", json!({ "tag_id": first["id"] }));
        let own: u64 = accounts
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["linked"] == json!(false))
            .map(|r| r["booking_count"].as_u64().unwrap())
            .sum();
        assert_eq!(json!(own), first["booking_count"]);
        assert!(accounts[0]["account_id"].is_i64());
        drop(v);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_key_accepts_base64_and_hex() {
        assert_eq!(
            crate::vault_key::parse_key(&"ab".repeat(32)).unwrap().len(),
            32
        );
        assert!(crate::vault_key::parse_key("AAAA").is_err());
    }

    /// Mitteilungen: switched off (or checked recently) no request is made; a stored
    /// file that does not verify is no document. No network in tests.
    #[test]
    fn announcements_calls() {
        let off = call_ok(
            "announcements_refresh",
            json!({ "enabled": false, "force": true, "last_check_ms": null }),
        );
        assert_eq!(off, json!({ "fetched": false }));
        let fresh = call_ok(
            "announcements_refresh",
            json!({ "enabled": true, "force": false, "last_check_ms": now_ms() }),
        );
        assert_eq!(fresh, json!({ "fetched": false }));
        assert_eq!(
            call_ok(
                "announcements_load",
                json!({ "json": "e30=", "signature": "" })
            ),
            Value::Null
        );
        assert_eq!(call_ok("announcements_load", json!({})), Value::Null);
    }

    fn now_ms() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as f64
    }
}

/// Dev helper: `GIROCRAB_FIXTURE_DIR=<dir> cargo test -- --ignored make_dev_vault`
/// writes a seeded `tresor-dev.girocrab` (dev key) for UI tests of the own vault.
#[cfg(test)]
#[test]
#[ignore]
fn make_dev_vault() {
    let dir = std::path::PathBuf::from(
        std::env::var("GIROCRAB_FIXTURE_DIR").expect("GIROCRAB_FIXTURE_DIR"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let v = Vault::open(
        dir.join("tresor-dev.girocrab").display().to_string(),
        vec![0x42; 32],
    )
    .unwrap();
    v.seed_demo_data().unwrap();
    v.checkpoint().unwrap();
}

/// `GIROCRAB_FIXTURE_DIR=… cargo test -q make_big_dev_vault -- --ignored`
/// writes a large `tresor-dev.girocrab` (dev key) with invented data in the size of
/// a real MoneyMoney archive: 8 accounts, ~7000 bookings, 240 categories. For
/// performance checks of the UI with many accounts, bookings and categories.
#[cfg(test)]
#[test]
#[ignore]
fn make_big_dev_vault() {
    let dir = std::path::PathBuf::from(
        std::env::var("GIROCRAB_FIXTURE_DIR").expect("GIROCRAB_FIXTURE_DIR"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tresor-dev.girocrab");
    let _ = std::fs::remove_file(&path);
    let v = Vault::open(path.display().to_string(), vec![0x42; 32]).unwrap();
    let mains = [
        "Haushalt",
        "Wohnen",
        "Mobilität",
        "Freizeit",
        "Gesundheit",
        "Versicherungen",
        "Steuern",
        "Kinder",
        "Reisen",
        "Einkommen",
        "Sparen",
        "Technik",
    ];
    let names = [
        "REWE Markt",
        "EDEKA",
        "Stadtwerke",
        "Deutsche Bahn",
        "Apotheke",
        "Allianz",
        "Finanzamt",
        "Kita Sonnenschein",
        "Lufthansa",
        "Arbeitgeber GmbH",
        "ETF Sparplan",
        "MediaMarkt",
    ];
    let mut seed: u64 = 7;
    let mut rnd = |m: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for acc in 0..8 {
        let mut csv = String::from(
            "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\";\"Notiz\"\n",
        );
        for i in 0..875u64 {
            // 875 bookings over 17 years (2009–2026), like a long-used MoneyMoney archive
            let day = 1 + (i * 3 + acc as u64) % 28;
            let month = 1 + (i / 4) % 12;
            let year = 2009 + i / 52;
            let m = rnd(mains.len() as u64) as usize;
            let sub = rnd(20);
            let cents = 100 + rnd(25_000) as i64;
            let amount = if m == 9 { cents * 10 } else { -cents };
            csv.push_str(&format!(
                "\"{day:02}.{month:02}.{year}\";\"{day:02}.{month:02}.{year}\";\"{}\\Unterkategorie {sub:02}\";\"{} {i}\";\"Buchung {i}\";\"DE02120300000000{acc:06}\";\"BYLADEM1001\";\"{},{:02}\";\"EUR\";\"\"\n",
                mains[m], names[m], amount / 100, (amount % 100).abs()
            ));
        }
        let file = dir.join(format!("Konto-{acc}.csv"));
        std::fs::write(&file, csv).unwrap();
        v.import_moneymoney_data(
            std::fs::read(&file).unwrap(),
            format!("Konto-{acc}.csv"),
            None,
            true,
            true,
        )
        .unwrap();
        let _ = std::fs::remove_file(&file);
    }
    v.checkpoint().unwrap();
}
