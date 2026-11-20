//! Vault merge for mandates and creditor IDs (iCloud peer snapshot, #143).
//!
//! A mandate is identified by creditor account + reference. The newest edit
//! (`updated_ms`) of a mandate wins as a whole; the due date of the last
//! collection is the later of both sides, so a collection made on one device
//! is never taken back (a collection does not touch `updated_ms`, so it never
//! overrides someone else's edit). A deleted mandate leaves a mark in `mandate_deletions`
//! and stays deleted unless it was edited again after that. Creditor IDs: the
//! newest value wins (a removed ID is an empty value with a stamp).

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

use crate::error::CoreResult;
use crate::schema::has_table;

/// Remembers that the mandate was deleted (the later stamp stays).
pub(super) fn mark_deleted(
    db: &Connection,
    account_id: i64,
    reference: &str,
    now_ms: i64,
) -> CoreResult<()> {
    db.execute(
        "INSERT INTO mandate_deletions(account_id, reference, deleted_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, reference) DO UPDATE SET deleted_ms = max(deleted_ms, ?3)",
        params![account_id, reference, now_ms],
    )?;
    Ok(())
}

/// A mandate saved again under this key is no longer a deleted one.
pub(super) fn unmark_deleted(db: &Connection, account_id: i64, reference: &str) -> CoreResult<()> {
    db.execute(
        "DELETE FROM mandate_deletions WHERE account_id = ?1 AND reference = ?2",
        params![account_id, reference],
    )?;
    Ok(())
}

struct Row {
    account_id: i64,
    debtor_name: String,
    debtor_iban: String,
    debtor_bic: String,
    reference: String,
    signed_date: String,
    scheme: String,
    one_off: i64,
    default_amount_cents: i64,
    default_purpose: String,
    note: String,
    active: i64,
    last_collected: Option<String>,
    created_ms: i64,
    updated_ms: i64,
}

/// The later of two optional ISO dates.
fn later(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Vault merge: takes over mandates, deletion marks and creditor IDs of the
/// attached `other` vault. `acc_map` maps its account ids to ours.
pub(crate) fn merge_attached(conn: &Connection, acc_map: &HashMap<i64, i64>) -> CoreResult<()> {
    if !has_table(conn, "other", "mandates")? {
        return Ok(());
    }
    let rows: Vec<Row> = {
        let mut q = conn.prepare(
            "SELECT account_id, debtor_name, debtor_iban, debtor_bic, reference, signed_date,
                    scheme, one_off, default_amount_cents, default_purpose, note, active,
                    last_collected, created_ms, updated_ms
             FROM other.mandates",
        )?;
        let it = q.query_map([], |r| {
            Ok(Row {
                account_id: r.get(0)?,
                debtor_name: r.get(1)?,
                debtor_iban: r.get(2)?,
                debtor_bic: r.get(3)?,
                reference: r.get(4)?,
                signed_date: r.get(5)?,
                scheme: r.get(6)?,
                one_off: r.get(7)?,
                default_amount_cents: r.get(8)?,
                default_purpose: r.get(9)?,
                note: r.get(10)?,
                active: r.get(11)?,
                last_collected: r.get(12)?,
                created_ms: r.get(13)?,
                updated_ms: r.get(14)?,
            })
        })?;
        it.collect::<Result<_, _>>()?
    };
    // deletion marks first: they remove local mandates the other side deleted later
    let marks: Vec<(i64, String, i64)> = {
        let mut q =
            conn.prepare("SELECT account_id, reference, deleted_ms FROM other.mandate_deletions")?;
        let it = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        it.collect::<Result<_, _>>()?
    };
    for (acc, reference, ms) in marks {
        let Some(&local) = acc_map.get(&acc) else {
            continue;
        };
        let updated: Option<i64> = conn
            .query_row(
                "SELECT updated_ms FROM mandates WHERE account_id = ?1 AND reference = ?2",
                params![local, reference],
                |r| r.get(0),
            )
            .optional()?;
        match updated {
            // edited after the other side deleted it: the edit wins
            Some(u) if u >= ms => {}
            Some(_) => {
                conn.execute(
                    "DELETE FROM mandates WHERE account_id = ?1 AND reference = ?2",
                    params![local, reference],
                )?;
                mark_deleted(conn, local, &reference, ms)?;
            }
            None => mark_deleted(conn, local, &reference, ms)?,
        }
    }
    for r in rows {
        let Some(&local) = acc_map.get(&r.account_id) else {
            continue;
        };
        let deleted_ms: Option<i64> = conn
            .query_row(
                "SELECT deleted_ms FROM mandate_deletions WHERE account_id = ?1 AND reference = ?2",
                params![local, r.reference],
                |r| r.get(0),
            )
            .optional()?;
        if deleted_ms.is_some_and(|d| d >= r.updated_ms) {
            continue; // deleted here after the other side's last edit
        }
        let mine: Option<(i64, i64, Option<String>)> = conn
            .query_row(
                "SELECT id, updated_ms, last_collected FROM mandates
                 WHERE account_id = ?1 AND reference = ?2",
                params![local, r.reference],
                |q| Ok((q.get(0)?, q.get(1)?, q.get(2)?)),
            )
            .optional()?;
        match mine {
            None => {
                conn.execute(
                    "INSERT INTO mandates(account_id, debtor_name, debtor_iban, debtor_bic,
                        reference, signed_date, scheme, one_off, default_amount_cents,
                        default_purpose, note, active, last_collected, created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        local,
                        r.debtor_name,
                        r.debtor_iban,
                        r.debtor_bic,
                        r.reference,
                        r.signed_date,
                        r.scheme,
                        r.one_off,
                        r.default_amount_cents,
                        r.default_purpose,
                        r.note,
                        r.active,
                        r.last_collected,
                        r.created_ms,
                        r.updated_ms
                    ],
                )?;
                unmark_deleted(conn, local, &r.reference)?;
            }
            Some((id, updated, last)) => {
                let last = later(last, r.last_collected.clone());
                if r.updated_ms > updated {
                    conn.execute(
                        "UPDATE mandates SET debtor_name=?2, debtor_iban=?3, debtor_bic=?4,
                            signed_date=?5, scheme=?6, one_off=?7, default_amount_cents=?8,
                            default_purpose=?9, note=?10, active=?11, last_collected=?12,
                            updated_ms=?13 WHERE id = ?1",
                        params![
                            id,
                            r.debtor_name,
                            r.debtor_iban,
                            r.debtor_bic,
                            r.signed_date,
                            r.scheme,
                            r.one_off,
                            r.default_amount_cents,
                            r.default_purpose,
                            r.note,
                            r.active,
                            last,
                            r.updated_ms
                        ],
                    )?;
                } else {
                    // ours is newer: only a later collection date is taken over
                    conn.execute(
                        "UPDATE mandates SET last_collected = ?2 WHERE id = ?1",
                        params![id, last],
                    )?;
                }
            }
        }
    }
    if has_table(conn, "other", "creditor_ids")? {
        let ids: Vec<(i64, String, i64)> = {
            let mut q =
                conn.prepare("SELECT account_id, creditor_id, updated_ms FROM other.creditor_ids")?;
            let it = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            it.collect::<Result<_, _>>()?
        };
        for (acc, id, ms) in ids {
            let Some(&local) = acc_map.get(&acc) else {
                continue;
            };
            conn.execute(
                "INSERT INTO creditor_ids(account_id, creditor_id, updated_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id) DO UPDATE SET creditor_id = ?2, updated_ms = ?3
                 WHERE ?3 > creditor_ids.updated_ms",
                params![local, id, ms],
            )?;
        }
    }
    Ok(())
}
