//! Announcements.swift for Linux: project news ("Mitteilungen", docs/announcements.md).
//!
//! A signed `announcements.json` (same Ed25519 keys and the same detached `.sig` as
//! `banking-policy.json`) with text and https links only. This module verifies,
//! parses, filters for this build (distribution `linux`, version) and localizes;
//! the state the Mac keeps in UserDefaults (`announcements.enabled`, `.dismissed`,
//! `.json`, `.signature`, `.lastCheck`) lives in QSettings (qml/Announcements.qml).
//!
//! The check is a plain GET without identifiers, cookies or User-Agent, at most every
//! six hours, and only while the switch in Settings › Allgemein is on. Failures stay
//! silent: announcements are optional.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::{Value, json};

const SUPPORTED_SCHEMA: i64 = 1;
/// `AnnouncementCenter.defaultURL`: next to the banking policy, independent of releases.
const URL: &str =
    "https://raw.githubusercontent.com/GiroCrab/GiroCrab/main/policy/announcements.json";
/// `URLSessionBankingPolicyFetcher.maxBytes`
const MAX_BYTES: usize = 64 * 1024;
/// `AnnouncementCenter.freshness`: between two downloads at least six hours.
const FRESHNESS_MS: f64 = 6.0 * 60.0 * 60.0 * 1000.0;

#[derive(Debug, Deserialize)]
struct RawItem {
    id: String,
    severity: Option<String>,
    from: Option<String>,
    until: Option<String>,
    distribution: Option<Vec<String>>,
    #[serde(rename = "minVersion")]
    min_version: Option<String>,
    #[serde(rename = "maxVersion")]
    max_version: Option<String>,
    title: std::collections::BTreeMap<String, String>,
    summary: Option<std::collections::BTreeMap<String, String>>,
    body: Option<std::collections::BTreeMap<String, String>>,
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDocument {
    schema: i64,
    sequence: u64,
    announcements: Vec<RawItem>,
}

/// The keys the feed is signed with: `BankingPolicy.publicKeys`; a debug build may use a
/// test key (`GIROCRAB_ANNOUNCEMENTS_KEY`, Swift: `#if DEBUG`).
fn keys() -> Vec<VerifyingKey> {
    #[cfg(debug_assertions)]
    if let Ok(k) = std::env::var("GIROCRAB_ANNOUNCEMENTS_KEY")
        && !k.is_empty()
    {
        return decode_keys(&[k.as_str()]);
    }
    decode_keys(crate::banking_policy::PUBLIC_KEYS)
}

fn decode_keys(list: &[&str]) -> Vec<VerifyingKey> {
    list.iter()
        .filter_map(|k| base64::engine::general_purpose::STANDARD.decode(k).ok())
        .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .filter_map(|b| VerifyingKey::from_bytes(&b).ok())
        .collect()
}

fn url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(u) = std::env::var("GIROCRAB_ANNOUNCEMENTS_URL")
        && !u.is_empty()
    {
        return u;
    }
    URL.to_string()
}

fn verify(json: &[u8], sig: &[u8], keys: &[VerifyingKey]) -> bool {
    let Ok(sig) = <[u8; 64]>::try_from(sig) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig);
    keys.iter().any(|k| k.verify(json, &sig).is_ok())
}

/// "yyyy-MM-dd" (UTC) → milliseconds; `end_of_day` adds a day minus a second.
fn day_ms(s: Option<&str>, end_of_day: bool) -> Option<f64> {
    let s = s?.trim();
    let mut p = s.splitn(3, '-');
    let (y, m, d) = (p.next()?, p.next()?, p.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let date = jiff::civil::Date::new(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?).ok()?;
    let ms = date
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_millisecond() as f64;
    Some(if end_of_day {
        ms + (24.0 * 3600.0 - 1.0) * 1000.0
    } else {
        ms
    })
}

/// `Announcement.localized`: the language, else English, else the first text by key.
fn localized(field: &std::collections::BTreeMap<String, String>, language: &str) -> String {
    field
        .get(language)
        .or_else(|| field.get("en"))
        .or_else(|| field.values().next())
        .cloned()
        .unwrap_or_default()
}

/// `AnnouncementsDocument.parse` + `applicable` (without the date window, which QML
/// checks against the clock) for `distribution` and `version`, localized to `language`.
/// Entries without id or title are dropped, unknown severities count as `info`, links
/// other than https are ignored. None = malformed or unknown schema.
fn document(json: &[u8], distribution: &str, version: &str, language: &str) -> Option<Value> {
    let raw: RawDocument = serde_json::from_slice(json).ok()?;
    if raw.schema != SUPPORTED_SCHEMA {
        return None;
    }
    let items: Vec<Value> = raw
        .announcements
        .iter()
        .filter_map(|r| {
            let id = r.id.trim();
            if id.is_empty() || r.title.is_empty() {
                return None;
            }
            if r
                .distribution
                .as_ref()
                .is_some_and(|d| !d.iter().any(|x| x == distribution))
            {
                return None;
            }
            use std::cmp::Ordering::{Greater, Less};
            let cmp = crate::banking_policy::compare_versions;
            if let Some(min) = &r.min_version
                && !version.is_empty()
                && cmp(version, min) == Less
            {
                return None;
            }
            if let Some(max) = &r.max_version
                && !version.is_empty()
                && cmp(version, max) == Greater
            {
                return None;
            }
            let empty = std::collections::BTreeMap::new();
            Some(json!({
                "id": id,
                "severity": if r.severity.as_deref() == Some("important") { "important" } else { "info" },
                "from_ms": day_ms(r.from.as_deref(), false),
                "until_ms": day_ms(r.until.as_deref(), true),
                "title": localized(&r.title, language),
                "summary": localized(r.summary.as_ref().unwrap_or(&empty), language),
                "body": localized(r.body.as_ref().unwrap_or(&empty), language),
                "link": r.link.as_deref().filter(|l| l.starts_with("https://")),
            }))
        })
        .collect();
    Some(json!({ "sequence": raw.sequence, "announcements": items }))
}

fn verified_document(json: &[u8], sig: &[u8], keys: &[VerifyingKey]) -> Value {
    if !verify(json, sig, keys) {
        return Value::Null;
    }
    document(
        json,
        crate::banking_policy::DISTRIBUTION,
        env!("GIROCRAB_VERSION"),
        crate::i18n::running(),
    )
    .unwrap_or(Value::Null)
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .unwrap_or_default()
}

/// `AnnouncementCenter.loadStored`: the stored file (QSettings `announcements/json` and
/// `/signature`, base64) as a document, or null if it does not verify.
pub fn load(json_b64: &str, sig_b64: &str) -> Value {
    verified_document(&unb64(json_b64), &unb64(sig_b64), &keys())
}

/// `URLSessionBankingPolicyFetcher.fetch`: file and `.sig`, 10 s, ≤ 64 KiB, HTTP 200 only.
fn fetch() -> Result<(Vec<u8>, Vec<u8>), String> {
    crate::sessions::RUNTIME.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let get = |url: String| {
            let client = client.clone();
            async move {
                let r = client.get(&url).send().await.map_err(|e| e.to_string())?;
                if r.status() != 200 {
                    return Err(format!("HTTP {}", r.status()));
                }
                let b = r.bytes().await.map_err(|e| e.to_string())?;
                if b.len() > MAX_BYTES {
                    return Err("too large".into());
                }
                Ok(b.to_vec())
            }
        };
        let url = url();
        let (j, s) = tokio::join!(get(url.clone()), get(format!("{url}.sig")));
        Ok((j?, s?))
    })
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `AnnouncementCenter.refresh(force:)` (worker thread, `Core.callAsync`): no request when
/// the switch is off, without a valid key, or while the last check (`last_check_ms`) is
/// younger than six hours. Returns `{fetched: false}` then (also on any network error),
/// else `{fetched: true, json, signature, document}`: QML stores the time of the check,
/// and the file if `document` verified and its `sequence` is not lower (anti-rollback).
pub fn refresh(enabled: bool, force: bool, last_check_ms: Option<f64>) -> Value {
    refresh_with(enabled, force, last_check_ms, now_ms(), &keys(), fetch)
}

fn refresh_with(
    enabled: bool,
    force: bool,
    last_check_ms: Option<f64>,
    now: f64,
    keys: &[VerifyingKey],
    fetch: impl FnOnce() -> Result<(Vec<u8>, Vec<u8>), String>,
) -> Value {
    if !enabled || keys.is_empty() {
        return json!({ "fetched": false });
    }
    if !force && last_check_ms.is_some_and(|t| t > 0.0 && now - t < FRESHNESS_MS) {
        return json!({ "fetched": false });
    }
    match fetch() {
        Err(_) => json!({ "fetched": false }),
        Ok((j, s)) => json!({
            "fetched": true,
            "json": b64(&j),
            "signature": b64(&s),
            "document": verified_document(&j, &s, keys),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    /// A signed feed like `policy/announcements.example.json`.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let json = serde_json::to_vec(&json!({
            "schema": 1, "sequence": 7,
            "announcements": [
                { "id": "mac-only", "distribution": ["developer-id", "mac-app-store"],
                  "title": { "de": "Nur Mac", "en": "Mac only" } },
                { "id": "2026-11-example", "severity": "important",
                  "from": "2026-11-15", "until": "2027-01-31",
                  "distribution": ["developer-id", "linux"],
                  "title": { "de": "Titel", "en": "Title" },
                  "summary": { "de": "Ein bis zwei Sätze.", "en": "One or two sentences." },
                  "body": { "fr": "Texte" },
                  "link": "https://github.com/GiroCrab/GiroCrab/discussions" },
                { "id": "old", "maxVersion": "0.1", "title": { "de": "Alt" } },
                { "id": "  ", "title": { "de": "Ohne ID" } },
                { "id": "plain", "severity": "urgent", "title": { "en": "Only English" },
                  "link": "http://example.com" }
            ]
        }))
        .unwrap();
        let sig = test_key().sign(&json).to_bytes().to_vec();
        (json, sig)
    }

    #[test]
    fn signed_feed_is_filtered_and_localized() {
        let keys = [test_key().verifying_key()];
        let (json, sig) = sample();
        let doc = verified_document(&json, &sig, &keys);
        assert_eq!(doc["sequence"], json!(7));
        let ids: Vec<&str> = doc["announcements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["2026-11-example", "plain"],
            "linux, version, id rules"
        );
        let a = &doc["announcements"][0];
        assert_eq!(a["severity"], "important");
        assert_eq!(
            a["link"],
            "https://github.com/GiroCrab/GiroCrab/discussions"
        );
        assert_eq!(a["body"], "Texte", "fallback to any text");
        assert_eq!(
            a["from_ms"].as_f64().unwrap(),
            day_ms(Some("2026-11-15"), false).unwrap()
        );
        assert_eq!(
            a["until_ms"].as_f64().unwrap() - day_ms(Some("2027-01-31"), false).unwrap(),
            86_399_000.0
        );
        let p = &doc["announcements"][1];
        assert_eq!(p["severity"], "info", "unknown severity counts as info");
        assert_eq!(p["link"], Value::Null, "http links are ignored");
        assert_eq!(p["title"], "Only English");
        assert_eq!(
            document(&json, "linux", "0.7.5", "en").unwrap()["announcements"][0]["title"],
            "Title"
        );
    }

    #[test]
    fn tampered_or_foreign_feed_is_refused() {
        let (json, sig) = sample();
        let mut tampered = json.clone();
        tampered[10] ^= 1;
        assert_eq!(
            verified_document(&tampered, &sig, &[test_key().verifying_key()]),
            Value::Null
        );
        // the release key does not accept the test signature
        assert_eq!(
            verified_document(
                &json,
                &sig,
                &decode_keys(crate::banking_policy::PUBLIC_KEYS)
            ),
            Value::Null
        );
        assert_eq!(load(&b64(&json), "not base64"), Value::Null);
    }

    #[test]
    fn switch_off_or_fresh_check_makes_no_request() {
        let keys = [test_key().verifying_key()];
        let no_request = || -> Result<(Vec<u8>, Vec<u8>), String> { panic!("network request") };
        let off = refresh_with(false, true, None, 1e12, &keys, no_request);
        assert_eq!(off, json!({ "fetched": false }));
        let fresh = refresh_with(true, false, Some(1e12 - 60_000.0), 1e12, &keys, no_request);
        assert_eq!(fresh, json!({ "fetched": false }));
        assert_eq!(
            refresh_with(true, true, None, 1e12, &[], no_request),
            json!({ "fetched": false }),
            "without a key"
        );
        // due: one request, verified document handed back with the raw file
        let r = refresh_with(
            true,
            false,
            Some(1e12 - FRESHNESS_MS - 1.0),
            1e12,
            &keys,
            || Ok(sample()),
        );
        assert_eq!(r["fetched"], json!(true));
        assert_eq!(r["document"]["sequence"], json!(7));
        assert_eq!(unb64(r["json"].as_str().unwrap()), sample().0);
        // network errors stay silent
        let failed = refresh_with(true, true, None, 1e12, &keys, || Err("offline".into()));
        assert_eq!(failed, json!({ "fetched": false }));
    }
}
