//! Booking list (filter, paging, enrichment), booking edits and CSV export.

use rusqlite::{params, params_from_iter, ToSql};

use super::Vault;
use crate::error::CoreError;
use crate::i18n::{display_name, display_path, trf};
use crate::merchants::{classify, clean_purpose, payment_method, FlowKind, MerchantResolver};
use crate::models::*;
use crate::money::parse_cents;

/// Booking between two own accounts. A category segment that marks a transfer
/// (MoneyMoney "Umbuchung", "Geldtransit" …) always wins. The counterparty-IBAN
/// heuristic only applies to bookings the user has not categorised and that
/// `classify()` did not already see as investment / perk: an explicit category
/// ("Sonstige Einnahmen" on a child's account) or an ETF savings plan on a depot
/// settlement account must keep its flow (Issue #94).
fn is_own_transfer(t: &Booking, own_ibans: &std::collections::HashSet<String>) -> bool {
    if t.category_path.split(" › ").any(|s| {
        matches!(
            s.trim().to_lowercase().as_str(),
            "umbuchung" | "umbuchungen" | "geldtransit" | "transfer" | "transfers"
        )
    }) {
        return true;
    }
    if t.category_id.is_some() || matches!(t.flow, FlowKind::Investment | FlowKind::Perk) {
        return false;
    }
    let iban = crate::connectors::fints::sepa::normalize_iban(&t.counterparty_iban);
    !iban.is_empty() && own_ibans.contains(&iban)
}

/// WHERE clause "booking `t` belongs to the categories `cat_match`" (e.g. `= ?3`,
/// `IS NULL`), split-aware like `planning::EFFECTIVE_SQL`: an unsplit booking by
/// its own category, a split booking only by the categories of its parts.
fn category_match_sql(cat_match: &str) -> String {
    format!(
        "((t.category_id {cat_match} AND NOT EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id)) \
         OR EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id {cat_match}))"
    )
}

/// Adds merchant, flow kind, payment method and clean purpose to a booking.
pub(crate) fn enrich(t: &mut Booking) {
    t.merchant = MerchantResolver::resolve(&t.counterparty, &t.purpose);
    t.merchant_name = t.merchant.name.clone();
    t.merchant_domain = Some(t.merchant.domain.clone()).filter(|d| !d.is_empty());
    t.merchant_svg_path = Some(t.merchant.svg_path.clone()).filter(|d| !d.is_empty());
    t.merchant_hex_color = t.merchant.color_hex.clone();
    let top = t
        .category_path
        .split(" › ")
        .next()
        .unwrap_or("")
        .to_string();
    t.flow = classify(
        t.amount_cents,
        &t.counterparty,
        &t.purpose,
        &t.booking_text,
        &t.merchant,
        &top,
        t.account_kind,
    );
    t.payment_method = payment_method(&t.booking_text, &t.purpose, t.account_kind, t.amount_cents);
    t.clean_purpose = clean_purpose(&t.purpose);
    t.recurrence_key = crate::recurring::series_key(t);
}

impl Vault {
    /// WHERE clause (plus optional CTE, bound arguments and the KPI share
    /// expression) of a `BookingFilter`; shared by the list and bulk updates.
    #[allow(clippy::type_complexity)]
    fn filter_clauses(
        db: &rusqlite::Connection,
        filter: &BookingFilter,
    ) -> Result<(String, String, Vec<Box<dyn ToSql>>, String), CoreError> {
        let mut wh: Vec<String> = vec!["t.deleted_ms IS NULL AND a.deleted_ms IS NULL".into()];
        let mut args: Vec<Box<dyn ToSql>> = vec![];
        let mut cte = String::new();
        if !filter.account_ids.is_empty() {
            let ph: Vec<String> = filter
                .account_ids
                .iter()
                .map(|id| {
                    args.push(Box::new(*id));
                    format!("?{}", args.len())
                })
                .collect();
            let mut clause = format!("t.account_id IN ({})", ph.join(","));
            // Loan view: also surface bookings of other accounts (e.g. the Girokonto's
            // monthly "Zins / Tilgung" debits) that reference the loan's IBAN / account number.
            let loans: Vec<(i64, String, String)> = {
                let mut q = db
                    .prepare("SELECT id, iban, account_number FROM accounts WHERE kind = 'loan' AND deleted_ms IS NULL")?;
                let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let loan_ids: Vec<i64> = loans.iter().map(|l| l.0).collect();
            for (id, iban, number) in loans.iter().filter(|l| filter.account_ids.contains(&l.0)) {
                let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
                let trimmed = digits.trim_start_matches('0').to_string();
                let mut ors: Vec<String> = Vec::new();
                if iban.len() >= 15 {
                    args.push(Box::new(iban.clone()));
                    ors.push(format!("t.counterparty_iban = ?{0} OR REPLACE(t.purpose,' ','') LIKE '%' || ?{0} || '%'", args.len()));
                }
                if trimmed.len() >= 6 {
                    args.push(Box::new(trimmed.clone()));
                    let n = args.len();
                    ors.push(format!(
                        "t.counterparty_iban LIKE '%' || ?{n} OR t.purpose LIKE '%' || ?{n} || '%'"
                    ));
                }
                // Only exact references (IBAN / full loan number) – no keyword or short-suffix
                // heuristics: those pulled unrelated bookings of every account into a loan.
                if !ors.is_empty() {
                    let own: Vec<String> = loan_ids.iter().map(|i| i.to_string()).collect();
                    clause.push_str(&format!(
                        " OR (t.account_id NOT IN ({}) AND ({}))",
                        own.join(","),
                        ors.join(" OR ")
                    ));
                }
                let _ = id;
            }
            // manual loans (#36): bookings linked by hand or by the loan's expression
            clause.push_str(&format!(
                " OR t.id IN (SELECT transaction_id FROM loan_links WHERE source != 'excluded' AND loan_account_id IN ({}))",
                filter.account_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            ));
            wh.push(format!("({clause})"));
        }
        // Category filters follow `planning::EFFECTIVE_SQL`: a split booking belongs
        // to the categories of its parts (never to its own category), and only the
        // parts of the filtered category count in the KPI sums (`share_sql`).
        let cat_match: Option<String> = if filter.uncategorized_only {
            Some("IS NULL".into())
        } else if let Some(cat) = filter.category_id {
            args.push(Box::new(cat));
            let n = args.len();
            if filter.include_subcategories {
                cte = format!(
                    "WITH RECURSIVE sub(id) AS (SELECT ?{n} UNION ALL SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id WHERE c.deleted_ms IS NULL) "
                );
                Some("IN (SELECT id FROM sub)".into())
            } else {
                Some(format!("= ?{n}"))
            }
        } else {
            None
        };
        let share_sql = match &cat_match {
            Some(m) => {
                wh.push(category_match_sql(m));
                format!(
                    "COALESCE((SELECT SUM(sp.amount_cents) FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id {m}), t.amount_cents)"
                )
            }
            None => "t.amount_cents".to_string(),
        };
        if let Some(from) = filter.date_from.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(from.clone()));
            wh.push(format!("t.booking_date >= ?{}", args.len()));
        }
        if let Some(to) = filter.date_to.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(to.clone()));
            wh.push(format!("t.booking_date <= ?{}", args.len()));
        }
        match filter.checked {
            None => {}
            Some(true) => wh.push("t.checked != 0".into()),
            Some(false) => wh.push("t.checked = 0".into()),
        }
        match filter.direction {
            BookingDirection::All => {}
            BookingDirection::Income => wh.push("t.amount_cents > 0".into()),
            BookingDirection::Expense => wh.push("t.amount_cents < 0".into()),
        }
        if let Some(q) = filter
            .search
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let mut ors = Vec::new();
            for term in q.split_whitespace() {
                let like = format!(
                    "%{}%",
                    term.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                args.push(Box::new(like));
                let n = args.len();
                let mut alt = format!(
                    "(t.counterparty LIKE ?{n} ESCAPE '\\' OR t.purpose LIKE ?{n} ESCAPE '\\' OR t.booking_text LIKE ?{n} ESCAPE '\\' \
                     OR t.counterparty_iban LIKE ?{n} ESCAPE '\\' OR cat.name LIKE ?{n} ESCAPE '\\' OR a.name LIKE ?{n} ESCAPE '\\'"
                );
                if term.chars().any(|c| c.is_ascii_digit()) {
                    if let Ok(cents) = parse_cents(term) {
                        args.push(Box::new(cents.abs()));
                        alt.push_str(&format!(" OR ABS(t.amount_cents) = ?{}", args.len()));
                    }
                }
                alt.push(')');
                ors.push(alt);
            }
            wh.push(ors.join(" AND "));
        }
        let where_sql = wh.join(" AND ");
        Ok((cte, where_sql, args, share_sql))
    }

    fn set_checked_ids(
        tx: &rusqlite::Connection,
        ids: &[i64],
        checked: bool,
    ) -> Result<(), CoreError> {
        let now = crate::user_groups::now_ms();
        for chunk in ids.chunks(500) {
            let ph = vec!["?"; chunk.len()].join(",");
            let sql =
                format!("UPDATE transactions SET checked = ?1, checked_ms = ?2 WHERE id IN ({ph})");
            let mut owned: Vec<Box<dyn ToSql>> = vec![Box::new(checked as i64), Box::new(now)];
            owned.extend(chunk.iter().map(|id| Box::new(*id) as Box<dyn ToSql>));
            tx.execute(&sql, params_from_iter(owned.iter().map(|b| b.as_ref())))?;
        }
        Ok(())
    }
}

#[uniffi::export]
impl Vault {
    /// Marks every booking matching `filter` as checked / unchecked in one
    /// SQL update (GitHub #236; with a scope or merchant filter the ids come
    /// from the enriched list first). Returns the ids that actually changed,
    /// so the caller can undo exactly those.
    pub fn set_checked_for_filter(
        &self,
        filter: BookingFilter,
        checked: bool,
    ) -> Result<Vec<i64>, CoreError> {
        // Scope (Alltag / Sparpläne) and merchant depend on the enriched booking, not
        // on SQL columns: `list_transactions` applies them after the query. Take
        // the ids from it, so the update touches exactly what the list shows.
        if !matches!(filter.scope, BookingScope::All) || filter.merchant_key.is_some() {
            let page = self.list_transactions(BookingFilter {
                limit: 0,
                offset: 0,
                whole_days: false,
                ..filter
            })?;
            let ids: Vec<i64> = page
                .transactions
                .iter()
                .filter(|b| b.checked != checked)
                .map(|b| b.id)
                .collect();
            self.set_checked_for_ids(ids.clone(), checked)?;
            return Ok(ids);
        }
        let mut db = self.db()?;
        let (cte, where_sql, args, _) = Self::filter_clauses(&db, &filter)?;
        let tx = db.transaction()?;
        let ids: Vec<i64> = {
            let sql = format!(
                "{cte}SELECT t.id FROM transactions t JOIN accounts a ON a.id = t.account_id \
                 LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL \
                 WHERE {where_sql} AND (t.checked != 0) != ?{}",
                args.len() + 1
            );
            let mut stmt = tx.prepare(&sql)?;
            let mut all: Vec<&dyn ToSql> = args.iter().map(|b| b.as_ref()).collect();
            let flag = checked as i64;
            all.push(&flag);
            let rows = stmt.query_map(all.as_slice(), |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        Self::set_checked_ids(&tx, &ids, checked)?;
        tx.commit()?;
        Ok(ids)
    }

    /// Sets `checked` for explicit ids (undo of `set_checked_for_filter`).
    pub fn set_checked_for_ids(&self, ids: Vec<i64>, checked: bool) -> Result<(), CoreError> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::set_checked_ids(&tx, &ids, checked)?;
        tx.commit()?;
        Ok(())
    }

    /// Number of bookings matching `filter` (`limit`/`offset` ignored) as one SQL
    /// count, without enriching every row like `list_transactions` (#266). Scope
    /// and merchant depend on the enriched booking: those filters take the list.
    pub fn count_transactions(&self, filter: BookingFilter) -> Result<u32, CoreError> {
        if !matches!(filter.scope, BookingScope::All) || filter.merchant_key.is_some() {
            return Ok(self
                .list_transactions(BookingFilter {
                    limit: 1,
                    offset: 0,
                    whole_days: false,
                    ..filter
                })?
                .total_count);
        }
        let db = self.db()?;
        let (cte, where_sql, args, _) = Self::filter_clauses(&db, &filter)?;
        let sql = format!(
            "{cte}SELECT count(*) FROM transactions t JOIN accounts a ON a.id = t.account_id \
             LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL \
             WHERE {where_sql}"
        );
        let n: i64 = db.query_row(
            &sql,
            params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    pub fn list_transactions(&self, filter: BookingFilter) -> Result<BookingPage, CoreError> {
        let db = self.db()?;
        let paths = Self::category_paths(&db)?;
        let (cte, where_sql, args, share_sql) = Self::filter_clauses(&db, &filter)?;
        let loan_links = crate::loans::link_map(&db)?;
        // a deleted category or rule reads as none (what the old SET NULL did)
        let from_sql = "FROM transactions t JOIN accounts a ON a.id = t.account_id
             LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL
             LEFT JOIN rules r ON r.id = t.rule_id AND r.deleted_ms IS NULL";
        let sql = format!(
            "{cte}SELECT t.id, t.account_id, a.name, t.booking_date, t.value_date, t.amount_cents, t.currency, t.counterparty,
                    t.counterparty_iban, t.purpose, t.booking_text, cat.id, t.category_source, r.id, t.checked, a.kind,
                    (SELECT count(*) FROM booking_documents d WHERE d.booking_id = t.id), t.note,
                    t.purpose_code, t.gvc, t.salary_flag, {share_sql},
                    (t.external_id LIKE 'pending-%')
             {from_sql} WHERE {where_sql} ORDER BY t.booking_date DESC, t.id DESC"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            let cat: Option<i64> = r.get(11)?;
            let src: String = r.get(12)?;
            let share: i64 = r.get(21)?;
            Ok((
                share,
                Booking {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    account_name: r.get(2)?,
                    booking_date: r.get(3)?,
                    value_date: r.get(4)?,
                    amount_cents: r.get(5)?,
                    currency: r.get(6)?,
                    counterparty: r.get(7)?,
                    counterparty_iban: r.get(8)?,
                    purpose: r.get(9)?,
                    booking_text: r.get(10)?,
                    category_id: cat,
                    category_name: String::new(),
                    category_path: String::new(),
                    category_color_hex: String::new(),
                    category_symbol: String::new(),
                    category_source: match src.as_str() {
                        "rule" => CategorySource::Rule,
                        "manual" => CategorySource::Manual,
                        _ => CategorySource::None,
                    },
                    rule_id: r.get(13)?,
                    checked: r.get::<_, i64>(14)? != 0,
                    merchant: MerchantResolver::placeholder(),
                    merchant_name: String::new(),
                    merchant_domain: None,
                    merchant_svg_path: None,
                    merchant_hex_color: String::new(),
                    payment_method: String::new(),
                    flow: FlowKind::Consumption,
                    clean_purpose: String::new(),
                    account_kind: AccountKind::from_str(&r.get::<_, String>(15)?),
                    document_count: r.get::<_, i64>(16)? as u32,
                    note: r.get(17)?,
                    purpose_code: r.get(18)?,
                    gvc: r.get(19)?,
                    salary_flag: r.get::<_, Option<i64>>(20)?.map(|f| f != 0),
                    is_salary: false,
                    recurrence_key: String::new(),
                    is_pending: r.get::<_, i64>(22)? != 0,
                },
            ))
        })?;
        let mut txs = Vec::new();
        let salary_ibans = crate::salary::marked_salary_ibans(&db)?;
        let overrides = crate::insights::load_overrides(&db).unwrap_or_default();
        // Own IBANs: uncategorised bookings to / from them are transfers (Issue #94).
        let own_ibans: std::collections::HashSet<String> = {
            let mut st =
                db.prepare("SELECT iban FROM accounts WHERE iban IS NOT NULL AND iban != '' AND deleted_ms IS NULL")?;
            let ibans = st
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .map(|i| crate::connectors::fints::sepa::normalize_iban(&i))
                .collect();
            ibans
        };
        let (mut income, mut expense, mut consumption, mut investment, mut perk) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut flow = SelectionFlow::default();
        for row in rows {
            let (share, mut t) = row?;
            if let Some((name, path, color, sym)) = t.category_id.and_then(|c| paths.get(&c)) {
                t.category_name = name.clone();
                t.category_path = path.clone();
                t.category_color_hex = color.clone();
                t.category_symbol = sym.clone();
            }
            enrich(&mut t);
            t.is_salary = crate::salary::is_salary(&t, &salary_ibans);
            if is_own_transfer(&t, &own_ibans) {
                t.flow = FlowKind::Transfer;
            }
            // Localise after `enrich`: flow classification needs the stored names.
            t.category_name = display_name(&t.category_name);
            t.category_path = display_path(&t.category_path);
            if let Some(d) = overrides.get(&t.merchant.key) {
                t.merchant.domain = d.clone();
                t.merchant.domain_candidates = vec![d.clone()];
                t.merchant.domain_source = "override".into();
                t.merchant_domain = Some(d.clone());
            }
            if loan_links.contains_key(&t.id) {
                t.flow = FlowKind::Loan;
            }
            let keep = match filter.scope {
                BookingScope::All => true,
                BookingScope::Everyday => !matches!(t.flow, FlowKind::Investment | FlowKind::Perk),
                BookingScope::Investments => {
                    matches!(t.flow, FlowKind::Investment | FlowKind::Perk)
                }
            } && filter
                .merchant_key
                .as_ref()
                .map(|k| *k == t.merchant.key)
                .unwrap_or(true);
            if !keep {
                continue;
            }
            // One KPI definition (`kpi_side`) for the page sums and `SelectionFlow`;
            // transfers between own accounts are neutral (else counted on both sides).
            // `share` is the full amount, or with a category filter only the split
            // parts of that category.
            match kpi_side(t.flow, share) {
                KpiSide::Income => income += share,
                KpiSide::Expense => expense += share,
                KpiSide::Neutral => {}
            }
            match t.flow {
                FlowKind::Consumption => consumption += share,
                FlowKind::Investment => investment += share,
                FlowKind::Perk => perk += share,
                _ => {}
            }
            flow.add(t.flow, share);
            txs.push(t);
        }
        let total = txs.len() as u32;
        let limit = if filter.limit == 0 {
            usize::MAX
        } else {
            filter.limit as usize
        };
        let start = (filter.offset as usize).min(txs.len());
        let mut end = start.saturating_add(limit).min(txs.len());
        if filter.whole_days && end > start {
            // sorted by day: the rest of the page's last day follows directly
            while end < txs.len() && txs[end].booking_date == txs[end - 1].booking_date {
                end += 1;
            }
        }
        let txs: Vec<Booking> = txs.drain(start..end).collect();
        Ok(BookingPage {
            transactions: txs,
            total_count: total,
            income_cents: income,
            expense_cents: expense,
            consumption_cents: consumption,
            investment_cents: investment,
            perk_cents: perk,
            flow,
        })
    }

    /// Manual categorisation (wins over rules). `None` = uncategorised.
    pub fn set_transaction_category(
        &self,
        transaction_id: i64,
        category_id: Option<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let n = db.execute(
            "UPDATE transactions SET category_id = ?1, rule_id = NULL, category_source = CASE WHEN ?1 IS NULL THEN 'none' ELSE 'manual' END, category_ms = ?3 WHERE id = ?2",
            params![category_id, transaction_id, crate::user_groups::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Booking day of one booking ("2026-11-09"); `None` if it does not exist.
    /// Lets the list load far enough back to show a booking it is asked to reveal.
    pub fn booking_date(&self, booking_id: i64) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare("SELECT booking_date FROM transactions WHERE id = ?1")?;
        let mut rows = stmt.query_map([booking_id], |r| r.get::<_, String>(0))?;
        Ok(rows.next().transpose()?)
    }

    /// Distinct counterparty names of all bookings, most recent first, at most
    /// `limit` (bug report #301: a log line naming a payee is scrubbed of it,
    /// whatever the list currently shows).
    pub fn counterparty_names(&self, limit: u32) -> Result<Vec<String>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT counterparty FROM transactions
              WHERE deleted_ms IS NULL AND counterparty <> ''
              GROUP BY counterparty ORDER BY MAX(booking_date) DESC LIMIT ?1",
        )?;
        let names = stmt
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    pub fn set_transaction_checked(
        &self,
        transaction_id: i64,
        checked: bool,
    ) -> Result<(), CoreError> {
        self.db()?.execute(
            "UPDATE transactions SET checked = ?1, checked_ms = ?3 WHERE id = ?2",
            params![checked as i64, transaction_id, crate::user_groups::now_ms()],
        )?;
        Ok(())
    }

    /// Manual text correction of a booking's counterparty / purpose.
    pub fn update_transaction_text(
        &self,
        transaction_id: i64,
        counterparty: String,
        purpose: String,
    ) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE transactions SET counterparty = ?1, purpose = ?2, text_ms = ?4 WHERE id = ?3",
            params![
                counterparty.trim(),
                purpose.trim(),
                transaction_id,
                crate::user_groups::now_ms()
            ],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Free-text user note of a booking (empty clears it).
    pub fn update_transaction_note(
        &self,
        transaction_id: i64,
        note: String,
    ) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE transactions SET note = ?1, note_ms = ?3 WHERE id = ?2",
            params![note.trim(), transaction_id, crate::user_groups::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Exports all bookings matching `filter` (its paging is ignored) as UTF-8 CSV
    /// with BOM, semicolon-separated, for Excel.
    pub fn export_bookings_csv(&self, filter: BookingFilter) -> Result<String, CoreError> {
        let mut out = String::from(
            "\u{FEFF}Datum;Konto;Name;IBAN;Verwendungszweck;Kategorie;Betrag;Währung\r\n",
        );
        let page_size = 1000u32;
        let mut offset = 0u32;
        loop {
            let mut f = filter.clone();
            f.limit = page_size;
            f.offset = offset;
            let page = self.list_transactions(f)?;
            let n = page.transactions.len() as u32;
            for b in &page.transactions {
                let row = [
                    csv_field(&b.booking_date, false),
                    csv_field(&b.account_name, true),
                    csv_field(&b.counterparty, true),
                    csv_field(&b.counterparty_iban, false),
                    csv_field(&b.purpose, true),
                    csv_field(&b.category_path, true),
                    csv_amount(b.amount_cents),
                    csv_field(&b.currency, false),
                ];
                out.push_str(&row.join(";"));
                out.push_str("\r\n");
            }
            if n < page_size {
                break;
            }
            offset += n;
        }
        Ok(out)
    }
}

/// Quotes a CSV field (semicolon dialect); text fields are neutralised against
/// spreadsheet formula injection.
fn csv_field(v: &str, text: bool) -> String {
    let mut s = v.replace(['\r', '\n'], " ");
    if text && s.starts_with(['=', '+', '@', '\t']) {
        s.insert(0, '\'');
    }
    if s.contains([';', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// Formats cents as `-1234,56` (German decimal comma, no grouping).
fn csv_amount(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let a = cents.unsigned_abs();
    format!("{sign}{},{:02}", a / 100, a % 100)
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    use std::sync::Arc;

    fn demo() -> (Arc<Vault>, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!("girocrab-page-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        (v, p)
    }

    #[test]
    fn counterparty_names_are_distinct_newest_first_and_limited() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let names = v.counterparty_names(1000).unwrap();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "distinct");
        assert!(!names.iter().any(String::is_empty));
        for b in all.iter().filter(|b| !b.counterparty.is_empty()) {
            assert!(
                names.contains(&b.counterparty),
                "{} missing",
                b.counterparty
            );
        }
        let newest = all.iter().find(|b| !b.counterparty.is_empty()).unwrap();
        let newest_date = &newest.booking_date;
        let first = all.iter().find(|b| b.counterparty == names[0]).unwrap();
        assert_eq!(&first.booking_date, newest_date);
        assert_eq!(v.counterparty_names(3).unwrap().len(), 3);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn whole_day_pages_never_split_a_day_and_add_up() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        assert!(all.len() > 40);
        let mut loaded: Vec<Booking> = Vec::new();
        let mut pages = 0;
        while loaded.len() < all.len() {
            let page = v
                .list_transactions(BookingFilter {
                    limit: 7,
                    offset: loaded.len() as u32,
                    whole_days: true,
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(page.total_count as usize, all.len());
            assert!(
                page.transactions.len() >= 7 || loaded.len() + page.transactions.len() == all.len()
            );
            loaded.extend(page.transactions);
            // the next booking, if any, belongs to another day
            if let Some(next) = all.get(loaded.len()) {
                assert_ne!(next.booking_date, loaded.last().unwrap().booking_date);
            }
            pages += 1;
            assert!(pages < 1000);
        }
        let ids = |l: &[Booking]| l.iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(ids(&loaded), ids(&all));
        // without the flag the limit is exact
        let exact = v
            .list_transactions(BookingFilter {
                limit: 7,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(exact.transactions.len(), 7);
        // an offset behind the end is an empty page, not an error
        let past = v
            .list_transactions(BookingFilter {
                limit: 7,
                offset: all.len() as u32 + 5,
                whole_days: true,
                ..Default::default()
            })
            .unwrap();
        assert!(past.transactions.is_empty());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn count_matches_the_list_total() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let some = &all[all.len() / 2];
        let filters = [
            BookingFilter::default(),
            BookingFilter {
                checked: Some(false),
                ..Default::default()
            },
            BookingFilter {
                date_from: Some(some.booking_date.clone()),
                direction: BookingDirection::Expense,
                limit: 3,
                ..Default::default()
            },
            BookingFilter {
                account_ids: vec![some.account_id],
                search: Some(some.counterparty.clone()),
                ..Default::default()
            },
            BookingFilter {
                category_id: all.iter().find_map(|b| b.category_id),
                include_subcategories: true,
                ..Default::default()
            },
            BookingFilter {
                uncategorized_only: true,
                ..Default::default()
            },
            BookingFilter {
                scope: BookingScope::Investments,
                ..Default::default()
            },
            BookingFilter {
                merchant_key: Some(some.merchant.key.clone()),
                ..Default::default()
            },
        ];
        for f in filters {
            let listed = v.list_transactions(f.clone()).unwrap().total_count;
            assert_eq!(v.count_transactions(f.clone()).unwrap(), listed, "{f:?}");
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_by_filter_and_undo() {
        let (v, p) = demo();
        let unchecked = BookingFilter {
            checked: Some(false),
            ..Default::default()
        };
        let before = v.list_transactions(unchecked.clone()).unwrap().total_count;
        assert!(before > 1);
        let ids = v.set_checked_for_filter(unchecked.clone(), true).unwrap();
        assert_eq!(ids.len() as u32, before);
        assert_eq!(
            v.list_transactions(unchecked.clone()).unwrap().total_count,
            0
        );
        // a second run changes nothing
        assert!(v
            .set_checked_for_filter(unchecked.clone(), true)
            .unwrap()
            .is_empty());
        // undo restores exactly the touched bookings
        v.set_checked_for_ids(ids, false).unwrap();
        assert_eq!(v.list_transactions(unchecked).unwrap().total_count, before);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_respects_search_category_and_period() {
        let (v, p) = demo();
        v.set_checked_for_filter(BookingFilter::default(), false)
            .unwrap();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let cat = all.transactions.iter().find_map(|b| b.category_id).unwrap();
        let mid = all.transactions[all.transactions.len() / 2]
            .booking_date
            .clone();
        let filters = [
            BookingFilter {
                search: Some("rewe".into()),
                ..Default::default()
            },
            BookingFilter {
                category_id: Some(cat),
                include_subcategories: true,
                ..Default::default()
            },
            BookingFilter {
                date_from: Some(mid.clone()),
                direction: BookingDirection::Expense,
                ..Default::default()
            },
        ];
        for f in filters {
            v.set_checked_for_filter(BookingFilter::default(), false)
                .unwrap();
            let shown = v
                .list_transactions(BookingFilter {
                    checked: Some(false),
                    ..f.clone()
                })
                .unwrap()
                .total_count;
            assert!(
                shown > 0 && shown < all.total_count,
                "filter must narrow: {shown}"
            );
            let ids = v
                .set_checked_for_filter(
                    BookingFilter {
                        checked: Some(false),
                        ..f
                    },
                    true,
                )
                .unwrap();
            assert_eq!(ids.len() as u32, shown);
            let left = v
                .list_transactions(BookingFilter {
                    checked: Some(false),
                    ..Default::default()
                })
                .unwrap()
                .total_count;
            assert_eq!(left, all.total_count - shown);
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_respects_scope() {
        let (v, p) = demo();
        // start with everything new
        v.set_checked_for_filter(BookingFilter::default(), false)
            .unwrap();
        let count = |scope: BookingScope| {
            v.list_transactions(BookingFilter {
                checked: Some(false),
                scope,
                ..Default::default()
            })
            .unwrap()
            .total_count
        };
        let (all, everyday) = (count(BookingScope::All), count(BookingScope::Everyday));
        assert!(everyday < all, "demo has investments");
        let ids = v
            .set_checked_for_filter(
                BookingFilter {
                    checked: Some(false),
                    scope: BookingScope::Everyday,
                    ..Default::default()
                },
                true,
            )
            .unwrap();
        assert_eq!(ids.len() as u32, everyday);
        // what is outside the scope stays new
        assert_eq!(count(BookingScope::All), all - everyday);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn checked_filter_selects_new_or_checked_bookings() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let first = all.first().unwrap();
        assert_eq!(
            v.booking_date(first.id).unwrap().as_deref(),
            Some(first.booking_date.as_str())
        );
        assert_eq!(v.booking_date(-1).unwrap(), None);
        let pick = all.iter().find(|b| !b.checked).expect("a new booking").id;
        v.set_transaction_checked(pick, true).unwrap();
        let of = |checked: Option<bool>| {
            v.list_transactions(BookingFilter {
                checked,
                ..Default::default()
            })
            .unwrap()
        };
        let (new, done) = (of(Some(false)), of(Some(true)));
        assert!(new.transactions.iter().all(|b| !b.checked));
        assert!(done.transactions.iter().all(|b| b.checked));
        assert!(done.transactions.iter().any(|b| b.id == pick));
        assert_eq!(
            new.total_count + done.total_count,
            of(None).total_count,
            "the count follows the filter"
        );
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod own_transfer_tests {
    use super::*;
    use crate::test_support::TempDir;
    use rusqlite::params;
    use std::sync::Arc;

    const GIRO_IBAN: &str = "DE89370400440532013000";
    const KIND_IBAN: &str = "DE02120300000000202051";

    /// Two own accounts (parent Giro, child account) and one booking per case on
    /// the child account, all coming from the parent's IBAN.
    fn vault_with(cases: &[(&str, i64, &str, Option<&str>)]) -> (TempDir, Arc<Vault>, Vec<i64>) {
        let dir = TempDir::new("own-transfer");
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:12345678', 'Bank', 'me')",
            [],
        )
        .unwrap();
        let con = db.last_insert_rowid();
        db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
            .unwrap();
        let group = db.last_insert_rowid();
        let mut accounts = vec![];
        for (ext, name, iban) in [("A1", "Eltern", GIRO_IBAN), ("A2", "Kind", KIND_IBAN)] {
            db.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, iban) VALUES (?1, ?5, ?2, ?3, 'checking', ?4)",
                params![con, ext, name, iban, group],
            )
            .unwrap();
            accounts.push(db.last_insert_rowid());
        }
        let mut ids = vec![];
        for (i, (cp, cents, purpose, cat)) in cases.iter().enumerate() {
            let cat_id: Option<i64> = cat.map(|path| {
                let mut parent: Option<i64> = None;
                for seg in path.split('/') {
                    let found: Option<i64> = db
                        .query_row(
                            "SELECT id FROM categories WHERE name = ?1 AND parent_id IS ?2",
                            params![seg, parent],
                            |r| r.get(0),
                        )
                        .ok();
                    parent = Some(found.unwrap_or_else(|| {
                        db.execute(
                            "INSERT INTO categories(name, parent_id) VALUES (?1, ?2)",
                            params![seg, parent],
                        )
                        .unwrap();
                        db.last_insert_rowid()
                    }));
                }
                parent.unwrap()
            });
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose, category_id, category_source)
                 VALUES (?1, ?2, '2024-03-05', ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    accounts[1],
                    format!("t{i}"),
                    cents,
                    cp,
                    GIRO_IBAN,
                    purpose,
                    cat_id,
                    if cat_id.is_some() { "manual" } else { "none" }
                ],
            )
            .unwrap();
            ids.push(db.last_insert_rowid());
        }
        drop(db);
        (dir, v, ids)
    }

    fn flow_of(v: &Vault, id: i64) -> FlowKind {
        v.list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .find(|b| b.id == id)
            .unwrap()
            .flow
    }

    #[test]
    fn uncategorised_booking_between_own_accounts_is_a_transfer() {
        let (_d, v, ids) = vault_with(&[("Papa", 500_00, "Taschengeld", None)]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Transfer);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 0);
        assert_eq!(page.flow.income_cents, 0);
    }

    #[test]
    fn explicit_non_transfer_category_keeps_its_flow() {
        let (_d, v, ids) =
            vault_with(&[("Papa", 500_00, "Taschengeld", Some("Sonstige Einnahmen"))]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Income);
        let f = v.selection_flow(vec![], None, None).unwrap();
        assert_eq!(f.income_cents, 500_00);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 500_00);
    }

    #[test]
    fn expense_with_category_to_own_account_counts_as_consumption() {
        let (_d, v, ids) = vault_with(&[("Papa", -40_00, "Kino", Some("Freizeit"))]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Consumption);
        assert_eq!(
            v.selection_flow(vec![], None, None)
                .unwrap()
                .consumption_cents,
            40_00
        );
    }

    #[test]
    fn investment_to_own_account_keeps_investment_flow() {
        let (_d, v, ids) = vault_with(&[
            ("Depot", -100_00, "Sparplan MSCI World", None),
            (
                "Depot",
                -100_00,
                "Sparplan MSCI World",
                Some("Sparen & Investieren"),
            ),
        ]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Investment);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Investment);
        assert_eq!(
            v.selection_flow(vec![], None, None).unwrap().invested_cents,
            200_00
        );
    }

    #[test]
    fn explicit_transfer_category_is_a_transfer() {
        let (_d, v, ids) = vault_with(&[
            ("Papa", 500_00, "x", Some("Umbuchung")),
            ("Papa", 500_00, "x", Some("Privat/Geldtransit")),
            // no own IBAN involved: the category alone decides
        ]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Transfer);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Transfer);
        assert_eq!(
            v.selection_flow(vec![], None, None).unwrap().income_cents,
            0
        );
    }
}
