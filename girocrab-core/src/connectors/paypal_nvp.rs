//! PayPal – classic NVP API with an API signature (prototype, #156).
//!
//! For accounts without access to the REST "Transaction Search" permission:
//! the user creates API credentials (API username, API password, signature)
//! under PayPal › Kontoeinstellungen › API-Zugriff.
//!
//!  POST https://api-3t.paypal.com/nvp   (form-encoded, answer form-encoded)
//!    METHOD=GetBalance&RETURNALLCURRENCIES=1        -> L_AMTn / L_CURRENCYCODEn
//!    METHOD=TransactionSearch&STARTDATE&ENDDATE     -> L_TIMESTAMPn, L_NAMEn, L_NETAMTn …
//!
//! `TransactionSearch` returns at most 100 rows and cannot page; a full answer
//! carries warning 11002. The window is then halved until it fits.

use std::collections::HashMap;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use super::{RemoteAccount, RemoteTransaction, http_client_same_host};
use crate::error::{CoreError, CoreResult};
use crate::models::AccountKind;
use crate::money::parse_cents;
use crate::secret::SecretString;

const VERSION: &str = "204.0";
/// Warning code of a TransactionSearch answer cut at 100 rows.
const TRUNCATED: &str = "11002";
/// Most rows one `TransactionSearch` returns (PayPal's documented limit).
const MAX_ROWS: usize = 100;
/// External id prefix of temporary holds: stored as provisional bookings
/// (`FetchResult::hold_prefix`), dropped once settled or no longer reported.
pub const HOLD_PREFIX: &str = "pending-hold-paypal-";
/// Window of the first request per step; halved on truncation.
const WINDOW_DAYS: i64 = 31;

pub struct NvpCredentials {
    pub user: String,
    pub password: SecretString,
    pub signature: SecretString,
}

pub struct NvpClient {
    client: reqwest::Client,
    endpoint: String,
    creds: NvpCredentials,
}

/// Decoded NVP answer (`KEY=value&…`).
pub type Nvp = HashMap<String, String>;

pub(crate) fn parse_nvp(body: &str) -> Nvp {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn ack_ok(r: &Nvp) -> bool {
    matches!(
        r.get("ACK").map(String::as_str),
        Some("Success") | Some("SuccessWithWarning")
    )
}

/// Whether more rows may exist than this answer holds: warning 11002, or the
/// documented limit reached without it (the warning is not documented for
/// `TransactionSearch`, so the row count decides too).
/// Smallest search window: below it a truncated answer is an error.
const MIN_WINDOW_SECS: i64 = 60;

/// End of the halved window `start..end`, or `None` when it would be shorter
/// than [`MIN_WINDOW_SECS`].
fn smaller_window(start: Timestamp, end: Timestamp) -> Option<Timestamp> {
    let half = end.duration_since(start) / 2;
    (half >= SignedDuration::from_secs(MIN_WINDOW_SECS)).then(|| start + half)
}

/// Start of `day` in UTC.
fn utc_midnight(day: Date) -> Result<Timestamp, jiff::Error> {
    day.to_zoned_datetime(TimeZone::UTC).map(|z| z.timestamp())
}

fn is_truncated(r: &Nvp) -> bool {
    error_codes(r).contains(&TRUNCATED)
        || (0..MAX_ROWS).all(|i| r.contains_key(&format!("L_TRANSACTIONID{i}")))
}

fn error_codes(r: &Nvp) -> Vec<&str> {
    (0..)
        .map_while(|i| r.get(&format!("L_ERRORCODE{i}")))
        .map(String::as_str)
        .collect()
}

fn error_of(r: &Nvp) -> CoreError {
    let code = r.get("L_ERRORCODE0").cloned().unwrap_or_default();
    let message = r
        .get("L_LONGMESSAGE0")
        .or(r.get("L_SHORTMESSAGE0"))
        .cloned()
        .unwrap_or_else(|| "PayPal NVP error".into());
    // 10002: "Security header is not valid" / wrong API credentials.
    if code == "10002" || code == "10008" {
        CoreError::Authentication { message }
    } else {
        CoreError::Bank { code, message }
    }
}

impl NvpClient {
    pub fn new(sandbox: bool, creds: NvpCredentials) -> CoreResult<Self> {
        Ok(NvpClient {
            client: http_client_same_host()?,
            endpoint: if sandbox {
                "https://api-3t.sandbox.paypal.com/nvp".into()
            } else {
                "https://api-3t.paypal.com/nvp".into()
            },
            creds,
        })
    }

    async fn call(&self, method: &str, params: &[(&str, String)]) -> CoreResult<Nvp> {
        let mut form: Vec<(&str, &str)> = vec![
            ("METHOD", method),
            ("VERSION", VERSION),
            ("USER", &self.creds.user),
            ("PWD", self.creds.password.expose()),
            ("SIGNATURE", self.creds.signature.expose()),
        ];
        form.extend(params.iter().map(|(k, v)| (*k, v.as_str())));
        let resp = self.client.post(&self.endpoint).form(&form).send().await?;
        let status = resp.status();
        let body = super::read_text_limited(resp).await.unwrap_or_default();
        let r = parse_nvp(&body);
        if !status.is_success() && r.is_empty() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: "PayPal NVP error".into(),
            });
        }
        if !ack_ok(&r) {
            return Err(error_of(&r));
        }
        Ok(r)
    }

    /// Validates the credentials (cheap call, also used by `begin`).
    pub async fn balances(&self, owner: &str) -> CoreResult<Vec<RemoteAccount>> {
        let r = self
            .call("GetBalance", &[("RETURNALLCURRENCIES", "1".into())])
            .await?;
        Ok(accounts_from_balance(&r, owner))
    }

    /// Bookings since `since`, and the `(account ref, id)` of internal entries left out.
    pub async fn transactions(
        &self,
        since: Date,
    ) -> CoreResult<(Vec<RemoteTransaction>, Vec<(String, String)>)> {
        let now = Timestamp::now();
        let mut out = Vec::new();
        let mut withdrawn = Vec::new();
        let mut start = utc_midnight(since).unwrap_or(now - SignedDuration::from_hours(90 * 24));
        while start < now {
            let mut end = (start + SignedDuration::from_hours(WINDOW_DAYS * 24)).min(now);
            loop {
                let r = self.search(start, end).await?;
                // Halve until the window fits. Never keep a truncated answer: the
                // missing rows would be lost silently, and the open holds of the
                // period (`hold_prefix`) would count as complete.
                if is_truncated(&r) {
                    end = smaller_window(start, end).ok_or_else(|| CoreError::Bank {
                        code: TRUNCATED.into(),
                        message: format!(
                            "PayPal meldet ab {start} mehr als {MAX_ROWS} Umsätze in einer Minute; \
                             der Abruf wurde abgebrochen, damit keine Umsätze fehlen."
                        ),
                    })?;
                    continue;
                }
                let (kept, gone) = transactions_from_search(&r);
                out.extend(kept);
                withdrawn.extend(gone);
                break;
            }
            start = end + SignedDuration::from_secs(1);
        }
        // Windows touch at the boundary second: drop duplicates.
        let mut seen = std::collections::HashSet::new();
        out.retain(|t| seen.insert((t.account_ref.clone(), t.external_id.clone())));
        Ok((out, withdrawn))
    }

    /// Fills the purpose of payments from `GetTransactionDetails` (transaction id, subject, note,
    /// items, invoice number: `TransactionSearch` has none of them).
    /// One request per booking, so only for ids not in `done` (ids already looked
    /// up in an earlier sync); those looked up now are appended to it. A failed
    /// lookup keeps the purpose from the search and is retried next time.
    pub async fn fill_details(&self, txs: &mut [RemoteTransaction], done: &mut Vec<String>) {
        for t in txs.iter_mut() {
            if !wants_details(t) || done.contains(&t.external_id) {
                continue;
            }
            let Ok(d) = self
                .call(
                    "GetTransactionDetails",
                    &[("TRANSACTIONID", t.external_id.clone())],
                )
                .await
            else {
                continue;
            };
            t.purpose = with_search_purpose(&purpose_from_details(&t.external_id, &d), &t.purpose);
            done.push(t.external_id.clone());
        }
    }

    /// Diagnostics (`examples/paypal_nvp.rs`, `PAYPAL_NVP_RAW=1`): the rows of one
    /// `TransactionSearch` from `start` until now, each as its fields without the
    /// index suffix (`L_TYPE`, `L_STATUS` …), unfiltered.
    /// `class`: optional `TRANSACTIONCLASS` filter (e.g. `BalanceAffecting`).
    pub async fn raw_rows(&self, start: Date, class: Option<&str>) -> CoreResult<Vec<Nvp>> {
        let from = utc_midnight(start).unwrap_or_else(|_| Timestamp::now());
        let r = self.search_with(from, Timestamp::now(), class).await?;
        let mut rows = Vec::new();
        for i in 0.. {
            let suffix = i.to_string();
            let row: Nvp = r
                .iter()
                .filter(|(k, _)| k.starts_with("L_") && k.ends_with(&suffix))
                .filter_map(|(k, v)| {
                    let base = &k[..k.len() - suffix.len()];
                    // L_ERRORCODE0 vs. L_TYPE10: only letters and '_' before the index
                    base.chars()
                        .all(|c| c.is_ascii_uppercase() || c == '_')
                        .then(|| (base.to_string(), v.clone()))
                })
                .collect();
            if !row.contains_key("L_TRANSACTIONID") {
                break;
            }
            rows.push(row);
        }
        Ok(rows)
    }

    async fn search(&self, start: Timestamp, end: Timestamp) -> CoreResult<Nvp> {
        self.search_with(start, end, None).await
    }

    async fn search_with(
        &self,
        start: Timestamp,
        end: Timestamp,
        class: Option<&str>,
    ) -> CoreResult<Nvp> {
        let fmt = |d: Timestamp| d.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut params = vec![("STARTDATE", fmt(start)), ("ENDDATE", fmt(end))];
        if let Some(c) = class.filter(|c| !c.is_empty()) {
            params.push(("TRANSACTIONCLASS", c.to_string()));
        }
        self.call("TransactionSearch", &params).await
    }
}

pub(crate) fn accounts_from_balance(r: &Nvp, owner: &str) -> Vec<RemoteAccount> {
    let mut out = Vec::new();
    for i in 0.. {
        let Some(cur) = r.get(&format!("L_CURRENCYCODE{i}")) else {
            break;
        };
        let amount = r
            .get(&format!("L_AMT{i}"))
            .and_then(|a| parse_cents(a).ok())
            .unwrap_or(0);
        out.push(RemoteAccount {
            external_ref: format!("paypal:{cur}"),
            // The first entry is the primary currency.
            name: if i == 0 {
                "PayPal".into()
            } else {
                format!("PayPal {cur}")
            },
            owner: owner.to_string(),
            iban: String::new(),
            account_number: owner.to_string(),
            currency: cur.clone(),
            kind: AccountKind::PaymentService,
            balance_cents: amount,
            balance_date: super::today(),
        });
    }
    out
}

/// Most ids remembered as "details looked up" in the connector state.
pub const MAX_DONE_IDS: usize = 2000;

/// Payments get their purpose from `GetTransactionDetails`; funding transfers,
/// currency conversions, unnamed internal entries and holds have none worth a request.
fn wants_details(t: &RemoteTransaction) -> bool {
    let k = t.booking_text.as_str();
    !(k.starts_with("Transfer")
        || k.starts_with("Currency Conversion")
        || k == UNNAMED_TYPE
        || t.external_id.starts_with(HOLD_PREFIX))
}

/// Purpose from a `GetTransactionDetails` answer: the PayPal transaction id (what
/// PayPal's mails and support quote), subject, note, item names and invoice
/// number, each once, joined by " / ". `CUSTOM` is left out: merchants and payment
/// providers put internal references there (e.g. `acct_…:pi_…`).
pub(crate) fn purpose_from_details(id: &str, d: &Nvp) -> String {
    let field = |k: &str| d.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    let items: Vec<String> = (0..)
        .map_while(|i| d.get(&format!("L_NAME{i}")))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let mut parts: Vec<String> = Vec::new();
    for p in [
        id.trim().to_string(),
        field("SUBJECT"),
        field("NOTE"),
        items.join(", "),
        field("INVNUM"),
    ] {
        if !p.is_empty() && !parts.contains(&p) {
            parts.push(p);
        }
    }
    parts.join(" / ")
}

/// `details` as the purpose, followed by what the search purpose had and
/// `details` lacks (its " / " parts: the e-mail address) and the fee note
/// (" · Gebühr …").
fn with_search_purpose(details: &str, search_purpose: &str) -> String {
    let mut out = details.to_string();
    let mut fee = None;
    for part in search_purpose.split(" · ").map(str::trim) {
        if part.starts_with("Gebühr ") {
            fee = Some(part);
            continue;
        }
        for p in part.split(" / ").map(str::trim) {
            if p.is_empty() || out.split(" / ").any(|d| d == p) {
                continue;
            }
            if !out.is_empty() {
                out.push_str(" / ");
            }
            out.push_str(p);
        }
    }
    if let Some(fee) = fee {
        out = format!("{out} · {fee}");
    }
    out
}

/// Booking text for NVP's "..." type (a transaction type the classic API cannot name).
const UNNAMED_TYPE: &str = "PayPal-Vorgang";

/// Bookings of one `TransactionSearch` answer, and the `(account ref, id)` of the
/// internal offsetting pairs left out (see [`internal_pairs`]).
pub(crate) fn transactions_from_search(r: &Nvp) -> (Vec<RemoteTransaction>, Vec<(String, String)>) {
    let mut out = Vec::new();
    // per booking: (timestamp, anonymous entry of an unnamed type)
    let mut meta: Vec<(String, bool)> = Vec::new();
    for i in 0.. {
        let Some(id) = r.get(&format!("L_TRANSACTIONID{i}")) else {
            break;
        };
        let get = |k: &str| r.get(&format!("{k}{i}")).cloned().unwrap_or_default();
        let status = get("L_STATUS");
        let kind = get("L_TYPE");
        // Failed or undone entries and mere authorisations move no money.
        if matches!(
            status.as_str(),
            "Denied" | "Reversed" | "Voided" | "Canceled" | "Failed"
        ) || matches!(kind.as_str(), "Authorization" | "Order")
        {
            continue;
        }
        // Net = gross + fee: what actually moves the balance.
        let net = parse_cents(&get("L_NETAMT")).or_else(|_| parse_cents(&get("L_AMT")));
        let Ok(amount_cents) = net else { continue };
        let fee = parse_cents(&get("L_FEEAMT")).unwrap_or(0);
        let date: String = get("L_TIMESTAMP").chars().take(10).collect();
        let currency = {
            let c = get("L_CURRENCYCODE");
            if c.is_empty() { "EUR".into() } else { c }
        };
        let name = get("L_NAME");
        let email = get("L_EMAIL");
        let unnamed = kind.trim().is_empty() || kind.trim() == "...";
        meta.push((
            get("L_TIMESTAMP"),
            unnamed && name.is_empty() && email.is_empty(),
        ));
        let kind = if unnamed {
            UNNAMED_TYPE.to_string()
        } else {
            kind
        };
        // Transaction id first (what PayPal's mails and support quote), then the
        // e-mail address when the name already is the counterparty.
        let mut purpose = id.trim().to_string();
        if !name.is_empty() && !email.is_empty() {
            purpose = format!("{purpose} / {email}");
        }
        if fee != 0 {
            let f = crate::money::format_cents(fee, currency.clone());
            purpose = format!("{purpose} · Gebühr {f}");
        }
        out.push(RemoteTransaction {
            account_ref: format!("paypal:{currency}"),
            // a temporary hold reserves money but books nothing (yet)
            external_id: if kind == "Temporary Hold" {
                format!("{HOLD_PREFIX}{id}")
            } else {
                id.clone()
            },
            booking_date: date.clone(),
            value_date: date,
            amount_cents,
            currency,
            counterparty: if name.is_empty() { email } else { name },
            counterparty_iban: String::new(),
            purpose,
            booking_text: kind,
            ..Default::default()
        });
    }
    let drop = internal_pairs(&out, &meta);
    let mut withdrawn = Vec::new();
    let mut kept = Vec::new();
    for (i, t) in out.into_iter().enumerate() {
        if drop.contains(&i) {
            withdrawn.push((t.account_ref, t.external_id));
        } else {
            kept.push(t);
        }
    }
    (kept, withdrawn)
}

/// Indexes of internal offsetting pairs: anonymous entries of an unnamed type
/// ("...", e.g. moves between a pool and the balance) with the same timestamp
/// and currency whose amounts cancel out. They move no money; the balance
/// comes from `GetBalance`. A single unnamed entry without counterpart stays.
fn internal_pairs(
    out: &[RemoteTransaction],
    meta: &[(String, bool)],
) -> std::collections::HashSet<usize> {
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, (ts, anonymous)) in meta.iter().enumerate() {
        if *anonymous && !ts.is_empty() {
            groups
                .entry((ts.as_str(), out[i].currency.as_str()))
                .or_default()
                .push(i);
        }
    }
    groups
        .into_values()
        .filter(|g| g.len() >= 2 && g.iter().map(|&i| out[i].amount_cents).sum::<i64>() == 0)
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_lists_every_currency() {
        let r = parse_nvp(
            "L_AMT0=123%2e45&L_AMT1=-5%2e00&L_CURRENCYCODE0=EUR&L_CURRENCYCODE1=USD\
             &TIMESTAMP=2026%2d11%2d16T10%3a00%3a00Z&ACK=Success&VERSION=204%2e0",
        );
        assert!(ack_ok(&r));
        let a = accounts_from_balance(&r, "x");
        assert_eq!(a.len(), 2);
        assert_eq!(
            (a[0].external_ref.as_str(), a[0].name.as_str()),
            ("paypal:EUR", "PayPal")
        );
        assert_eq!(a[0].balance_cents, 12345);
        assert_eq!(
            (a[1].name.as_str(), a[1].balance_cents),
            ("PayPal USD", -500)
        );
    }

    #[test]
    fn search_rows_book_net_and_skip_denied() {
        let r = parse_nvp(
            "L_TIMESTAMP0=2026%2d11%2d10T08%3a15%3a00Z&L_TYPE0=Payment&L_EMAIL0=shop%40example%2ecom\
             &L_NAME0=Beispiel%20Shop&L_TRANSACTIONID0=AAA&L_STATUS0=Completed&L_AMT0=%2d20%2e00\
             &L_FEEAMT0=0%2e00&L_NETAMT0=%2d20%2e00&L_CURRENCYCODE0=EUR\
             &L_TIMESTAMP1=2026%2d11%2d09T08%3a00%3a00Z&L_TYPE1=Payment&L_NAME1=Erika\
             &L_TRANSACTIONID1=BBB&L_STATUS1=Completed&L_AMT1=100%2e00&L_FEEAMT1=%2d2%2e85\
             &L_NETAMT1=97%2e15&L_CURRENCYCODE1=EUR\
             &L_TIMESTAMP2=2026%2d11%2d08T08%3a00%3a00Z&L_TYPE2=Payment&L_NAME2=Weg\
             &L_TRANSACTIONID2=CCC&L_STATUS2=Denied&L_AMT2=%2d5%2e00&L_NETAMT2=%2d5%2e00\
             &L_CURRENCYCODE2=EUR&ACK=Success",
        );
        let (t, gone) = transactions_from_search(&r);
        assert!(gone.is_empty());
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].external_id, "AAA");
        assert_eq!(t[0].booking_date, "2026-11-10");
        assert_eq!(t[0].amount_cents, -2000);
        assert_eq!(t[0].counterparty, "Beispiel Shop");
        // transaction id first, then the e-mail address (the name is the counterparty)
        assert_eq!(t[0].purpose, "AAA / shop@example.com");
        assert_eq!(t[0].account_ref, "paypal:EUR");
        assert_eq!(t[1].amount_cents, 9715);
        assert!(t[1].purpose.starts_with("BBB · Gebühr"), "{}", t[1].purpose);
    }

    #[test]
    fn offsetting_unnamed_pairs_are_left_out() {
        // pool <-> balance: same second, type "...", no name/e-mail, net +10 / -10
        let r = parse_nvp(
            "L_TIMESTAMP0=2026%2d11%2d17T05%3a59%3a44Z&L_TYPE0=%2e%2e%2e&L_TRANSACTIONID0=P1&L_STATUS0=Completed&L_AMT0=10%2e00&L_FEEAMT0=0%2e00&L_NETAMT0=10%2e00&L_CURRENCYCODE0=EUR&L_TIMESTAMP1=2026%2d11%2d17T05%3a59%3a44Z&L_TYPE1=%2e%2e%2e&L_TRANSACTIONID1=P2&L_STATUS1=Completed&L_AMT1=10%2e00&L_FEEAMT1=0%2e00&L_NETAMT1=%2d10%2e00&L_CURRENCYCODE1=EUR&L_TIMESTAMP2=2026%2d11%2d16T08%3a00%3a00Z&L_TYPE2=%2e%2e%2e&L_TRANSACTIONID2=P3&L_STATUS2=Completed&L_AMT2=5%2e00&L_NETAMT2=5%2e00&L_CURRENCYCODE2=EUR&L_TIMESTAMP3=2026%2d11%2d16T08%3a00%3a00Z&L_TYPE3=Payment&L_NAME3=Shop&L_TRANSACTIONID3=P4&L_STATUS3=Completed&L_AMT3=%2d5%2e00&L_NETAMT3=%2d5%2e00&L_CURRENCYCODE3=EUR&ACK=Success",
        );
        let (t, gone) = transactions_from_search(&r);
        let ids: Vec<&str> = t.iter().map(|t| t.external_id.as_str()).collect();
        // P3 has a counterpart only of a named type with a payee: it stays
        assert_eq!(ids, vec!["P3", "P4"]);
        assert_eq!(t[0].booking_text, UNNAMED_TYPE);
        assert_eq!(
            gone,
            vec![
                ("paypal:EUR".to_string(), "P1".to_string()),
                ("paypal:EUR".to_string(), "P2".to_string())
            ]
        );
    }

    #[test]
    fn details_make_the_purpose() {
        let d = parse_nvp(
            "SUBJECT=&NOTE=&INVNUM=1234567890123&CUSTOM=ab12cd-34ef-56gh\
             &L_NAME0=Beispielartikel&L_NAME1=Versand&ACK=Success",
        );
        // transaction id first, no CUSTOM
        assert_eq!(
            purpose_from_details("9AB12345CD678901E", &d),
            "9AB12345CD678901E / Beispielartikel, Versand / 1234567890123"
        );
        // repeated values once, an empty answer leaves the id
        let d = parse_nvp("SUBJECT=Abo&NOTE=Abo&ACK=Success");
        assert_eq!(purpose_from_details("T1", &d), "T1 / Abo");
        assert_eq!(purpose_from_details("T1", &parse_nvp("ACK=Success")), "T1");
        // the e-mail address and fee note of the search stay
        assert_eq!(
            with_search_purpose("T1 / Rechnung 1", "shop@example.com · Gebühr -0,35 €"),
            "T1 / Rechnung 1 / shop@example.com · Gebühr -0,35 €"
        );
        assert_eq!(
            with_search_purpose("T1 / Rechnung 1", "shop@example.com"),
            "T1 / Rechnung 1 / shop@example.com"
        );
        assert_eq!(
            with_search_purpose("T1", "Gebühr -0,35 €"),
            "T1 · Gebühr -0,35 €"
        );
        // the search purpose starts with the id too: not twice
        assert_eq!(
            with_search_purpose("T1 / Rechnung 1", "T1 / shop@example.com · Gebühr -0,35 €"),
            "T1 / Rechnung 1 / shop@example.com · Gebühr -0,35 €"
        );
        assert_eq!(with_search_purpose("T1", ""), "T1");
    }

    #[test]
    fn only_payments_ask_for_details() {
        let t = |text: &str, id: &str| RemoteTransaction {
            booking_text: text.into(),
            external_id: id.into(),
            ..Default::default()
        };
        assert!(wants_details(&t("Purchase", "X1")));
        assert!(wants_details(&t("Payment", "X2")));
        assert!(!wants_details(&t("Transfer", "X3")));
        assert!(!wants_details(&t("Transfer (charge)", "X4")));
        assert!(!wants_details(&t("Currency Conversion (debit)", "X5")));
        assert!(!wants_details(&t(UNNAMED_TYPE, "X6")));
        assert!(!wants_details(&t(
            "Temporary Hold",
            &format!("{HOLD_PREFIX}X7")
        )));
    }

    #[test]
    fn a_temporary_hold_is_a_provisional_booking() {
        let r = parse_nvp(
            "L_TIMESTAMP0=2026%2d11%2d10T08%3a15%3a00Z&L_TYPE0=Temporary%20Hold&L_NAME0=Beispiel%20Shop&L_TRANSACTIONID0=H1&L_STATUS0=Completed&L_AMT0=%2d30%2e00&L_NETAMT0=%2d30%2e00&L_CURRENCYCODE0=EUR&L_TIMESTAMP1=2026%2d11%2d10T08%3a15%3a01Z&L_TYPE1=Authorization&L_NAME1=Beispiel%20Shop&L_TRANSACTIONID1=A1&L_STATUS1=Completed&L_AMT1=%2d30%2e00&L_NETAMT1=%2d30%2e00&L_CURRENCYCODE1=EUR&ACK=Success",
        );
        let (t, gone) = transactions_from_search(&r);
        assert!(gone.is_empty());
        // the authorisation is left out, the hold becomes a provisional booking
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].external_id, format!("{HOLD_PREFIX}H1"));
        assert!(t[0].external_id.starts_with("pending-"));
        assert_eq!(t[0].amount_cents, -3000);
    }

    #[test]
    fn a_truncated_window_shrinks_down_to_a_minute_then_fails() {
        let start: Timestamp = "2026-11-01T00:00:00Z".parse().unwrap();
        let mut end = start + SignedDuration::from_hours(31 * 24);
        let mut steps = 0;
        while let Some(e) = smaller_window(start, end) {
            assert!(e > start && e < end);
            end = e;
            steps += 1;
        }
        // 31 days halve to just over a minute; one more halving would be below it
        assert!(steps > 10, "{steps}");
        assert!(end.duration_since(start) >= SignedDuration::from_secs(MIN_WINDOW_SECS));
        assert!(end.duration_since(start) < SignedDuration::from_secs(2 * MIN_WINDOW_SECS));
        assert_eq!(
            smaller_window(start, start + SignedDuration::from_secs(119)),
            None
        );
    }

    #[test]
    fn a_full_page_counts_as_truncated_without_the_warning() {
        let page: String = (0..MAX_ROWS)
            .map(|i| format!("L_TRANSACTIONID{i}=T{i}&"))
            .collect();
        assert!(is_truncated(&parse_nvp(&format!("{page}ACK=Success"))));
        let short: String = (0..MAX_ROWS - 1)
            .map(|i| format!("L_TRANSACTIONID{i}=T{i}&"))
            .collect();
        assert!(!is_truncated(&parse_nvp(&format!("{short}ACK=Success"))));
        assert!(is_truncated(&parse_nvp(
            "ACK=SuccessWithWarning&L_ERRORCODE0=11002"
        )));
    }

    #[test]
    fn truncation_warning_and_auth_error_are_recognised() {
        let r =
            parse_nvp("ACK=SuccessWithWarning&L_ERRORCODE0=11002&L_SHORTMESSAGE0=Search%20warning");
        assert!(ack_ok(&r));
        assert_eq!(error_codes(&r), vec![TRUNCATED]);
        let r = parse_nvp(
            "ACK=Failure&L_ERRORCODE0=10002&L_LONGMESSAGE0=Security%20header%20is%20not%20valid",
        );
        assert!(!ack_ok(&r));
        assert!(matches!(error_of(&r), CoreError::Authentication { .. }));
    }
}
