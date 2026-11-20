//! CAMT.053 account statements, read straight from the vault.

use rusqlite::{Connection, params};

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::iban::{has_iban_shape, normalize_iban};
use crate::models::{Account, AccountKind};
use crate::vault::{PENDING_SQL, Vault};

/// ISO 20022 message version of a CAMT.053 export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CamtVersion {
    /// `camt.053.001.02` – what older programs import.
    V02,
    /// `camt.053.001.08` – current standard.
    V08,
}

impl CamtVersion {
    /// Message number in the namespace (`camt.053.001.<nn>`).
    fn number(self) -> &'static str {
        match self {
            CamtVersion::V02 => "02",
            CamtVersion::V08 => "08",
        }
    }

    fn bic_tag(self) -> &'static str {
        match self {
            CamtVersion::V02 => "BIC",
            CamtVersion::V08 => "BICFI",
        }
    }
}

/// What a CAMT.053 export covers.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Camt053Request {
    pub account_id: i64,
    /// First day (`YYYY-MM-DD`); `None` = the day of the first exported booking.
    pub date_from: Option<String>,
    /// Last day; `None` = the day of the last exported booking. A day after
    /// today is capped at today (or at the last exported booking).
    pub date_to: Option<String>,
    /// Only these bookings (marked in the list); `None` = all of the period.
    pub booking_ids: Option<Vec<i64>>,
    pub version: CamtVersion,
    /// ISO date-time for `CreDtTm` (`2025-03-31T12:00:00+02:00`).
    pub created_at: String,
}

/// A booking as the bank delivered it (no merchant names, no cleaned purpose).
#[derive(Debug, Clone, Default)]
struct CamtEntry {
    date: String,
    value_date: String,
    amount_cents: i64,
    currency: String,
    counterparty: String,
    counterparty_iban: String,
    purpose: String,
    booking_text: String,
    purpose_code: String,
    gvc: String,
    end_to_end_id: String,
    mandate_id: String,
    creditor_id: String,
    /// Bank's own reference (`camt:<ref>` in `external_id`), for `AcctSvcrRef`.
    external_id: String,
    pending: bool,
}

/// Day part of a stored date (`2025-03-01T10:00:00` → `2025-03-01`).
const DAY_SQL: &str = "substr(t.booking_date, 1, 10)";

#[uniffi::export]
impl Vault {
    /// CAMT.053 statement of one account, read straight from the vault
    /// (#372, #483). Names, purposes and references are written as the bank
    /// delivered them. OPBD is the booked balance at the start of the first
    /// day, CLBD the one at the end of the last day, both in the account
    /// currency (see [`booked_balance`]). Pending entries are listed as
    /// `PDNG`, entries in another currency with their own; neither counts.
    /// For a period CLBD = OPBD + booked entries; for marked bookings both
    /// stay the account's real balances, so a marking must not leave out a
    /// booked booking between its first and last day. Loans and depots are refused: their
    /// balance is no sum of own bookings.
    pub fn camt053_export(&self, request: Camt053Request) -> Result<String, CoreError> {
        let mut account = self
            .list_accounts()?
            .into_iter()
            .find(|a| a.id == request.account_id)
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Konto {0}", &[&request.account_id]),
            })?;
        if matches!(account.kind, AccountKind::Loan | AccountKind::Brokerage) {
            return Err(CoreError::invalid(tr(
                "CAMT.053 gibt es nicht für Darlehen und Depots: Ihr Saldo ergibt sich nicht aus eigenen Umsätzen.",
            )));
        }
        let date_from = request.date_from.as_deref().map(parse_day).transpose()?;
        let date_to = request.date_to.as_deref().map(parse_day).transpose()?;
        let currency = account_currency(&account.currency);
        let today = crate::dates::today_iso();
        let db = self.db()?;
        // `list_accounts` replaces a stored 0 of a credit card with the open
        // cycle's amount, counted over every row; the balance walk below counts
        // booked rows in the account currency only, so recompute that amount.
        if !account.balance_derived && account.kind == AccountKind::CreditCard {
            let stored: i64 = db.query_row(
                "SELECT balance_cents FROM accounts WHERE id = ?1",
                [account.id],
                |r| r.get(0),
            )?;
            if stored == 0 && account.balance_cents != 0 {
                let mut st = db.prepare(&format!(
                    "SELECT t.booking_date, t.amount_cents,
                            t.counterparty || ' ' || t.purpose || ' ' || t.booking_text
                       FROM transactions t
                      WHERE t.account_id = ?1 AND t.deleted_ms IS NULL
                        AND NOT ({PENDING_SQL})
                        AND (trim(t.currency) = '' OR upper(trim(t.currency)) = ?2)
                      ORDER BY t.booking_date, t.id"
                ))?;
                let rows: Vec<(String, i64, String)> = st
                    .query_map(params![account.id, currency], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<Result<_, _>>()?;
                account.balance_cents = crate::connectors::open_card_balance(&rows).unwrap_or(0);
                account.balance_date = today.clone();
            }
        }
        let ids: Option<std::collections::HashSet<i64>> = request
            .booking_ids
            .as_ref()
            .map(|ids| ids.iter().copied().collect());
        let entries = camt_entries(
            &db,
            account.id,
            date_from.as_deref(),
            date_to.as_deref(),
            ids.as_ref(),
        )?;
        let entries: Vec<CamtEntry> = entries.into_iter().map(|(_, e)| e).collect();
        let first = entries.first().map(|e| e.date.clone());
        let last = entries.last().map(|e| e.date.clone());
        // An end date after today describes a statement up to today (or up to
        // the last exported booking, when that lies further ahead).
        let to = date_to
            .map(|to| to.min(last.clone().unwrap_or_default().max(today.clone())))
            .or(last)
            .unwrap_or(today);
        // Without bookings the end date also bounds the start, so a request for
        // "everything up to <past day>" stays a valid, empty statement.
        let from = date_from.or(first).unwrap_or_else(|| to.clone());
        if to < from {
            return Err(CoreError::invalid(tr(
                "Der Zeitraum endet vor seinem Beginn.",
            )));
        }
        // Marked bookings: OPBD + entries = CLBD only holds when no booked
        // booking between the first and the last day is left out.
        if let Some(ids) = &ids {
            let gap = camt_entries(&db, account.id, Some(&from), Some(&to), None)?
                .iter()
                .any(|(id, e)| !e.pending && !ids.contains(id));
            if gap {
                return Err(CoreError::invalid(tr(
                    "CAMT.053 erfordert eine lückenlose Auswahl. Markiere alle Umsätze eines zusammenhängenden Zeitraums.",
                )));
            }
        }
        let opening = booked_balance(&db, &account, &currency, &previous_day(&from))?;
        let closing = booked_balance(&db, &account, &currency, &to)?;
        Ok(statement_xml(
            &account,
            &currency,
            &entries,
            request.version,
            (opening, &from),
            (closing, &to),
            &request.created_at,
        ))
    }
}

fn parse_day(s: &str) -> CoreResult<String> {
    let day: String = s.trim().chars().take(10).collect();
    jiff::civil::Date::strptime("%Y-%m-%d", &day)
        .map(crate::dates::iso)
        .map_err(|_| CoreError::invalid(trf("Ungültiges Datum: {0}", &[&s])))
}

fn previous_day(day: &str) -> String {
    jiff::civil::Date::strptime("%Y-%m-%d", day)
        .ok()
        .and_then(|d| d.yesterday().ok())
        .map(crate::dates::iso)
        .unwrap_or_default()
}

/// ISO 4217 code of the account; `EUR` where none is stored.
fn account_currency(s: &str) -> String {
    let c = s.trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) {
        c
    } else {
        "EUR".into()
    }
}

/// The account's bookings in `[from, to]` (by day), oldest first, with their
/// ids; `ids` narrows them without reading the others.
fn camt_entries(
    db: &Connection,
    account_id: i64,
    from: Option<&str>,
    to: Option<&str>,
    ids: Option<&std::collections::HashSet<i64>>,
) -> CoreResult<Vec<(i64, CamtEntry)>> {
    let id_filter = ids
        .map(|ids| {
            // SQLite has no `IN ()`: an empty marking is an empty statement.
            if ids.is_empty() {
                return "AND 0".to_string();
            }
            let list = (0..ids.len())
                .map(|n| format!("?{}", n + 4))
                .collect::<Vec<_>>()
                .join(",");
            format!("AND t.id IN ({list})")
        })
        .unwrap_or_default();
    let mut st = db.prepare(&format!(
        "SELECT t.id, {DAY_SQL}, substr(t.value_date, 1, 10), t.amount_cents, t.currency, t.counterparty,
                t.counterparty_iban, t.purpose, t.booking_text, t.purpose_code, t.gvc, t.end_to_end_id,
                t.mandate_id, t.creditor_id, t.external_id, {PENDING_SQL}
           FROM transactions t
          WHERE t.account_id = ?1 AND t.deleted_ms IS NULL
            AND (?2 IS NULL OR t.booking_date >= ?2)
            AND (?3 IS NULL OR t.booking_date < date(?3, '+1 day')) {id_filter}
          ORDER BY t.booking_date, t.id"
    ))?;
    let mut args = vec![
        rusqlite::types::Value::Integer(account_id),
        from.map_or(rusqlite::types::Value::Null, |s| {
            rusqlite::types::Value::Text(s.to_string())
        }),
        to.map_or(rusqlite::types::Value::Null, |s| {
            rusqlite::types::Value::Text(s.to_string())
        }),
    ];
    if let Some(ids) = ids {
        args.extend(ids.iter().map(|&id| rusqlite::types::Value::Integer(id)));
    }
    let rows = st.query_map(rusqlite::params_from_iter(args), |r| {
        Ok((
            r.get(0)?,
            CamtEntry {
                date: r.get(1)?,
                value_date: r.get(2)?,
                amount_cents: r.get(3)?,
                currency: r.get(4)?,
                counterparty: r.get(5)?,
                counterparty_iban: r.get(6)?,
                purpose: r.get(7)?,
                booking_text: r.get(8)?,
                purpose_code: r.get(9)?,
                gvc: r.get(10)?,
                end_to_end_id: r.get(11)?,
                mandate_id: r.get(12)?,
                creditor_id: r.get(13)?,
                external_id: r.get(14)?,
                pending: r.get(15)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Booked balance at the end of `day` in `currency` – pending entries never
/// count, days lose their time part, other currencies stay out.
///
/// - A computed balance (`balance_derived`) is the opening balance plus every
///   booking from its date on, so the booked entries up to `day` are summed.
/// - A balance from the bank holds at the end of `balance_date` (today if
///   unknown) without pending entries: booked entries between that day and
///   `day` are taken off or added.
fn booked_balance(db: &Connection, a: &Account, currency: &str, day: &str) -> CoreResult<i64> {
    let booked = format!("t.deleted_ms IS NULL AND NOT ({PENDING_SQL})");
    let same_currency = "(trim(t.currency) = '' OR upper(trim(t.currency)) = ?4)";
    if a.balance_derived {
        let since = if a.opening_balance_cents.is_some() {
            a.opening_balance_date.as_str()
        } else {
            ""
        };
        // The Startsaldo is the balance at the start of its date, so it covers
        // every earlier booking. A day before that date takes the bookings
        // between it and the Startsaldo off again.
        let delta: i64 = db.query_row(
            &format!(
                "SELECT IFNULL(SUM(CASE
                      WHEN (?3 = '' OR {DAY_SQL} >= ?3) AND {DAY_SQL} <= ?2 THEN t.amount_cents
                      WHEN {DAY_SQL} > ?2 AND {DAY_SQL} < ?3 THEN -t.amount_cents
                      ELSE 0 END), 0)
                   FROM transactions t
                  WHERE t.account_id = ?1 AND {booked} AND {same_currency}
                    AND t.booking_date >= min(?2, ?3)
                    AND t.booking_date < date(max(?2, ?3), '+1 day')"
            ),
            params![a.id, day, since, currency],
            |r| r.get(0),
        )?;
        return Ok(a.opening_balance_cents.unwrap_or(0) + delta);
    }
    let at: String = a.balance_date.chars().take(10).collect();
    let anchor = if at.is_empty() {
        crate::dates::today_iso()
    } else {
        at
    };
    let delta: i64 = db.query_row(
        &format!(
            "SELECT IFNULL(SUM(CASE
                      WHEN {DAY_SQL} > ?3 AND {DAY_SQL} <= ?2 THEN t.amount_cents
                      WHEN {DAY_SQL} > ?2 AND {DAY_SQL} <= ?3 THEN -t.amount_cents
                      ELSE 0 END), 0)
               FROM transactions t
              WHERE t.account_id = ?1 AND {booked} AND {same_currency}
                AND t.booking_date >= min(?2, ?3)
                AND t.booking_date < date(max(?2, ?3), '+1 day')"
        ),
        params![a.id, day, anchor, currency],
        |r| r.get(0),
    )?;
    Ok(a.balance_cents + delta)
}

/// The XML of one statement; `entries` are sorted by day.
fn statement_xml(
    account: &Account,
    currency: &str,
    entries: &[CamtEntry],
    version: CamtVersion,
    opening: (i64, &str),
    closing: (i64, &str),
    created_at: &str,
) -> String {
    let ns = version.number();
    // Unique per export (ISO 20022): readers drop a repeated message id.
    let msg_id = format!("GC{}", uuid::Uuid::new_v4().simple());
    let iban = normalize_iban(&account.iban);
    let own_bic = if has_iban_shape(&iban) {
        crate::connectors::fints_directory::bic_for_iban(iban.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    let bic_tag = version.bic_tag();

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out += &format!("<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.{ns}\">\n");
    out += "<BkToCstmrStmt>\n";
    out += &format!(
        "<GrpHdr><MsgId>{msg_id}</MsgId><CreDtTm>{}</CreDtTm><MsgPgntn><PgNb>1</PgNb><LastPgInd>true</LastPgInd></MsgPgntn></GrpHdr>\n",
        esc(created_at)
    );
    out += "<Stmt>\n";
    out += &format!(
        "<Id>{msg_id}</Id><ElctrncSeqNb>1</ElctrncSeqNb><CreDtTm>{}</CreDtTm>",
        esc(created_at)
    );
    // The days OPBD/CLBD are dated for; `FrDtTm`/`ToDtTm` need a time part.
    if opening.1.len() == 10 && closing.1.len() == 10 {
        out += &format!(
            "<FrToDt><FrDtTm>{}T00:00:00</FrDtTm><ToDtTm>{}T23:59:59</ToDtTm></FrToDt>",
            esc(opening.1),
            esc(closing.1)
        );
    }
    out += "\n";
    // Acct: Id, Ccy, Nm, Ownr, Svcr – schema order.
    let fallback = format!("GC-{}", account.id);
    out += &format!(
        "<Acct><Id>{}</Id><Ccy>{currency}</Ccy>",
        account_id_xml(&iban, &account.account_number, &fallback)
    );
    if !account.name.trim().is_empty() {
        out += &format!("<Nm>{}</Nm>", esc(&truncate(account.name.trim(), 70)));
    }
    let owner = account.owner.trim();
    if !owner.is_empty() {
        out += &format!("<Ownr><Nm>{}</Nm></Ownr>", esc(&truncate(owner, 140)));
    }
    if !own_bic.is_empty() {
        out += &format!(
            "<Svcr><FinInstnId><{bic_tag}>{}</{bic_tag}></FinInstnId></Svcr>",
            esc(&own_bic)
        );
    }
    out += "</Acct>\n";
    out += &balance_xml("OPBD", opening.0, currency, opening.1);
    out += &balance_xml("CLBD", closing.0, currency, closing.1);
    for e in entries {
        out += &entry_xml(e, currency, owner, version);
    }
    out += "</Stmt>\n</BkToCstmrStmt>\n</Document>\n";
    out
}

fn balance_xml(code: &str, cents: i64, currency: &str, date: &str) -> String {
    let ind = if cents < 0 { "DBIT" } else { "CRDT" };
    format!(
        "<Bal><Tp><CdOrPrtry><Cd>{code}</Cd></CdOrPrtry></Tp><Amt Ccy=\"{}\">{}</Amt><CdtDbtInd>{ind}</CdtDbtInd><Dt><Dt>{}</Dt></Dt></Bal>\n",
        esc(currency),
        xml_amount(cents),
        esc(date)
    )
}

/// A party of `RltdPties` (`Dbtr` / `Cdtr`): name and, for a creditor, the
/// SEPA creditor identifier (`Id/PrvtId/Othr`, scheme `SEPA`).
fn party_xml(tag: &str, name: &str, creditor_id: &str, version: CamtVersion) -> String {
    let mut p = String::new();
    if !name.is_empty() {
        p += &format!("<Nm>{}</Nm>", esc(&truncate(name, 140)));
    }
    if !creditor_id.is_empty() {
        p += &format!(
            "<Id><PrvtId><Othr><Id>{}</Id><SchmeNm><Prtry>SEPA</Prtry></SchmeNm></Othr></PrvtId></Id>",
            esc(&truncate(creditor_id, 35))
        );
    }
    if p.is_empty() {
        return p;
    }
    match version {
        CamtVersion::V02 => format!("<{tag}>{p}</{tag}>"),
        CamtVersion::V08 => format!("<{tag}><Pty>{p}</Pty></{tag}>"),
    }
}

/// ISO bank transaction code (`Domn/Fmly/SubFmlyCd`) and the German DK code
/// for a GVC; `SALA` purpose codes are salary payments.
fn bk_tx_code(
    gvc: &str,
    debit: bool,
    purpose_code: &str,
) -> (&'static str, &'static str, &'static str) {
    if purpose_code.eq_ignore_ascii_case("SALA") {
        return ("NTRF", if debit { "IRCT" } else { "ICDT" }, "SALA");
    }
    match gvc {
        "004" | "005" | "104" | "105" | "171" | "835" => ("NDDT", "IDDT", "ESDD"),
        "009" | "109" => ("NDDT", "IDDT", "UPDD"),
        "006" | "106" | "108" => ("NCAR", "CCRD", "POSD"),
        "083" | "183" => ("NCAR", "CCRD", "CWDL"),
        "008" | "152" => ("NSTO", if debit { "IRCT" } else { "ICDT" }, "STDO"),
        "805" | "806" | "809" => ("NCHG", "MDOP", "CHRG"),
        "808" | "814" => ("NINT", "MDOP", "INTR"),
        "201" | "202" | "205" | "226" => ("NSEC", "TRAD", "SECU"),
        "020" | "051" | "052" | "053" | "116" | "119" | "151" | "153" | "159" | "166" | "169" => {
            ("NTRF", if debit { "IRCT" } else { "ICDT" }, "ESCT")
        }
        _ if gvc.len() == 3 && gvc.bytes().all(|c| c.is_ascii_digit()) => {
            ("NTRF", if debit { "IRCT" } else { "ICDT" }, "OTHR")
        }
        _ => ("NMSC", if debit { "IRCT" } else { "ICDT" }, "OTHR"),
    }
}

/// The purpose without a SEPA envelope (`EREF+…`, `MREF+…`, `SVWZ+`) when the
/// bank delivered the references inside it.
fn raw_remittance_purpose<'a>(purpose: &'a str, e2e: &str, mndt: &str) -> &'a str {
    let mut rest = purpose.trim();
    if !e2e.is_empty() {
        let prefix = format!("EREF+{e2e}");
        if let Some(after) = rest.strip_prefix(&prefix) {
            rest = after.trim_start();
        }
    }
    if !mndt.is_empty() {
        let prefix = format!("MREF+{mndt}");
        if let Some(after) = rest.strip_prefix(&prefix) {
            rest = after.trim_start();
        }
    }
    if let Some(after) = rest.strip_prefix("SVWZ+") {
        return after.trim();
    }
    purpose
}

/// SEPA return reason (`RtrInf`) from the GVC or the bank's text, when the
/// booking is a return.
fn detect_return_reason(gvc: &str, purpose: &str, booking_text: &str) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "AC01", "AC04", "AC06", "AG01", "AG02", "AM04", "AM05", "BE05", "CNOR", "DNOR", "FF01",
        "FOCR", "MD01", "MD02", "MD06", "MD07", "MS02", "MS03", "NARR", "RC01", "RR01", "RR02",
        "RR03", "RR04", "SL01", "TM01",
    ];
    let text = booking_text.to_lowercase();
    let is_return_context = matches!(gvc, "009" | "109" | "159")
        || text.contains("rücklastschrift")
        || text.contains("ruecklastschrift")
        || purpose.contains("RTRN+")
        || purpose.contains("RTCD+");
    for token in purpose
        .split(|c: char| !c.is_ascii_alphanumeric())
        .chain(booking_text.split(|c: char| !c.is_ascii_alphanumeric()))
    {
        if token.len() == 4 {
            let upper = token.to_ascii_uppercase();
            if let Some(&found) = CODES.iter().find(|&&c| c == upper)
                && (is_return_context
                    || matches!(found, "AC04" | "MD01" | "MD06" | "MS03" | "AM04"))
            {
                return Some(found);
            }
        }
    }
    if matches!(gvc, "009" | "109") {
        return Some("NARR");
    }
    None
}

fn entry_xml(b: &CamtEntry, account_currency: &str, owner: &str, version: CamtVersion) -> String {
    let debit = b.amount_cents < 0;
    let ind = if debit { "DBIT" } else { "CRDT" };
    let status = if b.pending { "PDNG" } else { "BOOK" };
    let currency = account_currency_or(&b.currency, account_currency);
    let name = b.counterparty.trim();
    let e2e = b.end_to_end_id.trim();
    let mandate = b.mandate_id.trim();
    let creditor_id = b.creditor_id.trim();
    let gvc = b.gvc.trim();
    let purpose_code = b.purpose_code.trim();
    let purpose = raw_remittance_purpose(&b.purpose, e2e, mandate);
    // The bank's own reference; `external_id` is `camt:<ref>` for CAMT syncs.
    let svcr_ref = b
        .external_id
        .strip_prefix("camt:")
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "NONREF");
    let peer_iban = normalize_iban(&b.counterparty_iban);
    let peer_bic = if has_iban_shape(&peer_iban) {
        crate::connectors::fints_directory::bic_for_iban(peer_iban.clone())
    } else {
        None
    };
    let bic_tag = version.bic_tag();

    let mut e = String::from("<Ntry>");
    e += &format!(
        "<Amt Ccy=\"{currency}\">{}</Amt><CdtDbtInd>{ind}</CdtDbtInd>",
        xml_amount(b.amount_cents)
    );
    // .08: Sts is a choice (ExternalEntryStatus1Code), .02: a plain code.
    e += &match version {
        CamtVersion::V02 => format!("<Sts>{status}</Sts>"),
        CamtVersion::V08 => format!("<Sts><Cd>{status}</Cd></Sts>"),
    };
    e += &format!("<BookgDt><Dt>{}</Dt></BookgDt>", esc(&b.date));
    if !b.value_date.is_empty() {
        e += &format!("<ValDt><Dt>{}</Dt></ValDt>", esc(&b.value_date));
    }
    if let Some(reference) = svcr_ref {
        e += &format!(
            "<AcctSvcrRef>{}</AcctSvcrRef>",
            esc(&truncate(reference, 35))
        );
    }
    // BkTxCd is mandatory in both versions: the ISO domain/family for the GVC
    // plus, for a numeric GVC, the German DK form `NTRF+<GVC>+000` that
    // importers read. Anything else is classified by the ISO code alone (no
    // proprietary code without an issuer).
    let (swift_code, family, sub_family) = bk_tx_code(gvc, debit, purpose_code);
    let domain = format!(
        "<Domn><Cd>PMNT</Cd><Fmly><Cd>{family}</Cd><SubFmlyCd>{sub_family}</SubFmlyCd></Fmly></Domn>"
    );
    e += &if gvc.len() == 3 && gvc.bytes().all(|c| c.is_ascii_digit()) {
        format!(
            "<BkTxCd>{domain}<Prtry><Cd>{swift_code}+{gvc}+000</Cd><Issr>DK</Issr></Prtry></BkTxCd>"
        )
    } else {
        format!("<BkTxCd>{domain}</BkTxCd>")
    };
    e += "<NtryDtls><TxDtls>";
    // TxDtls order: Refs, RltdPties, RltdAgts, Purp, RmtInf, RtrInf.
    if svcr_ref.is_some() || !e2e.is_empty() || !mandate.is_empty() {
        e += "<Refs>";
        if let Some(reference) = svcr_ref {
            e += &format!(
                "<AcctSvcrRef>{}</AcctSvcrRef>",
                esc(&truncate(reference, 35))
            );
        }
        if !e2e.is_empty() {
            e += &format!("<EndToEndId>{}</EndToEndId>", esc(&truncate(e2e, 35)));
        }
        if !mandate.is_empty() {
            e += &format!("<MndtId>{}</MndtId>", esc(&truncate(mandate, 35)));
        }
        e += "</Refs>";
    }
    // Roles of the original payment: the peer is the creditor of a debit and
    // the debtor of a credit. A return (GVC 009/109/159 or a return reason)
    // turns this around: a returned direct debit is a credit, yet the peer stays
    // the creditor; a direct debit the owner collected (a credit with the
    // owner's creditor id) comes back as a debit, yet the peer stays the debtor.
    // The creditor id belongs to the creditor.
    let return_reason = detect_return_reason(gvc, &b.purpose, &b.booking_text);
    let is_return = matches!(gvc, "009" | "109" | "159") || return_reason.is_some();
    let peer_is_creditor = debit != is_return;
    let (debtor, creditor) = if peer_is_creditor {
        (
            party_xml("Dbtr", owner, "", version),
            party_xml("Cdtr", name, creditor_id, version),
        )
    } else {
        (
            party_xml("Dbtr", name, "", version),
            party_xml("Cdtr", owner, creditor_id, version),
        )
    };
    let peer_acct = if peer_iban.is_empty() {
        String::new()
    } else {
        let acct = if peer_is_creditor {
            "CdtrAcct"
        } else {
            "DbtrAcct"
        };
        format!(
            "<{acct}><Id>{}</Id></{acct}>",
            account_id_xml(&peer_iban, &peer_iban, "")
        )
    };
    // RltdPties order: Dbtr, DbtrAcct, Cdtr, CdtrAcct.
    let parties = if peer_is_creditor {
        format!("{debtor}{creditor}{peer_acct}")
    } else {
        format!("{debtor}{peer_acct}{creditor}")
    };
    if !parties.is_empty() {
        e += &format!("<RltdPties>{parties}</RltdPties>");
    }
    if let Some(bic) = peer_bic.filter(|b| !b.is_empty()) {
        let agent = if peer_is_creditor {
            "CdtrAgt"
        } else {
            "DbtrAgt"
        };
        e += &format!(
            "<RltdAgts><{agent}><FinInstnId><{bic_tag}>{}</{bic_tag}></FinInstnId></{agent}></RltdAgts>",
            esc(&bic)
        );
    }
    // ExternalPurpose1Code: 1–4 characters.
    if (1..=4).contains(&purpose_code.chars().count()) {
        e += &format!("<Purp><Cd>{}</Cd></Purp>", esc(purpose_code));
    }
    let lines = ustrd_lines(purpose);
    if !lines.is_empty() {
        e += "<RmtInf>";
        for l in lines {
            e += &format!("<Ustrd>{}</Ustrd>", esc(&l));
        }
        e += "</RmtInf>";
    }
    if let Some(reason) = return_reason {
        e += &format!("<RtrInf><Rsn><Cd>{reason}</Cd></Rsn></RtrInf>");
    }
    e += "</TxDtls></NtryDtls>";
    let text = b.booking_text.trim();
    if !text.is_empty() {
        e += &format!("<AddtlNtryInf>{}</AddtlNtryInf>", esc(&truncate(text, 500)));
    }
    e += "</Ntry>\n";
    e
}

/// An entry's ISO 4217 code; the account's where none (or no valid one) is stored.
fn account_currency_or(s: &str, account_currency: &str) -> String {
    let c = s.trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) {
        c
    } else {
        account_currency.to_string()
    }
}

/// `<IBAN>` when `iban` is one, else `<Othr><Id>` (Max34Text): the schema's
/// IBAN pattern rejects account and card numbers.
fn account_id_xml(iban: &str, other: &str, fallback: &str) -> String {
    if has_iban_shape(iban) {
        return format!("<IBAN>{}</IBAN>", esc(iban));
    }
    let other = other.trim();
    let id = if other.is_empty() { fallback } else { other };
    format!("<Othr><Id>{}</Id></Othr>", esc(&truncate(id, 34)))
}

/// Remittance text as `Ustrd` lines of at most 140 characters (Max140Text).
fn ustrd_lines(text: &str) -> Vec<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    chars
        .chunks(140)
        .map(|c| c.iter().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn xml_amount(cents: i64) -> String {
    let a = cents.unsigned_abs();
    format!("{}.{:02}", a / 100, a % 100)
}

/// Escapes XML text and attribute values and drops what XML 1.0 forbids
/// (control characters below U+0020 except tab, LF, CR; U+FFFE, U+FFFF).
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
