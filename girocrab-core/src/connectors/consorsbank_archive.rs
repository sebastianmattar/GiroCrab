//! Consorsbank Online-Archiv next to a FinTS login (GitHub #1024).
//!
//! Consorsbank offers neither HKEKA nor HKEKP over FinTS; its statements,
//! trade confirmations and tax documents are only in the Online-Archiv of the
//! web portal. [`ConsorsbankArchive`] is a [`PostboxSource`] for
//! [`FintsWithPostbox`](super::comdirect_postbox::FintsWithPostbox): it logs
//! in to the portal's JSON API with the login's account number and PIN, asks
//! for the approval in the Consorsbank app, lists the archive, downloads the
//! documents not stored yet and logs out. It never creates accounts or
//! bookings. The interface is unofficial (see `docs/consorsbank-connector.md`).
//!
//! The archive's `docId` is an encrypted token that changes with every listing
//! and every download of the same document has different bytes (a new PDF
//! `/ID`). The stable key is the number in the file name (`…_id<n>.pdf`); the
//! file name is the bank's own, so documents imported with that name before
//! are recognised too. Every download marks the document read at the bank,
//! which is why known documents are skipped before downloading.
//!
//! Only the clearing account (`KTO`) has a document list. Depot documents are
//! in it with the clearing account's number; [`assign`] sends them to the
//! owner's depot.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use super::comdirect_postbox::PostboxSource;
use super::{
    Challenge, ChallengeKind, ChallengeResponse, ConnectContext, Phase, RemoteAccount,
    http_client_same_host, read_body_limited, read_json_or_null, read_text_limited,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::AccountKind;

const BASE: &str = "https://www.consorsbank.de";
const LOGIN_PAGE: &str = "/web/System/Sicherheit/Login?showLogin=true";
const ARCHIVE_PAGE: &str = "/web/Mein-Konto-und-Depot/Online-Archiv";
/// `source_url` prefix of archive documents.
pub(crate) const SOURCE_PREFIX: &str = "consorsbank:archive:";
/// The approval in the app expires after about five minutes.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Words the approval dialog's links send instead of a TAN (like Trade
/// Republic's "SMS"): switch to the typed TAN or back to the app approval.
pub(crate) const SWITCH_TO_TAN: &str = "TAN";
pub(crate) const SWITCH_TO_APP: &str = "APP";
/// `Challenge::method_name` of the steps. The app shows the switch link for
/// [`METHOD_APP`] and [`METHOD_TAN_OR_APP`] (`TwoFactorSheet.swift`), so they
/// must differ from the FinTS method names of the same bank.
const METHOD_APP: &str = "Online-Archiv · Consorsbank App";
const METHOD_TAN_OR_APP: &str = "Online-Archiv · TAN";
const METHOD_TAN: &str = "Online-Archiv · SecurePlus TAN";
/// Page size of the document list (the bank accepts 10 to 50).
const PAGE_SIZE: usize = 50;
/// Upper bound of list pages per account (10,000 documents), in case the
/// bank sends full pages without a `totalSize`.
const MAX_PAGES: usize = 200;

/// Is there another list page after `page` (1-based) with `n` documents?
/// A short page ends the list; without `totalSize` full pages go on (up to
/// [`MAX_PAGES`]) instead of silently stopping after the first one.
fn more_pages(page: usize, n: usize, total: Option<usize>) -> bool {
    n >= PAGE_SIZE && page < MAX_PAGES && total.is_none_or(|t| page * PAGE_SIZE < t)
}

/// The stable document number in the bank's file name (`…_id1734868089.pdf`).
pub(crate) fn stable_id(file_name: &str) -> Option<&str> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)_id(\d{4,})\.pdf$").expect("valid regex"));
    RE.captures(file_name.trim())
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

/// The portal's user name for a FinTS login: the account number without the
/// `001` the FinTS user ID appends and without leading zeros.
pub(crate) fn web_user(fints_login: &str) -> String {
    let login = fints_login.trim();
    let number = match login.strip_suffix("001") {
        Some(n) if login.len() == 13 && login.bytes().all(|b| b.is_ascii_digit()) => n,
        _ => login,
    };
    let trimmed = number.trim_start_matches('0');
    if trimmed.is_empty() {
        number.to_string()
    } else {
        trimmed.to_string()
    }
}

/// The CSRF token the portal embeds in its pages (`#modernWebXsrfToken`).
fn xsrf_token(html: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)id="modernWebXsrfToken"[^>]*>\s*(\{.*?\})\s*<"#).expect("valid regex")
    });
    let raw = RE.captures(html)?.get(1)?.as_str();
    let v: Value = serde_json::from_str(raw).ok()?;
    v["xsrfToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// An account number without leading zeros, for comparing the portal's
/// 9-digit numbers with FinTS's zero-padded ones.
fn norm(number: &str) -> &str {
    let n = number.trim();
    let t = n.trim_start_matches('0');
    if t.is_empty() { n } else { t }
}

/// Does `account` carry the account number `number`? Directly, or as the last
/// ten digits of a German IBAN.
fn has_number(account: &RemoteAccount, number: &str) -> bool {
    let n = norm(number);
    if n.is_empty() || n == "0" {
        return false;
    }
    if !account.account_number.is_empty() && norm(&account.account_number) == n {
        return true;
    }
    let iban: String = account
        .iban
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // `get`, not slicing: a non-ASCII byte in the bank's data must not panic
    iban.len() == 22 && iban.starts_with("DE") && iban.get(12..).is_some_and(|t| norm(t) == n)
}

/// Words of `docSubtypeDesc` (upper case) that make a document a securities
/// document: it goes to the owner's depot.
const SECURITIES_WORDS: &[&str] = &[
    "WERTPAPIER",
    "DEPOT",
    "KAUF",
    "DIVIDEND",
    "VORABPAUSCHALE",
    "ERTRAEGNIS",
    "ERTRAG",
    "HAUPTVERSAMMLUNG",
    "HV-",
    "UEBERTRAG",
    "SPARPLAN",
    "VERLUST",
    "ANSCHAFFUNG",
    "KAPITALMA",
    "FONDS",
];

fn is_securities(entry: &Value) -> bool {
    let has = |k: &str| entry[k].as_str().is_some_and(|s| !s.trim().is_empty());
    let subtype = entry["subtype"].as_str().unwrap_or_default().to_uppercase();
    has("isin") || has("wkn") || SECURITIES_WORDS.iter().any(|w| subtype.contains(w))
}

/// The account a listed archive document belongs to. Securities documents go
/// to a depot of the same owner (or the login's only depot); the others to the
/// account in `accountno`, owner-level documents (`accountno` 0: tax
/// certificates, terms) to the owner's clearing account. `None` = no match.
pub(crate) fn assign<'a>(
    entry: &Value,
    accounts: &[&'a RemoteAccount],
) -> Option<&'a RemoteAccount> {
    let find = |number: &str| accounts.iter().copied().find(|a| has_number(a, number));
    if is_securities(entry) {
        let own_depot = entry["depots"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(find);
        if own_depot.is_some() {
            return own_depot;
        }
        let depots: Vec<&RemoteAccount> = accounts
            .iter()
            .copied()
            .filter(|a| a.kind == AccountKind::Brokerage)
            .collect();
        if let [only] = depots.as_slice() {
            return Some(only);
        }
    }
    let accountno = entry["accountno"].as_str().unwrap_or_default();
    if norm(accountno) != "0"
        && let Some(a) = find(accountno)
    {
        return Some(a);
    }
    find(entry["kto"].as_str().unwrap_or_default())
}

/// GiroCrab's document type for an archive entry.
fn doc_type(subtype: &str) -> &'static str {
    let s = subtype.to_uppercase();
    if s.contains("STEUER") || s.contains("ERTRAEGNISAUFSTELLUNG") {
        "steuer"
    } else if s.contains("AUSZUG") || s.contains("ABSCHLUSS") {
        "kontoauszug"
    } else if s.contains("KAUF") || s.contains("DIVIDEND") || s.contains("ABRECHNUNG") {
        "abrechnung"
    } else {
        "postbox"
    }
}

/// "KONTOAUSZUG VERRECHNUNGSKONTO" → "Kontoauszug Verrechnungskonto".
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f
                    .to_uppercase()
                    .chain(c.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// A listed document without its bytes. Without bytes it is not stored, but
/// a stored copy still gets its account.
fn stub(entry: &Value) -> Option<RemoteDocument> {
    let id = entry["id"].as_str().filter(|s| !s.is_empty())?;
    let file = entry["fileName"].as_str().unwrap_or_default();
    let subtype = entry["subtype"].as_str().unwrap_or_default();
    let name = entry["name"].as_str().unwrap_or_default().trim();
    let title = title_case(if subtype.trim().is_empty() {
        name
    } else {
        subtype
    });
    Some(RemoteDocument {
        account_ref: String::new(),
        booking_external_id: None,
        title: if title.is_empty() {
            tr("Dokument aus dem Online-Archiv").into()
        } else {
            title
        },
        doc_type: doc_type(subtype).into(),
        mime_type: "application/pdf".into(),
        file_name: file.trim().replace(['/', '\\'], "-"),
        bytes: Vec::new(),
        source_url: format!("{SOURCE_PREFIX}{id}"),
        date: entry["date"].as_str().unwrap_or_default().to_string(),
    })
}

/// A QR code image (PNG/JPEG, base64) in the bank's approval-method answer,
/// with the key it came in. The field name is not documented, so any string
/// field that decodes to an image counts.
fn qr_image(v: &Value) -> Option<(String, Vec<u8>)> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    v.as_object()?.iter().find_map(|(k, x)| {
        let raw = x.as_str()?.trim();
        let raw = raw.split_once("base64,").map_or(raw, |(_, b)| b);
        if raw.len() < 64 {
            return None;
        }
        let bytes = STANDARD.decode(raw).ok()?;
        let image =
            bytes.starts_with(&[0x89, b'P', b'N', b'G']) || bytes.starts_with(&[0xFF, 0xD8]);
        image.then(|| (k.clone(), bytes))
    })
}

/// The bank's error text from a JSON error answer, shortened.
fn bank_message(v: &Value) -> String {
    v["defaultMessage"]
        .as_str()
        .or_else(|| v["errorMessages"][0]["message"].as_str())
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// The archive entries of one document list page, reduced to what the
/// download and the assignment need. `kto` = the clearing account the list
/// belongs to, `depots` = the owner's depot numbers.
fn entries(page: &Value, owner: &str, kto: &str, depots: &[String]) -> Vec<Value> {
    page["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let file = d["docFileName"].as_str()?;
            let id = stable_id(file)?;
            let doc_id = d["docId"].as_str().filter(|s| !s.is_empty())?;
            let text = |k: &str| d[k].as_str().unwrap_or_default().trim().to_string();
            Some(json!({
                "id": id,
                "docId": doc_id,
                "owner": owner,
                "kto": kto,
                "depots": depots,
                "accountno": text("accountno"),
                "subtype": text("docSubtypeDesc"),
                "name": text("docName"),
                "date": text("docDate"),
                "isin": text("securityIsin"),
                "wkn": text("securityWkn"),
                "fileName": file,
            }))
        })
        .collect()
}

pub(crate) struct ConsorsbankArchive {
    client: Option<reqwest::Client>,
    xsrf: String,
    tx_id: String,
    started: Option<Instant>,
    challenge: Option<Challenge>,
    /// The login's approval method types a TAN (TAN generator, SMS) instead
    /// of approving in the app.
    tan_mode: bool,
    /// The login can approve in the app too (method "sp"): the TAN step
    /// offers to switch back.
    push_available: bool,
    /// Stable ids of the documents already stored for this login.
    known: HashSet<String>,
    /// Listed entries by `source_url`, for [`assign`].
    listed: HashMap<String, Value>,
    logged_in: bool,
}

impl ConsorsbankArchive {
    pub(crate) fn new(known: HashSet<String>) -> Self {
        Self {
            client: None,
            xsrf: String::new(),
            tx_id: String::new(),
            started: None,
            challenge: None,
            tan_mode: false,
            push_available: false,
            known,
            listed: HashMap::new(),
            logged_in: false,
        }
    }

    fn client(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn log(msg: &str) {
        crate::depot_debug::log(&format!("consorsbank archive: {msg}"));
    }

    /// GET a portal page and keep its CSRF token.
    async fn refresh_token(&mut self, path: &str) -> CoreResult<()> {
        let resp = self.client()?.get(format!("{BASE}{path}")).send().await?;
        let status = resp.status();
        let html = read_text_limited(resp).await?;
        match xsrf_token(&html) {
            Some(t) => self.xsrf = t,
            None => Self::log(&format!("no CSRF token on {path} (HTTP {status})")),
        }
        Ok(())
    }

    /// POST JSON to the portal API: (status, JSON answer or null).
    async fn post(&self, path: &str, body: Option<Value>) -> CoreResult<(u16, Value)> {
        let mut req = self
            .client()?
            .post(format!("{BASE}{path}"))
            .header("Accept", "application/json")
            .header("wemocsrf", &self.xsrf);
        req = match body {
            Some(b) => req.json(&b),
            None => req.header("Content-Type", "application/json"),
        };
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let json = json_or_null(resp).await;
        Ok((status, json))
    }

    async fn get_json(&self, path: &str, query: &[(&str, String)]) -> CoreResult<(u16, Value)> {
        let resp = self
            .client()?
            .get(format!("{BASE}{path}"))
            .query(query)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status().as_u16();
        Ok((status, json_or_null(resp).await))
    }

    fn approval(&self) -> Challenge {
        Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: tr("In der Consorsbank App freigeben").into(),
            message: tr(
                "Öffnen Sie die Consorsbank App und geben Sie die Anmeldung frei. GiroCrab lädt danach die Dokumente aus dem Online-Archiv.",
            )
            .into(),
            method_name: METHOD_APP.into(),
            redirect_url: None,
            image: None,
            expires_in_seconds: Some(APPROVAL_TIMEOUT.as_secs() as u32),
            poll_interval_seconds: 2,
        }
    }

    /// Starts the approval in the Consorsbank app.
    async fn start_push(&mut self) -> CoreResult<Phase> {
        let (status, answer) = self
            .post("/web-sec-service/api/auth/2ndphase/createasync", None)
            .await?;
        if let Some(id) = answer["txId"].as_str().filter(|s| !s.is_empty()) {
            self.tx_id = id.to_string();
        }
        Self::log(&format!(
            "createasync HTTP {status}, txId {}",
            if self.tx_id.is_empty() {
                "missing"
            } else {
                "present"
            }
        ));
        if status != 200 || self.tx_id.is_empty() {
            if self.session_strong().await {
                return self.approved().await;
            }
            return Err(CoreError::Protocol {
                message: format!(
                    "consorsbank archive: approval not started (HTTP {status}) {}",
                    bank_message(&answer)
                ),
            });
        }
        self.tan_mode = false;
        self.started = Some(Instant::now());
        let c = self.approval();
        self.challenge = Some(c.clone());
        Ok(Phase::Challenge(c))
    }

    /// Asks for a typed TAN (SecurePlus generator or app TAN), with the QR
    /// code to scan when the bank sends one (the page's "other method" asks
    /// with `qrValidation`).
    async fn start_tan(&mut self, qr: Option<Vec<u8>>) -> CoreResult<Phase> {
        let qr = match qr {
            Some(q) => Some(q),
            None => {
                let (status, alt) = self
                    .get_json(
                        "/web-sec-service/api/authorizationmethod/interim",
                        &[("qrValidation", "true".into())],
                    )
                    .await?;
                let q = qr_image(&alt);
                Self::log(&format!(
                    "authorizationmethod qrValidation HTTP {status}, keys {:?}, qr image {}",
                    alt.as_object()
                        .map(|o| o.keys().cloned().collect::<Vec<_>>()),
                    q.as_ref()
                        .map_or("none".to_string(), |(k, _)| format!("in '{k}'"))
                ));
                q.map(|(_, image)| image)
            }
        };
        self.tan_mode = true;
        self.started = Some(Instant::now());
        let c = self.tan_challenge(qr);
        self.challenge = Some(c.clone());
        Ok(Phase::Challenge(c))
    }

    /// The step for a typed TAN: with the QR code to scan when the bank sends
    /// one, else just the input field.
    fn tan_challenge(&self, image: Option<Vec<u8>>) -> Challenge {
        let (kind, title, message) = if image.is_some() {
            (
                ChallengeKind::ImageCode,
                tr("QR-Code mit dem TAN-Generator scannen"),
                tr(
                    "Scannen Sie den QR-Code mit dem SecurePlus Generator und geben Sie die angezeigte TAN ein. GiroCrab lädt danach die Dokumente aus dem Online-Archiv.",
                ),
            )
        } else {
            (
                ChallengeKind::Code,
                tr("Anmeldung mit TAN bestätigen"),
                tr(
                    "Erzeugen Sie eine TAN mit der Consorsbank App oder dem SecurePlus Generator und geben Sie sie hier ein. GiroCrab lädt danach die Dokumente aus dem Online-Archiv.",
                ),
            )
        };
        Challenge {
            kind,
            title: title.into(),
            message: message.into(),
            method_name: if self.push_available {
                METHOD_TAN_OR_APP.into()
            } else {
                METHOD_TAN.into()
            },
            redirect_url: None,
            image,
            expires_in_seconds: Some(APPROVAL_TIMEOUT.as_secs() as u32),
            poll_interval_seconds: 2,
        }
    }

    /// Is the portal session approved (strong)?
    async fn session_strong(&self) -> bool {
        match self
            .get_json("/web-sec-service/api/user/sessiontime", &[])
            .await
        {
            Ok((200, v)) => {
                Self::log(&format!(
                    "session trust={} expired={}",
                    v["sessionTrustLevel"].as_str().unwrap_or("?"),
                    v["expired"]
                ));
                v["sessionTrustLevel"].as_str() == Some("STRONG") && v["expired"] != json!(true)
            }
            Ok((status, _)) => {
                Self::log(&format!("sessiontime HTTP {status}"));
                false
            }
            Err(_) => false,
        }
    }

    async fn approved(&mut self) -> CoreResult<Phase> {
        // the page's own flow ends with this call after the approval; harmless if not needed
        if !self.session_strong().await && !self.tan_mode {
            let (status, _) = self
                .post(
                    "/web-sec-service/api/auth/2ndphase",
                    Some(json!({ "authenticationToken": self.tx_id })),
                )
                .await?;
            Self::log(&format!("2ndphase HTTP {status}"));
            if !self.session_strong().await {
                return Err(CoreError::Authentication {
                    message: tr(
                        "Die Consorsbank hat die Anmeldung zum Online-Archiv nicht bestätigt.",
                    )
                    .into(),
                });
            }
        }
        self.logged_in = true;
        // a new session may come with a new CSRF token (needed for the logout)
        let _ = self.refresh_token(ARCHIVE_PAGE).await;
        Ok(Phase::Ready)
    }

    /// All archive entries since `since` (`YYYY-MM-DD`, empty = all) not stored yet.
    async fn entries_since(&self, since: &str) -> CoreResult<Vec<Value>> {
        let (status, info) = self.get_json("/web-sec-service/api/user/info", &[]).await?;
        let crm = info["crmCustomerNo"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status != 200 || info["isLoggedOut"] == json!(true) || crm.is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Die Sitzung beim Consorsbank-Online-Archiv ist abgelaufen.").into(),
            });
        }
        let (status, accounts) = self
            .get_json(
                "/web-sec-service/api/user/accounts",
                &[
                    ("includeExtraTypes", "LOA".into()),
                    ("includeDeletedOnlineArchiveAccount", "true".into()),
                ],
            )
            .await?;
        if status != 200 {
            return Err(CoreError::Protocol {
                message: format!("consorsbank archive: accounts HTTP {status}"),
            });
        }
        let from = if since.is_empty() {
            "2000-01-01"
        } else {
            since
        };
        let to = crate::dates::today_iso();
        let path = format!("/web-document-service/api/users/{crm}/documents/account-documents/v1");
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let (mut listed, mut known) = (0usize, 0usize);
        for owner in accounts["ownerAccounts"].as_array().into_iter().flatten() {
            let Some(owner_no) = owner["backendOwnerNumber"].as_str() else {
                continue;
            };
            let of_type = |t: &str| -> Vec<String> {
                owner["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|a| a["type"].as_str() == Some(t))
                    .filter_map(|a| a["number"].as_str().map(str::to_string))
                    .collect()
            };
            let depots = of_type("DEP");
            for kto in of_type("KTO") {
                for page in 1.. {
                    let (status, list) = self
                        .get_json(
                            &path,
                            &[
                                ("pageSize", PAGE_SIZE.to_string()),
                                ("pageNo", page.to_string()),
                                ("backendOwnerNumber", owner_no.to_string()),
                                ("accounts", kto.clone()),
                                ("sortCriteria", "D".into()),
                                ("sortDirection", "D".into()),
                                ("dateFrom", from.to_string()),
                                ("dateTo", to.clone()),
                                ("docTypeId", "0".into()),
                            ],
                        )
                        .await?;
                    if status != 200 {
                        return Err(CoreError::Protocol {
                            message: format!(
                                "consorsbank archive: list HTTP {status} {}",
                                bank_message(&list)
                            ),
                        });
                    }
                    let n = list["documents"].as_array().map_or(0, Vec::len);
                    for mut e in entries(&list, owner_no, &kto, &depots) {
                        listed += 1;
                        // the download needs the customer number validated above
                        e["crm"] = json!(crm);
                        let id = e["id"].as_str().unwrap_or_default().to_string();
                        if !seen.insert(id.clone()) {
                            continue; // owner-level documents are listed for every owner
                        }
                        if self.known.contains(&id) {
                            known += 1;
                        } else {
                            out.push(e);
                        }
                    }
                    let total = list["totalSize"].as_u64().map(|t| t as usize);
                    if !more_pages(page, n, total) {
                        break;
                    }
                }
            }
        }
        Self::log(&format!(
            "{listed} listed since '{from}', {} distinct, {known} already stored, {} to download",
            seen.len(),
            out.len()
        ));
        Ok(out)
    }

    async fn download_once(&self, entry: &Value) -> Result<Vec<u8>, String> {
        let crm_path =
            |crm: &str| format!("/web-document-service/api/users/{crm}/documents/downloads/v1");
        let crm = entry["crm"].as_str().unwrap_or_default();
        let resp = self
            .client()
            .map_err(|_| "no session".to_string())?
            .get(format!("{BASE}{}", crm_path(crm)))
            .query(&[
                (
                    "backendOwnerNumber",
                    entry["owner"].as_str().unwrap_or_default(),
                ),
                ("documents", entry["docId"].as_str().unwrap_or_default()),
            ])
            .header("Accept", "application/pdf")
            .send()
            .await
            .map_err(|_| "network".to_string())?;
        let status = resp.status().as_u16();
        let pdf = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|t| t.contains("pdf"));
        let bytes = read_body_limited(resp, super::MAX_DOC_BYTES)
            .await
            .map_err(|_| "network".to_string())?;
        if status == 200 && pdf && bytes.starts_with(b"%PDF-") {
            Ok(bytes)
        } else {
            Err(format!("HTTP {status}"))
        }
    }
}

async fn json_or_null(resp: reqwest::Response) -> Value {
    read_json_or_null(resp).await
}

#[async_trait]
impl PostboxSource for ConsorsbankArchive {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        *self = Self::new(std::mem::take(&mut self.known));
        self.client = Some(http_client_same_host()?);
        self.refresh_token(LOGIN_PAGE).await?;
        let user = web_user(&ctx.login_name);
        let (status, answer) = self
            .post(
                "/web-sec-service/api/auth",
                Some(json!({ "username": user, "nip": ctx.secret.expose() })),
            )
            .await?;
        Self::log(&format!(
            "auth HTTP {status}, answer keys {:?}",
            answer
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        ));
        if status != 200 {
            return Err(CoreError::Authentication {
                message: match bank_message(&answer) {
                    m if m.is_empty() => format!("Consorsbank: HTTP {status}"),
                    m => format!("Consorsbank: {m}"),
                },
            });
        }
        // which approval the login uses: "sp" = Consorsbank app (push), else a typed TAN
        let (status, method) = self
            .get_json("/web-sec-service/api/authorizationmethod/interim", &[])
            .await?;
        let kind = method["authorizationMethod"].as_str().unwrap_or_default();
        Self::log(&format!(
            "authorizationmethod HTTP {status}, method '{kind}', flow '{}', keys {:?}",
            method["authorizationFlow"].as_str().unwrap_or_default(),
            method
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>()),
        ));
        if let Some(id) = method["txId"].as_str() {
            self.tx_id = id.to_string();
        }
        self.push_available = kind.is_empty() || kind == "sp";
        if self.push_available {
            self.start_push().await
        } else {
            self.start_tan(qr_image(&method).map(|(_, image)| image))
                .await
        }
    }

    async fn respond(&mut self, _ctx: &ConnectContext, r: ChallengeResponse) -> CoreResult<Phase> {
        // the dialog's links, as on the bank's page: "Manuell mit TAN bestätigen" /
        // "Mit Consorsbank App bestätigen"
        if let ChallengeResponse::Code(word) = &r {
            match word.expose().trim() {
                SWITCH_TO_TAN if !self.tan_mode => return self.start_tan(None).await,
                SWITCH_TO_APP if self.tan_mode && self.push_available => {
                    return self.start_push().await;
                }
                _ => {}
            }
        }
        if self.tan_mode {
            let ChallengeResponse::Code(tan) = r else {
                return Ok(Phase::Challenge(
                    self.challenge
                        .clone()
                        .unwrap_or_else(|| self.tan_challenge(None)),
                ));
            };
            let (status, answer) = self
                .post(
                    "/web-sec-service/api/auth/2ndphase",
                    Some(json!({ "authenticationToken": tan.expose().trim() })),
                )
                .await?;
            Self::log(&format!("2ndphase with TAN HTTP {status}"));
            if status != 200 {
                return Err(CoreError::Authentication {
                    message: match bank_message(&answer) {
                        m if m.is_empty() => {
                            tr("Die Consorsbank hat die TAN für das Online-Archiv abgelehnt.")
                                .into()
                        }
                        m => format!("Consorsbank: {m}"),
                    },
                });
            }
            return self.approved().await;
        }
        if !matches!(r, ChallengeResponse::Poll) {
            return Err(CoreError::Protocol {
                message: "consorsbank archive: unexpected code during the app approval".into(),
            });
        }
        if self.started.is_none_or(|s| s.elapsed() > APPROVAL_TIMEOUT) {
            return Err(CoreError::Authentication {
                message: tr("Die Freigabe in der Consorsbank App ist abgelaufen.").into(),
            });
        }
        let (status, answer) = self
            .post(
                "/web-sec-service/api/auth/2ndphase/asyncstatus",
                Some(json!({ "txId": self.tx_id })),
            )
            .await?;
        let tx = answer["txStatus"]
            .as_str()
            .or(answer.as_str())
            .unwrap_or_default()
            .to_string();
        match tx.as_str() {
            "TX_EXEC_SUCCESS" => {
                Self::log("approval granted");
                self.approved().await
            }
            "TX_AUTH_REJECTED"
            | "TX_AUTH_USER_TIMEOUT"
            | "TX_AUTH_USER_OFFLINE"
            | "TX_AUTH_ERROR"
            | "TX_EXEC_FAILED"
            | "TX_EXEC_TIMEOUT" => {
                Self::log(&format!("approval ended with {tx}"));
                Err(CoreError::Authentication {
                    message: tr("Die Anmeldung zum Online-Archiv wurde in der Consorsbank App nicht freigegeben.")
                        .into(),
                })
            }
            _ => {
                if status != 200 {
                    Self::log(&format!(
                        "asyncstatus HTTP {status} {}",
                        bank_message(&answer)
                    ));
                }
                Ok(Phase::Pending(
                    self.challenge.clone().unwrap_or_else(|| self.approval()),
                ))
            }
        }
    }

    async fn list(&mut self, since: &str) -> Option<Vec<Value>> {
        if !self.logged_in {
            return None;
        }
        match self.entries_since(since).await {
            Ok(list) => {
                for e in &list {
                    if let Some(d) = stub(e) {
                        self.listed.insert(d.source_url, e.clone());
                    }
                }
                Some(list)
            }
            Err(e) => {
                Self::log(&format!(
                    "listing failed ({})",
                    e.to_string().chars().take(80).collect::<String>()
                ));
                None
            }
        }
    }

    async fn download(&mut self, entry: &Value) -> Option<RemoteDocument> {
        for attempt in 0..3u64 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(attempt)).await;
            }
            match self.download_once(entry).await {
                Ok(bytes) => {
                    let mut doc = stub(entry)?;
                    doc.bytes = bytes;
                    return Some(doc);
                }
                Err(why) => {
                    Self::log(&format!("download failed ({why}), attempt {}", attempt + 1));
                    if why.starts_with("HTTP 4") && why != "HTTP 429" {
                        break;
                    }
                }
            }
        }
        None
    }

    fn stub(&self, entry: &Value) -> Option<RemoteDocument> {
        stub(entry)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if self.logged_in
            && let Ok((status, _)) = self.post("/web-sec-service/api/logout", None).await
        {
            Self::log(&format!("logout HTTP {status}"));
        }
        self.logged_in = false;
        self.client = None;
    }

    fn label(&self) -> String {
        tr("Online-Archiv").into()
    }

    fn assign<'a>(
        &self,
        doc: &RemoteDocument,
        accounts: &[&'a RemoteAccount],
    ) -> Option<&'a RemoteAccount> {
        self.listed
            .get(&doc.source_url)
            .and_then(|e| assign(e, accounts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(kind: AccountKind, number: &str, iban: &str) -> RemoteAccount {
        RemoteAccount {
            external_ref: format!("ref-{number}"),
            name: String::new(),
            owner: String::new(),
            iban: iban.into(),
            account_number: number.into(),
            currency: "EUR".into(),
            kind,
            balance_cents: 0,
            balance_date: String::new(),
        }
    }

    fn entry(subtype: &str, accountno: &str, kto: &str, depots: &[&str]) -> Value {
        json!({"id": "1234567890", "subtype": subtype, "accountno": accountno,
               "kto": kto, "depots": depots, "isin": "", "wkn": ""})
    }

    #[test]
    fn stable_id_comes_from_the_file_name() {
        assert_eq!(
            stable_id("KONTOAUSZUG_VERRECHNUNGSKONTO_123456789_dat20261030_id1734868089.pdf"),
            Some("1734868089")
        );
        assert_eq!(
            stable_id("KAUF_123_ord456_001_wknA0B1C2_dat20261102_id1736554284.PDF"),
            Some("1736554284")
        );
        assert_eq!(stable_id("Kontoauszug.pdf"), None);
        assert_eq!(stable_id("x_id12.pdf"), None);
    }

    #[test]
    fn web_user_drops_suffix_and_zeros() {
        assert_eq!(web_user("0123456789001"), "123456789");
        assert_eq!(web_user(" 0123456789001 "), "123456789");
        assert_eq!(web_user("123456789"), "123456789");
        assert_eq!(web_user("musteruser"), "musteruser");
    }

    #[test]
    fn xsrf_token_is_read_from_the_page() {
        let html = r#"<script type="application/json" id="modernWebXsrfToken">
            {"xsrfToken":"abc-123"}
        </script>"#;
        assert_eq!(xsrf_token(html).as_deref(), Some("abc-123"));
        assert_eq!(xsrf_token("<html></html>"), None);
    }

    #[test]
    fn documents_go_to_the_matching_accounts() {
        // FinTS numbers are zero-padded, the portal's are not
        let kto = account(AccountKind::Checking, "0111111111", "");
        let dep = account(AccountKind::Brokerage, "0222222222", "");
        let kto2 = account(AccountKind::Checking, "", "DE00760300800333333333");
        let dep2 = account(AccountKind::Brokerage, "444444444", "");
        let all = [&kto, &dep, &kto2, &dep2];
        let ref_of = |e: &Value| assign(e, &all).map(|a| a.external_ref.clone());

        let statement = entry(
            "KONTOAUSZUG VERRECHNUNGSKONTO",
            "111111111",
            "111111111",
            &["222222222"],
        );
        assert_eq!(ref_of(&statement), Some(kto.external_ref.clone()));
        let trade = entry("KAUF", "111111111", "111111111", &["222222222"]);
        assert_eq!(ref_of(&trade), Some(dep.external_ref.clone()));
        let report = entry(
            "JAHRESKOSTENREPORT WERTPAPIERE",
            "0",
            "333333333",
            &["444444444"],
        );
        assert_eq!(ref_of(&report), Some(dep2.external_ref.clone()));
        // owner-level document: the owner's clearing account, found by its IBAN
        let tax = entry(
            "JAHRESSTEUERBESCHEINIGUNG",
            "0",
            "333333333",
            &["444444444"],
        );
        assert_eq!(ref_of(&tax), Some(kto2.external_ref.clone()));
        // ISIN alone makes it a securities document
        let mut info = entry(
            "WICHTIGE MITTEILUNG",
            "111111111",
            "111111111",
            &["222222222"],
        );
        info["isin"] = json!("DE0001234567");
        assert_eq!(ref_of(&info), Some(dep.external_ref.clone()));
        // nothing known: no account
        let foreign = entry("WICHTIGE MITTEILUNG", "999999999", "999999999", &[]);
        assert_eq!(ref_of(&foreign), None);
    }

    #[test]
    fn securities_document_without_own_depot_uses_the_only_depot() {
        let kto = account(AccountKind::Checking, "111111111", "");
        let dep = account(AccountKind::Brokerage, "222222222", "");
        let all = [&kto, &dep];
        let trade = entry(
            "DIVIDENDENGUTSCHRIFT",
            "111111111",
            "111111111",
            &["555555555"],
        );
        assert_eq!(
            assign(&trade, &all).map(|a| a.kind),
            Some(AccountKind::Brokerage)
        );
    }

    #[test]
    fn listing_keeps_only_what_the_download_needs() {
        let page = json!({"size": 2, "totalSize": 2, "documents": [
            {"docId": "token-1", "docFileName": "KAUF_1_dat20261102_id1736554284.pdf",
             "docSubtypeDesc": "KAUF", "docName": "KAUF ", "docDate": "2026-11-02",
             "accountno": "111111111", "securityIsin": "DE0001234567", "securityWkn": "",
             "docReadTimestamp": "2026-11-03 08:00:00", "docStatus": "R"},
            {"docId": "token-2", "docFileName": "ohne-id.pdf", "docSubtypeDesc": "X"}
        ]});
        let list = entries(&page, "9999999999", "111111111", &["222222222".into()]);
        assert_eq!(list.len(), 1);
        let e = &list[0];
        assert_eq!(e["id"], "1736554284");
        assert_eq!(e["docId"], "token-1");
        assert_eq!(e["owner"], "9999999999");
        assert_eq!(e["depots"], json!(["222222222"]));
        assert!(e.get("docReadTimestamp").is_none());
        let d = stub(e).unwrap();
        assert_eq!(d.source_url, "consorsbank:archive:1736554284");
        assert_eq!(d.title, "Kauf");
        assert_eq!(d.doc_type, "abrechnung");
        assert_eq!(d.file_name, "KAUF_1_dat20261102_id1736554284.pdf");
        assert_eq!(d.date, "2026-11-02");
        assert!(d.bytes.is_empty());
    }

    #[test]
    fn qr_image_is_found_in_any_field() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let mut png = vec![0x89, b'P', b'N', b'G'];
        png.extend([0u8; 80]);
        let answer = json!({"authorizationMethod": "tg", "qrCode": STANDARD.encode(&png)});
        assert_eq!(qr_image(&answer), Some(("qrCode".to_string(), png.clone())));
        let prefixed =
            json!({"challenge": format!("data:image/png;base64,{}", STANDARD.encode(&png))});
        assert_eq!(
            qr_image(&prefixed).map(|(k, _)| k),
            Some("challenge".to_string())
        );
        assert_eq!(
            qr_image(&json!({"authorizationMethod": "sp", "txId": "abc"})),
            None
        );
    }

    #[test]
    fn iban_with_unexpected_bytes_does_not_panic() {
        // 22 bytes, but a two-byte character across the slice boundary at 12
        let odd = account(AccountKind::Checking, "", "DE007603008ä111111111");
        assert_eq!(odd.iban.len(), 22);
        assert!(!has_number(&odd, "111111111"));
        let ok = account(AccountKind::Checking, "", "DE00 7603 0080 0111 1111 11");
        assert!(has_number(&ok, "111111111"));
    }

    #[test]
    fn list_pages_go_on_while_full() {
        assert!(more_pages(1, 50, Some(106)));
        assert!(more_pages(2, 50, Some(106)));
        assert!(!more_pages(3, 6, Some(106)));
        assert!(!more_pages(2, 50, Some(100)), "exactly two full pages");
        // no totalSize: full pages go on, a short one ends the list
        assert!(more_pages(1, 50, None));
        assert!(!more_pages(4, 12, None));
        assert!(!more_pages(MAX_PAGES, 50, None), "bounded");
    }

    #[test]
    fn document_types() {
        assert_eq!(doc_type("KONTOAUSZUG VERRECHNUNGSKONTO"), "kontoauszug");
        assert_eq!(doc_type("QUARTALSDEPOTAUSZUG WERTPAPIERE"), "kontoauszug");
        assert_eq!(doc_type("JAHRESSTEUERBESCHEINIGUNG"), "steuer");
        assert_eq!(doc_type("VERKAUF"), "abrechnung");
        assert_eq!(doc_type("WICHTIGE MITTEILUNG"), "postbox");
    }
}
