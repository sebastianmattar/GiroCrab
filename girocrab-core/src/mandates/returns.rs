//! Bank bookings that belong to a mandate: collected amounts and returns
//! (Rücklastschriften). A returned collection flags the mandate "prüfen" until
//! the user has looked at it; nothing is switched off automatically.
//!
//! A booking belongs to a mandate when its mandate reference matches, when its
//! EndToEndId starts with the reference (that is how `build_collection_files`
//! names them), or when it comes from the debtor's IBAN and the purpose
//! names the reference. On the creditor account a negative amount under a
//! mandate is a return.

use rusqlite::{Connection, params};

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::schema::has_column;
use crate::vault::Vault;

/// Step 22: date up to which the returns of a mandate were acknowledged.
pub(crate) fn mandate_return_ack(conn: &Connection) -> CoreResult<()> {
    if !has_column(conn, "main", "mandates", "returns_ack")? {
        conn.execute_batch(
            "ALTER TABLE mandates ADD COLUMN returns_ack TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

/// A booking under a mandate.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MandateBooking {
    pub booking_id: i64,
    pub mandate_id: i64,
    /// `YYYY-MM-DD`
    pub date: String,
    /// Positive = collected, negative = returned.
    pub amount_cents: i64,
    pub purpose: String,
    pub is_return: bool,
    /// ISO return reason code (`AC04`, …) or "" when the bank gave none.
    pub reason_code: String,
    /// Plain-language reason, "" when unknown.
    pub reason: String,
    /// The user has marked this return as seen.
    pub acknowledged: bool,
}

/// Plain-language text of the common SEPA return reason codes.
pub fn return_reason(code: &str) -> Option<String> {
    let text = match code {
        "AC01" => tr("IBAN ungültig"),
        "AC04" => tr("Konto aufgelöst"),
        "AC06" => tr("Konto gesperrt"),
        "AG01" => tr("Lastschrift auf diesem Konto nicht zulässig"),
        "AG02" => tr("Ungültige Transaktionsart"),
        "AM04" => tr("Konto nicht gedeckt"),
        "AM05" => tr("Doppelte Lastschrift"),
        "BE05" => tr("Gläubiger-ID unbekannt"),
        "MD01" => tr("Kein gültiges Mandat"),
        "MD06" => tr("Zahlungspflichtiger widerspricht"),
        "MD07" => tr("Zahlungspflichtiger verstorben"),
        "MS02" => tr("Abgelehnt durch den Zahlungspflichtigen"),
        "MS03" => tr("Grund nicht angegeben"),
        "RC01" => tr("BIC fehlerhaft"),
        "SL01" => tr("Spezielle Bankvereinbarung"),
        _ => return None,
    };
    Some(text.to_string())
}

/// The first known return code among the words of `text`.
pub(crate) fn find_reason_code(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| w.len() == 4 && return_reason(w).is_some())
        .map(str::to_string)
}

/// Bookings of the account that belong to one of `mandates` (all of that account),
/// found in one pass over its bookings: per-mandate queries would cost
/// (bookings × mandates) on a club account with thousands of members.
/// `only_returns` limits the scan to negative amounts. `only` restricts the result
/// to one mandate id.
fn scan(
    db: &Connection,
    account_id: i64,
    mandates: &[super::Mandate],
    only_returns: bool,
    only: Option<i64>,
) -> CoreResult<Vec<MandateBooking>> {
    use std::collections::HashMap;
    let Some(since) = mandates
        .iter()
        .map(|m| m.signed_date.as_str())
        .filter(|d| !d.is_empty())
        .min()
    else {
        return Ok(Vec::new());
    };
    let ack: HashMap<i64, String> = {
        let mut q = db.prepare("SELECT id, returns_ack FROM mandates WHERE account_id = ?1")?;
        let rows = q.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    // the EndToEndId of a collection is "<reference cut to 26>-<YYYYMMDD>"
    let key = |reference: &str| reference.chars().take(26).collect::<String>();
    let mut by_ref: HashMap<&str, usize> = HashMap::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut by_iban: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, m) in mandates.iter().enumerate() {
        by_ref.entry(m.reference.as_str()).or_insert(i);
        by_key.entry(key(&m.reference)).or_insert(i);
        by_iban.entry(m.debtor_iban.as_str()).or_default().push(i);
    }
    let mut stmt = db.prepare(
        "SELECT id, booking_date, amount_cents, purpose, booking_text, mandate_id, end_to_end_id,
                replace(counterparty_iban, ' ', '')
         FROM transactions
         WHERE account_id = ?1 AND deleted_ms IS NULL AND booking_date >= ?2
           AND (?3 = 0 OR amount_cents < 0)
         ORDER BY booking_date DESC, id DESC",
    )?;
    let mut rows = stmt.query(params![account_id, since, only_returns])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let (id, date, cents): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let (purpose, text, mandate_ref, e2e, iban): (String, String, String, String, String) =
            (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?);
        let idx = by_ref
            .get(mandate_ref.as_str())
            .filter(|_| !mandate_ref.is_empty())
            .copied()
            .or_else(|| {
                e2e.rsplit_once('-')
                    .and_then(|(left, _)| by_key.get(left))
                    .copied()
            })
            .or_else(|| {
                by_iban.get(iban.as_str()).and_then(|c| {
                    c.iter()
                        .copied()
                        .find(|&i| purpose.contains(mandates[i].reference.as_str()))
                })
            });
        let Some(i) = idx else { continue };
        let m = &mandates[i];
        if only.is_some_and(|o| o != m.id) {
            continue;
        }
        let is_return = cents < 0;
        let code = if is_return {
            find_reason_code(&purpose)
                .or_else(|| find_reason_code(&text))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let acked = ack.get(&m.id).is_some_and(|a| !a.is_empty() && date <= *a);
        out.push(MandateBooking {
            booking_id: id,
            mandate_id: m.id,
            acknowledged: is_return && acked,
            reason: return_reason(&code).unwrap_or_default(),
            reason_code: code,
            date,
            amount_cents: cents,
            purpose,
            is_return,
        });
    }
    Ok(out)
}

#[uniffi::export]
impl Vault {
    /// Bookings under `mandate_id`, newest first.
    pub fn mandate_bookings(&self, mandate_id: i64) -> Result<Vec<MandateBooking>, CoreError> {
        let m = self
            .mandate(mandate_id)?
            .ok_or_else(|| CoreError::invalid(tr("Das Mandat gibt es nicht mehr.")))?;
        // all mandates of the account take part, so a booking is never claimed by two
        let all = self.list_mandates(Some(m.account_id))?;
        let db = self.db()?;
        scan(&db, m.account_id, &all, false, Some(mandate_id))
    }

    /// Returns not yet acknowledged (`account_id` = one creditor account,
    /// `None` = all), newest first.
    pub fn open_mandate_returns(
        &self,
        account_id: Option<i64>,
    ) -> Result<Vec<MandateBooking>, CoreError> {
        let mandates = self.list_mandates(account_id)?;
        let mut accounts: Vec<i64> = mandates.iter().map(|m| m.account_id).collect();
        accounts.sort_unstable();
        accounts.dedup();
        let db = self.db()?;
        let mut out = Vec::new();
        for acc in accounts {
            let of_account: Vec<_> = mandates
                .iter()
                .filter(|m| m.account_id == acc)
                .cloned()
                .collect();
            out.extend(
                scan(&db, acc, &of_account, true, None)?
                    .into_iter()
                    .filter(|b| !b.acknowledged),
            );
        }
        out.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(out)
    }

    /// Marks every return booked so far under the mandate as seen.
    pub fn acknowledge_mandate_returns(&self, mandate_id: i64) -> Result<(), CoreError> {
        let latest = self
            .mandate_bookings(mandate_id)?
            .into_iter()
            .filter(|b| b.is_return)
            .map(|b| b.date)
            .max();
        if let Some(d) = latest {
            let db = self.db()?;
            db.execute(
                "UPDATE mandates SET returns_ack = ?2 WHERE id = ?1",
                params![mandate_id, d],
            )?;
        }
        Ok(())
    }
}

/// Vault merge: the later "seen up to" date of both sides stays, so a return
/// acknowledged on one device is not shown again on the other. Runs after
/// [`super::merge_attached`], which has already matched the mandates.
pub(crate) fn merge_ack(
    conn: &Connection,
    acc_map: &std::collections::HashMap<i64, i64>,
) -> CoreResult<()> {
    let rows: Vec<(i64, String, String)> = {
        let mut q = conn.prepare(
            "SELECT account_id, reference, returns_ack FROM other.mandates WHERE returns_ack <> ''",
        )?;
        let it = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        it.collect::<Result<_, _>>()?
    };
    for (acc, reference, ack) in rows {
        let Some(&local) = acc_map.get(&acc) else {
            continue;
        };
        conn.execute(
            "UPDATE mandates SET returns_ack = ?3
             WHERE account_id = ?1 AND reference = ?2 AND returns_ack < ?3",
            params![local, reference, ack],
        )?;
    }
    Ok(())
}
