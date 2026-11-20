//! Overall money totals vs. proof lists (#337, "Doppelzählung vermeiden").
//!
//! Bestellkonten (accounts of the Amazon order connector) start outside every
//! overall money figure; a per-account switch can add them back. Group
//! exclusions and archiving also leave the overall *flows*, and the three KPI
//! ways (`page.flow` with `respect_in_total`, `selection_flow_for_totals`,
//! dashboard / cashflow sums) all follow the same effective account set.
//! Booking lists, counts, paging, exports and the single-account view never
//! filter by it — a total context is not a proof context.
//!
//! Setup runs on an own connection to the vault file (`Tmp::conn`), the
//! assertions go through the public vault API.

use girocrab_core::models::{BookingFilter, PeriodGranularity};
use girocrab_core::vault::Vault;
use rusqlite::{Connection, OptionalExtension, params};

const KEY: [u8; 32] = [0x47; 32];

/// Own directory per vault, removed on drop.
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("girocrab-tests")
            .join(format!("{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into()
    }

    /// Own connection to the vault file (the Vault handle keeps its own).
    fn conn(&self, name: &str) -> Connection {
        let c = Connection::open(self.path(name)).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
            .unwrap();
        c
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn vault(tag: &str) -> (std::sync::Arc<Vault>, Tmp) {
    let tmp = Tmp::new(tag);
    let v = Vault::open(tmp.path("v.girocrab"), KEY.to_vec()).unwrap();
    (v, tmp)
}

/// One account, reusing a login of the same connector (`None` = no bank
/// connection) — like one bank login that reports several accounts.
fn add_account(
    c: &Connection,
    name: &str,
    kind: &str,
    connector: Option<&str>,
    balance: i64,
) -> i64 {
    c.execute(
        "INSERT OR IGNORE INTO account_groups(name) VALUES ('G')",
        [],
    )
    .unwrap();
    let group: i64 = c
        .query_row("SELECT id FROM account_groups WHERE name = 'G'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let con: Option<i64> = connector.map(|k| {
        let existing: Option<i64> = c
            .query_row(
                "SELECT id FROM bank_connections WHERE connector = ?1 ORDER BY id LIMIT 1",
                [k],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if let Some(id) = existing {
            return id;
        }
        c.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name) VALUES (?1, ?1, 'test')",
            [k],
        )
        .unwrap();
        c.last_insert_rowid()
    });
    c.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, currency, balance_cents)
         VALUES (?1, ?2, ?3, ?3, ?4, 'EUR', ?5)",
        params![con, group, name, kind, balance],
    )
    .unwrap();
    c.last_insert_rowid()
}

fn add_booking(c: &Connection, account: i64, ext: &str, date: &str, cents: i64, who: &str) {
    c.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
         VALUES (?1, ?2, ?3, ?4, ?5, 'Testprüfung 0815')",
        params![account, ext, date, cents, who],
    )
    .unwrap();
}

fn today() -> String {
    jiff::ZonedDateTime::now().date().to_string()
}

fn col(c: &Connection, sql: &str, id: i64) -> i64 {
    c.query_row(sql, [id], |r| r.get(0)).unwrap()
}

fn totals_filter() -> BookingFilter {
    BookingFilter {
        respect_in_total: true,
        ..Default::default()
    }
}

/// A bank account and an order-like account with identical bookings: one
/// shopping (-4 000) and one pay (+100 000) each, so "excluded" means exactly
/// "half" for every KPI. `classify` reads unknown negative text as Consumption
/// and positive as Income, so the amounts below are stable.
fn seeded_pair(c: &Connection) -> (i64, i64) {
    let a = add_account(c, "Giro Test", "checking", Some("fints:52090000"), 500_000);
    let b = add_account(c, "Bestell Test", "payment_service", None, 70_000);
    let d = today();
    add_booking(c, a, "a1", &d, -4_000, "Testmarkt Filiale");
    add_booking(c, a, "a2", &d, 100_000, "Testfirma Gehalt");
    add_booking(c, b, "b1", &d, -4_000, "Testmarkt Filiale");
    add_booking(c, b, "b2", &d, 100_000, "Testfirma Gehalt");
    (a, b)
}

/// Puts `account` into a new user group and returns its uuid.
fn in_new_group(v: &Vault, c: &Connection, account: i64, name: &str) -> String {
    let uuid = v.create_user_group(name.into(), "folder".into()).unwrap();
    c.execute(
        "UPDATE accounts SET user_group_uuid = ?1 WHERE id = ?2",
        params![uuid, account],
    )
    .unwrap();
    uuid
}

// ------------------------------------------------------- flows (E3, fail-first on old APIs)

/// E3: a group switched out of the total also leaves the overall flows — the
/// dashboard monthly values and the cashflow card agree with the filtered page.
#[test]
fn excluded_group_leaves_dashboard_and_cashflow() {
    let (v, tmp) = vault("totals-group-dash");
    let c = tmp.conn("v.girocrab");
    let (a, b) = seeded_pair(&c);
    let uuid = in_new_group(&v, &c, b, "Test-Gruppe");
    v.update_user_group(uuid, "Test-Gruppe".into(), "folder".into(), false)
        .unwrap();

    let d = jiff::ZonedDateTime::now().date();
    let month = format!("{:04}-{:02}", d.year(), d.month());
    let board = v.investment_dashboard(1).unwrap();
    let m = board.monthly.last().unwrap();
    assert_eq!(m.month, month);
    assert_eq!(m.income_cents, 100_000, "only the included account's pay");
    assert_eq!(
        m.consumption_cents, 4_000,
        "only the included account's shopping"
    );

    // the overview card is an overall money figure: totals context
    let cash = v
        .cashflow_by_period(PeriodGranularity::Month, 1, None, true)
        .unwrap();
    let cp = cash.last().unwrap();
    assert_eq!(cp.income_cents, 100_000);
    assert_eq!(cp.expense_cents, 4_000);

    // the booking list of the whole vault stays complete (proof)
    let full = v.list_transactions(BookingFilter::default()).unwrap();
    assert_eq!(full.total_count, 4);
    assert!(full.transactions.iter().any(|t| t.account_id == a));
    assert!(full.transactions.iter().any(|t| t.account_id == b));
}

/// E3: archived accounts leave the overall flows, not the single-account proof.
#[test]
fn archived_accounts_leave_the_overall_flow() {
    let (v, tmp) = vault("totals-arch-flow");
    let c = tmp.conn("v.girocrab");
    let (_, b) = seeded_pair(&c);
    v.set_account_archived(b, true).unwrap();

    let board = v.investment_dashboard(1).unwrap();
    let m = board.monthly.last().unwrap();
    assert_eq!(m.income_cents, 100_000);
    assert_eq!(m.consumption_cents, 4_000);
    let cash = v
        .cashflow_by_period(PeriodGranularity::Month, 1, None, true)
        .unwrap();
    assert_eq!(cash.last().unwrap().expense_cents, 4_000);

    // single-account KPIs of the archived account are untouched (proof view)
    let own = v
        .list_transactions(BookingFilter {
            account_ids: vec![b],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(own.total_count, 2);
    assert_eq!(own.flow.income_cents, 100_000);
    assert_eq!(own.flow.consumption_cents, 4_000);
}

/// The cashflow card follows its calculation context (#337): the overview card
/// (`respect_in_total`) sums the effective total set; a directly requested
/// account is a proof view and keeps its own flows — also when it is switched
/// out of the totals or archived, the card never goes blank.
#[test]
fn cashflow_card_single_account_is_proof_overview_is_totals() {
    let (v, tmp) = vault("totals-cash-context");
    let c = tmp.conn("v.girocrab");
    let (a, b) = seeded_pair(&c);
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, None, true)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        200_000,
        "both accounts count while both are included"
    );

    // b out of the totals: the overview card drops it …
    v.set_account_include_in_total(b, false).unwrap();
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, None, true)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        100_000,
        "overview = effective total set (a only)"
    );
    // … the single-account card keeps b's own flows (proof view, default) …
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, Some(vec![b]), false)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        100_000,
        "directly requested account keeps its sums"
    );
    // … and only an explicit totals context narrows that selection to zero.
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, Some(vec![b]), true)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        0,
        "totals context drops the switched-out account"
    );
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, Some(vec![a]), true)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        100_000,
        "included account unaffected"
    );

    // archived is the same proof rule: the account's own card keeps working
    v.set_account_archived(b, true).unwrap();
    assert_eq!(
        v.cashflow_by_period(PeriodGranularity::Month, 1, Some(vec![b]), false)
            .unwrap()
            .last()
            .unwrap()
            .income_cents,
        100_000,
        "archived account's card keeps its sums"
    );
}

// ------------------------------------------------- explicit totals context (new APIs)

/// E3 with the explicit totals context: a group switched out of the total also
/// leaves `page.flow` and `selection_flow_for_totals`; the list stays complete.
#[test]
fn excluded_group_leaves_the_overall_flow() {
    let (v, tmp) = vault("totals-group-flow");
    let c = tmp.conn("v.girocrab");
    let (_, b) = seeded_pair(&c);
    let uuid = in_new_group(&v, &c, b, "Test-Gruppe");
    v.update_user_group(uuid, "Test-Gruppe".into(), "folder".into(), false)
        .unwrap();

    let full = v.list_transactions(BookingFilter::default()).unwrap();
    let totals = v.list_transactions(totals_filter()).unwrap();

    assert_eq!(full.total_count, totals.total_count, "proof list stays");
    assert_eq!(
        (totals.flow.income_cents, totals.flow.consumption_cents),
        (100_000, 4_000),
        "only the included account counts"
    );
    assert_eq!(
        (full.flow.income_cents, full.flow.consumption_cents),
        (200_000, 8_000),
        "the proof flow still sums both accounts"
    );
    // the three KPI ways agree
    let via_method = v.selection_flow_for_totals(Vec::new(), None, None).unwrap();
    assert_eq!(
        (
            via_method.income_cents,
            via_method.consumption_cents,
            via_method.invested_cents
        ),
        (
            totals.flow.income_cents,
            totals.flow.consumption_cents,
            totals.flow.invested_cents
        ),
        "selection_flow_for_totals agrees"
    );
}

/// The account's own switch keeps its proof view and changes only totals.
#[test]
fn account_flag_moves_only_the_overall_sums() {
    let (v, tmp) = vault("totals-flag");
    let c = tmp.conn("v.girocrab");
    let (_, b) = seeded_pair(&c);
    v.set_account_include_in_total(b, false).unwrap();

    let account = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.name == "Bestell Test")
        .unwrap();
    assert!(!account.include_in_total, "own value stored");
    assert!(!account.in_total, "effective value follows");

    let totals = v.list_transactions(totals_filter()).unwrap();
    let full = v.list_transactions(BookingFilter::default()).unwrap();
    assert_eq!(full.total_count, totals.total_count);
    assert_eq!(
        (totals.flow.income_cents, totals.flow.consumption_cents),
        (100_000, 4_000)
    );

    // switching it back restores the full sums
    v.set_account_include_in_total(b, true).unwrap();
    let totals = v.list_transactions(totals_filter()).unwrap();
    assert_eq!(
        (totals.flow.income_cents, totals.flow.consumption_cents),
        (200_000, 8_000)
    );
}

/// Identical id list, different context: the single account is a proof view,
/// the one-account group is a totals view with zero sums (#337 contract).
#[test]
fn same_id_single_account_vs_one_account_group() {
    let (v, tmp) = vault("totals-context");
    let c = tmp.conn("v.girocrab");
    let (a, b) = seeded_pair(&c);
    v.set_account_include_in_total(b, false).unwrap();
    let _uuid = in_new_group(&v, &c, b, "Nur Bestell");

    // proof context (default): the excluded account's KPIs stay
    let own = v
        .list_transactions(BookingFilter {
            account_ids: vec![b],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(own.total_count, 2);
    assert_eq!(
        (own.flow.income_cents, own.flow.consumption_cents),
        (100_000, 4_000)
    );

    // totals context with the same id list: zero sums, full list
    let group = v
        .list_transactions(BookingFilter {
            account_ids: vec![b],
            respect_in_total: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(group.total_count, 2, "group proof list stays");
    assert_eq!(
        (group.flow.income_cents, group.flow.consumption_cents),
        (0, 0),
        "empty allowed set = zero, never all"
    );
    assert!(group.transactions.iter().all(|t| t.account_id == b));

    // a group of only included accounts sums normally
    let mixed = v
        .list_transactions(BookingFilter {
            account_ids: vec![a, b],
            respect_in_total: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(mixed.total_count, 4);
    assert_eq!(
        (mixed.flow.income_cents, mixed.flow.consumption_cents),
        (100_000, 4_000)
    );
}

/// Effective flag: an excluded group wins over the account's own switch; the
/// group's header keeps showing its own sum (review point 1 on #1100) while
/// the overview figures leave it out.
#[test]
fn excluded_group_beats_the_account_switch() {
    let (v, tmp) = vault("totals-precedence");
    let c = tmp.conn("v.girocrab");
    let (_, b) = seeded_pair(&c);
    let uuid = in_new_group(&v, &c, b, "Gewinnt");
    v.update_user_group(uuid.clone(), "Gewinnt".into(), "folder".into(), false)
        .unwrap();
    v.set_account_include_in_total(b, true).unwrap();

    let account = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.id == b)
        .unwrap();
    assert!(account.include_in_total, "own value stays on");
    assert!(!account.in_total, "the group keeps it out");

    let groups = v.list_user_groups().unwrap();
    let g = groups.iter().find(|g| g.uuid == uuid).unwrap();
    assert_eq!(
        g.balance_cents, 70_000,
        "an excluded group keeps showing its own sum (review point 1 on #1100)"
    );
    assert_eq!(g.accounts.len(), 1, "members stay listed");

    let totals = v.list_transactions(totals_filter()).unwrap();
    assert_eq!(
        totals.flow.income_cents, 100_000,
        "the other account still counts; only the group's member is out"
    );
    assert_eq!(totals.total_count, 4, "proof list stays complete");

    // group back in: the account's own switch (on) makes it count again
    v.update_user_group(uuid.clone(), "Gewinnt".into(), "folder".into(), true)
        .unwrap();
    let groups = v.list_user_groups().unwrap();
    let g = groups.iter().find(|g| g.uuid == uuid).unwrap();
    assert_eq!(g.balance_cents, 70_000);
}

// ------------------------------------------------------------------ group sums (L1)

/// Group and kind-section sums follow the effective exclusion; the excluded
/// account keeps its memberships; subgroups are not double-counted.
#[test]
fn group_and_kind_sums_follow_the_effective_exclusion() {
    let (v, tmp) = vault("totals-group-sum");
    let c = tmp.conn("v.girocrab");
    let (a, b) = seeded_pair(&c);
    let uuid = in_new_group(&v, &c, a, "Summen");
    let sub = v
        .create_user_group("Unter".into(), "folder".into())
        .unwrap();
    c.execute(
        "UPDATE accounts SET user_group_uuid = ?1 WHERE id = ?2",
        params![sub, b],
    )
    .unwrap();
    v.set_user_group_parent(sub.clone(), Some(uuid.clone()))
        .unwrap();

    // both in: the parent sum includes the subgroup
    let groups = v.list_user_groups().unwrap();
    let g = groups.iter().find(|g| g.uuid == uuid).unwrap();
    assert_eq!(g.balance_cents, 570_000);

    // own account flag off: the subgroup keeps its members, the sums drop it
    v.set_account_include_in_total(b, false).unwrap();
    let groups = v.list_user_groups().unwrap();
    let g = groups.iter().find(|g| g.uuid == uuid).unwrap();
    let s = groups.iter().find(|g| g.uuid == sub).unwrap();
    assert_eq!(s.accounts.len(), 1, "member stays in the subgroup");
    assert_eq!(s.balance_cents, 0);
    assert_eq!(
        g.balance_cents, 500_000,
        "parent without the excluded account"
    );

    // kind sections ("Nach Kontoart") sum the same way: b moves to its own
    // section, which then holds only the excluded account
    c.execute(
        "INSERT OR IGNORE INTO account_groups(name) VALUES ('PS')",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE accounts SET group_id = (SELECT id FROM account_groups WHERE name = 'PS') WHERE id = ?1",
        [b],
    )
    .unwrap();
    let kind = v
        .list_account_groups()
        .unwrap()
        .into_iter()
        .find(|g| g.accounts.iter().any(|x| x.id == b))
        .unwrap();
    assert_eq!(kind.accounts.len(), 1, "still listed in its kind section");
    assert_eq!(kind.balance_cents, 0, "kind sum follows the exclusion");
    // … and switching the account back on fills the section sum again
    v.set_account_include_in_total(b, true).unwrap();
    let kind = v
        .list_account_groups()
        .unwrap()
        .into_iter()
        .find(|g| g.accounts.iter().any(|x| x.id == b))
        .unwrap();
    assert_eq!(kind.balance_cents, 70_000);
}

// ------------------------------------------------------------------ migration

/// Rewinds a vault to schema 24 (drops the new columns) so reopening runs the
/// migration of step 34 — what a vault written by an older build looks like.
fn rewind_to_v24(path: &str) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
        .unwrap();
    c.execute_batch(
        "ALTER TABLE accounts DROP COLUMN include_in_total;
         ALTER TABLE accounts DROP COLUMN total_ms;
         PRAGMA user_version = 24;",
    )
    .unwrap();
}

/// Step 34: amazon-orders accounts start excluded; everything else stays; a
/// user decision made after the migration survives reopening.
#[test]
fn migration_excludes_amazon_connector_accounts_only() {
    let tmp = Tmp::new("totals-migration");
    let path = tmp.path("v.girocrab");
    {
        let v = Vault::open(path.clone(), KEY.to_vec()).unwrap();
        let c = tmp.conn("v.girocrab");
        let amazon = add_account(
            &c,
            "Amazon Test",
            "payment_service",
            Some("amazon-orders"),
            0,
        );
        let paypalish = add_account(&c, "Anderer Dienst", "payment_service", Some("paypal"), 0);
        let unlinked = add_account(&c, "Ohne Bank", "payment_service", None, 0);
        drop(c);
        drop(v);
        rewind_to_v24(&path);
        let v = Vault::open(path.clone(), KEY.to_vec()).unwrap();
        let c = tmp.conn("v.girocrab");
        for (id, want) in [(amazon, 0), (paypalish, 1), (unlinked, 1)] {
            assert_eq!(
                col(
                    &c,
                    "SELECT include_in_total FROM accounts WHERE id = ?1",
                    id
                ),
                want,
                "account {id}"
            );
            assert_eq!(
                col(&c, "SELECT total_ms FROM accounts WHERE id = ?1", id),
                0
            );
        }
        drop(c);
        // a user decision made after the migration survives reopening
        v.set_account_include_in_total(amazon, true).unwrap();
    }
    let v = Vault::open(path, KEY.to_vec()).unwrap();
    let amazon = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Amazon Test")
        .unwrap();
    assert!(amazon.include_in_total, "user choice survives reopening");
}

/// A vault that a development build already gave the columns keeps every user
/// decision: the refill must not run again (has-column guard).
#[test]
fn existing_column_keeps_user_decisions() {
    let tmp = Tmp::new("totals-guard");
    let path = tmp.path("v.girocrab");
    {
        let v = Vault::open(path.clone(), KEY.to_vec()).unwrap();
        let c = tmp.conn("v.girocrab");
        let amazon = add_account(
            &c,
            "Amazon Test",
            "payment_service",
            Some("amazon-orders"),
            0,
        );
        drop(c);
        drop(v);
        // "development build": columns exist, version not yet bumped
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
            .unwrap();
        c.execute(
            "UPDATE accounts SET include_in_total = 1, total_ms = 1 WHERE id = ?1;",
            [amazon],
        )
        .unwrap();
        c.execute_batch("PRAGMA user_version = 24;").unwrap();
    }
    let v = Vault::open(path, KEY.to_vec()).unwrap();
    let amazon = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Amazon Test")
        .unwrap();
    assert!(amazon.include_in_total, "no refill over a present column");
}

// ------------------------------------------------------------------ sync (E4)

/// Seed helper for the merge tests: one bank account per vault, matched by
/// (connection, external_ref) with different local ids.
fn sync_pair(
    tag: &str,
) -> (
    std::sync::Arc<Vault>,
    Tmp,
    std::sync::Arc<Vault>,
    Tmp,
    i64,
    i64,
) {
    let (a, ta) = vault(&format!("{tag}-a"));
    let (b, tb) = vault(&format!("{tag}-b"));
    let ca = ta.conn("v.girocrab");
    let cb = tb.conn("v.girocrab");
    // B gets its own extra account first, so X lands on different local ids
    let _extra_b = add_account(&cb, "Nur B", "checking", Some("fints:52090000"), 1_000);
    let x_a = add_account(&ca, "Konto X", "checking", Some("fints:52090000"), 10_000);
    let x_b = add_account(&cb, "Konto X", "checking", Some("fints:52090000"), 10_000);
    add_booking(&ca, x_a, "x1", "2026-01-05", -1_000, "Testmarkt");
    add_booking(&cb, x_b, "x1", "2026-01-05", -1_000, "Testmarkt");
    (a, ta, b, tb, x_a, x_b)
}

fn flag_of(c: &Connection, id: i64) -> (bool, i64) {
    c.query_row(
        "SELECT include_in_total != 0, total_ms FROM accounts WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// The field merge syncs value and stamp by (stamp, value); both directions
/// converge, and an inserted account takes the other side's values.
#[test]
fn merge_syncs_the_account_flag_by_stamp() {
    let (a, ta, b, tb, x_a, x_b) = sync_pair("totals-merge");
    // A switches off "now", B has the older default on (stamp 0)
    a.set_account_include_in_total(x_a, false).unwrap();
    let (_, stamp_a) = flag_of(&ta.conn("v.girocrab"), x_a);
    assert!(stamp_a > 0);

    a.merge_other_vault(b.file_path(), KEY.to_vec()).unwrap();
    assert_eq!(
        flag_of(&ta.conn("v.girocrab"), x_a),
        (false, stamp_a),
        "older stamp 0 loses"
    );
    b.merge_other_vault(a.file_path(), KEY.to_vec()).unwrap();
    assert_eq!(
        flag_of(&tb.conn("v.girocrab"), x_b),
        (false, stamp_a),
        "value and stamp travel"
    );
    // converged: a second merge changes nothing
    let before = a.change_count();
    a.merge_other_vault(b.file_path(), KEY.to_vec()).unwrap();
    assert_eq!(a.change_count(), before, "second merge is a no-op");
    assert_eq!(flag_of(&ta.conn("v.girocrab"), x_a), (false, stamp_a));

    // B switches back on later: the newer stamp wins in both directions
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.set_account_include_in_total(x_b, true).unwrap();
    let (_, stamp_b) = flag_of(&tb.conn("v.girocrab"), x_b);
    assert!(stamp_b > stamp_a);
    a.merge_other_vault(b.file_path(), KEY.to_vec()).unwrap();
    assert_eq!(flag_of(&ta.conn("v.girocrab"), x_a), (true, stamp_b));
    b.merge_other_vault(a.file_path(), KEY.to_vec()).unwrap();
    assert_eq!(flag_of(&tb.conn("v.girocrab"), x_b), (true, stamp_b));

    // B's extra account arrived in A with its own default (stamp 0, included)
    let extra = a
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.name == "Nur B")
        .expect("inserted account");
    assert!(extra.include_in_total);
}

/// Equal stamps with different values: `true` wins (like the group flag).
#[test]
fn merge_tie_breaks_to_true() {
    let (a, ta, b, tb, x_a, x_b) = sync_pair("totals-tie");
    let stamp = 1_700_000_000_000i64;
    let ca = ta.conn("v.girocrab");
    let cb = tb.conn("v.girocrab");
    for (c, id, flag) in [(&ca, x_a, 0i64), (&cb, x_b, 1)] {
        c.execute(
            "UPDATE accounts SET include_in_total = ?2, total_ms = ?3 WHERE id = ?1",
            params![id, flag, stamp],
        )
        .unwrap();
    }
    a.merge_other_vault(b.file_path(), KEY.to_vec()).unwrap();
    assert!(flag_of(&ca, x_a).0, "tie: true wins on A");
    b.merge_other_vault(a.file_path(), KEY.to_vec()).unwrap();
    assert!(flag_of(&cb, x_b).0, "both sides identical");
}

/// An inserted account adopts the other side's flag and stamp unconditionally.
#[test]
fn merge_inserts_account_with_flag_and_stamp() {
    let (_a, _ta, b, tb, _, x_b) = sync_pair("totals-insert");
    // A2 starts empty; B's X is switched off with a real stamp
    let a2 = vault("totals-insert-a2");
    b.set_account_include_in_total(x_b, false).unwrap();
    let (_, stamp_b) = flag_of(&tb.conn("v.girocrab"), x_b);
    a2.0.merge_other_vault(b.file_path(), KEY.to_vec()).unwrap();
    let x_new =
        a2.0.list_accounts()
            .unwrap()
            .into_iter()
            .find(|x| x.name == "Konto X")
            .expect("inserted");
    assert!(!x_new.include_in_total, "flag adopted");
    assert_eq!(
        flag_of(&a2.1.conn("v.girocrab"), x_new.id).1,
        stamp_b,
        "stamp adopted"
    );
}

/// A schema-24 vault merges without overwriting a newer user decision: the
/// migration fills stamp 0, the merge keeps the local stamp > 0.
#[test]
fn old_vault_merge_keeps_newer_user_decision() {
    let (a, ta, b, _tb, x_a, x_b) = sync_pair("totals-old");
    // A: explicit user decision against the default, with a newer stamp
    a.set_account_include_in_total(x_a, false).unwrap();
    let (_, stamp_a) = flag_of(&ta.conn("v.girocrab"), x_a);
    assert!(stamp_a > 0, "the explicit decision carries a stamp");
    // B: old build without the columns (default on, stamp 0 after migration)
    let path_b = b.file_path();
    drop(b);
    rewind_to_v24(&path_b);
    let _b = Vault::open(path_b.clone(), KEY.to_vec()).unwrap();
    let cb = Connection::open(&path_b).unwrap();
    cb.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
        .unwrap();
    assert_eq!(flag_of(&cb, x_b), (true, 0), "migration stamps 0");
    drop(cb);

    a.merge_other_vault(path_b, KEY.to_vec()).unwrap();
    assert_eq!(
        flag_of(&ta.conn("v.girocrab"), x_a),
        (false, stamp_a),
        "the migrated stamp-0 default (true) does not beat the user's newer decision (false)"
    );
    let name: String = ta
        .conn("v.girocrab")
        .query_row("SELECT name FROM accounts WHERE id = ?1", [x_a], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(name, "Konto X", "other fields still merge");
}

fn amazon_capture_with_credit(credit: &str) -> String {
    format!(
        r#"{{"orders":[{{"code":"028-0000337-0000009","date":"17. November 2026","rows":[{{"label":"Gesamtsumme","value":"45,98 €","bold":true}},{{"label":"Erstattung gesamt","value":"{credit} €"}}]}}]}}"#
    )
}

// ---------------------------------- amazon default vs. the bank (no double counting)

#[test]
fn amazon_default_and_opt_in_match_the_bank_double_counting_contract() {
    use girocrab_core::models::ConnectorKind;
    let (v, tmp) = vault("amazon-bank-contract");
    let c = tmp.conn("v.girocrab");
    let bank = add_account(&c, "Bank Test", "checking", None, 10000);
    add_booking(&c, bank, "bank-debit", "2026-11-17", -4598, "Amazon");
    add_booking(&c, bank, "bank-credit", "2026-11-17", 1000, "Amazon");
    let cid = v
        .add_bank_connection(
            ConnectorKind::AmazonOrders,
            "Amazon Test".into(),
            "demo".into(),
        )
        .unwrap();
    v.clone()
        .import_amazon_orders(cid, amazon_capture_with_credit("10,00"))
        .unwrap();
    let order = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.connection_id == Some(cid))
        .unwrap();
    assert_eq!(order.kind.as_str(), "payment_service");
    assert!(!order.include_in_total && !order.in_total);
    let baseline = v.list_transactions(totals_filter()).unwrap();
    assert_eq!(
        (baseline.flow.consumption_cents, baseline.flow.income_cents),
        (4598, 1000)
    );
    assert_eq!(baseline.total_count, 4);
    let available = || {
        v.list_accounts()
            .unwrap()
            .into_iter()
            .filter(|a| a.in_total)
            .map(|a| a.balance_cents)
            .sum::<i64>()
    };
    assert_eq!(available(), 10000);
    v.set_account_include_in_total(order.id, true).unwrap();
    let opt_in = v.list_transactions(totals_filter()).unwrap();
    assert_eq!(
        (opt_in.flow.consumption_cents, opt_in.flow.income_cents),
        (9196, 2000)
    );
    assert_eq!(opt_in.total_count, 4);
    assert_eq!(available(), 6402);
    v.set_account_include_in_total(order.id, false).unwrap();
    let off = v.list_transactions(totals_filter()).unwrap();
    assert_eq!(
        (off.flow.consumption_cents, off.flow.income_cents),
        (4598, 1000)
    );
    assert_eq!(available(), 10000);
    assert_eq!(
        v.list_transactions(BookingFilter::default())
            .unwrap()
            .total_count,
        4
    );
}

/// Review point 2 on #1100: an older partner snapshot (schema 24 — the step-34
/// columns are missing) is migrated by step 34 inside merge_other_vault BEFORE
/// the merge SQL touches include_in_total/total_ms. The merge succeeds and the
/// migrated defaults arrive with the inserted accounts.
#[test]
fn merge_migrates_older_snapshot_before_the_column_merge() {
    let (a, _ta, b, _tb, _x_a, _x_b) = sync_pair("totals-migrate-older");
    // rewind B's snapshot file to schema 24: a snapshot written by an older
    // build, without the step-34 columns and with user_version still 24.
    // closing B's vault releases its connection; the rewind then edits the
    // snapshot file itself (Tmp stays alive for the path).
    let other_path = b.file_path();
    drop(b);
    rewind_to_v24(&other_path);

    a.merge_other_vault(other_path.clone(), KEY.to_vec())
        .unwrap();

    // B's accounts arrived with the step-34 defaults (checking accounts start
    // included, stamp 0) — the merge SQL found the migrated columns.
    let arrived = a
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|x| x.name == "Nur B")
        .collect::<Vec<_>>();
    assert_eq!(arrived.len(), 1, "B's extra account arrived");
    assert!(arrived[0].include_in_total, "step-34 default on migration");

    // the other snapshot file itself sits on the current schema afterwards
    let c_other = Connection::open(&other_path).unwrap();
    c_other
        .execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
        .unwrap();
    let has_cols: i64 = c_other
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('accounts')
              WHERE name IN ('include_in_total', 'total_ms')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_cols, 2, "both step-34 columns exist on the snapshot");
    let version: i64 = c_other
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(version >= 34, "snapshot migrated to at least schema 34");
}
