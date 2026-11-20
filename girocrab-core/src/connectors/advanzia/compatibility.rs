//! Retain account and booking identities from 9a3d7a6. No content-only deduplication.
use super::*;

pub(super) const LEGACY_HOLD_PREFIX: &str = "advanzia-hold-";
pub(crate) fn is_hold(id: &str) -> bool {
    id.starts_with(HOLD_PREFIX) || id.starts_with(LEGACY_HOLD_PREFIX)
}
pub(super) fn legacy_id(pending: bool, reference: &str) -> String {
    format!(
        "{}{:016x}",
        if pending {
            LEGACY_HOLD_PREFIX
        } else {
            "advanzia-post-"
        },
        crate::fnv::fnv1a64(reference)
    )
}
fn legacy_reference(reference: &str) -> bool {
    reference
        .strip_prefix("portal-advanzia-")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}
/// Only one active EUR credit card already bound to this exact login is eligible.
pub(super) fn legacy_account(db: &Connection, id: i64) -> CoreResult<Option<(i64, String)>> {
    let mut q = db.prepare("SELECT id,external_ref,kind,currency,archived_unix FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL")?;
    let rows = q
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match rows.as_slice() {
        [(account, reference, kind, currency, None)]
            if legacy_reference(reference) && kind == "credit_card" && currency == "EUR" =>
        {
            Some((*account, reference.clone()))
        }
        _ => None,
    })
}
/// Runs both before capture construction and again inside the storage transaction.
/// A legacy reference carries no bank UUID, so first adoption requires explicit choice.
pub(super) fn reference(
    db: &Connection,
    id: i64,
    state: &State,
    bank: &str,
    target: Option<i64>,
) -> CoreResult<(String, Option<i64>)> {
    if let Some((account, reference)) = legacy_account(db, id)? {
        if state.bank_account_id.is_none() && target != Some(account) {
            return Err(invalid(
                "Bitte das bestehende Advanzia-Konto einmalig für diesen Zugang bestätigen.",
            ));
        }
        if target.is_some_and(|t| t != account) {
            return Err(invalid(
                "Dieser Zugang ist bereits einer anderen Karte zugeordnet.",
            ));
        }
        return Ok((reference, Some(account)));
    }
    Ok((account_ref(bank), None))
}

/// Prefer an already stored identity, including tombstones. Legacy accounts keep
/// generating the old FNV IDs, so the archived version can still find these rows.
pub(super) fn prepare_ids(
    db: &Connection,
    connection: i64,
    data: &mut FetchResult,
    aliases: &HashMap<String, String>,
    legacy: bool,
) -> CoreResult<()> {
    let reference = &data.accounts[0].external_ref;
    let account: Option<i64> = db
        .query_row(
            "SELECT id FROM accounts WHERE connection_id=?1 AND external_ref=?2",
            params![connection, reference],
            |r| r.get(0),
        )
        .optional()?;
    let mut stored = HashMap::new();
    if let Some(account) = account {
        let mut q = db.prepare("SELECT external_id,booking_date,amount_cents,currency FROM transactions WHERE account_id=?1 AND (external_id LIKE 'advanzia-post-%' OR external_id LIKE 'advanzia-hold-%' OR external_id LIKE 'pending-hold-portal-advanzia-%')")?;
        let rows = q.query_map([account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ),
            ))
        })?;
        for row in rows {
            let (id, content) = row?;
            stored.insert(id, content);
        }
    }
    let mut chosen = HashMap::new();
    for t in &mut data.transactions {
        let old = aliases
            .get(&t.external_id)
            .ok_or_else(|| invalid("Ungültiger Sitzungszustand."))?;
        let old_row = stored.get(old);
        let new_row = stored.get(&t.external_id);
        if old_row.is_some() && new_row.is_some() {
            return Err(invalid(
                "Für einen Advanzia-Umsatz bestehen mehrere Kennungen. Es wurden keine Daten verändert.",
            ));
        }
        let row = old_row.or(new_row);
        if row.is_some_and(|(date, cents, currency)| {
            date != &t.booking_date || *cents != t.amount_cents || currency != &t.currency
        }) {
            return Err(invalid(
                "Eine bestehende Advanzia-Buchungskennung widerspricht den Bankdaten. Es wurden keine Daten verändert.",
            ));
        }
        let original = t.external_id.clone();
        if old_row.is_some() || (legacy && new_row.is_none()) {
            t.external_id = old.clone();
        }
        // FNV collisions in the same response must fail instead of dropping a booking.
        if let Some(previous) = chosen.insert(t.external_id.clone(), original.clone())
            && previous != original
        {
            return Err(invalid("Widersprüchliche Buchungsreferenz."));
        }
    }
    Ok(())
}
