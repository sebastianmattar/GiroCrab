//! BankingPolicy.swift for Linux: the signed "Sicherheitssperre".
//!
//! The publisher can publish a signed `banking-policy.json` (Ed25519, detached
//! `.sig`) with a minimum version per distribution. An older build may then no
//! longer contact banks; reading stays possible. This build's distribution is
//! `linux`. The file can only restrict; it never unlocks anything.
//!
//! Same rules as the Swift controller: the higher `sequence` wins, a policy is
//! fresh for 15 minutes, the policy bundled at build time is trusted for 90
//! days after the build date, and a device that never verified a policy online
//! (and has no valid bundled one) must not contact banks. The resulting block is
//! handed to the core (`banking_gate::set_banking_block`) as second line of defence.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::i18n::tr;

pub const DISTRIBUTION: &str = "linux";
const SUPPORTED_SCHEMA: i64 = 1;
const URL: &str =
    "https://raw.githubusercontent.com/GiroCrab/GiroCrab/main/policy/banking-policy.json";
const MAX_BYTES: usize = 64 * 1024;
const FRESHNESS: Duration = Duration::from_secs(15 * 60);
const BUNDLED_GRACE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// `BankingPolicy.publicKeys` (raw 32-byte Ed25519 keys, base64).
pub(crate) const PUBLIC_KEYS: &[&str] = &["sy8puSCv1QXXl0hU5cYaPaaShIltXDIXiKLRXLuNW+A="];
const BUNDLED_JSON: &[u8] = include_bytes!("../../policy/banking-policy.json");
const BUNDLED_SIG: &[u8] = include_bytes!("../../policy/banking-policy.json.sig");

#[derive(Debug, Clone, Deserialize)]
struct RawRule {
    distribution: Vec<String>,
    channels: Option<Vec<String>>,
    #[serde(rename = "minimumVersion")]
    minimum_version: String,
    connectors: Option<Vec<String>>,
    advisory: Option<String>,
    #[serde(rename = "updateAvailable")]
    update_available: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct Document {
    schema: i64,
    sequence: u64,
    rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Gate {
    /// "outdated" | "checkUnavailable"
    pub reason: String,
    pub all: bool,
    pub connector_ids: Vec<String>,
    pub minimum_version: Option<String>,
    pub advisory: Option<String>,
    pub update_available: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    json: String,
    signature: String,
    verified_online: bool,
}

struct State {
    document: Option<Document>,
    verified_online: bool,
    last_check: Option<SystemTime>,
    last_failure: Option<String>,
    gate: Option<Gate>,
    loaded: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    document: None,
    verified_online: false,
    last_check: None,
    last_failure: None,
    gate: None,
    loaded: false,
});

fn keys() -> Vec<VerifyingKey> {
    PUBLIC_KEYS
        .iter()
        .filter_map(|k| base64::engine::general_purpose::STANDARD.decode(k).ok())
        .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .filter_map(|b| VerifyingKey::from_bytes(&b).ok())
        .collect()
}

/// Without a valid key the whole feature is off: no request, no block.
/// `GIROCRAB_NO_BANKING_POLICY` is a test hook of debug builds only (Swift: `#if DEBUG`),
/// so nobody can switch the check off in a release build.
fn enabled() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("GIROCRAB_NO_BANKING_POLICY").is_some() {
        return false;
    }
    !keys().is_empty()
}

/// `AppUpdateChannel.inferred(fromBundleVersion:)`: a pre-release version
/// ("0.7.0-nightly.3") or one with four or more parts is a nightly build.
fn channel(version: &str) -> &'static str {
    if version.contains('-') || version.split('.').count() >= 4 {
        "nightly"
    } else {
        "stable"
    }
}

pub fn verify(json: &[u8], sig: &[u8]) -> bool {
    let Ok(sig) = <[u8; 64]>::try_from(sig) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig);
    keys().iter().any(|k| k.verify(json, &sig).is_ok())
}

fn parse(json: &[u8]) -> Result<Document, String> {
    let d: Document = serde_json::from_slice(json).map_err(|_| tr("Unbekanntes Format"))?;
    if d.schema != SUPPORTED_SCHEMA {
        return Err(tr("Unbekanntes Format"));
    }
    Ok(d)
}

fn verified(json: &[u8], sig: &[u8]) -> Option<Document> {
    if verify(json, sig) {
        parse(json).ok()
    } else {
        None
    }
}

/// BankingVersion.compare: numeric dotted parts (padded), letters sort before numbers' end;
/// anything after the first "-" is ignored.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let split = |s: &str| -> Vec<String> {
        let s = s.split('-').next().unwrap_or("");
        let mut parts: Vec<String> = Vec::new();
        for c in s.chars() {
            let num = c.is_ascii_digit();
            match parts.last_mut() {
                Some(p)
                    if c != '.'
                        && p.chars().all(|x| x.is_ascii_digit()) == num
                        && !p.is_empty()
                        && !p.starts_with('.') =>
                {
                    p.push(c)
                }
                _ => parts.push(c.to_string()),
            }
        }
        parts.into_iter().filter(|p| p != ".").collect()
    };
    let (mut pa, mut pb) = (split(a), split(b));
    let len = pa.len().max(pb.len());
    let pad = |v: &mut Vec<String>| {
        while v.len() < len && v.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
            v.push("0".into())
        }
    };
    pad(&mut pa);
    pad(&mut pb);
    for (x, y) in pa.iter().zip(pb.iter()) {
        let (nx, ny) = (x.parse::<u128>().ok(), y.parse::<u128>().ok());
        let o = match (nx, ny) {
            (Some(p), Some(q)) => p.cmp(&q),
            (None, Some(_)) => Less,
            (Some(_), None) => Greater,
            (None, None) => x.cmp(y),
        };
        if o != Equal {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

/// BankingPolicyEvaluation.gate for this build (`linux`, channel from the version).
fn evaluate(doc: &Document) -> Option<Gate> {
    evaluate_for(doc, env!("GIROCRAB_VERSION"))
}

fn evaluate_for(doc: &Document, version: &str) -> Option<Gate> {
    let channel = channel(version);
    let blocking: Vec<&RawRule> = doc
        .rules
        .iter()
        .filter(|r| {
            r.distribution.iter().any(|d| d == DISTRIBUTION)
                && r.channels
                    .as_ref()
                    .is_none_or(|c| c.iter().any(|c| c == channel))
                && !r.minimum_version.trim().is_empty()
                && compare_versions(version, r.minimum_version.trim()) == std::cmp::Ordering::Less
        })
        .collect();
    if blocking.is_empty() {
        return None;
    }
    let all = blocking.iter().any(|r| {
        r.connectors
            .as_ref()
            .is_none_or(|c| c.iter().all(|x| x.trim().is_empty()))
    });
    let mut ids: Vec<String> = Vec::new();
    if !all {
        for id in blocking
            .iter()
            .flat_map(|r| r.connectors.clone().unwrap_or_default())
        {
            let id = id.trim().to_string();
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id)
            }
        }
    }
    let strictest = blocking
        .iter()
        .map(|r| r.minimum_version.trim().to_string())
        .max_by(|a, b| compare_versions(a, b));
    Some(Gate {
        reason: "outdated".into(),
        all,
        connector_ids: ids,
        minimum_version: strictest,
        advisory: blocking
            .iter()
            .filter_map(|r| r.advisory.clone())
            .find(|a| a.starts_with("https://")),
        update_available: blocking.iter().all(|r| r.update_available.unwrap_or(true)),
    })
}

fn store_path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("GiroCrab/banking-policy.json")
}

fn load_stored() -> Option<Stored> {
    serde_json::from_slice(&std::fs::read(store_path()).ok()?).ok()
}

fn save_stored(json: &[u8], sig: &[u8], online: bool) {
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let s = Stored {
        json: b64(json),
        signature: b64(sig),
        verified_online: online,
    };
    let p = store_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, serde_json::to_vec(&s).unwrap_or_default());
}

fn build_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(env!("GIROCRAB_BUILD_UNIX").parse().unwrap_or(0))
}

/// Stored and bundled policy; the higher `sequence` wins.
fn load_local(st: &mut State) {
    st.loaded = true;
    let stored = load_stored();
    let decode = |s: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .unwrap_or_default()
    };
    let mut doc = stored
        .as_ref()
        .and_then(|s| verified(&decode(&s.json), &decode(&s.signature)));
    let was_online = stored.as_ref().is_some_and(|s| s.verified_online);
    if let Some(b) = verified(BUNDLED_JSON, BUNDLED_SIG)
        && doc.as_ref().is_none_or(|d| b.sequence > d.sequence)
    {
        save_stored(BUNDLED_JSON, BUNDLED_SIG, was_online);
        doc = Some(b);
    }
    st.document = doc;
    st.verified_online = was_online;
}

fn relies_on_bundled() -> bool {
    verified(BUNDLED_JSON, BUNDLED_SIG).is_some()
        && SystemTime::now() < build_time() + BUNDLED_GRACE
}

fn unchecked_title() -> String {
    tr("Sicherheitsprüfung nicht möglich")
}

fn unchecked_detail() -> String {
    tr(
        "GiroCrab konnte nicht prüfen, ob diese Version noch sicher mit Banken kommunizieren darf. Bitte später erneut versuchen oder in der Firewall Verbindungen zu raw.githubusercontent.com erlauben.",
    )
}

/// Title and detail as one message (the core's error while no check has succeeded).
fn unchecked() -> String {
    format!("{}. {}", unchecked_title(), unchecked_detail())
}

/// Recomputes the gate and hands it to the core.
fn apply(st: &mut State) {
    use girocrab_core::banking_gate::{clear_banking_block, set_banking_block};
    let new_gate = if !enabled() {
        None
    } else if !st.verified_online {
        let bundled_gate = st.document.as_ref().and_then(evaluate);
        if bundled_gate.is_some() {
            bundled_gate
        } else if relies_on_bundled() {
            None
        } else if st.last_failure.is_none() {
            // Not tried yet: no banner, but the core stays closed until the first check.
            st.gate = None;
            set_banking_block(true, vec![], unchecked());
            return;
        } else {
            Some(Gate {
                reason: "checkUnavailable".into(),
                all: true,
                connector_ids: vec![],
                minimum_version: None,
                advisory: None,
                update_available: true,
            })
        }
    } else {
        st.document.as_ref().and_then(evaluate)
    };
    match &new_gate {
        Some(g) => set_banking_block(g.all, g.connector_ids.clone(), message(g)),
        None => clear_banking_block(),
    }
    st.gate = new_gate;
}

/// `BankingGate.title`: banner headline and alert title.
pub fn title(g: &Gate) -> String {
    if g.reason == "checkUnavailable" {
        unchecked_title()
    } else {
        tr("Bankverbindungen gesperrt")
    }
}

/// `BankingGate.message`: full sentence(s) for the core error.
pub fn message(g: &Gate) -> String {
    if g.reason == "checkUnavailable" {
        return unchecked();
    }
    detail(g)
}

/// `BankingGate.displayName(forConnectorID:)`: "fints" → "FinTS",
/// "fints:50050201" → "FinTS (BLZ 50050201)", …
pub fn display_name(id: &str) -> String {
    let (head, tail) = match id.split_once(':') {
        Some((h, t)) => (h, Some(t)),
        None => (id, None),
    };
    let base = match head {
        "fints" => "FinTS",
        "comdirect" => "comdirect",
        "paypal" => "PayPal",
        "traderepublic" => "Trade Republic",
        "enablebanking" => "Enable Banking",
        "bunq" => "bunq",
        "scalable" => "Scalable Capital",
        "amazon-orders" => "Amazon",
        "fnz" => "FNZ Bank",
        "ibkr" => "Interactive Brokers",
        "equateplus" => "EquatePlus",
        "quirion" => "quirion",
        "uberspace" => "Uberspace",
        "schwaebischhall" => "Schwäbisch Hall",
        "fin4u" => "Alte Leipziger",
        "ffb" => "FFB",
        "fordmoney" => "Ford Money",
        "opeldirektbank" => "Opel Direktbank",
        "hanseatic" => "Hanseatic Bank",
        "bhw" => "BHW Bausparkasse",
        "kontist" => "Kontist",
        other => other,
    };
    match tail {
        None => base.to_string(),
        Some(t) if base == "FinTS" => format!("{base} (BLZ {t})"),
        Some(t) => format!("{base} ({t})"),
    }
}

/// ListFormatter: "A", "A und B", "A, B und C" (English "A, B, and C").
fn list(items: &[String]) -> String {
    let english = crate::i18n::running() == "en";
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] if english => format!("{a} and {b}"),
        [init @ .., last] if english => format!("{}, and {last}", init.join(", ")),
        [init @ .., last] => format!("{} und {last}", init.join(", ")),
    }
}

/// `BankingGate.detail`: explanation below the headline (alert text).
pub fn detail(g: &Gate) -> String {
    if g.reason == "checkUnavailable" {
        return unchecked_detail();
    }
    let mut t = if g.all {
        tr(
            "Bankverbindungen sind gesperrt, weil diese Version eine Sicherheitslücke oder einen schweren Fehler im Bankzugang enthält. Ihre Daten bleiben lesbar.",
        )
    } else {
        let names: Vec<String> = g.connector_ids.iter().map(|id| display_name(id)).collect();
        tr("Verbindungen über %1 sind gesperrt, weil diese Version eine Sicherheitslücke oder einen schweren Fehler im Bankzugang enthält. Ihre Daten bleiben lesbar.")
            .replace("%1", &list(&names))
    };
    t.push(' ');
    t.push_str(&if g.update_available {
        tr("Bitte aktualisieren Sie GiroCrab.")
    } else {
        tr("Ein Update ist in Vorbereitung.")
    });
    t
}

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
                    return Err(tr("Antwort zu groß"));
                }
                Ok(b.to_vec())
            }
        };
        // A local test server: debug builds only (Swift: `#if DEBUG`), so nobody can
        // redirect the check of a release build.
        #[cfg(debug_assertions)]
        let url = std::env::var("GIROCRAB_BANKING_POLICY_URL").unwrap_or_else(|_| URL.to_string());
        #[cfg(not(debug_assertions))]
        let url = URL.to_string();
        let (j, s) = tokio::join!(get(url.clone()), get(format!("{url}.sig")));
        Ok((j?, s?))
    })
}

/// `refresh(force:)`: downloads and verifies unless the last check is fresh.
/// Returns the state for QML (`{gate, verified_online, last_failure, enabled}`).
pub fn refresh(force: bool) -> Value {
    {
        let mut st = STATE.lock().unwrap();
        if !st.loaded {
            load_local(&mut st);
            apply(&mut st);
        }
        let fresh = st
            .last_check
            .is_some_and(|t| t.elapsed().unwrap_or(FRESHNESS) < FRESHNESS);
        if !enabled() || (!force && fresh) {
            return state_json(&st);
        }
    }
    let result = fetch();
    let mut st = STATE.lock().unwrap();
    match result {
        Err(e) => st.last_failure = Some(e),
        Ok((json, sig)) => {
            if !verify(&json, &sig) {
                st.last_failure = Some(tr("Ungültige Signatur"));
            } else {
                match parse(&json) {
                    Err(e) => st.last_failure = Some(e),
                    Ok(doc)
                        if st
                            .document
                            .as_ref()
                            .is_some_and(|c| doc.sequence < c.sequence) =>
                    {
                        st.last_failure = Some(
                            tr("Veraltete Richtlinie (sequence %1 < %2)")
                                .replace("%1", &doc.sequence.to_string())
                                .replace(
                                    "%2",
                                    &st.document
                                        .as_ref()
                                        .map(|c| c.sequence)
                                        .unwrap_or(0)
                                        .to_string(),
                                ),
                        )
                    }
                    Ok(doc) => {
                        save_stored(&json, &sig, true);
                        st.document = Some(doc);
                        st.verified_online = true;
                        st.last_check = Some(SystemTime::now());
                        st.last_failure = None;
                    }
                }
            }
        }
    }
    apply(&mut st);
    state_json(&st)
}

/// `BankingPolicy.usesDebugOverrides`: a test hook is set (debug builds only).
fn debug_overrides() -> bool {
    cfg!(debug_assertions)
        && (std::env::var_os("GIROCRAB_BANKING_POLICY_URL").is_some()
            || std::env::var_os("GIROCRAB_NO_BANKING_POLICY").is_some())
}

fn state_json(st: &State) -> Value {
    json!({
        "enabled": enabled(),
        "gate": st.gate,
        "message": st.gate.as_ref().map(message),
        "title": st.gate.as_ref().map(title),
        "detail": st.gate.as_ref().map(detail),
        "verified_online": st.verified_online,
        "last_check_ms": st.last_check
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as f64),
        "sequence": st.document.as_ref().map(|d| d.sequence),
        "relies_on_bundled": !st.verified_online && relies_on_bundled(),
        "bundled_valid_until_ms": (build_time() + BUNDLED_GRACE)
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0),
        "debug_overrides": debug_overrides(),
        "last_failure": st.last_failure,
        "distribution": DISTRIBUTION,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_policy_verifies() {
        assert!(
            verify(BUNDLED_JSON, BUNDLED_SIG),
            "repo policy must carry a valid signature"
        );
        let mut tampered = BUNDLED_JSON.to_vec();
        tampered[0] ^= 1;
        assert!(!verify(&tampered, BUNDLED_SIG));
    }

    #[test]
    fn version_order() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("0.6.3", "0.6.10"), Less);
        assert_eq!(compare_versions("0.7", "0.6.9"), Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Equal);
        assert_eq!(compare_versions("0.6.3-nightly.5", "0.6.3"), Equal);
    }

    #[test]
    fn channel_from_version() {
        assert_eq!(channel("0.6.3"), "stable");
        assert_eq!(channel("0.7.0-nightly.3"), "nightly");
        assert_eq!(channel("0.7.0.12"), "nightly");
        let doc: Document = serde_json::from_value(json!({
            "schema": 1, "sequence": 3,
            "rules": [{ "distribution": ["linux"], "channels": ["nightly"], "minimumVersion": "99.0" }]
        }))
        .unwrap();
        assert!(evaluate_for(&doc, "0.6.3").is_none());
        assert!(evaluate_for(&doc, "0.6.3-nightly.1").is_some());
    }

    #[test]
    fn detail_names_connectors() {
        let g = Gate {
            reason: "outdated".into(),
            all: false,
            connector_ids: vec![
                "fints:52090000".into(),
                "paypal".into(),
                "traderepublic".into(),
            ],
            minimum_version: None,
            advisory: None,
            update_available: true,
        };
        assert!(detail(&g).starts_with(
            "Verbindungen über FinTS (BLZ 52090000), PayPal und Trade Republic sind gesperrt"
        ));
        assert_eq!(title(&g), "Bankverbindungen gesperrt");
        assert_eq!(message(&g), detail(&g));
    }

    #[test]
    fn gate_applies_to_linux_rules_only() {
        let doc: Document = serde_json::from_value(json!({
            "schema": 1, "sequence": 2,
            "rules": [
                { "distribution": ["developer-id"], "minimumVersion": "99.0" },
                { "distribution": ["linux"], "minimumVersion": "99.0", "connectors": ["fints"] }
            ]
        }))
        .unwrap();
        let g = evaluate(&doc).unwrap();
        assert!(!g.all);
        assert_eq!(g.connector_ids, vec!["fints".to_string()]);
    }
}
