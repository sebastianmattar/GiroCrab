//! Personal-finance planning: monthly category budgets, split bookings,
//! period cashflow comparison and SEPA transfer templates.

use std::collections::HashMap;

use rusqlite::params;

use crate::error::CoreError;
use crate::i18n::{role_name, tr, trf};
use crate::iban::normalize_iban;
use crate::models::*;
use crate::vault::Vault;

pub(crate) const PLANNING_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS transaction_splits (
    id             INTEGER PRIMARY KEY,
    transaction_id INTEGER NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    category_id    INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    amount_cents   INTEGER NOT NULL,
    note           TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS split_tx ON transaction_splits(transaction_id);
CREATE INDEX IF NOT EXISTS split_cat ON transaction_splits(category_id);
CREATE TABLE IF NOT EXISTS transfer_templates (
    id                   INTEGER PRIMARY KEY,
    name                 TEXT    NOT NULL,
    recipient_name       TEXT    NOT NULL,
    recipient_iban       TEXT    NOT NULL,
    recipient_bic        TEXT    NOT NULL DEFAULT '',
    default_amount_cents INTEGER,
    default_purpose      TEXT    NOT NULL DEFAULT ''
);";

/// Whether a booking in `currency` enters the sums (category totals, history, KPI tiles) of a
/// selection of `account_count` accounts (#1049, as in `balances::chosen` and #569): one account
/// counts in its own currency, several (or all) only EUR; amounts in other currencies are never
/// added to them. An empty currency is EUR.
pub(crate) fn counts_in_sums(account_count: usize, currency: &str) -> bool {
    account_count == 1 || currency.is_empty() || currency.eq_ignore_ascii_case("EUR")
}

/// SQL condition on `t` that is `counts_in_sums` for the whole selection.
pub(crate) fn sums_condition(account_count: usize) -> &'static str {
    if account_count == 1 {
        ""
    } else {
        " AND t.currency COLLATE NOCASE IN ('', 'EUR')"
    }
}

/// Whether any live booking is in a currency other than EUR: the cheap check that lets the
/// "Zusätzlich" queries return at once for the many vaults that have none.
pub(crate) fn has_other_currencies(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM transactions WHERE deleted_ms IS NULL AND upper(currency) NOT IN ('', 'EUR'))",
        [],
        |r| r.get(0),
    )
}

/// Bookings as (category_id, amount_cents, booking_date) rows where a split
/// booking contributes its split amounts instead of the parent category.
/// `extra`: more columns of `t` (e.g. ", t.currency AS currency"); `condition`:
/// more conditions on `t` (e.g. " AND t.account_id = ?1"). Both appear in both
/// halves of the UNION, so `condition` must use numbered parameters.
pub(crate) fn effective_sql(extra: &str, condition: &str) -> String {
    format!(
        "(
    SELECT t.category_id AS category_id, t.amount_cents AS amount_cents, t.booking_date AS booking_date{extra}
      FROM transactions t
     WHERE t.deleted_ms IS NULL{condition}
       AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)
    UNION ALL
    SELECT s.category_id, s.amount_cents, t.booking_date{extra}
      FROM transaction_splits s JOIN transactions t ON t.id = s.transaction_id
     WHERE t.deleted_ms IS NULL{condition}
)"
    )
}

fn month_range(year: i32, month: u32) -> Result<(String, String), CoreError> {
    if !(1..=12).contains(&month) {
        return Err(CoreError::invalid(tr("Ungültiger Monat.")));
    }
    Ok((
        format!("{year:04}-{month:02}-01"),
        format!("{year:04}-{month:02}-31"),
    ))
}

#[uniffi::export]
impl Vault {
    // ------------------------------------------------------------ budgets

    /// Sets (or clears with `None`) the monthly budget of a category.
    pub fn set_category_budget(
        &self,
        category_id: i64,
        budget_cents: Option<i64>,
    ) -> Result<(), CoreError> {
        if budget_cents.is_some_and(|b| b <= 0) {
            return Err(CoreError::invalid(tr("Das Budget muss größer als 0 sein.")));
        }
        let db = self.db()?;
        let n = db.execute(
            "UPDATE categories SET budget_cents = ?1, budget_ms = ?3 WHERE id = ?2",
            params![budget_cents, category_id, crate::dates::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Kategorie {0}", &[&category_id]),
            });
        }
        Ok(())
    }

    /// Spent vs. budget for every category with a budget in the given month.
    /// Spending includes sub-categories; refunds reduce it; split bookings
    /// count with their split amounts.
    pub fn list_category_budget_progress(
        &self,
        year: i32,
        month: u32,
    ) -> Result<Vec<CategoryBudgetStatus>, CoreError> {
        let (from, to) = month_range(year, month)?;
        let db = self.db()?;
        let mut own: HashMap<Option<i64>, i64> = HashMap::new();
        {
            let mut stmt = db.prepare(&format!(
                "SELECT category_id, SUM(amount_cents) FROM {}
                 WHERE booking_date >= ?1 AND booking_date <= ?2 GROUP BY category_id",
                // budgets are amounts in EUR: other currencies are never added to them (#1049)
                effective_sql("", sums_condition(0))
            ))?;
            for row in stmt.query_map(params![from, to], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
            })? {
                let (c, s) = row?;
                own.insert(c, s);
            }
        }
        let mut stmt = db.prepare(
            "SELECT id, parent_id, name, sf_symbol, color_hex, budget_cents, builtin_role FROM categories WHERE deleted_ms IS NULL ORDER BY sort_order, name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    role_name(
                        &r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(6)?.as_deref(),
                    ),
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut by_id = HashMap::new();
        for r in &rows {
            by_id.insert(r.0, r);
            if let Some(p) = r.1 {
                children.entry(p).or_default().push(r.0);
            }
        }
        let path_of = |id: i64| {
            let mut parts = Vec::new();
            let mut cur = Some(id);
            let mut guard = 0;
            while let Some(c) = cur {
                guard += 1;
                match by_id.get(&c) {
                    Some(r) if guard < 32 => {
                        parts.push(r.2.clone());
                        cur = r.1;
                    }
                    _ => break,
                }
            }
            parts.reverse();
            parts.join(" › ")
        };
        let mut out = Vec::new();
        for r in rows.iter().filter(|r| r.5.is_some()) {
            let budget = r.5.unwrap_or(0);
            let mut stack = vec![r.0];
            let mut sum = 0i64;
            let mut guard = 0;
            while let Some(id) = stack.pop() {
                guard += 1;
                if guard > 10_000 {
                    break;
                }
                sum += own.get(&Some(id)).copied().unwrap_or(0);
                if let Some(ch) = children.get(&id) {
                    stack.extend(ch);
                }
            }
            let spent = (-sum).max(0);
            out.push(CategoryBudgetStatus {
                category_id: r.0,
                name: r.2.clone(),
                path: path_of(r.0),
                sf_symbol: r.3.clone(),
                color_hex: r.4.clone(),
                budget_cents: budget,
                spent_cents: spent,
                remaining_cents: budget - spent,
                percent: if budget > 0 {
                    spent as f64 * 100.0 / budget as f64
                } else {
                    0.0
                },
            });
        }
        out.sort_by(|a, b| b.percent.total_cmp(&a.percent));
        Ok(out)
    }

    // ------------------------------------------------------------- splits

    /// Replaces the splits of a booking. An empty list removes the split.
    /// The amounts must add up to the booking amount.
    pub fn set_transaction_splits(
        &self,
        transaction_id: i64,
        splits: Vec<TransactionSplitInput>,
    ) -> Result<(), CoreError> {
        let mut db = self.db()?;
        let amount: i64 = db
            .query_row(
                "SELECT amount_cents FROM transactions WHERE id = ?1 AND deleted_ms IS NULL",
                [transaction_id],
                |r| r.get(0),
            )
            .map_err(|_| CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            })?;
        if !splits.is_empty() {
            if splits.len() < 2 || splits.iter().any(|s| s.amount_cents == 0) {
                return Err(CoreError::invalid(tr(
                    "Eine Splitbuchung braucht mindestens zwei Teilbeträge ungleich 0.",
                )));
            }
            // checked: an overflowing (wrapping) sum must not pass as "equal"
            let sum = splits
                .iter()
                .try_fold(0i64, |acc, s| acc.checked_add(s.amount_cents))
                .ok_or_else(|| CoreError::invalid(tr("Die Teilbeträge sind zu groß.")))?;
            if sum != amount {
                return Err(CoreError::invalid(trf(
                    "Die Teilbeträge ({0} ¢) ergeben nicht den Buchungsbetrag ({1} ¢).",
                    &[&sum, &amount],
                )));
            }
        }
        let lines: Vec<_> = splits
            .iter()
            .map(|s| (s.category_id, s.amount_cents, s.note.as_str()))
            .collect();
        let tx = db.transaction()?;
        store_splits(&tx, transaction_id, &lines)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_transaction_splits(
        &self,
        transaction_id: i64,
    ) -> Result<Vec<TransactionSplit>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT s.id, s.transaction_id, s.category_id, s.amount_cents, s.note FROM transaction_splits s
             JOIN transactions t ON t.id = s.transaction_id AND t.deleted_ms IS NULL
             WHERE s.transaction_id = ?1 ORDER BY s.id",
        )?;
        let rows = stmt
            .query_map([transaction_id], |r| {
                Ok(TransactionSplit {
                    id: r.get(0)?,
                    transaction_id: r.get(1)?,
                    category_id: r.get(2)?,
                    amount_cents: r.get(3)?,
                    note: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ----------------------------------------------------------- cashflow

    /// Income vs. expenses for the last `count` periods (oldest first, the
    /// current period last; empty periods are included with zeros).
    /// `account_ids` limits the sums to these accounts, so the cashflow card can
    /// follow the sidebar selection (#731); `None` or empty = all accounts.
    /// `respect_in_total`: the card as an overall money figure (#337) — only
    /// accounts of the effective total set contribute. Default `false` keeps
    /// the proof semantics of a directly requested selection (single account).
    #[uniffi::method(default(account_ids = None, respect_in_total = false))]
    pub fn cashflow_by_period(
        &self,
        granularity: PeriodGranularity,
        count: u32,
        account_ids: Option<Vec<i64>>,
        respect_in_total: bool,
    ) -> Result<Vec<PeriodCashflow>, CoreError> {
        let today = crate::dates::today();
        crate::planning::cashflow_for(
            self,
            granularity,
            count,
            account_ids.as_deref().unwrap_or_default(),
            today.year().into(),
            today.month() as u32,
            respect_in_total,
        )
    }

    // ---------------------------------------------------------- templates

    pub fn list_transfer_templates(&self) -> Result<Vec<TransferTemplate>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT id, name, recipient_name, recipient_iban, recipient_bic, default_amount_cents, default_purpose
             FROM transfer_templates ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TransferTemplate {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    recipient_name: r.get(2)?,
                    recipient_iban: r.get(3)?,
                    recipient_bic: r.get(4)?,
                    default_amount_cents: r.get(5)?,
                    default_purpose: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Inserts (`id == 0`) or updates a template; returns its id.
    pub fn save_transfer_template(&self, template: TransferTemplate) -> Result<i64, CoreError> {
        let name = template.name.trim().to_string();
        let rname = template.recipient_name.trim().to_string();
        let iban = normalize_iban(&template.recipient_iban);
        if name.is_empty() || rname.is_empty() || iban.is_empty() {
            return Err(CoreError::invalid(tr(
                "Name, Empfänger und IBAN der Vorlage dürfen nicht leer sein.",
            )));
        }
        if template.default_amount_cents.is_some_and(|c| c <= 0) {
            return Err(CoreError::invalid(tr(
                "Der Vorlagenbetrag muss größer als 0 sein.",
            )));
        }
        let db = self.db()?;
        let bic = template.recipient_bic.trim().to_uppercase();
        if template.id == 0 {
            db.execute(
                "INSERT INTO transfer_templates(name, recipient_name, recipient_iban, recipient_bic, default_amount_cents, default_purpose)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![name, rname, iban, bic, template.default_amount_cents, template.default_purpose],
            )?;
            Ok(db.last_insert_rowid())
        } else {
            let n = db.execute(
                "UPDATE transfer_templates SET name=?1, recipient_name=?2, recipient_iban=?3, recipient_bic=?4,
                    default_amount_cents=?5, default_purpose=?6 WHERE id=?7",
                params![name, rname, iban, bic, template.default_amount_cents, template.default_purpose, template.id],
            )?;
            if n == 0 {
                return Err(CoreError::NotFound {
                    what: trf("Vorlage {0}", &[&template.id]),
                });
            }
            Ok(template.id)
        }
    }

    pub fn delete_transfer_template(&self, id: i64) -> Result<(), CoreError> {
        self.db()?
            .execute("DELETE FROM transfer_templates WHERE id = ?1", [id])?;
        Ok(())
    }
}

/// Testable core of [`Vault::cashflow_by_period`] with an explicit "today".
pub(crate) fn cashflow_for(
    v: &Vault,
    granularity: PeriodGranularity,
    count: u32,
    account_ids: &[i64],
    year: i32,
    month: u32,
    respect_in_total: bool,
) -> Result<Vec<PeriodCashflow>, CoreError> {
    let count = count.clamp(1, 60) as i32;
    let per_year = match granularity {
        PeriodGranularity::Month => 12i32,
        PeriodGranularity::Quarter => 4,
        PeriodGranularity::Year => 1,
    };
    let cur = match granularity {
        PeriodGranularity::Month => month as i32 - 1,
        PeriodGranularity::Quarter => (month as i32 - 1) / 3,
        PeriodGranularity::Year => 0,
    };
    let mut keys = Vec::new();
    let (mut y, mut i) = (year, cur);
    for _ in 0..count {
        keys.push(match granularity {
            PeriodGranularity::Month => format!("{y:04}-{:02}", i + 1),
            PeriodGranularity::Quarter => format!("{y:04}-Q{}", i + 1),
            PeriodGranularity::Year => format!("{y:04}"),
        });
        i -= 1;
        if i < 0 {
            i = per_year - 1;
            y -= 1;
        }
    }
    keys.reverse();
    let first = &keys[0];
    let from = match granularity {
        PeriodGranularity::Month => format!("{first}-01"),
        PeriodGranularity::Quarter => {
            let q: u32 = first
                .get(6..)
                .and_then(|x| x.parse().ok())
                .unwrap_or(1)
                .clamp(1, 4);
            format!(
                "{}-{:02}-01",
                first.get(..4).unwrap_or("1970"),
                (q - 1) * 3 + 1
            )
        }
        PeriodGranularity::Year => format!("{first}-01-01"),
    };
    // Same bookings, transfer detection and flow classification as the booking
    // list and its KPI tiles (`list_transactions` / `SelectionFlow`). In a
    // totals context (#337) the card is an overall money figure: only accounts
    // of the effective total set (own switch, group, archiving) contribute —
    // a directly requested selection (single account) is a proof view and
    // keeps its sums.
    // one guard for both reads, so a merge committing in between can never
    // combine old eligibility with new booking amounts
    let db = v.db()?;
    let allowed: Option<std::collections::HashSet<i64>> = if respect_in_total {
        Some(crate::user_groups::total_account_ids(&db)?)
    } else {
        None
    };
    let filter = BookingFilter {
        account_ids: account_ids.to_vec(),
        date_from: Some(from),
        ..Default::default()
    };
    let mut acc: HashMap<String, (i64, i64)> = HashMap::new();
    v.visit_bookings(&db, &filter, |_, b| {
        // other currencies are never added to the sums, unless one account is
        // selected (#1049); in a totals context only the effective total set
        // contributes (#337) — a directly requested account is a proof view
        if !allowed
            .as_ref()
            .is_none_or(|set| set.contains(&b.account_id))
            || !counts_in_sums(account_ids.len(), &b.currency)
        {
            return;
        }
        let (date, amt) = (&b.booking_date, b.amount_cents);
        // Non-ASCII / malformed dates are skipped instead of panicking on a slice.
        let key = match granularity {
            PeriodGranularity::Month => date.get(..7).map(str::to_string),
            PeriodGranularity::Quarter => {
                let m: u32 = date
                    .get(5..7)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(1)
                    .clamp(1, 12);
                date.get(..4).map(|y| format!("{y}-Q{}", (m - 1) / 3 + 1))
            }
            PeriodGranularity::Year => date.get(..4).map(str::to_string),
        };
        let Some(key) = key else { return };
        let e = acc.entry(key).or_default();
        match kpi_side(b.flow, amt) {
            KpiSide::Income => e.0 = e.0.saturating_add(amt),
            // net: refunds reduce the expenses of their period
            KpiSide::Expense => e.1 = e.1.saturating_sub(amt),
            KpiSide::Neutral => {}
        }
    })?;
    Ok(keys
        .into_iter()
        .map(|k| {
            let (income, expense) = acc.get(&k).copied().unwrap_or((0, 0));
            PeriodCashflow {
                key: k,
                income_cents: income,
                expense_cents: expense,
            }
        })
        .collect())
}

/// Replaces the splits of booking `id` with `lines` (category, amount, note)
/// and stamps `splits_ms`. The amounts are the caller's to check.
pub(crate) fn store_splits(
    conn: &rusqlite::Connection,
    id: i64,
    lines: &[(Option<i64>, i64, impl AsRef<str>)],
) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM transaction_splits WHERE transaction_id = ?1",
        [id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO transaction_splits(transaction_id, category_id, amount_cents, note) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (category, cents, note) in lines {
        insert.execute(params![id, category, cents, note.as_ref().trim()])?;
    }
    conn.prepare_cached("UPDATE transactions SET splits_ms = ?2 WHERE id = ?1")?
        .execute(params![id, crate::dates::now_ms()])?;
    Ok(())
}

/// A booking turned into a split booking by an import keeps no category of its
/// own (its splits carry them, #654), unless the user set it.
pub(crate) fn clear_split_category(conn: &rusqlite::Connection, id: i64) -> Result<(), CoreError> {
    conn.prepare_cached(
        "UPDATE transactions SET category_id = NULL, category_source = 'none', rule_id = NULL, category_ms = ?2
          WHERE id = ?1 AND category_id IS NOT NULL AND category_source != 'manual'",
    )?
    .execute(params![id, crate::dates::now_ms()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (std::sync::Arc<Vault>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "gc-plan-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![3u8; 32]).unwrap();
        {
            let c = v.db().unwrap();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'K')",
                [],
            )
            .unwrap();
        }
        (v, dir)
    }

    fn cat(v: &Vault, name: &str, parent: Option<i64>) -> i64 {
        v.create_category(name.into(), parent, "tag".into(), "#000000".into())
            .unwrap()
    }

    fn tx(v: &Vault, ext: &str, date: &str, cents: i64, category: Option<i64>) -> i64 {
        let c = v.db().unwrap();
        c.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, category_id)
             VALUES ((SELECT id FROM accounts LIMIT 1), ?1, ?2, ?3, ?4)",
            params![ext, date, cents, category],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn budget_progress_includes_subcategories_and_month_only() {
        let (v, dir) = vault();
        let food = cat(&v, "Test-Essen", None);
        let sub = cat(&v, "Test-Bäcker", Some(food));
        v.set_category_budget(food, Some(10_000)).unwrap();
        tx(&v, "1", "2025-03-05", -4_000, Some(food));
        tx(&v, "2", "2025-03-20", -5_000, Some(sub));
        tx(&v, "3", "2025-03-21", 1_000, Some(food)); // refund
        tx(&v, "4", "2025-04-01", -9_999, Some(food)); // other month
        let p = v.list_category_budget_progress(2025, 3).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].spent_cents, 8_000);
        assert_eq!(p[0].remaining_cents, 2_000);
        assert!((p[0].percent - 80.0).abs() < 1e-9);
        v.set_category_budget(food, None).unwrap();
        assert!(v.list_category_budget_progress(2025, 3).unwrap().is_empty());
        assert!(v.set_category_budget(food, Some(0)).is_err());
        assert!(v.list_category_budget_progress(2025, 13).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn category_history_sums_per_month_with_subcategories() {
        let (v, dir) = vault();
        let ins = cat(&v, "Test-Versicherung", None);
        let sub = cat(&v, "Test-BU", Some(ins));
        let other = cat(&v, "Test-Anderes", None);
        tx(&v, "1", "2025-01-05", -1_000, Some(ins));
        tx(&v, "2", "2025-01-20", -500, Some(sub));
        tx(&v, "3", "2025-03-01", -2_000, Some(sub));
        tx(&v, "4", "2025-03-02", -9_999, Some(other));
        let h = v.category_history(ins, vec![], None, None).unwrap();
        let got: Vec<(&str, i64)> = h.iter().map(|p| (p.period.as_str(), p.cents)).collect();
        assert_eq!(got, vec![("2025-01", -1_500), ("2025-03", -2_000)]);
        let h = v
            .category_history(ins, vec![], Some("2025-02-01".into()), None)
            .unwrap();
        assert_eq!(h.len(), 1);
        assert!(
            v.category_history(ins, vec![-1], None, None)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn splits_validate_and_drive_category_totals() {
        let (v, dir) = vault();
        let a = cat(&v, "Test-A", None);
        let b = cat(&v, "Test-B", None);
        v.set_category_budget(b, Some(5_000)).unwrap();
        let id = tx(&v, "1", "2025-05-02", -3_000, Some(a));
        let inp = |c, amount| TransactionSplitInput {
            category_id: Some(c),
            amount_cents: amount,
            note: String::new(),
        };
        // wrong sum / single part rejected
        assert!(
            v.set_transaction_splits(id, vec![inp(a, -1_000), inp(b, -1_000)])
                .is_err()
        );
        assert!(v.set_transaction_splits(id, vec![inp(a, -3_000)]).is_err());
        v.set_transaction_splits(id, vec![inp(a, -1_000), inp(b, -2_000)])
            .unwrap();
        assert_eq!(v.list_transaction_splits(id).unwrap().len(), 2);
        let p = v.list_category_budget_progress(2025, 5).unwrap();
        assert_eq!(p[0].spent_cents, 2_000);
        let cats = v.list_categories(None, None).unwrap();
        let own = |cid| cats.iter().find(|c| c.id == cid).unwrap().own_total_cents;
        assert_eq!((own(a), own(b)), (-1_000, -2_000));
        // clearing restores the parent category
        v.set_transaction_splits(id, vec![]).unwrap();
        let cats = v.list_categories(None, None).unwrap();
        assert_eq!(
            cats.iter().find(|c| c.id == a).unwrap().own_total_cents,
            -3_000
        );
        assert!(v.set_transaction_splits(9_999, vec![]).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// #427: +59 EUR split into +200 EUR and a counter part of -141 EUR (signed sum = booking).
    #[test]
    fn splits_allow_mixed_signs_when_the_signed_sum_matches() {
        let (v, dir) = vault();
        let a = cat(&v, "Test-A", None);
        let b = cat(&v, "Test-B", None);
        let id = tx(&v, "1", "2025-05-02", 5_900, Some(a));
        let inp = |c, amount| TransactionSplitInput {
            category_id: Some(c),
            amount_cents: amount,
            note: String::new(),
        };
        v.set_transaction_splits(id, vec![inp(a, 20_000), inp(b, -14_100)])
            .unwrap();
        let parts = v.list_transaction_splits(id).unwrap();
        assert_eq!(parts.iter().map(|p| p.amount_cents).sum::<i64>(), 5_900);
        assert!(parts.iter().any(|p| p.amount_cents == -14_100));
        // signed sum off, or a zero part: still rejected
        assert!(
            v.set_transaction_splits(id, vec![inp(a, 20_000), inp(b, -14_000)])
                .is_err()
        );
        assert!(
            v.set_transaction_splits(id, vec![inp(a, 5_900), inp(b, 0)])
                .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn templates_roundtrip() {
        let (v, dir) = vault();
        let mut t = TransferTemplate {
            id: 0,
            name: "Miete".into(),
            recipient_name: "Vermieter".into(),
            recipient_iban: "de89 3704 0044 0532 0130 00".into(),
            recipient_bic: String::new(),
            default_amount_cents: Some(80_000),
            default_purpose: "Miete".into(),
        };
        let id = v.save_transfer_template(t.clone()).unwrap();
        let l = v.list_transfer_templates().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].recipient_iban, "DE89370400440532013000");
        t.id = id;
        t.default_amount_cents = Some(85_000);
        v.save_transfer_template(t.clone()).unwrap();
        assert_eq!(
            v.list_transfer_templates().unwrap()[0].default_amount_cents,
            Some(85_000)
        );
        t.default_amount_cents = Some(0);
        assert!(v.save_transfer_template(t).is_err());
        v.delete_transfer_template(id).unwrap();
        assert!(v.list_transfer_templates().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cashflow_periods() {
        let (v, dir) = vault();
        tx(&v, "1", "2025-01-10", 100_000, None);
        tx(&v, "2", "2025-02-10", -30_000, None);
        tx(&v, "3", "2025-05-10", -10_000, None);
        tx(&v, "4", "2024-12-31", -5_000, None);
        let m = cashflow_for(&v, PeriodGranularity::Month, 3, &[], 2025, 2, false).unwrap();
        assert_eq!(
            m.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            ["2024-12", "2025-01", "2025-02"]
        );
        assert_eq!((m[1].income_cents, m[2].expense_cents), (100_000, 30_000));
        let q = cashflow_for(&v, PeriodGranularity::Quarter, 2, &[], 2025, 5, false).unwrap();
        assert_eq!(q[0].key, "2025-Q1");
        assert_eq!(
            (q[0].income_cents, q[0].expense_cents, q[1].expense_cents),
            (100_000, 30_000, 10_000)
        );
        let y = cashflow_for(&v, PeriodGranularity::Year, 2, &[], 2025, 5, false).unwrap();
        assert_eq!(
            (y[0].key.as_str(), y[0].expense_cents, y[1].expense_cents),
            ("2024", 5_000, 40_000)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn category_filter_matches_split_parts_and_csv_export() {
        let (v, dir) = vault();
        let food = cat(&v, "Test-Essen2", None);
        let fun = cat(&v, "Test-Spass2", None);
        let id = tx(&v, "s1", "2025-03-05", -3_000, Some(food));
        v.set_transaction_splits(
            id,
            vec![
                TransactionSplitInput {
                    category_id: Some(food),
                    amount_cents: -1_000,
                    note: String::new(),
                },
                TransactionSplitInput {
                    category_id: Some(fun),
                    amount_cents: -2_000,
                    note: String::new(),
                },
            ],
        )
        .unwrap();
        let f = BookingFilter {
            category_id: Some(fun),
            limit: 50,
            ..Default::default()
        };
        let page = v.list_transactions(f.clone()).unwrap();
        assert_eq!(page.total_count, 1);
        // only the split part of the filtered category counts in the KPIs
        assert_eq!(page.expense_cents, -2_000);
        assert_eq!(page.flow.consumption_cents, 2_000);
        let food_page = v
            .list_transactions(BookingFilter {
                category_id: Some(food),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(food_page.expense_cents, -1_000);
        // fully split: not "ohne Kategorie", even though the booking's own category is set
        let unc = v
            .list_transactions(BookingFilter {
                uncategorized_only: true,
                ..Default::default()
            })
            .unwrap();
        assert!(unc.transactions.iter().all(|t| t.id != id));
        // overflowing parts are rejected instead of wrapping to the booking amount
        assert!(
            v.set_transaction_splits(
                id,
                vec![
                    TransactionSplitInput {
                        category_id: Some(food),
                        amount_cents: i64::MAX,
                        note: String::new(),
                    },
                    TransactionSplitInput {
                        category_id: Some(fun),
                        amount_cents: i64::MAX,
                        note: String::new(),
                    },
                ],
            )
            .is_err()
        );
        let csv = v.export_bookings_csv(f).unwrap();
        assert!(csv.starts_with(
            "\u{FEFF}Datum;Konto;Name;IBAN;Verwendungszweck;Kategorie;Betrag;Währung;Tags\r\n"
        ));
        assert!(csv.contains(";-30,00;"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cashflow_ignores_transfers_between_own_accounts() {
        let (v, dir) = vault();
        let today = crate::dates::today();
        let date = crate::dates::iso(today);
        {
            let c = v.db().unwrap();
            c.execute(
                "UPDATE accounts SET iban = 'DE02 1203 0000 0000 2020 51'",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty_iban)
                 VALUES ((SELECT id FROM accounts LIMIT 1), 'own', ?1, -50000, 'de02120300000000202051')",
                [&date],
            )
            .unwrap();
        }
        tx(&v, "real", &date, -1_000, None);
        let cf = cashflow_for(
            &v,
            PeriodGranularity::Month,
            1,
            &[],
            today.year().into(),
            today.month() as u32,
            false,
        )
        .unwrap();
        assert_eq!(cf.last().unwrap().expense_cents, 1_000);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cashflow_limited_to_selected_accounts() {
        let (v, dir) = vault();
        let other = {
            let c = v.db().unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'B', 'L')",
                [],
            )
            .unwrap();
            let other = c.last_insert_rowid();
            c.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
                 VALUES (?1, 'b1', '2025-02-10', -7000)",
                [other],
            )
            .unwrap();
            other
        };
        tx(&v, "a1", "2025-02-05", -3_000, None);
        let first: i64 = v
            .db()
            .unwrap()
            .query_row(
                "SELECT id FROM accounts WHERE external_ref = 'A'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expense = |ids: &[i64]| {
            cashflow_for(&v, PeriodGranularity::Month, 1, ids, 2025, 2, false).unwrap()[0]
                .expense_cents
        };
        assert_eq!(expense(&[]), 10_000);
        assert_eq!(expense(&[first]), 3_000);
        assert_eq!(expense(&[other]), 7_000);
        assert_eq!(expense(&[first, other]), 10_000);
        std::fs::remove_dir_all(dir).ok();
    }
}
