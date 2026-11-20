//! The category tree: listing with period totals, create, edit, delete.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::Vault;
use crate::error::{CoreError, CoreResult};
use crate::i18n::{role_name, tr};
use crate::models::*;

/// Classification metadata never passes through a booking's display fields.
#[derive(Default)]
pub(super) struct CategoryFlow {
    pub(super) top_role: String,
    pub(super) is_transfer: bool,
}

impl Vault {
    fn category_rows(
        conn: &Connection,
    ) -> CoreResult<Vec<(i64, Option<i64>, String, String, String, i32, Option<i64>)>> {
        let mut stmt = conn.prepare("SELECT id, parent_id, name, sf_symbol, color_hex, sort_order, budget_cents, builtin_role FROM categories WHERE deleted_ms IS NULL")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    role_name(
                        &r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(7)?.as_deref(),
                    ),
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(super) fn category_flows(conn: &Connection) -> CoreResult<HashMap<i64, CategoryFlow>> {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, builtin_role FROM categories WHERE deleted_ms IS NULL",
        )?;
        let rows: HashMap<i64, (Option<i64>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        let mut out = HashMap::new();
        for id in rows.keys() {
            let mut flow = CategoryFlow::default();
            let mut cur = Some(*id);
            // Match the display tree's depth bound; malformed cycles cannot hang a load.
            for _ in 0..32 {
                let Some((parent, role)) = cur.and_then(|c| rows.get(&c)) else {
                    break;
                };
                flow.top_role = role.clone().unwrap_or_default();
                flow.is_transfer |=
                    matches!(role.as_deref(), Some("Umbuchungen" | "import:transfers"));
                cur = *parent;
            }
            out.insert(*id, flow);
        }
        Ok(out)
    }

    pub(crate) fn category_paths(
        conn: &Connection,
    ) -> CoreResult<HashMap<i64, (String, String, String, String)>> {
        // id -> (name, path, color, symbol)
        let rows = Self::category_rows(conn)?;
        let by_id: HashMap<i64, &(i64, Option<i64>, String, String, String, i32, Option<i64>)> =
            rows.iter().map(|r| (r.0, r)).collect();
        let mut out = HashMap::new();
        for r in &rows {
            let mut parts = vec![r.2.clone()];
            let mut cur = r.1;
            let mut guard = 0;
            while let Some(pid) = cur {
                guard += 1;
                if guard > 32 {
                    break;
                }
                if let Some(p) = by_id.get(&pid) {
                    parts.push(p.2.clone());
                    cur = p.1;
                } else {
                    break;
                }
            }
            parts.reverse();
            out.insert(
                r.0,
                (r.2.clone(), parts.join(" › "), r.4.clone(), r.3.clone()),
            );
        }
        Ok(out)
    }
}

/// Refuses putting category `id` below itself (or below one of its sub-categories).
fn ensure_not_below(db: &Connection, id: i64, parent_id: Option<i64>) -> CoreResult<()> {
    let Some(mut p) = parent_id else {
        return Ok(());
    };
    let mut guard = 0;
    loop {
        if p == id {
            return Err(CoreError::invalid(tr(
                "Eine Kategorie kann nicht in sich selbst verschoben werden.",
            )));
        }
        guard += 1;
        match db
            .query_row("SELECT parent_id FROM categories WHERE id = ?1", [p], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()?
        {
            Some(Some(next)) if guard < 32 => p = next,
            _ => return Ok(()),
        }
    }
}

#[uniffi::export]
impl Vault {
    /// All categories in tree order (depth-first, siblings by sort order) with
    /// totals for the optional period.
    pub fn list_categories(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<Vec<Category>, CoreError> {
        self.list_categories_filtered(date_from, date_to, Vec::new(), None, BookingDirection::All)
    }

    /// Like `list_categories`, but the totals also follow the booking list's filters
    /// (GitHub #621): empty `account_ids` = all accounts, `checked` = `Some(true)` only
    /// checked, `Some(false)` only new bookings, `None` = both; `direction` keeps only
    /// inflows / outflows by the booking's amount, like the list.
    pub fn list_categories_filtered(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
        account_ids: Vec<i64>,
        checked: Option<bool>,
        direction: BookingDirection,
    ) -> Result<Vec<Category>, CoreError> {
        self.categories_with_filters(
            date_from,
            date_to,
            account_ids,
            checked,
            direction,
            None,
            None,
        )
    }

    /// `list_categories_filtered` plus the list's amount range (GitHub #357): only bookings whose
    /// absolute amount lies between `amount_min_cents` and `amount_max_cents` (`None` = open).
    #[allow(clippy::too_many_arguments)]
    pub fn list_categories_filtered_amount(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
        account_ids: Vec<i64>,
        checked: Option<bool>,
        direction: BookingDirection,
        amount_min_cents: Option<i64>,
        amount_max_cents: Option<i64>,
    ) -> Result<Vec<Category>, CoreError> {
        self.categories_with_filters(
            date_from,
            date_to,
            account_ids,
            checked,
            direction,
            amount_min_cents,
            amount_max_cents,
        )
    }

    /// Monthly sums of one category including its subcategories (GitHub #502), for the
    /// chart above a category's bookings. Splits count with their split amounts; empty
    /// `account_ids` = all accounts. Months without bookings are left out.
    pub fn category_history(
        &self,
        category_id: i64,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<Vec<CategoryPeriodSum>, CoreError> {
        self.category_history_by_unit(category_id, account_ids, date_from, date_to, 7)
    }

    /// Like `category_history`, with the bar width chosen by `key_length` (GitHub #557):
    /// 4 = years (`yyyy`), 7 = months (`yyyy-MM`), 10 = days (`yyyy-MM-dd`); other values mean months.
    pub fn category_history_by_unit(
        &self,
        category_id: i64,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
        key_length: u32,
    ) -> Result<Vec<CategoryPeriodSum>, CoreError> {
        let key_length = if matches!(key_length, 4 | 10) {
            key_length
        } else {
            7
        };
        let db = self.db()?;
        let rows = Self::category_rows(&db)?;
        let ids = Self::category_subtree(&rows, category_id);
        let in_list = |v: &[i64]| v.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let account_clause = if account_ids.is_empty() {
            String::new()
        } else {
            format!(" AND account_id IN ({})", in_list(&account_ids))
        };
        let sql = format!(
            "SELECT substr(booking_date, 1, {key_length}) AS m, SUM(amount_cents) FROM {}
             WHERE category_id IN ({}){account_clause}
               AND (?1 IS NULL OR booking_date >= ?1) AND (?2 IS NULL OR booking_date <= ?2)
             GROUP BY m ORDER BY m",
            crate::planning::effective_sql(
                ", t.account_id AS account_id",
                crate::planning::sums_condition(account_ids.len())
            ),
            in_list(&ids)
        );
        let mut stmt = db.prepare(&sql)?;
        let out = stmt
            .query_map(params![date_from, date_to], |r| {
                Ok(CategoryPeriodSum {
                    period: r.get(0)?,
                    cents: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// What a category (with subcategories) holds in currencies other than EUR, one sum per currency
    /// (alphabetical), for the "Zusätzlich" line next to its total (#1049). Empty for a single
    /// account: its total is already in its own currency. Splits count with their split amounts.
    pub fn category_other_currencies(
        &self,
        category_id: i64,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<Vec<CurrencyAmount>, CoreError> {
        if account_ids.len() == 1 {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        // No booking in another currency (the usual vault): nothing to add, no category tree read.
        if !crate::planning::has_other_currencies(&db)? {
            return Ok(Vec::new());
        }
        let rows = Self::category_rows(&db)?;
        let ids = Self::category_subtree(&rows, category_id);
        let in_list = |v: &[i64]| v.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let account_clause = if account_ids.is_empty() {
            String::new()
        } else {
            format!(" AND account_id IN ({})", in_list(&account_ids))
        };
        let sql = format!(
            "SELECT upper(currency), SUM(amount_cents) FROM {}
             WHERE category_id IN ({}){account_clause}
               AND (?1 IS NULL OR booking_date >= ?1) AND (?2 IS NULL OR booking_date <= ?2)
             GROUP BY upper(currency) ORDER BY upper(currency)",
            crate::planning::effective_sql(
                ", t.account_id AS account_id, t.currency AS currency",
                " AND upper(t.currency) NOT IN ('', 'EUR')"
            ),
            in_list(&ids)
        );
        let mut stmt = db.prepare(&sql)?;
        let out = stmt
            .query_map(params![date_from, date_to], |r| {
                Ok(CurrencyAmount {
                    currency: r.get(0)?,
                    cents: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// "Zusätzlich" next to the sidebar totals (#1049): per category (with its subcategories) and
    /// currency other than EUR, what `list_categories_filtered_amount` leaves out. Takes the same
    /// filters; empty for a single account, and without a query when no booking has another currency.
    #[allow(clippy::too_many_arguments)]
    pub fn category_other_currency_totals(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
        account_ids: Vec<i64>,
        checked: Option<bool>,
        direction: BookingDirection,
        amount_min_cents: Option<i64>,
        amount_max_cents: Option<i64>,
    ) -> Result<Vec<CategoryCurrencyAmount>, CoreError> {
        if account_ids.len() == 1 {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        if !crate::planning::has_other_currencies(&db)? {
            return Ok(Vec::new());
        }
        let rows = Self::category_rows(&db)?;
        let filters =
            Self::category_condition(&account_ids, direction, amount_min_cents, amount_max_cents);
        let condition = format!(
            "{filters} AND upper(t.currency) NOT IN ('', 'EUR') AND (?3 IS NULL OR (t.checked != 0) = ?3)"
        );
        let mut stmt = db.prepare(&format!(
            "SELECT category_id, upper(currency), SUM(amount_cents) FROM {}
             WHERE category_id IS NOT NULL
               AND (?1 IS NULL OR booking_date >= ?1) AND (?2 IS NULL OR booking_date <= ?2)
             GROUP BY category_id, upper(currency)",
            crate::planning::effective_sql(", t.currency AS currency", &condition)
        ))?;
        let checked = checked.map(i64::from);
        let own = stmt
            .query_map(params![date_from, date_to, checked], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // roll every sum up to its ancestors, like the totals of the tree
        let parent: HashMap<i64, Option<i64>> = rows.iter().map(|r| (r.0, r.1)).collect();
        let mut sums: std::collections::BTreeMap<(i64, String), i64> = Default::default();
        for (cat, currency, cents) in own {
            let mut at = Some(cat);
            let mut depth = 0;
            while let Some(id) = at {
                *sums.entry((id, currency.clone())).or_insert(0) += cents;
                at = parent.get(&id).copied().flatten();
                depth += 1;
                if depth > 32 {
                    break;
                }
            }
        }
        Ok(sums
            .into_iter()
            .filter(|(_, cents)| *cents != 0)
            .map(|((category_id, currency), cents)| CategoryCurrencyAmount {
                category_id,
                currency,
                cents,
            })
            .collect())
    }

    pub fn create_category(
        &self,
        name: String,
        parent_id: Option<i64>,
        sf_symbol: String,
        color_hex: String,
    ) -> Result<i64, CoreError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(CoreError::invalid(tr("Der Kategoriename ist leer.")));
        }
        let db = self.db()?;
        let order: i32 = db.query_row(
            "SELECT IFNULL(MAX(sort_order), 0) + 1 FROM categories WHERE parent_id IS ?1",
            [parent_id],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO categories(parent_id, name, sf_symbol, color_hex, sort_order,
                    name_ms, parent_ms, symbol_ms, color_ms, order_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?6, ?6, ?6)",
            params![
                parent_id,
                name,
                if sf_symbol.is_empty() {
                    "tag".to_string()
                } else {
                    sf_symbol
                },
                color_hex,
                order,
                crate::dates::now_ms()
            ],
        )?;
        let id = db.last_insert_rowid();
        crate::sync_merge::fill_missing_uuids(&db)?;
        Ok(id)
    }

    pub fn update_category(
        &self,
        id: i64,
        name: String,
        parent_id: Option<i64>,
        sf_symbol: String,
        color_hex: String,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        ensure_not_below(&db, id, parent_id)?;
        // The UI shows built-in names translated; saving that unchanged
        // display name keeps the stored German default (and its logic).
        let stored: Option<(String, Option<i64>, String, String, Option<String>)> = db
            .query_row(
                "SELECT name, parent_id, sf_symbol, color_hex, builtin_role FROM categories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let name = match &stored {
            Some((stored, _, _, _, role))
                if *stored != name && role_name(stored, role.as_deref()) == name =>
            {
                stored.clone()
            }
            _ => name,
        };
        // only fields that really change get a stamp (newest edit wins per field)
        let now = crate::dates::now_ms();
        let (old_name, old_parent, old_symbol, old_color, _) = stored.unwrap_or_default();
        db.execute(
            "UPDATE categories SET name = ?1, parent_id = ?2, sf_symbol = ?3, color_hex = ?4,
                    name_ms = CASE WHEN ?6 THEN ?10 ELSE name_ms END,
                    parent_ms = CASE WHEN ?7 THEN ?10 ELSE parent_ms END,
                    symbol_ms = CASE WHEN ?8 THEN ?10 ELSE symbol_ms END,
                    color_ms = CASE WHEN ?9 THEN ?10 ELSE color_ms END
             WHERE id = ?5",
            params![
                name,
                parent_id,
                sf_symbol,
                color_hex,
                id,
                name != old_name,
                parent_id != old_parent,
                sf_symbol != old_symbol,
                color_hex != old_color,
                now
            ],
        )?;
        Ok(())
    }

    /// Puts `ids` as the children of `parent_id` (`None` = top level), in this order
    /// (sidebar drag and drop, GitHub #977). A category of another parent is moved
    /// here; one below itself is refused. Only changed fields get a sync stamp.
    /// Ids that no longer exist are skipped.
    pub fn reorder_categories(
        &self,
        parent_id: Option<i64>,
        ids: Vec<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        if let Some(p) = parent_id {
            let live: Option<i64> = tx
                .query_row(
                    "SELECT id FROM categories WHERE id = ?1 AND deleted_ms IS NULL",
                    [p],
                    |r| r.get(0),
                )
                .optional()?;
            if live.is_none() {
                return Err(CoreError::invalid(tr(
                    "Die Zielkategorie existiert nicht mehr.",
                )));
            }
        }
        let now = crate::dates::now_ms();
        let mut seen = std::collections::HashSet::new();
        let mut order = 0;
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let stored: Option<(Option<i64>, i32)> = tx
                .query_row(
                    "SELECT parent_id, sort_order FROM categories WHERE id = ?1 AND deleted_ms IS NULL",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((old_parent, old_order)) = stored else {
                continue;
            };
            if old_parent != parent_id {
                ensure_not_below(&tx, id, parent_id)?;
            }
            tx.execute(
                "UPDATE categories SET parent_id = ?2, sort_order = ?3,
                        parent_ms = CASE WHEN ?4 THEN ?6 ELSE parent_ms END,
                        order_ms = CASE WHEN ?5 THEN ?6 ELSE order_ms END
                 WHERE id = ?1",
                params![
                    id,
                    parent_id,
                    order,
                    old_parent != parent_id,
                    old_order != order,
                    now
                ],
            )?;
            order += 1;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes a category and its sub-categories (soft: the rows stay with a
    /// deletion stamp, so a sync does not bring them back); their rules go
    /// too and their transactions and splits become uncategorised. The
    /// uncategorising is derived, so it carries no `category_ms` stamp.
    pub fn delete_category(&self, id: i64) -> Result<(), CoreError> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        crate::sync_merge::soft_delete_categories(&tx, id)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod reorder_tests;
#[cfg(test)]
mod role_tests;

impl Vault {
    /// Conditions on `t` shared by the category totals and their "other currencies": accounts,
    /// direction and amount range (#357). Ids and amounts are integers, so inlining them cannot inject anything.
    fn category_condition(
        account_ids: &[i64],
        direction: BookingDirection,
        amount_min_cents: Option<i64>,
        amount_max_cents: Option<i64>,
    ) -> String {
        let accounts = if account_ids.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = account_ids.iter().map(i64::to_string).collect();
            format!(" AND t.account_id IN ({})", list.join(","))
        };
        // `+`: no `tx_amount_date` range scan here (a row lookup per match, 10x slower
        // than the plain pass that sums every booking anyway).
        let side = match direction {
            BookingDirection::All => "",
            BookingDirection::Income => " AND +t.amount_cents > 0",
            BookingDirection::Expense => " AND +t.amount_cents < 0",
        };
        // absolute amount of the booking, like the list's amount range (#357)
        let mut amount = String::new();
        if let Some(min) = amount_min_cents {
            amount.push_str(&format!(" AND ABS(t.amount_cents) >= {min}"));
        }
        if let Some(max) = amount_max_cents {
            amount.push_str(&format!(" AND ABS(t.amount_cents) <= {max}"));
        }
        format!("{accounts}{side}{amount}")
    }

    /// `category_id` and all its descendants (capped like the history queries).
    fn category_subtree(
        rows: &[(i64, Option<i64>, String, String, String, i32, Option<i64>)],
        category_id: i64,
    ) -> Vec<i64> {
        let mut ids = vec![category_id];
        let mut i = 0;
        while i < ids.len() && ids.len() < 4096 {
            let cur = ids[i];
            for r in rows.iter().filter(|r| r.1 == Some(cur)) {
                if !ids.contains(&r.0) {
                    ids.push(r.0);
                }
            }
            i += 1;
        }
        ids
    }

    #[allow(clippy::too_many_arguments)]
    fn categories_with_filters(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
        account_ids: Vec<i64>,
        checked: Option<bool>,
        direction: BookingDirection,
        amount_min_cents: Option<i64>,
        amount_max_cents: Option<i64>,
    ) -> Result<Vec<Category>, CoreError> {
        let db = self.db()?;
        let rows = Self::category_rows(&db)?;
        let mut totals: HashMap<Option<i64>, (i64, u32)> = HashMap::new();
        {
            let filters = Self::category_condition(
                &account_ids,
                direction,
                amount_min_cents,
                amount_max_cents,
            );
            let currency = crate::planning::sums_condition(account_ids.len());
            let condition =
                format!("{filters}{currency} AND (?3 IS NULL OR (t.checked != 0) = ?3)");
            let mut stmt = db.prepare(&format!(
                "SELECT category_id, SUM(amount_cents), COUNT(*) FROM {}
                 WHERE (?1 IS NULL OR booking_date >= ?1) AND (?2 IS NULL OR booking_date <= ?2)
                 GROUP BY category_id",
                crate::planning::effective_sql("", &condition)
            ))?;
            let checked = checked.map(i64::from);
            let it = stmt.query_map(params![date_from, date_to, checked], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)? as u32,
                ))
            })?;
            for row in it {
                let (c, s, n) = row?;
                totals.insert(c, (s, n));
            }
        }
        let mut children: HashMap<
            Option<i64>,
            Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
        > = HashMap::new();
        for r in &rows {
            children.entry(r.1).or_default().push(r);
        }
        for list in children.values_mut() {
            list.sort_by(|a, b| {
                a.5.cmp(&b.5)
                    .then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase()))
            });
        }
        fn subtotal(
            id: i64,
            children: &HashMap<
                Option<i64>,
                Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
            >,
            totals: &HashMap<Option<i64>, (i64, u32)>,
            depth: u32,
        ) -> (i64, u32) {
            let own = totals.get(&Some(id)).copied().unwrap_or((0, 0));
            if depth > 32 {
                return own;
            }
            let mut sum = own;
            if let Some(kids) = children.get(&Some(id)) {
                for k in kids {
                    let (s, n) = subtotal(k.0, children, totals, depth + 1);
                    sum.0 += s;
                    sum.1 += n;
                }
            }
            sum
        }
        let mut out = Vec::with_capacity(rows.len());
        fn walk(
            parent: Option<i64>,
            depth: u32,
            prefix: &str,
            children: &HashMap<
                Option<i64>,
                Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
            >,
            totals: &HashMap<Option<i64>, (i64, u32)>,
            out: &mut Vec<Category>,
        ) {
            if depth > 32 {
                return;
            }
            if let Some(kids) = children.get(&parent) {
                for k in kids {
                    let path = if prefix.is_empty() {
                        k.2.clone()
                    } else {
                        format!("{prefix} › {}", k.2)
                    };
                    let own = totals.get(&Some(k.0)).copied().unwrap_or((0, 0));
                    let (total, count) = subtotal(k.0, children, totals, 0);
                    out.push(Category {
                        id: k.0,
                        parent_id: k.1,
                        name: k.2.clone(),
                        sf_symbol: k.3.clone(),
                        color_hex: k.4.clone(),
                        sort_order: k.5,
                        depth,
                        path: path.clone(),
                        own_total_cents: own.0,
                        total_cents: total,
                        transaction_count: count,
                        budget_cents: k.6,
                    });
                    walk(Some(k.0), depth + 1, &path, children, totals, out);
                }
            }
        }
        walk(None, 0, "", &children, &totals, &mut out);
        Ok(out)
    }
}
