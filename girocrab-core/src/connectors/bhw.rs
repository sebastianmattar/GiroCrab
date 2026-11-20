//! BHW Bausparkasse (myBHW, `opra.bhw.de`): balances and bookings of the
//! building-savings contracts (Bausparverträge) and loans (Darlehen) and the
//! PDF documents of the myBHW mailbox. The portal is a single-page app on a
//! JSON API (`https://opra.bhw.de/api/bhw/`); the connector calls the same
//! endpoints (unofficial, they may change at any time). Shapes derived from
//! the portal's public JavaScript (2026-11).
//!
//!  POST transactionId {product, flavor, application, channel, type, country, promotionId}
//!                                    -> `transactionId`, sent as `x-request-id` from then on
//!  POST login {idType, id, firstName, dateOfBirth, zip}
//!                                    -> `data`, `email` / `phone` (where the one-time code
//!                                       can go), `permits` (terms accepted) or `error`
//!  PATCH login {method: email|phone, data}
//!                                    -> `hashedTan`, `data`; the BHW sends the code
//!  PUT  login {data, method, hashedTan, tan}
//!                                    -> `verified`
//!  GET  contracts                    -> `contracts[]`: id, productLongName, accountBalance, currency
//!  GET  contracts/<id>/transactions  -> `transactions[]`: postingDate, valueDate (DD.MM.YYYY),
//!                                       text, amount, currency
//!  GET  messages                     -> `messages[]`: id, name, contractId, creationDate
//!  GET  messages/<id>/document       -> the PDF
//!  PUT  logout
//!
//! There is no password: the login is the contract number (10 digits) or the
//! BHW ID (11 digits) plus first name, date of birth and postcode, which go
//! into the secret field as `Vorname;TT.MM.JJJJ;PLZ`. Every login is
//! confirmed with a one-time code the BHW sends by e-mail (or SMS).

use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{Value, json};
use zeroize::Zeroize;

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, Documents,
    FetchResult, KnownDocuments, Phase, RemoteAccount, RemoteTransaction,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::AccountKind;
use crate::vault::Vault;

const HOST: &str = "opra.bhw.de";
const ORIGIN: &str = "https://opra.bhw.de";
const PORTAL: &str = "https://opra.bhw.de/csp/";
const API: &str = "https://opra.bhw.de/api/bhw/";
/// Prefix of the account external refs (`bhw-<contract id>`).
pub(crate) const REF_PREFIX: &str = "bhw-";

// ------------------------------------------------------------------ errors

fn page_changed() -> CoreError {
    CoreError::protocol(tr("BHW: Antwort nicht erkannt (myBHW geändert?)."))
}

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "BHW: Unvollständige oder ungültige Antwort. Es wurden keine Daten übernommen.",
    ))
}

fn missing_id() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Bitte die 10-stellige Vertragsnummer oder die 11-stellige BHW-ID als Anmeldenamen eintragen.").into(),
    }
}

fn bad_identity() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Bitte Vorname, Geburtsdatum und Postleitzahl getrennt durch Semikolon eintragen, z. B. „Erika;31.12.1970;12345“.").into(),
    }
}

fn wrong_identity(code: &str) -> CoreError {
    let message = tr(
        "BHW: Anmeldung fehlgeschlagen. Bitte Vertragsnummer oder BHW-ID, Vorname, Geburtsdatum und Postleitzahl prüfen.",
    );
    CoreError::Authentication {
        message: if code.is_empty() {
            message.into()
        } else {
            format!("{message} ({code})")
        },
    }
}

fn no_contact() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Für den Einmal-Code ist weder eine E-Mail-Adresse noch eine Mobilnummer hinterlegt.").into(),
    }
}

fn terms_missing() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Bitte einmal im Browser bei myBHW anmelden und den Nutzungsbedingungen zustimmen.").into(),
    }
}

fn code_not_sent() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Der Einmal-Code konnte nicht angefordert werden.").into(),
    }
}

fn wrong_code() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Der Einmal-Code wurde nicht akzeptiert. Bitte den Abruf erneut starten.")
            .into(),
    }
}

fn session_ended() -> CoreError {
    CoreError::Authentication {
        message: tr("BHW: Die Sitzung wurde beendet. Bitte den Abruf erneut starten.").into(),
    }
}

/// The BHW's error code of a refused request (`error`, `errorCode`, `code`):
/// its first word, reduced to the characters such codes use, so it names the
/// cause without carrying personal data into the message.
fn bank_error_code(body: &Value) -> String {
    ["error", "errorCode", "error_code", "code"]
        .iter()
        .filter_map(|k| body[*k].as_str())
        .map(|c| {
            c.split_whitespace()
                .next()
                .unwrap_or("")
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
                .take(40)
                .collect::<String>()
        })
        .find(|c| !c.is_empty())
        .unwrap_or_default()
}

/// A refused request; `step` (a fixed technical name) and the BHW's error
/// code tell where it failed.
fn rejected(status: reqwest::StatusCode, step: &str, body: &Value) -> CoreError {
    let code = bank_error_code(body);
    let detail = if code.is_empty() {
        step.to_string()
    } else {
        format!("{step}, {code}")
    };
    CoreError::Bank {
        code: status.as_u16().to_string(),
        message: format!("{} ({detail})", tr("BHW: Die Anfrage wurde abgelehnt.")),
    }
}

/// Only `https://opra.bhw.de` is ever talked to.
fn ensure_host(url: &url::Url) -> CoreResult<()> {
    if url.scheme() == "https" && url.host_str() == Some(HOST) {
        return Ok(());
    }
    Err(CoreError::protocol(tr(
        "BHW: Unerwartete Weiterleitung zu einer fremden Adresse – abgebrochen.",
    )))
}

// ------------------------------------------------------------------ login data

/// Where the BHW sends the one-time code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeMethod {
    Email,
    Phone,
}

impl CodeMethod {
    fn as_str(self) -> &'static str {
        match self {
            CodeMethod::Email => "email",
            CodeMethod::Phone => "phone",
        }
    }
}

fn code_challenge(method: CodeMethod) -> Challenge {
    let (message, method_name) = match method {
        CodeMethod::Email => (
            tr(
                "Die BHW hat einen Einmal-Code an die hinterlegte E-Mail-Adresse geschickt. Bitte hier eingeben.",
            ),
            tr("Einmal-Code per E-Mail"),
        ),
        CodeMethod::Phone => (
            tr(
                "Die BHW hat einen Einmal-Code per SMS an die hinterlegte Mobilnummer geschickt. Bitte hier eingeben.",
            ),
            tr("Einmal-Code per SMS"),
        ),
    };
    Challenge {
        kind: ChallengeKind::Code,
        title: "myBHW".into(),
        message: message.into(),
        method_name: method_name.into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 0,
    }
}

/// The login data: contract number or BHW ID (login name) plus first name,
/// date of birth and postcode (secret field, `Vorname;TT.MM.JJJJ;PLZ`).
/// Wiped when dropped.
#[derive(Debug, PartialEq)]
struct Identity {
    id_type: &'static str,
    id: String,
    first_name: String,
    /// `YYYY-MM-DD`
    birth_date: String,
    zip: String,
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.id.zeroize();
        self.first_name.zeroize();
        self.birth_date.zeroize();
        self.zip.zeroize();
    }
}

fn identity(login: &str, secret: &str) -> CoreResult<Identity> {
    let id: String = login.chars().filter(|c| !c.is_whitespace()).collect();
    // Checked before any personal data is sent.
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(missing_id());
    }
    let id_type = match id.chars().count() {
        10 => "ContractId",
        11 => "BhwId",
        _ => return Err(missing_id()),
    };
    let parts: Vec<&str> = secret.split(';').map(str::trim).collect();
    let [first_name, birth, zip] = parts.as_slice() else {
        return Err(bad_identity());
    };
    if first_name.is_empty() || zip.is_empty() {
        return Err(bad_identity());
    }
    let birth_date = civil_date(birth)
        .map(crate::dates::iso)
        .ok_or_else(bad_identity)?;
    Ok(Identity {
        id_type,
        id,
        first_name: first_name.to_string(),
        birth_date,
        zip: zip.to_string(),
    })
}

/// A contact the code can go to: present and not blocked. The BHW answers
/// with a (masked) address or an object that may carry `isBlocked`.
fn usable_contact(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(o) => o.get("isBlocked").and_then(Value::as_bool) != Some(true),
        Value::Bool(b) => *b,
        _ => false,
    }
}

/// The answer to `POST login`: where to send the code, or why not.
fn code_method(login: &Value) -> CoreResult<CodeMethod> {
    let permits = &login["permits"];
    let accepted = ["permitV2", "permitV1"]
        .iter()
        .any(|k| !permits[*k].is_null() && permits[*k] != Value::Bool(false));
    if !accepted {
        return Err(terms_missing());
    }
    if usable_contact(&login["email"]) {
        Ok(CodeMethod::Email)
    } else if usable_contact(&login["phone"]) {
        Ok(CodeMethod::Phone)
    } else {
        Err(no_contact())
    }
}

// ------------------------------------------------------------------ contracts and bookings

/// `DD.MM.YYYY` (also `D.M.YYYY`, with a time after it) or `YYYY-MM-DD[T…]`.
fn civil_date(s: &str) -> Option<jiff::civil::Date> {
    let s = s.trim();
    if let Some(iso) = s.get(..10).filter(|p| p.as_bytes().get(4) == Some(&b'-')) {
        return jiff::civil::Date::strptime("%Y-%m-%d", iso).ok();
    }
    let s = s.split_whitespace().next()?;
    let mut it = s.split('.');
    let (d, m, y) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 {
        return None;
    }
    jiff::civil::Date::new(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?).ok()
}

fn iso_date(s: &str) -> Option<String> {
    civil_date(s).map(crate::dates::iso)
}

/// `1234.5`, `-80`, `"1.234,56"`, `"-7,50 €"` → cents (no floats).
fn cents(v: &Value) -> Option<i64> {
    let text = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s
            .trim()
            .trim_end_matches(['€', ' ', '\u{a0}'])
            .trim_end_matches("EUR")
            .trim()
            .replace('\u{2212}', "-"),
        _ => return None,
    };
    crate::money::parse_cents(&text).ok()
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().map(str::trim).unwrap_or("").to_string()
}

/// An id as the API writes it (string or number).
fn id_of(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The list under `key`, or the answer itself when it is the list.
fn list_of<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    v[key].as_array().or_else(|| v.as_array())
}

#[derive(Debug, Clone, PartialEq)]
struct Contract {
    id: String,
    name: String,
    kind: AccountKind,
    /// `None`: the BHW listed no balance for it.
    balance_cents: Option<i64>,
    currency: String,
    /// Every number the BHW uses for it (`id`, `contractId`, …), as
    /// [`contract_key`]: the mailbox names the contract by one of them.
    keys: Vec<String>,
}

/// A contract number reduced to compare: its digits without leading zeros
/// ("0012 3456-78" = "12345678"); letters only when there are no digits.
fn contract_key(s: &str) -> String {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    } else {
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".into()
        } else {
            trimmed.into()
        }
    }
}

/// Every row of the contract list carries an id: only then does the list
/// show all contracts of the login, and a missing one counts as gone.
fn contracts_complete(v: &Value) -> bool {
    list_of(v, "contracts").is_some_and(|list| {
        list.iter()
            .all(|c| !id_of(c, "id").is_empty() || !id_of(c, "contractId").is_empty())
    })
}

/// Some PDFs of the mailbox could not be downloaded.
fn documents_missing() -> String {
    tr("BHW: Nicht alle Dokumente konnten geladen werden. Sie folgen mit dem nächsten Abruf.")
        .into()
}

fn parse_contracts(v: &Value) -> CoreResult<Vec<Contract>> {
    let list = list_of(v, "contracts").ok_or_else(invalid)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in list {
        let mut id = id_of(c, "id");
        if id.is_empty() {
            id = id_of(c, "contractId");
        }
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let mut name = text(c, "productLongName");
        if name.is_empty() {
            name = text(c, "productName");
        }
        if name.is_empty() {
            name = "Bausparvertrag".into();
        }
        let balance = match &c["accountBalance"] {
            Value::Null => None,
            b => Some(cents(b).ok_or_else(invalid)?),
        };
        let lower = name.to_lowercase();
        let kind = if lower.contains("darlehen")
            || lower.contains("kredit")
            || lower.contains("finanzierung")
            || balance.is_some_and(|b| b < 0)
        {
            AccountKind::Loan
        } else {
            AccountKind::Savings
        };
        // A loan is debt: shown as a negative balance like the other loan accounts.
        let balance_cents = balance.map(|b| {
            if kind == AccountKind::Loan && b > 0 {
                -b
            } else {
                b
            }
        });
        let currency = match text(c, "currency") {
            cur if cur.len() == 3 => cur.to_uppercase(),
            _ => "EUR".into(),
        };
        let mut keys: Vec<String> = ["id", "contractId", "contractNumber", "accountNumber"]
            .iter()
            .map(|k| contract_key(&id_of(c, k)))
            .filter(|k| !k.is_empty())
            .collect();
        keys.dedup();
        out.push(Contract {
            id,
            name,
            kind,
            balance_cents,
            currency,
            keys,
        });
    }
    // The contract list is complete (paid-out contracts disappear from it), so
    // an empty answer is no answer: it would mark every account as gone.
    if out.is_empty() {
        return Err(invalid());
    }
    Ok(out)
}

fn account_ref(contract: &Contract) -> String {
    format!("{REF_PREFIX}{}", contract.id)
}

/// The bookings of one contract on or after `since` (`YYYY-MM-DD`). The BHW
/// gives no booking ids: `external_id` stays empty and the vault derives a
/// content id.
fn parse_transactions(
    v: &Value,
    contract: &Contract,
    since: &str,
) -> CoreResult<Vec<RemoteTransaction>> {
    let list = list_of(v, "transactions").ok_or_else(invalid)?;
    let mut out = Vec::new();
    for t in list {
        let date = iso_date(&text(t, "postingDate"))
            .or_else(|| iso_date(&text(t, "bookingDate")))
            .ok_or_else(invalid)?;
        let value_date = iso_date(&text(t, "valueDate")).unwrap_or_else(|| date.clone());
        let amount_cents = cents(&t["amount"]).ok_or_else(invalid)?;
        let currency = match text(t, "currency") {
            cur if cur.len() == 3 => cur.to_uppercase(),
            _ => contract.currency.clone(),
        };
        if date.as_str() < since {
            continue;
        }
        out.push(RemoteTransaction {
            account_ref: account_ref(contract),
            external_id: String::new(),
            booking_date: date,
            value_date,
            amount_cents,
            currency,
            purpose: text(t, "text"),
            ..Default::default()
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------ mailbox

#[derive(Debug, Clone, PartialEq)]
struct Message {
    id: String,
    contract: String,
    name: String,
    date: String,
}

fn parse_messages(v: &Value) -> CoreResult<Vec<Message>> {
    let list = list_of(v, "messages").ok_or_else(invalid)?;
    Ok(list
        .iter()
        .filter_map(|m| {
            let id = id_of(m, "id");
            if id.is_empty() {
                return None;
            }
            Some(Message {
                id,
                contract: id_of(m, "contractId"),
                name: text(m, "name"),
                date: iso_date(&text(m, "creationDate")).unwrap_or_else(super::today),
            })
        })
        .collect())
}

/// The account a mailbox message belongs to: the contract it names, or the
/// only contract there is when it names none the BHW listed.
fn message_account(m: &Message, contracts: &[Contract], logins: &[String]) -> Option<String> {
    match (
        contract_by_number(&m.contract, contracts, logins),
        contracts,
    ) {
        (Some(c), _) => Some(account_ref(c)),
        (None, [only]) => Some(account_ref(only)),
        (None, _) => None,
    }
}

/// The digits of a BHW contract number when it has the BHW form: ten digits,
/// an eight-digit contract stem and two digits for the part (Bausparvertrag,
/// Darlehen, the contract as a whole: "6 727 751 7 00").
fn bhw_number(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    (digits.len() == 10).then_some(digits)
}

/// The listed contract a number belongs to: the one contract with exactly
/// this number (a number several contracts carry is no match). A number for
/// the contract as a whole ("…00") goes to the only part with its eight-digit
/// stem, else, when several share it, to the one the login names, else to the
/// Bausparvertrag (the only part that is not a loan): the contract the
/// Darlehen grew out of. Another part's number ("…52") never matches by stem.
fn contract_by_number<'a>(
    number: &str,
    contracts: &'a [Contract],
    logins: &[String],
) -> Option<&'a Contract> {
    let key = contract_key(number);
    if key.is_empty() {
        return None;
    }
    match contracts
        .iter()
        .filter(|c| c.keys.contains(&key))
        .collect::<Vec<_>>()
        .as_slice()
    {
        [c] => return Some(c),
        [] => {}
        _ => return None,
    }
    let whole = bhw_number(number).filter(|d| d.ends_with("00"))?;
    let stem = whole[..8].to_string();
    let same_stem: Vec<&Contract> = contracts
        .iter()
        .filter(|c| bhw_number(&c.id).is_some_and(|d| d.starts_with(&stem)))
        .collect();
    match same_stem.as_slice() {
        [only] => Some(only),
        [] => None,
        several => {
            let logins: Vec<String> = logins.iter().filter_map(|l| bhw_number(l)).collect();
            if let Some(c) = several
                .iter()
                .find(|c| bhw_number(&c.id).is_some_and(|d| logins.contains(&d)))
            {
                return Some(c);
            }
            match several
                .iter()
                .filter(|c| c.kind != AccountKind::Loan)
                .collect::<Vec<_>>()
                .as_slice()
            {
                [savings] => Some(savings),
                _ => None,
            }
        }
    }
}

/// Every value in `v` that can be a contract number (6–14 digits, as a
/// whole number or a string of digits and spaces), as [`contract_key`].
fn numbers_in(v: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 5 {
        return;
    }
    let mut add = |s: &str| {
        let digits = s.chars().filter(char::is_ascii_digit).count();
        if (6..=14).contains(&digits) && s.chars().all(|c| c.is_ascii_digit() || c == ' ') {
            out.push(contract_key(s));
        }
    };
    match v {
        Value::String(s) => add(s.trim()),
        Value::Number(n) if n.is_u64() => add(&n.to_string()),
        Value::Array(a) => a.iter().for_each(|x| numbers_in(x, depth + 1, out)),
        Value::Object(o) => o.values().for_each(|x| numbers_in(x, depth + 1, out)),
        _ => {}
    }
}

/// How many runs of exactly `len` digits a text holds (debug log only).
fn digit_runs(text: &str, len: usize) -> usize {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| run.len() == len)
        .count()
}

/// How a document's own text names its contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextMatch {
    /// The number after "Vertragsnummer" / "Vertrags-Nr." is a listed contract.
    Label,
    /// Exactly one listed contract number occurs in the text.
    Unique,
    /// Several do: left without an account rather than guessed.
    Ambiguous,
    /// None does (or the text could not be read).
    None,
}

/// Separators the BHW writes between the digit groups of a number.
fn number_separator(c: char) -> bool {
    matches!(c, ' ' | '.' | '\u{a0}')
}

/// Where `digits` stand in `hay` as a number of their own: written with or
/// without single spaces or dots between the digits ("12 3456 7801"), but not
/// as part of a longer number, grouped or not ("Kundennummer 9 12 3456 7801",
/// "91234567801"). A date may follow ("1234567801 19.01.2024").
fn number_at(hay: &str, digits: &str) -> Option<usize> {
    static DATE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[0-9]{1,2}\.[0-9]{1,2}\.[0-9]{2,4}(?:[^0-9]|$)").expect("date pattern")
    });
    let body = digits
        .chars()
        .map(String::from)
        .collect::<Vec<_>>()
        .join("[ .\u{a0}]?");
    let re = regex::Regex::new(&body).ok()?;
    let mut from = 0;
    while let Some(m) = re.find_at(hay, from) {
        let mut before = hay[..m.start()].chars().rev();
        let starts_alone = match before.next() {
            None => true,
            Some(c) if c.is_ascii_digit() => false,
            Some(c) if number_separator(c) => !before.next().is_some_and(|d| d.is_ascii_digit()),
            Some(_) => true,
        };
        let after = &hay[m.end()..];
        let mut next = after.chars();
        let ends_alone = match next.next() {
            None => true,
            Some(c) if c.is_ascii_digit() => false,
            Some(c) if number_separator(c) && next.next().is_some_and(|d| d.is_ascii_digit()) => {
                DATE.is_match(&after[c.len_utf8()..])
            }
            Some(_) => true,
        };
        if starts_alone && ends_alone {
            return Some(m.start());
        }
        from = m.start() + hay[m.start()..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The contract a mailbox PDF is about, read from its text: the BHW prints
/// the contract number ("Vertrags-Nr.") on every letter, while the mailbox
/// field `contractId` can name another number. Only the listed contracts'
/// own numbers (at least six digits) and the numbers of their contracts as a
/// whole ("…00") count, each as a number of its own.
fn contract_in_text(
    text: &str,
    contracts: &[Contract],
    logins: &[String],
) -> (Option<String>, TextMatch) {
    let numbers: Vec<(String, &Contract)> = contracts
        .iter()
        .map(|c| {
            (
                c.id.chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>(),
                c,
            )
        })
        .filter(|(digits, _)| digits.len() >= 6)
        .collect();
    // The windows right after a label ("Vertragsnummer", "Vertrags-/Kontonummer").
    // ASCII only: the label offsets must fit the original text.
    let lower = text.to_ascii_lowercase();
    let windows: Vec<String> = lower
        .match_indices("vertrag")
        .map(|(pos, _)| text.get(pos..).unwrap_or("").chars().take(80).collect())
        .collect();
    // The number right after a label: the first listed one that follows it.
    for after in &windows {
        let first = numbers
            .iter()
            .filter_map(|(digits, c)| number_at(after, digits).map(|at| (at, *c)))
            .min_by_key(|(at, _)| *at);
        if let Some((_, c)) = first {
            return (Some(account_ref(c)), TextMatch::Label);
        }
    }
    let found: Vec<&Contract> = numbers
        .iter()
        .filter(|(digits, _)| number_at(text, digits).is_some())
        .map(|(_, c)| *c)
        .collect();
    match found.as_slice() {
        [c] => return (Some(account_ref(c)), TextMatch::Unique),
        [] => {}
        _ => return (None, TextMatch::Ambiguous),
    }
    // No listed number written out: the number of a listed contract as a
    // whole ("6 727 751 7 00"), first after a label, then anywhere in the text.
    let mut wholes: Vec<String> = contracts
        .iter()
        .filter_map(|c| bhw_number(&c.id))
        .map(|d| format!("{}00", &d[..8]))
        .filter(|w| numbers.iter().all(|(digits, _)| digits != w))
        .collect();
    wholes.sort();
    wholes.dedup();
    for after in &windows {
        let first = wholes
            .iter()
            .filter_map(|w| number_at(after, w).map(|at| (at, w)))
            .min_by_key(|(at, _)| *at)
            .and_then(|(_, w)| contract_by_number(w, contracts, logins));
        if let Some(c) = first {
            return (Some(account_ref(c)), TextMatch::Label);
        }
    }
    let mut found: Vec<&Contract> = wholes
        .iter()
        .filter(|w| number_at(text, w).is_some())
        .filter_map(|w| contract_by_number(w, contracts, logins))
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found.dedup_by(|a, b| a.id == b.id);
    match found.as_slice() {
        [c] => (Some(account_ref(c)), TextMatch::Unique),
        [] => (None, TextMatch::None),
        _ => (None, TextMatch::Ambiguous),
    }
}

/// How a contract number the mailbox names relates to the listed contracts,
/// for the debug log: lengths and prefix/suffix hits only, never a number.
fn key_relation(m: &Message, contracts: &[Contract]) -> String {
    let key = contract_key(&m.contract);
    let lens: Vec<String> = contracts
        .iter()
        .flat_map(|c| c.keys.iter().map(|k| k.len().to_string()))
        .collect();
    let related = |f: &dyn Fn(&str, &str) -> bool| {
        contracts
            .iter()
            .any(|c| c.keys.iter().any(|k| !key.is_empty() && f(k, &key)))
    };
    let same_stem = bhw_number(&m.contract).is_some_and(|n| {
        contracts
            .iter()
            .any(|c| bhw_number(&c.id).is_some_and(|d| d[..8] == n[..8]))
    });
    format!(
        "len={} keys=[{}] same_stem={same_stem} key_starts={} key_ends={} in_key={} key_in={}",
        key.len(),
        lens.join(","),
        related(&|k, m| k.starts_with(m)),
        related(&|k, m| k.ends_with(m)),
        related(&|k, m| k.contains(m)),
        related(&|k, m| m.contains(k)),
    )
}

/// The structure of an answer for the debug log: types, field names and
/// list lengths, never a value.
fn shape(v: &Value) -> String {
    fn walk(v: &Value, depth: usize) -> String {
        match v {
            Value::Null => "null".into(),
            Value::Bool(_) => "bool".into(),
            Value::Number(_) => "num".into(),
            Value::String(_) => "str".into(),
            Value::Array(a) if depth < 3 => match a.first() {
                Some(first) => format!("[{}x {}]", a.len(), walk(first, depth + 1)),
                None => "[]".into(),
            },
            Value::Array(a) => format!("[{}x]", a.len()),
            Value::Object(o) if depth < 3 => format!(
                "{{{}}}",
                o.iter()
                    .map(|(k, v)| format!("{}:{}", clean_key(k), walk(v, depth + 1)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Value::Object(o) => format!("{{{} keys}}", o.len()),
        }
    }
    fn clean_key(k: &str) -> String {
        k.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(30)
            .collect()
    }
    walk(v, 0)
}

fn message_title(m: &Message) -> String {
    let name = m
        .name
        .trim_end_matches(".pdf")
        .trim_end_matches(".PDF")
        .trim();
    if name.is_empty() {
        "myBHW Postfach".into()
    } else {
        name.to_string()
    }
}

fn document_type(m: &Message) -> &'static str {
    let n = m.name.to_lowercase();
    if n.contains("kontoauszug") || n.contains("jahresauszug") {
        "kontoauszug"
    } else if n.contains("steuer") || n.contains("zinsbescheinigung") {
        "steuer"
    } else {
        "postbox"
    }
}

/// `YYYY-MM-DD_<title>_<id>.pdf`, without path separators.
fn document_file_name(m: &Message) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let title: String = clean(&message_title(m)).chars().take(60).collect();
    let id: String = clean(&m.id).chars().take(16).collect();
    format!("{}_{title}_{id}.pdf", m.date)
}

// ------------------------------------------------------------------ connector

/// The login started by `begin`, waiting for the one-time code.
struct PendingCode {
    method: CodeMethod,
    data: Value,
    hashed_tan: Value,
}

fn client() -> CoreResult<reqwest::Client> {
    use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER};
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("de-DE,de;q=0.9"));
    headers.insert(reqwest::header::ORIGIN, HeaderValue::from_static(ORIGIN));
    headers.insert(REFERER, HeaderValue::from_static(PORTAL));
    Ok(super::client_builder(std::time::Duration::from_secs(60))
        .user_agent(super::BROWSER_UA)
        .default_headers(headers)
        .cookie_store(true)
        .redirect(super::same_host_redirects())
        .build()?)
}

#[derive(Default)]
pub struct BhwConnector {
    client: Option<reqwest::Client>,
    /// `x-request-id` of this session.
    request_id: Option<String>,
    pending: Option<PendingCode>,
    logged_in: bool,
}

impl BhwConnector {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn url(path: &str) -> CoreResult<url::Url> {
        let url = url::Url::parse(API)
            .and_then(|base| base.join(path))
            .map_err(|_| page_changed())?;
        ensure_host(&url)?;
        Ok(url)
    }

    /// One API call; returns the status and the JSON answer (`Null` if none).
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> CoreResult<(reqwest::StatusCode, Value)> {
        let mut req = self.http()?.request(method, Self::url(path)?);
        if let Some(id) = &self.request_id {
            req = req.header("x-request-id", id.as_str());
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        ensure_host(resp.url())?;
        let status = resp.status();
        Ok((status, super::read_json_or_null(resp).await))
    }

    /// GET of the logged-in API; 401/403 = session ended.
    async fn get_json(&self, path: &str, step: &str) -> CoreResult<Value> {
        if !self.logged_in {
            return Err(CoreError::SessionClosed);
        }
        let (status, v) = self.call(reqwest::Method::GET, path, None).await?;
        if matches!(status.as_u16(), 401 | 403) {
            return Err(session_ended());
        }
        if !status.is_success() {
            return Err(rejected(status, step, &v));
        }
        Ok(v)
    }

    /// The contracts with the numbers of their details (`GET contracts/<id>`)
    /// added to their keys. Best effort: a detail that fails adds nothing.
    async fn with_detail_keys(&self, contracts: &[Contract]) -> Vec<Contract> {
        let mut out = contracts.to_vec();
        for c in &mut out {
            let path = format!(
                "contracts/{}",
                url::form_urlencoded::byte_serialize(c.id.as_bytes()).collect::<String>()
            );
            match self.get_json(&path, "contract").await {
                Ok(details) => {
                    let before = c.keys.len();
                    numbers_in(&details, 0, &mut c.keys);
                    c.keys.sort();
                    c.keys.dedup();
                    crate::depot_debug::log(&format!(
                        "bhw contract details: keys {before} -> {} answer={}",
                        c.keys.len(),
                        shape(&details)
                    ));
                }
                Err(_) => crate::depot_debug::log("bhw contract details: failed"),
            }
        }
        out
    }

    /// PDFs of the mailbox messages of the accounts in `wanted` (external
    /// refs) that are not in `known`, and whether some could not be
    /// downloaded (they stay unknown and come with a later fetch). A PDF
    /// that turns out to belong to another account, or to one in `skipped`,
    /// is left for that account's fetch and not remembered.
    async fn documents(
        &self,
        contracts: &[Contract],
        wanted: &HashSet<String>,
        skipped: &HashSet<String>,
        known: &mut KnownDocuments,
        logins: &[String],
    ) -> CoreResult<(Vec<RemoteDocument>, bool)> {
        let raw = self.get_json("messages", "messages").await?;
        crate::depot_debug::log(&format!(
            "bhw mailbox: contracts={} wanted={} answer={}",
            contracts.len(),
            wanted.len(),
            shape(&raw)
        ));
        let messages = parse_messages(&raw)?;
        // The mailbox may name a contract by a number only its details carry.
        let detailed;
        let contracts = if messages
            .iter()
            .any(|m| message_account(m, contracts, logins).is_none())
        {
            detailed = self.with_detail_keys(contracts).await;
            detailed.as_slice()
        } else {
            contracts
        };
        let mut out = Vec::new();
        let mut incomplete = false;
        for (n, m) in messages.into_iter().enumerate() {
            let account = message_account(&m, contracts, logins);
            let source = format!("bhw:message:{}", m.id);
            crate::depot_debug::log(&format!(
                "bhw message #{n}: names_contract={} matched={} wanted={} known={}{}",
                !m.contract.is_empty(),
                account.is_some(),
                account.as_ref().is_some_and(|r| wanted.contains(r)),
                known.contains(&source),
                if account.is_none() {
                    format!(" {}", key_relation(&m, contracts))
                } else {
                    String::new()
                }
            ));
            // Another account's document waits for that account's fetch.
            if account
                .as_ref()
                .is_some_and(|r| !wanted.contains(r) || skipped.contains(r))
            {
                continue;
            }
            if known.contains(&source) {
                // Stored before without an account: an entry without bytes
                // gives it the account found now (`store_fetch`).
                if let Some(account_ref) = account {
                    out.push(RemoteDocument {
                        account_ref,
                        booking_external_id: None,
                        title: message_title(&m),
                        doc_type: document_type(&m).into(),
                        mime_type: "application/pdf".into(),
                        file_name: document_file_name(&m),
                        bytes: Vec::new(),
                        source_url: source,
                        date: m.date.clone(),
                    });
                }
                continue;
            }
            let path = format!(
                "messages/{}/document",
                url::form_urlencoded::byte_serialize(m.id.as_bytes()).collect::<String>()
            );
            // The portal opens the file in a new browser tab: no JSON.
            let mut req = self.http()?.get(Self::url(&path)?).header(
                reqwest::header::ACCEPT,
                "application/pdf,application/octet-stream,*/*",
            );
            if let Some(id) = &self.request_id {
                req = req.header("x-request-id", id.as_str());
            }
            // A failed download stays unknown for a later fetch; the other
            // PDFs are still downloaded.
            let Ok(resp) = req.send().await else {
                crate::depot_debug::log(&format!("bhw document #{n}: request failed"));
                incomplete = true;
                continue;
            };
            ensure_host(resp.url())?;
            let status = resp.status();
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || "/;=-+. ".contains(*c))
                .take(60)
                .collect::<String>();
            if !status.is_success() {
                crate::depot_debug::log(&format!(
                    "bhw document #{n}: status={} type={content_type}",
                    status.as_u16()
                ));
                incomplete = true;
                continue;
            }
            let Ok(bytes) = super::read_body_limited(resp, super::MAX_DOC_BYTES).await else {
                crate::depot_debug::log(&format!("bhw document #{n}: body failed"));
                incomplete = true;
                continue;
            };
            crate::depot_debug::log(&format!(
                "bhw document #{n}: status={} type={content_type} bytes={} pdf={}",
                status.as_u16(),
                bytes.len(),
                bytes.starts_with(b"%PDF")
            ));
            if !bytes.starts_with(b"%PDF") {
                continue;
            }
            // The mailbox field names no listed contract: the letter itself does.
            let account_ref = match account {
                Some(r) => r,
                None => {
                    let text = crate::documents::extract_pdf_text(&bytes);
                    let (found, how) = contract_in_text(&text, contracts, logins);
                    crate::depot_debug::log(&format!(
                        "bhw document #{n}: text_chars={} digit_runs10={} contract_from_text={how:?}",
                        text.chars().count(),
                        digit_runs(&text, 10)
                    ));
                    found.unwrap_or_default()
                }
            };
            // The letter names another account than the one asked for.
            if !account_ref.is_empty()
                && (!wanted.contains(&account_ref) || skipped.contains(&account_ref))
            {
                continue;
            }
            known.insert(source.clone());
            out.push(RemoteDocument {
                account_ref,
                booking_external_id: None,
                title: message_title(&m),
                doc_type: document_type(&m).into(),
                mime_type: "application/pdf".into(),
                file_name: document_file_name(&m),
                bytes,
                source_url: source,
                date: m.date.clone(),
            });
        }
        Ok((out, incomplete))
    }
}

#[async_trait]
impl BankConnector for BhwConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let who = identity(&ctx.login_name, ctx.secret.expose())?;
        self.client = Some(client()?);
        self.request_id = None;
        self.logged_in = false;

        let (status, tx) = self
            .call(
                reqwest::Method::POST,
                "transactionId",
                Some(&json!({
                    "product": "002", "flavor": "BH", "application": "OPRA", "channel": "1",
                    "type": "2", "country": "DE", "promotionId": "00000000",
                })),
            )
            .await?;
        if !status.is_success() {
            return Err(rejected(status, "transactionId", &tx));
        }
        let request_id = id_of(&tx, "transactionId");
        if request_id.is_empty() {
            return Err(page_changed());
        }
        self.request_id = Some(request_id);

        let mut body = json!({
            "idType": who.id_type,
            "id": who.id,
            "firstName": who.first_name,
            "dateOfBirth": who.birth_date,
            "zip": who.zip,
        });
        drop(who);
        let sent = self.call(reqwest::Method::POST, "login", Some(&body)).await;
        body.zeroize_strings();
        let (status, login) = sent?;
        let code = bank_error_code(&login);
        if login["error"].is_string() || matches!(status.as_u16(), 400 | 401 | 403) {
            return Err(wrong_identity(&code));
        }
        if !status.is_success() {
            return Err(rejected(status, "login", &login));
        }
        let method = code_method(&login)?;
        let data = login["data"].clone();
        if data.is_null() {
            return Err(page_changed());
        }

        let (status, otp) = self
            .call(
                reqwest::Method::PATCH,
                "login",
                Some(&json!({ "method": method.as_str(), "data": data })),
            )
            .await?;
        if !status.is_success() || otp["error"].is_string() || otp["hashedTan"].is_null() {
            return Err(code_not_sent());
        }
        self.pending = Some(PendingCode {
            method,
            data: otp["data"].clone(),
            hashed_tan: otp["hashedTan"].clone(),
        });
        Ok(Phase::Challenge(code_challenge(method)))
    }

    async fn respond(&mut self, _ctx: &ConnectContext, r: ChallengeResponse) -> CoreResult<Phase> {
        let Some(pending) = self.pending.as_ref() else {
            return Err(CoreError::SessionClosed);
        };
        let ChallengeResponse::Code(tan) = r else {
            return Ok(Phase::Pending(code_challenge(pending.method)));
        };
        let tan: String = tan
            .expose()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let mut body = json!({
            "data": pending.data,
            "method": pending.method.as_str(),
            "hashedTan": pending.hashed_tan,
            "tan": tan,
        });
        let sent = self.call(reqwest::Method::PUT, "login", Some(&body)).await;
        body.zeroize_strings();
        let (status, res) = sent?;
        self.pending = None;
        if status.is_success() && res["verified"] == Value::Bool(true) {
            self.logged_in = true;
            return Ok(Phase::Ready);
        }
        Err(wrong_code())
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let raw_contracts = self.get_json("contracts", "contracts").await?;
        crate::depot_debug::log(&format!("bhw contracts: answer={}", shape(&raw_contracts)));
        let contracts = parse_contracts(&raw_contracts)?;
        let mut known = KnownDocuments::from_state(&ctx.state);

        if let Documents::StatementsOnly(only) = &ctx.documents {
            let wanted = HashSet::from([only.clone()]);
            let logins = [ctx.login_name.clone()];
            let (documents, incomplete) = self
                .documents(
                    &contracts,
                    &wanted,
                    &ctx.skipped_accounts,
                    &mut known,
                    &logins,
                )
                .await?;
            return Ok(FetchResult {
                documents,
                documents_incomplete: incomplete,
                warnings: if incomplete {
                    vec![documents_missing()]
                } else {
                    Vec::new()
                },
                state: known.to_state(),
                // No bookings were fetched: the login's sync time stays.
                single_account: true,
                ..Default::default()
            });
        }

        let mut out = FetchResult {
            bank_name: Some("BHW Bausparkasse".into()),
            // A row the list could not be read for must not mark a stored
            // contract as gone.
            accounts_complete: contracts_complete(&raw_contracts),
            ..Default::default()
        };
        for contract in &contracts {
            let account_ref = account_ref(contract);
            // One account's sync (#1124) asks only for its bookings: another
            // contract's failing request must not stop it.
            if ctx.only_account.as_ref().is_some_and(|o| *o != account_ref) {
                continue;
            }
            let since = ctx.since_for(&account_ref).to_string();
            let path = format!(
                "contracts/{}/transactions",
                url::form_urlencoded::byte_serialize(contract.id.as_bytes()).collect::<String>()
            );
            let raw = self.get_json(&path, "transactions").await?;
            out.transactions
                .extend(parse_transactions(&raw, contract, &since)?);
            if contract.balance_cents.is_some() {
                out.balances_reported.push(account_ref.clone());
            } else {
                out.balances_unknown.push(account_ref.clone());
            }
            out.accounts.push(remote_account(contract, &today));
        }

        let due = ctx.documents.due_statements();
        if !due.is_empty() {
            let wanted: HashSet<String> = due.iter().cloned().collect();
            // Optional: a failed download never fails the sync.
            let mut fetched = known.clone();
            let logins = [ctx.login_name.clone()];
            match self
                .documents(
                    &contracts,
                    &wanted,
                    &ctx.skipped_accounts,
                    &mut fetched,
                    &logins,
                )
                .await
            {
                // Statements stay due until every PDF came.
                Ok((documents, false)) => {
                    out.documents = documents;
                    out.statements_fetched = due.to_vec();
                    known = fetched;
                }
                Ok((documents, true)) => {
                    out.documents = documents;
                    out.documents_incomplete = true;
                    known = fetched;
                }
                Err(_) => out.documents_incomplete = true,
            }
        }
        out.state = known.to_state();
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if self.logged_in {
            let _ = self.call(reqwest::Method::PUT, "logout", None).await;
        }
        self.logged_in = false;
        self.pending = None;
        self.request_id = None;
        self.client = None;
    }
}

fn remote_account(contract: &Contract, today: &str) -> RemoteAccount {
    RemoteAccount {
        external_ref: account_ref(contract),
        name: if contract.name.to_lowercase().starts_with("bhw") {
            contract.name.clone()
        } else {
            format!("BHW {}", contract.name)
        },
        owner: String::new(),
        iban: String::new(),
        account_number: contract.id.clone(),
        currency: contract.currency.clone(),
        kind: contract.kind,
        balance_cents: contract.balance_cents.unwrap_or(0),
        balance_date: today.to_string(),
    }
}

/// Overwrites the string values of a request body (login data, code) once it
/// was sent.
trait ZeroizeStrings {
    fn zeroize_strings(&mut self);
}

impl ZeroizeStrings for Value {
    fn zeroize_strings(&mut self) {
        match self {
            Value::String(s) => s.zeroize(),
            Value::Array(a) => a.iter_mut().for_each(ZeroizeStrings::zeroize_strings),
            Value::Object(o) => o.values_mut().for_each(ZeroizeStrings::zeroize_strings),
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ placing mailbox PDFs by their text

/// Account id, external ref, kind and eligibility of every BHW account.
/// All of them take part in matching a letter's number; only an eligible
/// one (not archived, not switched off or pending in the account
/// selection) gets the document.
fn bhw_accounts(db: &rusqlite::Connection) -> CoreResult<Vec<(i64, String, String, bool)>> {
    let mut st = db.prepare(
        "SELECT a.id, a.external_ref, a.kind, a.connection_id, a.archived_unix IS NULL
           FROM accounts a
           JOIN bank_connections c ON c.id = a.connection_id
          WHERE c.connector = ?1 AND c.deleted_ms IS NULL AND a.deleted_ms IS NULL",
    )?;
    let rows = st
        .query_map([crate::models::ConnectorKind::Bhw.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<(i64, String, String, i64, bool)>, _>>()?;
    let mut skipped: std::collections::HashMap<i64, Vec<String>> = Default::default();
    let mut out = Vec::new();
    for (id, external_ref, kind, connection_id, active) in rows {
        let refs = match skipped.entry(connection_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(super::account_selection::skipped_refs(db, connection_id)?)
            }
        };
        let eligible = active && !refs.contains(&external_ref);
        out.push((id, external_ref, kind, eligible));
    }
    Ok(out)
}

const UNPLACED: &str = "account_id IS NULL AND source_url LIKE 'bhw:message:%'";

#[uniffi::export]
impl Vault {
    /// Mailbox PDFs of BHW logins stored without an account (the mailbox
    /// named no listed contract). The app reads their text layer (PDFKit,
    /// which reads fonts the core's simple extractor cannot) and passes it to
    /// [`place_bhw_document`](Self::place_bhw_document).
    pub fn bhw_unplaced_documents(&self) -> Result<Vec<i64>, CoreError> {
        let db = self.db()?;
        let mut st = db.prepare(&format!(
            "SELECT id FROM booking_documents WHERE {UNPLACED} ORDER BY id"
        ))?;
        let ids = st
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(ids)
    }

    /// Moves an unplaced BHW mailbox PDF to the BHW account whose contract
    /// number its `text` names (see `contract_in_text`). Returns that
    /// account's id, or `None` when the text names none or several, or the
    /// document is not an unplaced BHW one.
    pub fn place_bhw_document(&self, doc_id: i64, text: String) -> Result<Option<i64>, CoreError> {
        let accounts = {
            let db = self.db()?;
            let unplaced: bool = db.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM booking_documents WHERE id = ?1 AND {UNPLACED})"
                ),
                [doc_id],
                |r| r.get(0),
            )?;
            if !unplaced {
                return Ok(None);
            }
            let mut st = db.prepare(
                "SELECT login_name FROM bank_connections WHERE connector = ?1 AND deleted_ms IS NULL",
            )?;
            let logins = st
                .query_map([crate::models::ConnectorKind::Bhw.as_str()], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            (bhw_accounts(&db)?, logins)
        };
        let (accounts, logins) = accounts;
        let contracts: Vec<Contract> = accounts
            .iter()
            .filter_map(|(_, r, kind, _)| Some((r.strip_prefix(REF_PREFIX)?, kind)))
            .map(|(id, kind)| Contract {
                id: id.to_string(),
                name: String::new(),
                kind: AccountKind::from_str(kind),
                balance_cents: None,
                currency: "EUR".into(),
                keys: vec![contract_key(id)],
            })
            .collect();
        let (found, _) = contract_in_text(&text, &contracts, &logins);
        // The letter's contract, if it may get documents: an archived or
        // excluded one leaves the PDF unplaced, never moved to another.
        let Some(account) = found.and_then(|r| {
            accounts
                .iter()
                .find(|(_, x, _, eligible)| *x == r && *eligible)
                .map(|(id, _, _, _)| *id)
        }) else {
            return Ok(None);
        };
        // Only while it is still unplaced: an account or booking the user
        // gave it in the meantime stays.
        let moved = self.db()?.execute(
            &format!(
                "UPDATE booking_documents SET account_id = ?1
                  WHERE id = ?2 AND booking_id IS NULL AND {UNPLACED}"
            ),
            rusqlite::params![account, doc_id],
        )?;
        Ok((moved == 1).then_some(account))
    }
}

#[cfg(test)]
mod tests;
