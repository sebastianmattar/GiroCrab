//! Versioned vault schema (GitHub #206).
//!
//! `PRAGMA user_version` is the number of steps in [`STEPS`] a vault has run.
//! Opening a vault runs the missing steps in order, all in one transaction
//! ([`migrate`]); a vault with a higher version than this build
//! knows is refused with [`CoreError::SchemaTooNew`] and left untouched.
//!
//! - 1: the complete schema for a new vault ([`BASELINE`]).
//! - 2–8: empty. Vaults written before #206 carry these numbers but reached
//!   them through idempotent probes, so their schema may lack pieces.
//! - 9: [`repair_to_v8`] runs those probes once more, so every vault ends up
//!   with the same schema whatever its history (no-op on a new vault).
//! - 10: opening balances (#212): table `opening_balances` and
//!   `accounts.balance_derived`.
//! - 11: sync log (#238): table `sync_log`, local to the device.
//! - 12: SEPA references on bookings: `end_to_end_id`, `mandate_id`, `creditor_id`.
//! - 13: Sachwerte (#87): items, dated quantities and values, metal prices.
//! - 14: securities transactions (#314): table `security_transactions`, extra
//!   position fields on `holdings` (previous day, profit/loss, custody type).
//! - 15: item references on bookings (#354): `transactions.item_refs`, the
//!   product ids (ASIN) of an Amazon order, one entry per item.
//! - 16: asset classification (#351): persistent per-field user overrides and local holding metadata.
//! - 17: scheduled transfers (#240): table `planned_transfers`.
//! - 18: standing orders (#239): table `standing_orders`, replaced per fetch.
//! - 19: repair: vaults that an early draft of the scheduled transfers had migrated as "15" skipped
//!   step 15 and lack `transactions.item_refs`; adds it when missing.
//! - 20: direct debit mandates and creditor IDs (#143): tables `mandates`, `creditor_ids`,
//!   `mandate_deletions` (marks for the vault merge).
//! - 21: `planned_transfers.kind` (#143): `transfer` or `debit` (submitted collection, incoming).
//! - 22: `mandates.returns_ack` (#143): returns acknowledged up to this date.
//! - 23: tags on bookings (#228): tables `tags`, `transaction_tags` and the
//!   edit stamp `transactions.tags_ms`.
//! - 24: rules for several accounts (#562): table `rule_accounts`; `rules.account_id` mirrors the
//!   lowest id of the set (foreign key / older readers), the table is the source of truth.
//! - 25: index `tx_amount_date`: the payment-service transfer pairing
//!   (`vault/transfer_pairs.rs`) looks up counter bookings by amount and date.
//!
//! - 26: native currency of a depot position and its prices (#625): `holdings.currency`, `native_*_cents`.
//! - 27: built-in category and account-group identities, separate from names (#678).
//! - 28: rules that set tags (#580): `rules.tag_uuids`, and `rules.category_id` may be NULL
//!   (a rule that only adds tags); the table is rebuilt for the dropped NOT NULL.
//! - 29: `depot_values`: value of a securities depot per day, one point per fetch.
//! - 30: `tx_date_id` covers `deleted_ms`, `account_id` and `category_id` ([`TX_DATE_ID_COVERING`]).
//! - 31: `booking_documents.seen_at`: read status of documents (NULL = unread); documents that exist
//!   count as read; the list index carries the column, a partial index finds the unread ones.
//! - 32: account notes sync (#980): `accounts.note` / `note_ms` replace the vault setting
//!   `account.note.<id>` ([`account_notes`]).
//! - 33: `hidden_recipients` (#978): IBANs left out of the transfer suggestions; index
//!   `tx_recipients` over the outgoing bookings with an IBAN.
//! - 34: repair (#998): accounts the MoneyMoney import archived because their last refresh there
//!   was cancelled (`cancel_tsp`) are active again ([`unarchive_cancelled_refreshes`]).
//!
//! Schema changes are a new step at the end of [`STEPS`]; never edit an
//! existing step. After a bump, run
//! `cargo run -p girocrab-core --example make_fixture_vault` and commit the
//! new fixture (see `docs/ARCHITECTURE.md`).

use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};

enum Step {
    Sql(&'static str),
    Hook(fn(&Connection) -> CoreResult<()>),
}

const STEPS: &[Step] = &[
    Step::Sql(BASELINE), // 1
    Step::Sql(""),       // 2–8: reached through probes before #206
    Step::Sql(""),
    Step::Sql(""),
    Step::Sql(""),
    Step::Sql(""),
    Step::Sql(""),
    Step::Sql(""),
    Step::Hook(repair_to_v8),                                  // 9
    Step::Hook(opening_balances),                              // 10
    Step::Sql(SYNC_LOG),                                       // 11
    Step::Hook(sepa_refs),                                     // 12
    Step::Hook(tangible_assets),                               // 13
    Step::Hook(security_transactions),                         // 14
    Step::Hook(item_refs),                                     // 15
    Step::Sql(crate::asset_classification::store::SCHEMA),     // 16: asset classification (#351)
    Step::Hook(crate::planned::planned_transfers),             // 17
    Step::Hook(crate::standing_orders::standing_orders_table), // 18
    Step::Hook(item_refs),                                     // 19 (repair, see above)
    Step::Hook(crate::mandates::mandates_tables),              // 20
    Step::Hook(crate::planned::planned_kind),                  // 21
    Step::Hook(crate::mandates::mandate_return_ack),           // 22
    Step::Hook(crate::tags::tags_tables),                      // 23
    Step::Sql(RULE_ACCOUNTS),                                  // 24
    Step::Sql(TX_AMOUNT_DATE),                                 // 25
    Step::Hook(holding_native_currency),                       // 26
    Step::Hook(builtin_roles),                                 // 27 (#678)
    Step::Hook(rule_tags),                                     // 28 (#580)
    Step::Sql(crate::depot_values::SCHEMA),                    // 29: depot value history
    Step::Sql(TX_DATE_ID_COVERING),                            // 30
    Step::Hook(document_seen),                                 // 31: read status of documents
    Step::Hook(account_notes),                                 // 32: account notes sync (#980)
    Step::Sql(crate::recipients::SCHEMA),                      // 33: hidden recipients (#978)
    Step::Hook(unarchive_cancelled_refreshes),                 // 34: repair (#998)
];

/// Step 32 (#980): the account note moves from the vault setting `account.note.<id>` (keyed by
/// this device's account id and never merged) to `accounts.note` with its edit stamp `note_ms`,
/// so it syncs like the account name. Moved notes get [`MOVED_NOTE_MS`]; a peer's snapshot from
/// an older build is migrated the same way before every merge. Idempotent.
///
/// [`MOVED_NOTE_MS`]: crate::user_groups::MOVED_NOTE_MS
fn account_notes(conn: &Connection) -> CoreResult<()> {
    for (col, def) in [
        ("note", "TEXT NOT NULL DEFAULT ''"),
        ("note_ms", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !has_column(conn, "main", "accounts", col)? {
            conn.execute_batch(&format!("ALTER TABLE accounts ADD COLUMN {col} {def};"))?;
        }
    }
    conn.execute(
        "UPDATE accounts SET note = (SELECT value FROM settings WHERE key = 'account.note.' || accounts.id),
                note_ms = ?1
          WHERE note = ''
            AND EXISTS (SELECT 1 FROM settings WHERE key = 'account.note.' || accounts.id AND value != '')",
        [crate::user_groups::MOVED_NOTE_MS],
    )?;
    conn.execute("DELETE FROM settings WHERE key GLOB 'account.note.*'", [])?;
    Ok(())
}

/// An archive the app (`set_account_archived`, also the account selection) or the MoneyMoney
/// import sets carries its time and its stamp from the same second. Step 34 takes an archive
/// whose time lies more than this before its stamp for one the import of v0.8.5 made.
pub(crate) const ARCHIVE_STAMP_GAP_MS: i64 = 2000;

/// Step 34 (#998): the MoneyMoney import of v0.8.5 read `cancel_tsp` (when the last refresh of an
/// account was cancelled there, e.g. a TAN not confirmed) as "closed" and archived accounts that
/// are in use: they left the totals and syncs skipped them. Such an archive carries MoneyMoney's
/// earlier time next to the import's stamp (see [`ARCHIVE_STAMP_GAP_MS`]); only accounts the
/// importer owns were archived that way, and the vault merge copies time and stamp as a pair.
/// The reactivation is stamped, so the merge carries it to the other devices. Some of these
/// accounts are closed in MoneyMoney after all, which the vault cannot tell: they are noted
/// ([`moneymoney_db::ARCHIVE_UNKNOWN_KEY`]) so the next import gives them MoneyMoney's state.
///
/// [`moneymoney_db::ARCHIVE_UNKNOWN_KEY`]: crate::importers::moneymoney_db::ARCHIVE_UNKNOWN_KEY
fn unarchive_cancelled_refreshes(conn: &Connection) -> CoreResult<()> {
    let ids: Vec<i64> = conn
        .prepare(
            "UPDATE accounts SET archived_unix = NULL, archived_ms = ?1
              WHERE archived_unix IS NOT NULL AND archived_unix * 1000 < archived_ms - ?2
                AND deleted_ms IS NULL AND external_ref LIKE 'moneymoney-db:%'
              RETURNING id",
        )?
        .query_map([crate::dates::now_ms(), ARCHIVE_STAMP_GAP_MS], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    crate::importers::moneymoney_db::note_archive_unknown(conn, &ids)
}

/// Step 31: `booking_documents.seen_at` (NULL = unread). The documents a vault has already count
/// as read: an update must not turn a whole archive into "new". The index the document list is
/// read from carries the column (it must not read through the PDFs), and a partial index finds
/// the unread ones for the badge. Idempotent.
fn document_seen(conn: &Connection) -> CoreResult<()> {
    if !has_column(conn, "main", "booking_documents", "seen_at")? {
        conn.execute_batch(
            "ALTER TABLE booking_documents ADD COLUMN seen_at INTEGER;
             UPDATE booking_documents SET seen_at = created_at;",
        )?;
    }
    conn.execute_batch(
        "DROP INDEX IF EXISTS booking_documents_info;
         CREATE INDEX booking_documents_info ON booking_documents(
             doc_date DESC, id DESC, booking_id, account_id, title, doc_type, mime_type, file_name,
             length(pdf_bytes), source_url, created_at, seen_at);
         CREATE INDEX IF NOT EXISTS booking_documents_unseen ON booking_documents(id) WHERE seen_at IS NULL;",
    )?;
    Ok(())
}

/// Step 28 (#580): rules may set tags (`tag_uuids`, a JSON array of tag uuids, so a
/// rename or a merge on any device keeps them) and may leave the category out. SQLite
/// can't drop a NOT NULL in place: the table is rebuilt from its own columns (whatever
/// earlier steps and repairs added), with its foreign keys, indexes and triggers.
fn rule_tags(conn: &Connection) -> CoreResult<()> {
    let nullable: bool = conn.query_row(
        "SELECT \"notnull\" = 0 FROM pragma_table_info('rules') WHERE name = 'category_id'",
        [],
        |r| r.get(0),
    )?;
    if !nullable {
        let cols: Vec<(String, String, bool, Option<String>, bool)> = conn
            .prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info('rules') ORDER BY cid")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? > 0)))?
            .collect::<Result<_, _>>()?;
        let fks: Vec<(String, String, String, String)> = conn
            .prepare("SELECT \"from\", \"table\", \"to\", on_delete FROM pragma_foreign_key_list('rules')")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let others: Vec<String> = conn
            .prepare(
                "SELECT sql FROM sqlite_master WHERE tbl_name = 'rules'
                 AND type IN ('index', 'trigger') AND sql IS NOT NULL",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let defs: Vec<String> = cols
            .iter()
            .map(|(name, ty, notnull, default, pk)| {
                let mut d = format!("{name} {ty}");
                if *pk {
                    d.push_str(" PRIMARY KEY");
                }
                if *notnull && name != "category_id" {
                    d.push_str(" NOT NULL");
                }
                if let Some(v) = default {
                    d.push_str(&format!(" DEFAULT {v}"));
                }
                if let Some((_, table, to, on_delete)) = fks.iter().find(|f| &f.0 == name) {
                    d.push_str(&format!(" REFERENCES {table}({to}) ON DELETE {on_delete}"));
                }
                d
            })
            .collect();
        let names: Vec<&str> = cols.iter().map(|c| c.0.as_str()).collect();
        conn.execute_batch(&format!(
            "CREATE TABLE rules_v28 ({defs});
             INSERT INTO rules_v28({names}) SELECT {names} FROM rules;
             DROP TABLE rules;
             ALTER TABLE rules_v28 RENAME TO rules;",
            defs = defs.join(", "),
            names = names.join(", ")
        ))?;
        for sql in others {
            conn.execute_batch(&sql)?;
        }
    }
    if !has_column(conn, "main", "rules", "tag_uuids")? {
        conn.execute_batch("ALTER TABLE rules ADD COLUMN tag_uuids TEXT NOT NULL DEFAULT '';")?;
    }
    Ok(())
}

/// Step 26 (#625): native currency of a position and its price/value in it
/// (`NULL` / "" = same as the account). Idempotent like [`item_refs`].
fn holding_native_currency(conn: &Connection) -> CoreResult<()> {
    for (col, def) in [
        ("currency", "TEXT NOT NULL DEFAULT ''"),
        ("native_price_cents", "INTEGER"),
        ("native_purchase_price_cents", "INTEGER"),
        ("native_value_cents", "INTEGER"),
    ] {
        if !has_column(conn, "main", "holdings", col)? {
            conn.execute_batch(&format!("ALTER TABLE holdings ADD COLUMN {col} {def};"))?;
        }
    }
    Ok(())
}

/// Step 27: immutable built-in identity, separate from editable names. The
/// original German catalog keys are stable role identifiers, never user input.
fn builtin_roles(conn: &Connection) -> CoreResult<()> {
    // Development vaults may already carry one of the two columns. Repair
    // each table independently, without relabelling explicitly custom rows.
    let categories_need_roles = !has_column(conn, "main", "categories", "builtin_role")?;
    let groups_need_roles = !has_column(conn, "main", "account_groups", "builtin_role")?;
    if categories_need_roles {
        conn.execute_batch("ALTER TABLE categories ADD COLUMN builtin_role TEXT;")?;
    }
    if groups_need_roles {
        conn.execute_batch(
            "CREATE TABLE account_groups_roles (
                id INTEGER PRIMARY KEY, name TEXT NOT NULL,
                sf_symbol TEXT NOT NULL DEFAULT 'folder', sort_order INTEGER NOT NULL DEFAULT 0,
                builtin_role TEXT);
            INSERT INTO account_groups_roles(id,name,sf_symbol,sort_order)
                SELECT id,name,sf_symbol,sort_order FROM account_groups;
            DROP TABLE account_groups;
            ALTER TABLE account_groups_roles RENAME TO account_groups;
            CREATE UNIQUE INDEX account_groups_role ON account_groups(builtin_role)
                WHERE builtin_role IS NOT NULL;
            CREATE UNIQUE INDEX account_groups_custom_name ON account_groups(name)
                WHERE builtin_role IS NULL;",
        )?;
    }
    if categories_need_roles {
        // Legacy vaults have no provenance flag. Only recognise unchanged catalog
        // entries with the starter icon, colour and parent, never by name alone.
        for (name, symbol, color, kids) in crate::demo::STARTER_TREE {
            conn.execute(
                "UPDATE categories SET builtin_role=?1 WHERE name=?1 AND parent_id IS NULL
            AND sf_symbol=?2 AND color_hex=?3 AND name_ms=0",
                rusqlite::params![name, symbol, color],
            )?;
            for (kid, symbol) in *kids {
                conn.execute(
                    "UPDATE categories SET builtin_role=?1 WHERE name=?1
                AND parent_id IN (SELECT id FROM categories WHERE builtin_role=?2)
                AND sf_symbol=?3 AND color_hex=?4 AND name_ms=0",
                    rusqlite::params![kid, name, symbol, color],
                )?;
            }
        }
    }
    if groups_need_roles {
        for kind in [
            crate::models::AccountKind::Checking,
            crate::models::AccountKind::CreditCard,
            crate::models::AccountKind::Brokerage,
            crate::models::AccountKind::Loan,
        ] {
            let (name, symbol, order) = crate::connectors::group_for(kind);
            conn.execute("UPDATE account_groups SET builtin_role=?1 WHERE name=?1 AND sf_symbol=?2 AND sort_order=?3",
            rusqlite::params![name, symbol, order])?;
        }
        conn.execute(
            "UPDATE account_groups SET builtin_role='Importierte Konten'
        WHERE name='Importierte Konten' AND sf_symbol='archivebox' AND sort_order=90",
            [],
        )?;
    }
    if categories_need_roles {
        // Legacy imports used this distinctive icon/colour combination. Preserve
        // their transfer semantics without granting translation of the name.
        conn.execute(
            "UPDATE categories SET builtin_role='import:transfers'
        WHERE builtin_role IS NULL AND sf_symbol='tag' AND color_hex='#8E8E93'
        AND lower(name) IN ('umbuchung','umbuchungen','geldtransit','transfer','transfers')",
            [],
        )?;
    }
    Ok(())
}

/// Step 30. A count over a date range (`count_transactions` with a period, the list's
/// jump to a day) reads only the index: besides the date it checks `deleted_ms` and joins
/// accounts and categories. Without these columns it read every booking row in the range
/// (eleven years of a 44,000-booking vault: 31 ms → 8.5 ms).
const TX_DATE_ID_COVERING: &str = "
DROP INDEX IF EXISTS tx_date_id;
CREATE INDEX tx_date_id ON transactions(booking_date DESC, id DESC, deleted_ms, account_id, category_id);
";

/// Step 25. Not partial on `deleted_ms IS NULL`: SQLite then picked it for
/// plain scans of live bookings (`list_categories`), six times slower.
const TX_AMOUNT_DATE: &str = "
CREATE INDEX IF NOT EXISTS tx_amount_date ON transactions(amount_cents, booking_date);
";

/// Step 24 (#562): a rule can be restricted to several accounts.
const RULE_ACCOUNTS: &str = "
CREATE TABLE IF NOT EXISTS rule_accounts (
    rule_id    INTEGER NOT NULL REFERENCES rules(id)    ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (rule_id, account_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS rule_accounts_account ON rule_accounts(account_id);
INSERT OR IGNORE INTO rule_accounts(rule_id, account_id)
    SELECT id, account_id FROM rules WHERE account_id IS NOT NULL;
";

/// Step 14 (#314): securities transactions (buy, sell, dividend, fee, tax …)
/// of a depot, beside the cash bookings: they never count as cash flow and
/// have no category. `UNIQUE(account_id, external_id)` is the merge key.
/// `cash_transaction_id` is the matching cash booking on a settlement account
/// (local id, set by `link_cash_bookings`, not merged). Also the position
/// fields the banks report besides the stock. Idempotent like the steps above.
/// Step 15 (#354): product ids of an order's items (Amazon ASIN), comma
/// separated, one entry per item in purpose order ("" entry = unknown).
/// "" = none. Idempotent like [`sepa_refs`].
fn item_refs(conn: &Connection) -> CoreResult<()> {
    if !has_column(conn, "main", "transactions", "item_refs")? {
        conn.execute_batch(
            "ALTER TABLE transactions ADD COLUMN item_refs TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

fn security_transactions(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS security_transactions (
            id                     INTEGER PRIMARY KEY,
            account_id             INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            external_id            TEXT    NOT NULL,
            booking_date           TEXT    NOT NULL,
            settlement_date        TEXT    NOT NULL DEFAULT '',
            isin                   TEXT    NOT NULL DEFAULT '',
            wkn                    TEXT    NOT NULL DEFAULT '',
            name                   TEXT    NOT NULL DEFAULT '',
            direction              TEXT    NOT NULL DEFAULT 'other', -- buy | sell | dividend | fee | tax | other
            quantity               REAL    NOT NULL DEFAULT 0,
            price_cents            INTEGER NOT NULL DEFAULT 0,
            currency               TEXT    NOT NULL DEFAULT 'EUR',
            value_cents            INTEGER NOT NULL DEFAULT 0,
            fees_cents             INTEGER NOT NULL DEFAULT 0,
            taxes_cents            INTEGER NOT NULL DEFAULT 0,
            accrued_interest_cents INTEGER NOT NULL DEFAULT 0,
            fx_rate                REAL,
            booking_text           TEXT    NOT NULL DEFAULT '',
            cash_transaction_id    INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
            changed_ms             INTEGER NOT NULL DEFAULT 0,
            deleted_ms             INTEGER,
            UNIQUE(account_id, external_id)
        );
        CREATE INDEX IF NOT EXISTS security_transactions_account
            ON security_transactions(account_id, booking_date, deleted_ms);
        CREATE INDEX IF NOT EXISTS security_transactions_isin
            ON security_transactions(isin, deleted_ms);",
    )?;
    for (col, def) in [
        ("prev_day_price_cents", "INTEGER"),
        ("prev_day_value_cents", "INTEGER"),
        ("profit_loss_purchase_cents", "INTEGER"),
        ("profit_loss_purchase_rel", "REAL"),
        ("profit_loss_prev_day_cents", "INTEGER"),
        ("profit_loss_prev_day_rel", "REAL"),
        ("custody_type", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !has_column(conn, "main", "holdings", col)? {
            conn.execute_batch(&format!("ALTER TABLE holdings ADD COLUMN {col} {def};"))?;
        }
    }
    Ok(())
}

/// Step 12: SEPA references of a booking, bank-independent (end-to-end id,
/// direct-debit mandate reference, creditor id). "" = unknown. Idempotent:
/// a vault rewound by a test or a development build may carry the columns.
fn sepa_refs(conn: &Connection) -> CoreResult<()> {
    for col in ["end_to_end_id", "mandate_id", "creditor_id"] {
        if !has_column(conn, "main", "transactions", col)? {
            conn.execute_batch(&format!(
                "ALTER TABLE transactions ADD COLUMN {col} TEXT NOT NULL DEFAULT '';"
            ))?;
        }
    }
    Ok(())
}

/// Step 13 (#87): Sachwerte – items, dated quantities and values, metal
/// prices. Idempotent: vaults of a development build of the feature already
/// carry the tables, maybe without later columns. Those builds numbered
/// this step 12, so such a vault never ran [`sepa_refs`]: run it here as well.
fn tangible_assets(conn: &Connection) -> CoreResult<()> {
    sepa_refs(conn)?;
    crate::assets::migrate(conn)
}

/// Step 11 (#238): the Abrufprotokoll. No foreign key: the log outlives a
/// deleted connection (its name is kept in the row). Not part of a vault merge.
const SYNC_LOG: &str = "
CREATE TABLE sync_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms           INTEGER NOT NULL,
    connection_id   INTEGER,
    connection_name TEXT    NOT NULL DEFAULT '',
    source          TEXT    NOT NULL DEFAULT 'manual', -- manual | auto | statements
    outcome         TEXT    NOT NULL,                  -- ok | error | cancelled
    message         TEXT    NOT NULL DEFAULT '',       -- scrubbed, see sync_log.rs
    imported        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX sync_log_ts ON sync_log(ts_ms);
";

/// Schema version of a vault written by this build.
pub(crate) const SCHEMA_VERSION: i64 = STEPS.len() as i64;

/// Step 10 (#212): balance computed from bookings (connector reports none)
/// plus the opening balance per account. Idempotent: vaults opened by a
/// development build of the feature already carry both at version 8 or 9.
fn opening_balances(conn: &Connection) -> CoreResult<()> {
    if !has_column(conn, "main", "accounts", "balance_derived")? {
        conn.execute_batch(
            "ALTER TABLE accounts ADD COLUMN balance_derived INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS opening_balances (
            account_id    INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
            opening_date  TEXT    NOT NULL DEFAULT '', -- YYYY-MM-DD, start of the day; '' = before the first booking
            opening_cents INTEGER,                     -- i64 cents; NULL = cleared
            updated_ms    INTEGER NOT NULL DEFAULT 0   -- newest edit wins in a vault merge
        );",
    )?;
    Ok(())
}

/// Refuses a vault written by a newer build, before anything touches the file.
pub(crate) fn check_not_too_new(conn: &Connection) -> CoreResult<()> {
    let vault_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if vault_version > SCHEMA_VERSION {
        return Err(CoreError::SchemaTooNew {
            vault_version,
            app_version: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Brings the vault to [`SCHEMA_VERSION`]; nothing changes if a step fails.
pub(crate) fn migrate(conn: &mut Connection) -> CoreResult<()> {
    check_not_too_new(conn)?;
    let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if done == SCHEMA_VERSION {
        return Ok(());
    }
    // SQLite's table-rebuild procedure requires foreign keys off outside the
    // transaction (step 27 removes the old UNIQUE(name) constraint). Restore
    // enforcement on both success and failure; validate before committing.
    let foreign_keys: bool = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = migrate_transaction(conn);
    conn.pragma_update(None, "foreign_keys", foreign_keys)?;
    result
}

fn migrate_transaction(conn: &mut Connection) -> CoreResult<()> {
    let tx = conn.transaction()?;
    // read again inside the transaction: another connection may have migrated meanwhile
    let done: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if done >= SCHEMA_VERSION {
        return Ok(());
    }
    for step in &STEPS[done as usize..] {
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Hook(f) => f(&tx)?,
        }
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    let invalid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |r| r.get(0),
    )?;
    if invalid {
        return Err(crate::error::CoreError::Database {
            message: "Schema migration would violate foreign keys".into(),
        });
    }
    tx.commit()?;
    Ok(())
}

/// Whether database `schema` (`main` or an attached one) has `table`; a
/// view of that name does not count.
pub(crate) fn has_table(conn: &Connection, schema: &str, table: &str) -> CoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_list(?1) WHERE schema = ?2 AND type = 'table')",
        [table, schema],
        |r| r.get(0),
    )?)
}

/// Whether `table` in database `schema` (`main` or an attached one) has `col`;
/// false if the table is missing.
pub(crate) fn has_column(
    conn: &Connection,
    schema: &str,
    table: &str,
    col: &str,
) -> CoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1, ?2) WHERE name = ?3)",
        [table, schema, col],
        |r| r.get(0),
    )?)
}

/// Step 9: the probe-based schema setup of schema 8, run once on every vault.
/// Each part checks what is there (never `user_version`), so it repairs a
/// vault left half-migrated by an older build and is a no-op on [`BASELINE`].
fn repair_to_v8(conn: &Connection) -> CoreResult<()> {
    // v5: loan_principal_cents -> initial_balance_cents (v1 had neither column)
    if !has_column(conn, "main", "accounts", "initial_balance_cents")? {
        if has_column(conn, "main", "accounts", "loan_principal_cents")? {
            conn.execute_batch(
                "ALTER TABLE accounts RENAME COLUMN loan_principal_cents TO initial_balance_cents;",
            )?;
        } else {
            conn.execute_batch("ALTER TABLE accounts ADD COLUMN initial_balance_cents INTEGER;")?;
        }
    }
    // v3: merchant_logo_cache(domain, image_bytes, updated_at, source) – a cache,
    // rebuilt only if it is missing or still has the v2 layout
    if !has_column(conn, "main", "merchant_logo_cache", "source")? {
        conn.execute_batch(
            "DROP TABLE IF EXISTS merchant_logo_cache;
             CREATE TABLE merchant_logo_cache (domain TEXT PRIMARY KEY, image_bytes BLOB,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')), source TEXT NOT NULL DEFAULT '');",
        )?;
    }
    // v6: user notes + import fingerprint
    if !has_column(conn, "main", "transactions", "note")? {
        conn.execute_batch("ALTER TABLE transactions ADD COLUMN note TEXT NOT NULL DEFAULT '';")?;
    }
    if !has_column(conn, "main", "transactions", "import_fp")? {
        conn.execute_batch(
            "ALTER TABLE transactions ADD COLUMN import_fp TEXT NOT NULL DEFAULT '';",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS tx_import_fp ON transactions(account_id, import_fp) WHERE import_fp != '';",
    )?;
    // accounts.access_revoked (PSD2 access withdrawn)
    if !has_column(conn, "main", "accounts", "access_revoked")? {
        conn.execute_batch(
            "ALTER TABLE accounts ADD COLUMN access_revoked INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    // transactions.purpose_code / gvc / salary_flag (salary detection)
    for (col, def) in [
        ("purpose_code", "TEXT NOT NULL DEFAULT ''"),
        ("gvc", "TEXT NOT NULL DEFAULT ''"),
        ("salary_flag", "INTEGER"),
    ] {
        if !has_column(conn, "main", "transactions", col)? {
            conn.execute_batch(&format!("ALTER TABLE transactions ADD COLUMN {col} {def};"))?;
        }
    }
    // v4: encrypted documents (Beleg-Abruf & PostBox)
    conn.execute_batch(crate::documents::DOC_SCHEMA)?;
    if !has_column(conn, "main", "booking_documents", "exported_at")? {
        conn.execute_batch("ALTER TABLE booking_documents ADD COLUMN exported_at INTEGER;")?;
    }
    conn.execute_batch(crate::insights::OVERRIDE_SCHEMA)?;
    conn.execute_batch(crate::vault::HOLDINGS_SCHEMA)?;
    conn.execute_batch(crate::planning::PLANNING_SCHEMA)?;
    conn.execute_batch(crate::recurring::RECURRING_SCHEMA)?;
    // v7: user-defined account groups
    crate::user_groups::migrate(conn)?;
    // v8: sync keys, per-field edit stamps and soft deletes
    crate::sync_merge::migrate(conn)?;
    // manual loans (#36)
    conn.execute_batch(crate::loans::LOAN_SCHEMA)?;
    // after all column migrations: the indexes need the newest columns
    conn.execute_batch(crate::vault::PERF_INDEXES)?;
    Ok(())
}

/// Step 1: the schema of a new vault – identical (up to layout and comments)
/// to what schema 8 built step by step; `tests/fixtures/schema-v9.sql` pins it.
const BASELINE: &str = r#"
CREATE TABLE bank_connections (
    id              INTEGER PRIMARY KEY,
    connector       TEXT    NOT NULL,              -- ConnectorKind::as_str()
    display_name    TEXT    NOT NULL,
    login_name      TEXT    NOT NULL DEFAULT '',
    bank_code       TEXT    NOT NULL DEFAULT '',
    sf_symbol       TEXT    NOT NULL DEFAULT 'building.columns',
    state_json      TEXT    NOT NULL DEFAULT '{}',  -- connector state (FinTS system id, BPD, PSD2 session ...)
    last_sync_unix  INTEGER,
    created_unix    INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    created_ms      INTEGER NOT NULL DEFAULT 0,
    deleted_ms      INTEGER                        -- NULL = live (sync tombstone)
);

CREATE TABLE account_groups (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    sf_symbol   TEXT    NOT NULL DEFAULT 'folder',
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE accounts (
    id              INTEGER PRIMARY KEY,
    connection_id   INTEGER REFERENCES bank_connections(id) ON DELETE SET NULL,
    group_id        INTEGER NOT NULL REFERENCES account_groups(id),
    external_ref    TEXT    NOT NULL DEFAULT '',   -- IBAN / bank account id / PSD2 uid
    name            TEXT    NOT NULL,
    bank_name       TEXT    NOT NULL DEFAULT '',
    owner           TEXT    NOT NULL DEFAULT '',
    iban            TEXT    NOT NULL DEFAULT '',
    account_number  TEXT    NOT NULL DEFAULT '',
    currency        TEXT    NOT NULL DEFAULT 'EUR',
    kind            TEXT    NOT NULL DEFAULT 'checking',
    balance_cents   INTEGER NOT NULL DEFAULT 0,    -- i64 cents
    balance_date    TEXT    NOT NULL DEFAULT '',
    sf_symbol       TEXT    NOT NULL DEFAULT 'building.columns',
    sort_order      INTEGER NOT NULL DEFAULT 0,
    initial_balance_cents INTEGER,                 -- original amount of a Darlehen (i64 cents)
    access_revoked  INTEGER NOT NULL DEFAULT 0,    -- bank / PSD2 consent no longer grants access
    user_group_uuid TEXT,                          -- user_groups.uuid (NULL = not placed)
    user_sort_order INTEGER NOT NULL DEFAULT 0,
    placement_ms    INTEGER NOT NULL DEFAULT 0,
    name_ms         INTEGER NOT NULL DEFAULT 0,
    color_hex       TEXT,
    color_ms        INTEGER NOT NULL DEFAULT 0,
    symbol_ms       INTEGER NOT NULL DEFAULT 0,
    archived_unix   INTEGER,
    archived_ms     INTEGER NOT NULL DEFAULT 0,
    created_ms      INTEGER NOT NULL DEFAULT 0,
    deleted_ms      INTEGER,
    UNIQUE (connection_id, external_ref)
);

CREATE TABLE categories (
    id            INTEGER PRIMARY KEY,
    parent_id     INTEGER REFERENCES categories(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    sf_symbol     TEXT    NOT NULL DEFAULT 'tag',
    color_hex     TEXT    NOT NULL DEFAULT '#8E8E93',
    budget_cents  INTEGER,                         -- optional monthly budget, i64 cents
    sort_order    INTEGER NOT NULL DEFAULT 0,
    uuid          TEXT,                            -- sync key
    name_ms       INTEGER NOT NULL DEFAULT 0,
    parent_ms     INTEGER NOT NULL DEFAULT 0,
    symbol_ms     INTEGER NOT NULL DEFAULT 0,
    color_ms      INTEGER NOT NULL DEFAULT 0,
    budget_ms     INTEGER NOT NULL DEFAULT 0,
    order_ms      INTEGER NOT NULL DEFAULT 0,
    deleted_ms    INTEGER,
    merged_into   TEXT                             -- tombstone of a combined category → survivor uuid
);

CREATE UNIQUE INDEX categories_unique_name
    ON categories(IFNULL(parent_id, 0), name) WHERE deleted_ms IS NULL;
CREATE UNIQUE INDEX categories_uuid ON categories(uuid);

CREATE TABLE rules (
    id                INTEGER PRIMARY KEY,
    name              TEXT    NOT NULL,
    category_id       INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    field             TEXT    NOT NULL DEFAULT 'any',
    match_kind        TEXT    NOT NULL DEFAULT 'keyword',   -- keyword | regex | expression
    pattern           TEXT    NOT NULL,
    case_sensitive    INTEGER NOT NULL DEFAULT 0,
    min_amount_cents  INTEGER,
    max_amount_cents  INTEGER,
    account_id        INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
    priority          INTEGER NOT NULL DEFAULT 100,
    enabled           INTEGER NOT NULL DEFAULT 1,
    uuid              TEXT,                                 -- sync key
    updated_ms        INTEGER NOT NULL DEFAULT 0,
    deleted_ms        INTEGER,
    merged_into       TEXT
);

CREATE UNIQUE INDEX rules_uuid ON rules(uuid);

CREATE TABLE transactions (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    external_id        TEXT    NOT NULL,           -- de-duplication key from the bank
    booking_date       TEXT    NOT NULL,           -- YYYY-MM-DD
    value_date         TEXT    NOT NULL DEFAULT '',
    amount_cents       INTEGER NOT NULL,           -- i64 cents, negative = debit
    currency           TEXT    NOT NULL DEFAULT 'EUR',
    counterparty       TEXT    NOT NULL DEFAULT '',
    counterparty_iban  TEXT    NOT NULL DEFAULT '',
    purpose            TEXT    NOT NULL DEFAULT '',
    booking_text       TEXT    NOT NULL DEFAULT '',
    category_id        INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    category_source    TEXT    NOT NULL DEFAULT 'none',   -- none | rule | manual
    rule_id            INTEGER REFERENCES rules(id) ON DELETE SET NULL,
    checked            INTEGER NOT NULL DEFAULT 0,
    imported_unix      INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    note               TEXT    NOT NULL DEFAULT '',        -- user note (MoneyMoney import / detail view)
    import_fp          TEXT    NOT NULL DEFAULT '',        -- MoneyMoney de-duplication fingerprint
    purpose_code       TEXT    NOT NULL DEFAULT '',        -- SEPA purpose code (SALA, PENS, …)
    gvc                TEXT    NOT NULL DEFAULT '',        -- German Geschäftsvorfallcode (153, …)
    salary_flag        INTEGER,                            -- NULL = auto, 1 = salary, 0 = not salary (user)
    category_ms        INTEGER NOT NULL DEFAULT 0,         -- per-field edit stamps (sync)
    note_ms            INTEGER NOT NULL DEFAULT 0,
    checked_ms         INTEGER NOT NULL DEFAULT 0,
    text_ms            INTEGER NOT NULL DEFAULT 0,
    splits_ms          INTEGER NOT NULL DEFAULT 0,
    deleted_ms         INTEGER,                            -- NULL = live
    UNIQUE (account_id, external_id)
);

CREATE INDEX tx_account_date ON transactions(account_id, booking_date DESC);
CREATE INDEX tx_category ON transactions(category_id);
CREATE INDEX tx_import_fp ON transactions(account_id, import_fp) WHERE import_fp != '';
-- counts filter `deleted_ms IS NULL`: these indexes must cover it
CREATE INDEX tx_rule ON transactions(rule_id, deleted_ms);
CREATE INDEX tx_account_category ON transactions(account_id, category_id, deleted_ms);
CREATE INDEX tx_account_checked ON transactions(account_id, checked, deleted_ms);
CREATE INDEX tx_date_id ON transactions(booking_date DESC, id DESC);
CREATE INDEX tx_salary_marked ON transactions(counterparty_iban) WHERE salary_flag = 1;

CREATE TABLE merchant_logo_cache (
    domain        TEXT PRIMARY KEY,
    image_bytes   BLOB,                            -- PNG/ICO; NULL/empty = nothing found (negative cache)
    updated_at    INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    source        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    is_secret  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE booking_documents (
    id          INTEGER PRIMARY KEY,
    booking_id  INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    title       TEXT    NOT NULL DEFAULT '',
    doc_type    TEXT    NOT NULL DEFAULT 'beleg',   -- beleg | abrechnung | kontoauszug | postbox | steuer
    mime_type   TEXT    NOT NULL DEFAULT 'application/pdf',
    file_name   TEXT    NOT NULL DEFAULT '',
    pdf_bytes   BLOB    NOT NULL,
    source_url  TEXT    NOT NULL DEFAULT '',
    doc_date    TEXT    NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    exported_at INTEGER                                -- copied to the mirror folder (GitHub #57)
);

CREATE UNIQUE INDEX booking_documents_source ON booking_documents(source_url) WHERE source_url != '';
CREATE INDEX booking_documents_booking ON booking_documents(booking_id);
CREATE INDEX booking_documents_account ON booking_documents(account_id);
CREATE INDEX booking_documents_info ON booking_documents(
    doc_date DESC, id DESC, booking_id, account_id, title, doc_type, mime_type, file_name,
    length(pdf_bytes), source_url, created_at);

CREATE TABLE merchant_domain_overrides (
    merchant_key TEXT PRIMARY KEY,
    domain       TEXT NOT NULL,
    updated_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

CREATE TABLE holdings (
    id                   INTEGER PRIMARY KEY,
    account_id           INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    isin                 TEXT    NOT NULL DEFAULT '',
    wkn                  TEXT    NOT NULL DEFAULT '',
    name                 TEXT    NOT NULL DEFAULT '',
    quantity             REAL    NOT NULL DEFAULT 0,
    current_price_cents  INTEGER NOT NULL DEFAULT 0,
    purchase_price_cents INTEGER NOT NULL DEFAULT 0,
    value_cents          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX holdings_account ON holdings(account_id);

CREATE TABLE transaction_splits (
    id             INTEGER PRIMARY KEY,
    transaction_id INTEGER NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    category_id    INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    amount_cents   INTEGER NOT NULL,
    note           TEXT    NOT NULL DEFAULT ''
);

CREATE INDEX split_tx ON transaction_splits(transaction_id);
CREATE INDEX split_cat ON transaction_splits(category_id);

CREATE TABLE transfer_templates (
    id                   INTEGER PRIMARY KEY,
    name                 TEXT    NOT NULL,
    recipient_name       TEXT    NOT NULL,
    recipient_iban       TEXT    NOT NULL,
    recipient_bic        TEXT    NOT NULL DEFAULT '',
    default_amount_cents INTEGER,
    default_purpose      TEXT    NOT NULL DEFAULT ''
);

CREATE TABLE recurring_series (
    id            INTEGER PRIMARY KEY,
    series_key    TEXT    NOT NULL UNIQUE,
    status        TEXT    NOT NULL DEFAULT 'manual',   -- manual | ignored
    interval      TEXT    NOT NULL DEFAULT 'monthly',  -- monthly | quarterly | halfyearly | yearly | custom
    custom_dates  TEXT    NOT NULL DEFAULT '',         -- custom: comma-separated MM-DD
    created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

CREATE TABLE user_groups (
    uuid             TEXT    PRIMARY KEY,
    name             TEXT    NOT NULL,
    sf_symbol        TEXT    NOT NULL DEFAULT 'folder',
    sort_order       INTEGER NOT NULL DEFAULT 0,
    include_in_total INTEGER NOT NULL DEFAULT 1,
    updated_ms       INTEGER NOT NULL DEFAULT 0,     -- last edit of any field
    name_ms          INTEGER NOT NULL DEFAULT 0,
    symbol_ms        INTEGER NOT NULL DEFAULT 0,
    total_ms         INTEGER NOT NULL DEFAULT 0,
    order_ms         INTEGER NOT NULL DEFAULT 0,
    deleted_ms       INTEGER,                      -- tombstone, kept for sync
    merged_into      TEXT,                         -- tombstone of a combined group → survivor
    parent_uuid      TEXT,                         -- subgroup of this group (NULL = top level)
    parent_ms        INTEGER NOT NULL DEFAULT 0,
    color_hex        TEXT,
    color_ms         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE loan_terms (
    account_id           INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    anchor_date          TEXT    NOT NULL,           -- YYYY-MM-DD, Stand at the end of this day
    anchor_balance_cents INTEGER NOT NULL,           -- outstanding balance (positive, i64 cents)
    interest_rate_ppm    INTEGER,                    -- nominal rate p.a. in millionths (4.99 % = 49900)
    match_expr           TEXT    NOT NULL DEFAULT '' -- search expression that links bookings ('' = none)
);

CREATE TABLE loan_links (
    transaction_id  INTEGER PRIMARY KEY REFERENCES transactions(id) ON DELETE CASCADE,
    loan_account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    source          TEXT    NOT NULL DEFAULT 'manual'  -- manual | rule | excluded
);

CREATE INDEX loan_links_loan ON loan_links(loan_account_id);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_at(version: usize) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for step in &STEPS[..version] {
            match step {
                Step::Sql(sql) => conn.execute_batch(sql).unwrap(),
                Step::Hook(f) => f(&conn).unwrap(),
            }
        }
        conn.pragma_update(None, "user_version", version as i64)
            .unwrap();
        conn
    }

    /// Step 32 (#980): notes move from the settings onto their accounts; notes of accounts
    /// that no longer exist go, other settings stay.
    #[test]
    fn account_notes_move_from_settings_to_the_account() {
        let mut conn = schema_at(31);
        conn.execute_batch(
            "INSERT INTO account_groups(name) VALUES ('G');
             INSERT INTO accounts(id, group_id, external_ref, name)
                 SELECT 1, id, 'a', 'Giro' FROM account_groups WHERE name = 'G'
                 UNION ALL SELECT 2, id, 'b', 'Bar' FROM account_groups WHERE name = 'G';
             INSERT INTO settings(key, value) VALUES
                 ('account.note.1', 'Reisekasse'), ('account.note.99', 'weg'),
                 ('ui.uncategorized_name', 'Sonstiges');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let notes = |conn: &Connection| -> Vec<(String, String, i64)> {
            conn.prepare("SELECT name, note, note_ms FROM accounts ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let expected = vec![
            ("Giro".to_string(), "Reisekasse".to_string(), 1),
            ("Bar".to_string(), String::new(), 0),
        ];
        assert_eq!(notes(&conn), expected);
        let keys: Vec<String> = conn
            .prepare("SELECT key FROM settings ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(keys, ["ui.uncategorized_name"]);
        // idempotent: a second run changes nothing
        account_notes(&conn).unwrap();
        assert_eq!(notes(&conn), expected);
    }

    #[test]
    fn table_and_column_probes_stay_in_their_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "ATTACH DATABASE ':memory:' AS other;
             CREATE TABLE other.tags (uuid TEXT);
             CREATE VIEW main.tags AS SELECT 1 AS uuid;",
        )
        .unwrap();
        assert!(has_table(&conn, "other", "tags").unwrap());
        assert!(!has_table(&conn, "main", "tags").unwrap());
        assert!(has_column(&conn, "other", "tags", "uuid").unwrap());
        assert!(!has_column(&conn, "other", "tags", "name").unwrap());
        assert!(!has_column(&conn, "main", "accounts", "uuid").unwrap());
    }

    #[test]
    fn failed_role_migration_rolls_back_and_restores_foreign_keys() {
        let mut conn = schema_at(25);
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
            INSERT INTO accounts(group_id,external_ref,name) VALUES (999,'test','Test');
            PRAGMA foreign_keys=ON;",
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        assert!(!has_column(&conn, "main", "categories", "builtin_role").unwrap());
        assert!(!has_column(&conn, "main", "account_groups", "builtin_role").unwrap());
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            25
        );
        assert!(
            conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
                .unwrap()
        );
    }

    #[test]
    fn partial_development_migration_preserves_custom_provenance() {
        let mut conn = schema_at(25);
        conn.execute_batch("ALTER TABLE categories ADD COLUMN builtin_role TEXT;
            INSERT INTO categories(name,sf_symbol,color_hex) VALUES ('Freizeit','sparkles','#2F8F9D');").unwrap();
        migrate(&mut conn).unwrap();
        assert!(has_column(&conn, "main", "account_groups", "builtin_role").unwrap());
        assert_eq!(
            conn.query_row("SELECT builtin_role FROM categories", [], |r| r
                .get::<_, Option<String>>(0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn roles_migrate_catalog_entries_but_not_custom_names() {
        let mut conn = schema_at(25);
        let (name, symbol, color, _) = crate::demo::STARTER_TREE
            .iter()
            .find(|r| r.0 == "Freizeit")
            .unwrap();
        conn.execute(
            "INSERT INTO categories(name,sf_symbol,color_hex) VALUES (?1,?2,?3)",
            rusqlite::params![name, symbol, color],
        )
        .unwrap();
        let builtin = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO categories(name,parent_id) VALUES ('Freizeit',?1)",
            [builtin],
        )
        .unwrap();
        let custom = conn.last_insert_rowid();
        conn.execute_batch("INSERT INTO account_groups(name,sf_symbol,sort_order) VALUES ('Kreditkarten','creditcard',2);
            INSERT INTO accounts(group_id,external_ref,name) VALUES (1,'test','Test');
            PRAGMA user_version=25;").unwrap();
        migrate(&mut conn).unwrap();
        let role = |conn: &Connection, id| {
            conn.query_row(
                "SELECT builtin_role FROM categories WHERE id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
        };
        assert_eq!(role(&conn, builtin).as_deref(), Some("Freizeit"));
        assert_eq!(role(&conn, custom), None);
        assert!(
            conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
                .unwrap()
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM accounts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // Reopening does not reassign provenance to subsequently created rows.
        migrate(&mut conn).unwrap();
        assert_eq!(role(&conn, custom), None);
    }

    #[test]
    fn schema_version_counts_the_steps() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 34);
    }

    /// #998: what the MoneyMoney import archived from `cancel_tsp` is active again and noted for
    /// the next import; an account the user archived, a deleted one, an archive from before the
    /// stamps and one the importer does not own stay as they are.
    #[test]
    fn accounts_archived_for_a_cancelled_refresh_are_reactivated() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO account_groups(name,sf_symbol,sort_order) VALUES ('Konten','building.columns',1);
             INSERT INTO accounts(group_id,external_ref,name,archived_unix,archived_ms,deleted_ms) VALUES
               (1,'moneymoney-db:1','Girokonto',1790000000,1790001000123,NULL),
               (1,'moneymoney-db:2','Alte Karte',1790002000,1790002000456,NULL),
               (1,'moneymoney-db:3','Gelöscht',1790000000,1790001000123,1790003000000),
               (1,'moneymoney-db:4','Früher archiviert',1700000000,0,NULL),
               (1,'DE89370400440532013000','Bankkonto',1790000000,1790001000123,NULL);
             PRAGMA user_version = 33;",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        // (id, archived_unix, archived_ms)
        let row = |name: &str| {
            conn.query_row(
                "SELECT id, archived_unix, archived_ms FROM accounts WHERE name = ?1",
                [name],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap()
        };
        let (giro, archived, stamp) = row("Girokonto");
        assert_eq!(archived, None);
        assert!(stamp > 1790001000123, "stamped, so the merge carries it");
        assert_eq!(row("Alte Karte").1, Some(1790002000));
        assert_eq!(row("Gelöscht").1, Some(1790000000));
        assert_eq!(row("Früher archiviert").1, Some(1700000000));
        assert_eq!(row("Bankkonto").1, Some(1790000000));
        assert_eq!(
            crate::importers::moneymoney_db::archive_unknown(&conn).unwrap(),
            vec![giro]
        );
    }

    /// A vault that an early draft migrated as version 15 (scheduled transfers
    /// in place of the item references) gets `item_refs` back.
    #[test]
    fn vault_that_skipped_item_refs_is_repaired() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE transactions DROP COLUMN item_refs; PRAGMA user_version = 15;",
        )
        .unwrap();
        assert!(!has_column(&conn, "main", "transactions", "item_refs").unwrap());
        migrate(&mut conn).unwrap();
        assert!(has_column(&conn, "main", "transactions", "item_refs").unwrap());
    }

    #[test]
    fn newer_version_is_refused_without_changes() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        match migrate(&mut conn) {
            Err(CoreError::SchemaTooNew {
                vault_version: 99,
                app_version,
            }) => assert_eq!(app_version, SCHEMA_VERSION),
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn failed_step_leaves_the_vault_untouched() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 10; DROP TABLE sync_log; CREATE TABLE sync_log (x);",
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 10);
    }
}
