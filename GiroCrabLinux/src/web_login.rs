//! Web logins (WebLoginSheet.qml): the browser profile's cookies and the core
//! calls of the portal flows (PortalUI.swift, AmazonUI.swift, FnzUI.swift,
//! QuirionUI.swift, SchwaebischHallUI.swift, EquatePlusUI.swift, ConnectionsUI.swift › WebLoginSheet).
//!
//! Cookies are read from the QtWebEngine cookie store (cpp/webcookies.cpp) and
//! go straight into the vault; they never travel through QML / JavaScript and
//! are never written to a plain file. The browser profile is off the record;
//! the portal's cookies are kept per login in the encrypted vault (setting
//! `web.secret_cookies.<connection_id>`, flagged secret) and put back into the
//! profile before the next login page loads – what WKWebsiteDataStore.default()
//! does on the Mac, without a cookie file on disk.

use std::sync::Arc;

use base64::Engine;
use girocrab_core::Vault;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

type R = Result<Value, String>;

unsafe extern "C" {
    fn girocrab_web_attach() -> bool;
    fn girocrab_web_cookies_json() -> *mut std::os::raw::c_char;
    fn girocrab_web_free(s: *mut std::os::raw::c_char);
    fn girocrab_web_set_cookie(json: *const std::os::raw::c_char) -> bool;
    fn girocrab_web_delete_cookie(
        domain: *const std::os::raw::c_char,
        name: *const std::os::raw::c_char,
        path: *const std::os::raw::c_char,
    );
}

/// The `Core.call` methods of this module.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        "web_attach"
            | "web_has_cookie"
            | "web_clear_cookies"
            | "web_save_cookies"
            | "web_restore_cookies"
            | "web_forget_cookies"
            | "apply_web_session"
            | "import_portal_capture"
            | "preview_portal_capture"
            | "import_statement_file"
            | "import_amazon_orders"
            | "import_fnz_overview"
            | "pending_fnz_documents"
            | "import_fnz_document"
            | "import_quirion_snapshot"
            | "pending_quirion_documents"
            | "import_quirion_document"
            | "import_schwaebisch_hall_snapshot"
            | "pending_schwaebisch_hall_documents"
            | "import_schwaebisch_hall_document"
            | "import_equateplus_snapshot"
            | "pending_equateplus_documents"
            | "import_equateplus_document"
    )
}

fn arg<T: DeserializeOwned>(a: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(a.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("argument `{name}`: {e}"))
}

fn ok<T: serde::Serialize>(v: T) -> R {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn err(e: girocrab_core::CoreError) -> String {
    e.to_string()
}

fn bytes(a: &Value) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(arg::<String>(a, "bytes_base64")?)
        .map_err(|e| e.to_string())
}

pub fn call(method: &str, a: &Value, vault: fn() -> Result<Arc<Vault>, String>) -> R {
    let id = || arg::<i64>(a, "connection_id");
    match method {
        // SAFETY: Core.call runs on the GUI thread, where the QML objects live.
        "web_attach" => ok(unsafe { girocrab_web_attach() }),
        "web_has_cookie" => {
            let domains: Vec<String> = arg(a, "domains")?;
            let name: String = arg(a, "name")?;
            ok(cookies()
                .iter()
                .any(|c| c.name == name && domain_matches(&c.domain, &domains)))
        }
        "web_clear_cookies" => {
            let domains: Vec<String> = arg(a, "domains")?;
            let name: Option<String> = arg(a, "name")?;
            // with a login: its cookies kept in the vault go as well
            if let Some(id) = arg::<Option<i64>>(a, "connection_id")? {
                forget(vault()?.as_ref(), id)?;
            }
            let mut n = 0;
            for c in cookies() {
                if domain_matches(&c.domain, &domains) && name.as_ref().is_none_or(|w| *w == c.name)
                {
                    delete(&c);
                    n += 1;
                }
            }
            ok(n)
        }
        // After a login and when the sheet closes: the portal's cookies -> vault.
        "web_save_cookies" => {
            let domains: Vec<String> = arg(a, "domains")?;
            // SAFETY: GUI thread (see above).
            if !unsafe { girocrab_web_attach() } {
                return ok(false); // no browser profile: keep what is stored
            }
            let skip: Vec<String> = arg::<Option<_>>(a, "skip_names")?.unwrap_or_default();
            let mut all = cookies();
            all.retain(|c| !skip.contains(&c.name));
            let json = to_store(&all, &domains, now());
            vault()?
                .set_setting(stored_key(id()?), json.to_string())
                .map_err(err)?;
            ok(true)
        }
        // Before the first page of a login loads: vault -> browser profile.
        "web_restore_cookies" => {
            let domains: Vec<String> = arg(a, "domains")?;
            let skip: Vec<String> = arg::<Option<_>>(a, "skip_names")?.unwrap_or_default();
            let stored = vault()?.get_setting(stored_key(id()?)).map_err(err)?;
            let stored = Zeroizing::new(stored.unwrap_or_default());
            let mut n = 0;
            for c in from_store(&stored, &domains, now(), &skip) {
                let json = Zeroizing::new(serde_json::to_string(&c).map_err(|e| e.to_string())?);
                let Ok(cs) = std::ffi::CString::new(json.as_str()) else {
                    continue;
                };
                // SAFETY: GUI thread; the string outlives the call.
                if unsafe { girocrab_web_set_cookie(cs.as_ptr()) } {
                    n += 1;
                }
                let mut bytes = cs.into_bytes();
                zeroize::Zeroize::zeroize(&mut bytes);
            }
            ok(n)
        }
        "web_forget_cookies" => {
            forget(vault()?.as_ref(), id()?)?;
            ok(())
        }
        "apply_web_session" => {
            let domains: Vec<String> = arg(a, "domains")?;
            let header = cookie_header(&cookies(), &domains);
            vault()?
                .set_connection_session(id()?, header.to_string())
                .map_err(err)?;
            ok(())
        }
        "import_portal_capture" => ok(vault()?
            .import_portal_capture(id()?, arg(a, "json_bodies")?, arg(a, "dom_rows_json")?)
            .map_err(err)?),
        "preview_portal_capture" => ok(vault()?
            .preview_portal_capture(id()?, arg(a, "json_bodies")?, arg(a, "dom_rows_json")?)
            .map_err(err)?),
        "import_statement_file" => {
            let path: String = arg(a, "path")?;
            let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
            let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // pdf_text_hint: empty, the core extracts the PDF text itself
            ok(vault()?
                .import_statement(id()?, name, data, String::new())
                .map_err(err)?)
        }
        "import_amazon_orders" => ok(vault()?
            .import_amazon_orders(id()?, arg(a, "orders_json")?)
            .map_err(err)?),
        "import_fnz_overview" => ok(vault()?
            .import_fnz_overview(id()?, arg(a, "overview_json")?)
            .map_err(err)?),
        "pending_fnz_documents" => ok(vault()?
            .pending_fnz_documents(id()?, arg(a, "documents_json")?)
            .map_err(err)?),
        "import_fnz_document" => ok(vault()?
            .import_fnz_document(id()?, arg(a, "document_json")?, bytes(a)?)
            .map_err(err)?),
        "import_quirion_snapshot" => ok(vault()?
            .import_quirion_snapshot(id()?, arg(a, "snapshot_json")?)
            .map_err(err)?),
        "pending_quirion_documents" => ok(vault()?
            .pending_quirion_documents(id()?, arg(a, "documents_json")?)
            .map_err(err)?),
        "import_quirion_document" => ok(vault()?
            .import_quirion_document(id()?, arg(a, "document_json")?, bytes(a)?)
            .map_err(err)?),
        "import_schwaebisch_hall_snapshot" => ok(vault()?
            .import_schwaebisch_hall_snapshot(id()?, arg(a, "snapshot_json")?)
            .map_err(err)?),
        "pending_schwaebisch_hall_documents" => ok(vault()?
            .pending_schwaebisch_hall_documents(id()?, arg(a, "documents_json")?)
            .map_err(err)?),
        "import_schwaebisch_hall_document" => ok(vault()?
            .import_schwaebisch_hall_document(id()?, arg(a, "document_json")?, bytes(a)?)
            .map_err(err)?),
        "import_equateplus_snapshot" => ok(vault()?
            .import_equateplus_snapshot(id()?, arg(a, "snapshot_json")?)
            .map_err(err)?),
        "pending_equateplus_documents" => ok(vault()?
            .pending_equateplus_documents(id()?, arg(a, "documents_json")?)
            .map_err(err)?),
        "import_equateplus_document" => ok(vault()?
            .import_equateplus_document(id()?, arg(a, "document_json")?, bytes(a)?)
            .map_err(err)?),
        _ => Err(format!("unknown core method `{method}`")),
    }
}

/// One browser cookie, as cpp/webcookies.cpp reads and writes it.
#[derive(Serialize, Deserialize)]
struct Cookie {
    domain: String,
    name: String,
    value: String,
    path: String,
    /// Unix seconds; `None` = session cookie.
    #[serde(default)]
    expiry: Option<i64>,
    #[serde(default)]
    secure: bool,
    #[serde(default)]
    http_only: bool,
    #[serde(default)]
    same_site: String,
}

impl Drop for Cookie {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}

fn cookies() -> Vec<Cookie> {
    // SAFETY: GUI thread (see `call`); the string is freed by its allocator.
    unsafe {
        girocrab_web_attach();
        let p = girocrab_web_cookies_json();
        if p.is_null() {
            return Vec::new();
        }
        let json = Zeroizing::new(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned());
        girocrab_web_free(p);
        serde_json::from_str(&json).unwrap_or_default()
    }
}

fn stored_key(connection_id: i64) -> String {
    // "secret" in the key flags the setting as a secret (vault/settings.rs)
    format!("web.secret_cookies.{connection_id}")
}

fn forget(v: &Vault, connection_id: i64) -> Result<(), String> {
    v.set_setting(stored_key(connection_id), String::new())
        .map_err(err)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn alive(c: &Cookie, now: i64) -> bool {
    c.expiry.is_none_or(|e| e > now)
}

/// JSON of the portal's live cookies for the vault ("" = nothing to keep, deletes the setting).
fn to_store(cookies: &[Cookie], domains: &[String], now: i64) -> Zeroizing<String> {
    let keep: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| domain_matches(&c.domain, domains) && alive(c, now) && !c.name.is_empty())
        .collect();
    Zeroizing::new(if keep.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&keep).unwrap_or_default()
    })
}

/// Stored cookies to put back: only the portal's domains, not expired, not in `skip`.
fn from_store(json: &str, domains: &[String], now: i64, skip: &[String]) -> Vec<Cookie> {
    let all: Vec<Cookie> = serde_json::from_str(json).unwrap_or_default();
    all.into_iter()
        .filter(|c| domain_matches(&c.domain, domains) && alive(c, now) && !skip.contains(&c.name))
        .collect()
}

fn delete(c: &Cookie) {
    let cs = |s: &str| std::ffi::CString::new(s).unwrap_or_default();
    let (d, n, p) = (cs(&c.domain), cs(&c.name), cs(&c.path));
    // SAFETY: GUI thread; the strings outlive the call.
    unsafe { girocrab_web_delete_cookie(d.as_ptr(), n.as_ptr(), p.as_ptr()) };
}

/// WebLoginSheet.isTradeRepublicHost / PortalDomains.allows: the cookie domain
/// is one of `domains` or a subdomain (`.traderepublic.com` matches,
/// `nottraderepublic.com` and `traderepublic.com.evil.io` do not).
fn domain_matches(domain: &str, domains: &[String]) -> bool {
    let d = domain
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_lowercase();
    !d.is_empty()
        && domains.iter().any(|w| {
            let w = w.to_lowercase();
            d == w || d.ends_with(&format!(".{w}"))
        })
}

/// WebLoginSheet.trCookies: "name=value; …" of the matching cookies.
fn cookie_header(cookies: &[Cookie], domains: &[String]) -> Zeroizing<String> {
    let mut s = Zeroizing::new(String::new());
    for c in cookies
        .iter()
        .filter(|c| domain_matches(&c.domain, domains))
    {
        if !s.is_empty() {
            s.push_str("; ");
        }
        s.push_str(&c.name);
        s.push('=');
        s.push_str(&c.value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str, value: &str) -> Cookie {
        Cookie {
            domain: domain.into(),
            name: name.into(),
            value: value.into(),
            path: "/".into(),
            expiry: None,
            secure: true,
            http_only: false,
            same_site: "Lax".into(),
        }
    }

    /// Save -> app restart (vault closed and reopened) -> restore; forgetting deletes it.
    #[test]
    fn cookies_survive_a_vault_reopen_encrypted() {
        let dir = std::env::temp_dir().join(format!("girocrab-webcookies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.girocrab").display().to_string();
        let key = vec![7u8; 32];
        let amex = vec!["americanexpress.com".to_string()];
        let mut c = cookie(".americanexpress.com", "aat", "fake-session-value");
        c.expiry = Some(now() + 3600);
        {
            let v = Vault::open(path.clone(), key.clone()).unwrap();
            let json = to_store(&[c], &amex, now());
            v.set_setting(stored_key(42), json.to_string()).unwrap();
            v.checkpoint().unwrap();
        }
        // the value is not readable in the file
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(18).any(|w| w == b"fake-session-value"));
        let v = Vault::open(path.clone(), key.clone()).unwrap();
        let stored = v.get_setting(stored_key(42)).unwrap().unwrap_or_default();
        let back = from_store(&stored, &amex, now(), &[]);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].value, "fake-session-value");
        forget(&v, 42).unwrap();
        assert!(v.get_setting(stored_key(42)).unwrap().is_none());
        drop(v);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stored_cookies_round_trip_without_expired_or_foreign_ones() {
        let now = 1_800_000_000;
        let mut session = cookie(".americanexpress.com", "aat", "s3cr3t");
        session.http_only = true;
        let mut lasting = cookie("global.americanexpress.com", "blueboxid", "abc");
        lasting.expiry = Some(now + 3600);
        lasting.path = "/activity".into();
        let mut expired = cookie(".americanexpress.com", "old", "x");
        expired.expiry = Some(now - 1);
        let foreign = cookie(".tracker.example", "id", "y");
        let amex = vec!["americanexpress.com".to_string(), "aexp.com".to_string()];
        let json = to_store(&[session, lasting, expired, foreign], &amex, now);
        let back = from_store(&json, &amex, now, &[]);
        let names: Vec<_> = back.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["aat", "blueboxid"]);
        assert!(back[0].http_only && back[0].secure && back[0].expiry.is_none());
        assert_eq!(back[0].same_site, "Lax");
        assert_eq!(back[1].expiry, Some(now + 3600));
        assert_eq!(back[1].path, "/activity");
        assert_eq!(back[1].domain, "global.americanexpress.com");
        // later: expired by then, or restored for another portal, or skipped by name
        assert_eq!(from_store(&json, &amex, now + 7200, &[]).len(), 1);
        assert!(from_store(&json, &["fnz.de".to_string()], now, &[]).is_empty());
        assert_eq!(from_store(&json, &amex, now, &["aat".to_string()]).len(), 1);
        // nothing left to keep: the setting is deleted
        assert!(to_store(&[], &amex, now).is_empty());
        assert!(from_store("", &amex, now, &[]).is_empty());
    }

    static EQUATEPLUS_VAULT: std::sync::OnceLock<Arc<Vault>> = std::sync::OnceLock::new();
    fn equateplus_vault() -> Result<Arc<Vault>, String> {
        EQUATEPLUS_VAULT
            .get()
            .cloned()
            .ok_or_else(|| "no vault".into())
    }

    /// EquatePlusWebLoginFlow.qml: the three core calls with the JSON the flow sends
    /// (synthetic snapshot and library as in girocrab-core's equateplus tests).
    #[test]
    fn equateplus_calls_round_trip() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("girocrab-equateplus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("t.girocrab").display().to_string(), vec![7; 32]).unwrap();
        let id = v
            .add_bank_connection(
                girocrab_core::ConnectorKind::EquatePlus,
                "Test EquatePlus".into(),
                "test".into(),
            )
            .unwrap();
        assert!(EQUATEPLUS_VAULT.set(v).is_ok());
        let call_ok = |m: &str, a: Value| {
            call(m, &a, equateplus_vault).unwrap_or_else(|e| panic!("{m}: {e}"))
        };
        assert!(handles("import_equateplus_snapshot"));
        let snapshot = json!({"version":1,"date":"2026-11-15","user":{"participant":{"userId":"test-user"},"reportingCurrency":{"code":"EUR"}},
          "plans":[{"id":"plan-a","name":"Test Programm","details":{"totals":[{"type":"TOTAL","quantity":{"amount":6.875},"value":{"amount":848.72}}],"entries":[{"entries":[{
            "marketPrice":{"amount":123.45,"unit":{"code":"EUR"}},"canTrade":true,
            "entries":[{"VEHICLE":"Test Aktien","QUANTITY":{"amount":2.5},"LOCKED_QTY":{"amount":3.25},"SELL_PURCHASE_PRICE":{"amount":100}},
                       {"VEHICLE":"Test RSU","LOCKED_PERF_QTY":{"amount":1.125}}]}]}]}}]});
        let s = call_ok(
            "import_equateplus_snapshot",
            json!({ "connection_id": id, "snapshot_json": snapshot.to_string() }),
        );
        assert_eq!(s["accounts_updated"], json!(1));
        let library =
            json!([{"id": "d1", "date": "2011-05-26T00:00:00", "description": "Test Bestätigung",
          "documentType": "Dividendenbestätigung", "plan": "Test Programm",
          "url": "/services/statements/download?documentId=d1"}])
            .to_string();
        let args = json!({ "connection_id": id, "documents_json": library });
        let pending = call_ok("pending_equateplus_documents", args.clone());
        let pending = pending.as_array().unwrap();
        assert_eq!(pending.len(), 1);
        let pdf = base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 test");
        let stored = call_ok(
            "import_equateplus_document",
            json!({ "connection_id": id, "document_json": pending[0], "bytes_base64": pdf }),
        );
        assert_eq!(stored, json!(true));
        let again = call_ok("pending_equateplus_documents", args);
        assert!(again.as_array().unwrap().is_empty(), "stored once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trade_republic_host_on_label_boundaries() {
        let tr = vec!["traderepublic.com".to_string()];
        assert!(domain_matches("traderepublic.com", &tr));
        assert!(domain_matches(".traderepublic.com", &tr));
        assert!(domain_matches("app.TradeRepublic.com.", &tr));
        assert!(!domain_matches("nottraderepublic.com", &tr));
        assert!(!domain_matches("traderepublic.com.evil.io", &tr));
        assert!(!domain_matches("", &tr));
    }

    #[test]
    fn header_holds_only_the_portal_cookies() {
        let all = [
            cookie(".traderepublic.com", "tr_session", "a"),
            cookie("app.traderepublic.com", "tr_refresh", "b"),
            cookie(".americanexpress.com", "amex", "c"),
        ];
        let h = cookie_header(&all, &["traderepublic.com".to_string()]);
        assert_eq!(h.as_str(), "tr_session=a; tr_refresh=b");
        assert!(cookie_header(&all, &["fnz.de".to_string()]).is_empty());
    }
}
