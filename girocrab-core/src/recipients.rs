//! Known recipients of a transfer (#881, #978): who the outgoing bookings went to, one entry per
//! IBAN under its most used name, the most recently used first.
//!
//! There is no stored recipient list: every entry comes from the bookings. "Nicht mehr
//! vorschlagen" therefore hides an IBAN (`hidden_recipients`) instead of deleting anything; its
//! bookings stay. Showing it again keeps the row with `hidden = 0`, so the vault merge carries
//! that change to the other devices too (newest change wins).
//!
//! The list is read once per state of the data (`vault::cache`): the form asks on every
//! keystroke, and the read goes over every outgoing booking.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{Connection, params};

use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::iban::{normalize_iban, validate_iban};
use crate::models::KnownRecipient;
use crate::vault::Vault;

#[cfg(test)]
mod tests;

/// Step 33 (#978). `hidden_recipients`: IBANs left out of the suggestions; `hidden = 0` is an
/// IBAN shown again, kept for the merge. `tx_recipients` covers the read of the known recipients
/// (outgoing live bookings with an IBAN, in group order), so it reads no booking rows; SQLite
/// counts the index as covering only with the columns of its own WHERE in it.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hidden_recipients (
    iban       TEXT    PRIMARY KEY,      -- normalised (upper case, no spaces)
    hidden     INTEGER NOT NULL,         -- 1 = not suggested, 0 = shown again
    changed_ms INTEGER NOT NULL          -- newest change wins in a vault merge
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS tx_recipients
    ON transactions(counterparty_iban, counterparty, booking_date, amount_cents, deleted_ms)
    WHERE amount_cents < 0 AND deleted_ms IS NULL AND counterparty_iban <> '';
";

/// One IBAN with every name it was paid under.
struct Entry {
    recipient: KnownRecipient,
    /// Every name and the IBAN, lower case without spaces, for the search.
    keys: Vec<String>,
}

/// Every known recipient of one state of the data, in suggestion order.
pub(crate) struct Recipients {
    entries: Vec<Entry>,
}

fn search_key(s: &str) -> String {
    s.replace(' ', "").to_lowercase()
}

/// Folds the (IBAN, name) groups into one entry per IBAN: the most used name (then the most
/// recent), use counts summed, the latest booking date. Invalid IBANs are left out.
fn build(rows: Vec<(String, String, u32, String)>, hidden: &HashSet<String>) -> Recipients {
    // per IBAN: name -> (uses, last date); a name booked under several spellings of the IBAN
    // ("DE89…" and "DE89 3704 …") counts once, with its uses summed
    let mut by_iban: HashMap<String, HashMap<String, (u32, String)>> = HashMap::new();
    for (iban, name, n, last) in rows {
        let iban = normalize_iban(&iban);
        if validate_iban(&iban) {
            let slot = by_iban.entry(iban).or_default().entry(name).or_default();
            slot.0 += n;
            if last > slot.1 {
                slot.1 = last;
            }
        }
    }
    let mut entries: Vec<Entry> = by_iban
        .into_iter()
        .map(|(iban, names)| {
            let mut names: Vec<(String, u32, String)> = names
                .into_iter()
                .map(|(name, (n, last))| (name, n, last))
                .collect();
            names.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
            let times_used = names.iter().map(|n| n.1).sum();
            let last_used = names.iter().map(|n| n.2.clone()).max().unwrap_or_default();
            let mut keys: Vec<String> = names.iter().map(|n| search_key(&n.0)).collect();
            keys.push(iban.to_lowercase());
            Entry {
                keys,
                recipient: KnownRecipient {
                    name: names.swap_remove(0).0,
                    hidden: hidden.contains(&iban),
                    iban,
                    times_used,
                    last_used,
                },
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        let (a, b) = (&a.recipient, &b.recipient);
        b.last_used
            .cmp(&a.last_used)
            .then(b.times_used.cmp(&a.times_used))
            .then(a.iban.cmp(&b.iban))
    });
    Recipients { entries }
}

/// Uses per (IBAN, name) of the live outgoing bookings. The WHERE repeats the one of
/// `tx_recipients`, so the read stays on that index.
const READ: &str = "SELECT counterparty_iban, counterparty, count(*), MAX(booking_date)
    FROM transactions
    WHERE amount_cents < 0 AND deleted_ms IS NULL AND counterparty_iban <> '' AND counterparty <> ''
    GROUP BY counterparty_iban, counterparty";

/// The rows [`build`] folds, and the hidden IBANs.
type Read = (Vec<(String, String, u32, String)>, HashSet<String>);

fn read(conn: &Connection) -> CoreResult<Read> {
    let rows = conn
        .prepare(READ)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let hidden = conn
        .prepare("SELECT iban FROM hidden_recipients WHERE hidden = 1")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<HashSet<String>, _>>()?;
    Ok((rows, hidden))
}

/// Vault merge (iCloud peer snapshot): per IBAN the newest change wins, hiding wins an equal
/// stamp. Rows that would not change are left alone, so a repeated merge changes nothing.
pub(crate) fn merge_attached(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(
        "INSERT INTO hidden_recipients(iban, hidden, changed_ms)
         SELECT iban, hidden, changed_ms FROM other.hidden_recipients WHERE true
         ON CONFLICT(iban) DO UPDATE SET hidden = excluded.hidden, changed_ms = excluded.changed_ms
         WHERE excluded.changed_ms > hidden_recipients.changed_ms
            OR (excluded.changed_ms = hidden_recipients.changed_ms
                AND excluded.hidden > hidden_recipients.hidden);",
    )?;
    Ok(())
}

impl Vault {
    /// The known recipients for the current data: the cached list, or read again after a
    /// change (only the SQL part under the vault lock).
    fn known_recipients(&self) -> CoreResult<Arc<Recipients>> {
        let db = self.db()?;
        let stamp = self.data_stamp(&db);
        if let Some(r) = self.recipients.get(stamp) {
            return Ok(r);
        }
        let (rows, hidden) = read(&db)?;
        // inside an open transaction a rollback would not lower the count
        let keep = db.is_autocommit();
        drop(db);
        let recipients = Arc::new(build(rows, &hidden));
        if keep {
            self.recipients.put(stamp, recipients.clone());
        }
        Ok(recipients)
    }

    fn matching_recipients(
        &self,
        query: &str,
        include_hidden: bool,
        limit: usize,
    ) -> CoreResult<Vec<KnownRecipient>> {
        let q = search_key(query.trim());
        Ok(self
            .known_recipients()?
            .entries
            .iter()
            .filter(|e| include_hidden || !e.recipient.hidden)
            .filter(|e| q.is_empty() || e.keys.iter().any(|k| k.contains(&q)))
            .take(limit)
            .map(|e| e.recipient.clone())
            .collect())
    }
}

#[uniffi::export]
impl Vault {
    /// Known recipients (name + IBAN) from past outgoing bookings for the transfer form: one per
    /// IBAN under its most used name, the most recently used first, hidden ones left out.
    /// `query` filters by name (any spelling) or IBAN substring (empty = all).
    pub fn suggest_recipients(
        &self,
        query: String,
        limit: u32,
    ) -> Result<Vec<KnownRecipient>, CoreError> {
        self.matching_recipients(&query, false, limit as usize)
    }

    /// Every known recipient, hidden ones included ("Empfänger verwalten"), in the order of
    /// [`suggest_recipients`](Self::suggest_recipients) and filtered the same way.
    pub fn list_known_recipients(&self, query: String) -> Result<Vec<KnownRecipient>, CoreError> {
        self.matching_recipients(&query, true, usize::MAX)
    }

    /// "Nicht mehr vorschlagen" (`hidden`) or show again. The bookings stay. A new stamp is
    /// above the stored one even if this device's clock is behind the one that wrote it.
    pub fn set_recipient_hidden(&self, iban: String, hidden: bool) -> Result<(), CoreError> {
        let iban = normalize_iban(&iban);
        if !validate_iban(&iban) {
            return Err(CoreError::invalid(tr("Die Empfänger-IBAN ist ungültig.")));
        }
        let db = self.db()?;
        if hidden {
            db.execute(
                "INSERT INTO hidden_recipients(iban, hidden, changed_ms) VALUES (?1, 1, ?2)
                 ON CONFLICT(iban) DO UPDATE SET hidden = 1,
                    changed_ms = MAX(hidden_recipients.changed_ms + 1, excluded.changed_ms)
                 WHERE hidden_recipients.hidden = 0",
                params![iban, now_ms()],
            )?;
        } else {
            db.execute(
                "UPDATE hidden_recipients SET hidden = 0, changed_ms = MAX(changed_ms + 1, ?2)
                 WHERE iban = ?1 AND hidden = 1",
                params![iban, now_ms()],
            )?;
        }
        Ok(())
    }
}
