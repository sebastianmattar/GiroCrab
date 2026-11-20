//! Times the core calls behind a category change (write + the reloads AppModel
//! used to run synchronously), on a throw-away demo vault whose bookings are
//! cloned COPIES times (each copy shifted further back).
//! Usage: COPIES=600 cargo run --release -q -p girocrab-core --example category_perf
//!
//! Reference (M-series Mac, release build, COPIES=600 → 31,252 bookings, 2026-11-12):
//!   set_transaction_category        0.6 ms
//!   list_transactions(limit 3000) 150 ms   (only ~10 % below no limit: every row is still finished)
//!   list_categories                 35 ms
//!   list_account_groups / _user_groups  1.8 ms each (41 ms before the covering indexes)
//!   list_rules                      0.6 ms (84 ms before the covering indexes)
//!   investment_dashboard           150 ms
//!   selection_flow                 ~150 ms (a full pass over all bookings)
//!   merchant_stats                  25 ms (153 ms via list_transactions before #591)
//!   merchant_history                32 ms (total + own account; was 2 × merchant_stats)
//!   suggest_categories              45 ms after a change (reads the history, #664; 50–90 ms with PAYPAL=3000,
//!                                   varies run to run); 0.3 ms (1) / 1.5 ms (100) while the data is unchanged (2026-11-17)
//!   local_api transactions(limit 100)  ~1.2 ms (first page, cursor page, single account; 2026-11-15)
//!   local_api summary                  ~1.2 ms per year, 2.4 ms for the three-year maximum
//!   preview_rule(limit 20)       1.3 ms, 31 ms if nothing matches (163 ms while it enriched
//!                                  every booking; 31,853 bookings, 2026-11-16)
//!   preview_manual_loan(limit 20)   55 ms (212 ms; the rest is the expression over every booking)
//!   auto_match_document             10 ms (176 ms; SQL prefilter on the receipt's amounts and dates)
//! After the flow cache (`Vault::flow_map`, 2026-11-16, COPIES=600 STEP_DAYS=7):
//!   list_transactions(limit 3000)    44 ms (was 172), selection_flow 14 ms (was 290),
//!   category selection, period "all" 102 ms (was 446), list_categories_filtered with the
//!   expense filter 32 ms (was 336: `tx_amount_date` range scan). The first list after any
//!   write still enriches every booking once (~155 ms) to refill the cache.
//! After caching the `BookingFinisher` and each booking's merchant key (`ListCache`,
//! 2026-11-17, COPIES=600 STEP_DAYS=7 PAYPAL=2000, 43,469 bookings): local_api page 1.5 ms
//! (was 5.3), preview_rule with hits 0.7 ms (4.2), count_transactions(uncategorized)
//! 18 ms (58), merchant_history 1.4 ms (33; the first one after a write still resolves
//! every booking). investment_dashboard still finishes every booking (~220 ms).
//! Tags (#228, 2026-11-17, COPIES=600, 41,469 bookings, a third tagged): tag_totals 19 ms
//!   (94 ms as a SQL join that read a booking row per link; 22 ms with a scope filter, was
//!   179), tag_account_counts 21 ms (25), list_tags 1.5 ms (68: same per-link row lookup).
//! Running balances (#577, 41,469 bookings, 2026-11-17): page of 300 35 ms → 35 ms at the top,
//!   34 ms → 61 ms at offset 25,000 (one SQL sum over the newer bookings; load-more runs off main).
//! OUT=<file> writes the vault with the app's demo key and stops (open it with
//! GIROCRAB_DEMO_VAULT=<file> .build/debug/GiroCrabMac --demo-seed).
//! RULE_INDEX=1 creates the covering indexes by hand on a vault that lacks them.
//! STEP_DAYS (default 183) is the shift between copies: 7 with COPIES=600 spans ~11.5 years.
//! PAYPAL=n adds a payment-service account with n uncategorised bookings, half of them
//! mirroring a bank booking (the transfer pairing in `vault/transfer_pairs.rs`).
//! COPIES=600 STEP_DAYS=7 PAYPAL=3000 (2026-11-16): category selection, period "all"
//! ~360 ms (selection_flow 175 ms of it); local_api page 6.5 ms (did not finish in
//! 10 minutes before the `tx_amount_date` index).
//! Rules that add tags (#580, 2026-11-17, 41,469 bookings): apply_rules(fetch) 127 ms with one
//! tag rule (124 without; only bookings whose tags nobody touched), apply_rules(all) 230 ms;
//! rule_tag_counts (rule list, four tag rules, a third of the bookings tagged) 34 ms (off main).
//! Known recipients (#978, 44,474 bookings, 2026-11-19): suggest_recipients 259 ms per keystroke
//!   (a scan over every booking row) → 0.6 ms for the first call after a write (index
//!   `tx_recipients`, 4,207 bookings with an IBAN; the write itself not timed), 0.0 ms while the
//!   data is unchanged; 10.7 ms after a write when every outgoing booking (36,060) carries an IBAN.
use girocrab_core::models::{
    BookingDirection, BookingFilter, BookingScope, MatchKind, Rule, RuleField,
};
use girocrab_core::vault::Vault;

fn time<T>(label: &str, mut f: impl FnMut() -> T) -> f32 {
    let mut ms: Vec<f32> = (0..5)
        .map(|_| {
            let t = std::time::Instant::now();
            let _ = f();
            t.elapsed().as_secs_f32() * 1000.0
        })
        .collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("{label:<52} {:>8.1} ms", ms[2]);
    ms[2]
}

fn main() {
    let copies = env_usize("COPIES", 80);
    let step = env_usize("STEP_DAYS", 183);
    // OUT=file writes the vault with the app's demo key and stops, so the app can open it:
    // GIROCRAB_DEMO_VAULT=file .build/debug/GiroCrabMac --demo-seed
    let out = std::env::var("OUT").ok().filter(|v| !v.is_empty());
    let key: Vec<u8> = if out.is_some() {
        (0..32u32).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect()
    } else {
        vec![9u8; 32]
    };
    let tmp = std::env::temp_dir().join(format!("gc-cat-perf-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = out
        .clone()
        .unwrap_or_else(|| tmp.join("vault.db").to_string_lossy().to_string());
    if out.is_some() {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }
    {
        let v = Vault::open(path.clone(), key.clone()).unwrap();
        v.seed_demo_data().unwrap();
    }
    {
        // Clone the demo bookings straight in SQL (the vault's own connection is crate-private).
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(&key)))
            .unwrap();
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('transactions') WHERE name != 'id'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let tx = c.unchecked_transaction().unwrap();
        for k in 1..=copies {
            let select: Vec<String> = cols
                .iter()
                .map(|n| match n.as_str() {
                    "booking_date" | "value_date" => format!("date({n}, '-{} days')", k * step),
                    "external_id" => format!("{n} || '#{k}'"),
                    "uuid" => "lower(hex(randomblob(16)))".into(),
                    "import_fp" => "''".into(),
                    _ => n.clone(),
                })
                .collect();
            tx.execute_batch(&format!(
                "INSERT INTO transactions ({}) SELECT {} FROM transactions WHERE external_id NOT LIKE '%#%'",
                cols.join(","),
                select.join(",")
            ))
            .unwrap();
        }
        tx.commit().unwrap();
        let paypal = env_usize("PAYPAL", 0);
        if paypal > 0 {
            c.execute(
                "INSERT INTO accounts (group_id, external_ref, name, kind, iban) \
                 SELECT group_id, 'perf-pp', 'PayPal', 'payment_service', '' FROM accounts LIMIT 1",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO transactions (account_id, external_id, booking_date, amount_cents, counterparty, purpose) \
                 SELECT ?2, 'pp-' || t.id, t.booking_date, \
                        CASE WHEN t.id % 2 = 0 THEN -t.amount_cents ELSE t.amount_cents END, \
                        'PayPal Europe', 'PayPal Zahlung ' || t.counterparty \
                 FROM transactions t WHERE t.amount_cents < 0 ORDER BY t.booking_date DESC LIMIT ?1",
                [paypal as i64, c.last_insert_rowid()],
            )
            .unwrap();
        }
        if std::env::var("RULE_INDEX").is_ok_and(|v| !v.is_empty()) {
            c.execute_batch("CREATE INDEX tx_rule_live ON transactions(rule_id, deleted_ms); CREATE INDEX tx_acct_live ON transactions(account_id, category_id, checked, deleted_ms); ANALYZE;").unwrap();
        }
    }
    if let Some(out) = out {
        // Open once so WAL data is checkpointed into the file.
        Vault::open(out.clone(), key).unwrap().checkpoint().unwrap();
        println!("vault written: {out}");
        return;
    }
    let vault = Vault::open(path, key).unwrap();
    let all = vault
        .list_transactions(BookingFilter {
            scope: BookingScope::All,
            ..Default::default()
        })
        .unwrap();
    let cats = vault.list_categories(None, None).unwrap();
    println!(
        "bookings: {}, categories: {}\n",
        all.transactions.len(),
        cats.len()
    );
    let b = all.transactions[0].clone();
    let (c1, c2) = (cats[3].id, cats[5].id);

    // The UI's list filter: newest 3000 bookings, all accounts, current period.
    let ui_filter = || BookingFilter {
        scope: BookingScope::All,
        limit: 3000,
        ..Default::default()
    };
    let mut flip = false;
    let mut total = 0.0;
    total += time("set_transaction_category", || {
        flip = !flip;
        vault.set_transaction_category(b.id, Some(if flip { c1 } else { c2 }))
    });
    total += time("reloadBookings: list_transactions(limit 3000)", || {
        vault.list_transactions(ui_filter())
    });
    total += time("reloadCategories: list_categories", || {
        vault.list_categories(None, None)
    });
    total += time("reloadAccounts: list_account_groups", || {
        vault.list_account_groups()
    });
    total += time("reloadAccounts: list_user_groups", || {
        vault.list_user_groups()
    });
    total += time("reloadAccounts: list_bank_connections", || {
        vault.list_bank_connections()
    });
    total += time("reloadRules: list_rules", || vault.list_rules());
    total += time("reloadDashboard: investment_dashboard(6)", || {
        vault.investment_dashboard(6)
    });
    println!(
        "{:<52} {:>8.1} ms  (synchronous on the main thread)",
        "sum", total
    );
    println!();
    time("merchant_stats (total only, one pass)", || {
        vault.merchant_stats(b.merchant.key.clone())
    });
    time("merchant_history (off main: loadMerchantStats)", || {
        vault.merchant_history(b.merchant.key.clone(), b.account_id)
    });
    let open: Vec<i64> = vault
        .list_transactions(BookingFilter {
            scope: BookingScope::All,
            uncategorized_only: true,
            limit: 100,
            ..Default::default()
        })
        .unwrap()
        .transactions
        .iter()
        .map(|t| t.id)
        .collect();
    if let Some(&first) = open.first() {
        // the history is kept until the data changes: time both cases
        time("suggest_categories (1 booking, after a change)", || {
            flip = !flip;
            let cat = Some(if flip { c1 } else { c2 });
            vault.set_transaction_category(b.id, cat).unwrap();
            vault.suggest_categories(vec![first])
        });
        time("suggest_categories (1 booking, data unchanged)", || {
            vault.suggest_categories(vec![first])
        });
        let mut found = 0;
        time("suggest_categories (100 bookings, data unchanged)", || {
            found = vault.suggest_categories(open.clone()).unwrap().len();
        });
        println!(
            "  → {found} of {} uncategorised bookings got a suggestion",
            open.len()
        );
    } else {
        println!("suggest_categories: no uncategorised booking, skipped");
    }
    time(
        "list_transactions(all, no limit) – what the dashboard reads",
        || {
            vault.list_transactions(BookingFilter {
                scope: BookingScope::All,
                ..Default::default()
            })
        },
    );
    // running balances (#577): first page and a deep page, all accounts
    for (label, offset, balances) in [
        ("list page 300, no balances", 0, false),
        ("list page 300 + running balances", 0, true),
        ("list page 300 at offset 25000, no balances", 25_000, false),
        (
            "list page 300 at offset 25000 + running balances",
            25_000,
            true,
        ),
    ] {
        time(label, || {
            vault.list_transactions(BookingFilter {
                scope: BookingScope::All,
                limit: 300,
                offset,
                whole_days: true,
                with_balances: balances,
                ..Default::default()
            })
        });
    }
    time(
        "'Ohne Kategorie': list_transactions(uncategorized, 3000)",
        || {
            vault.list_transactions(BookingFilter {
                scope: BookingScope::All,
                uncategorized_only: true,
                limit: 3000,
                ..Default::default()
            })
        },
    );
    time(
        "'Ohne Kategorie': + selection_flow (reloadSelectionKPIs)",
        || vault.selection_flow(vec![], None, None),
    );
    // Header filter "Ausgaben" / "Neu": list, totals and sidebar sums behind it.
    time("direction expense: list_transactions(3000)", || {
        vault.list_transactions(BookingFilter {
            direction: BookingDirection::Expense,
            limit: 3000,
            ..Default::default()
        })
    });
    time("direction expense: list_categories_filtered", || {
        vault.list_categories_filtered(None, None, vec![], None, BookingDirection::Expense)
    });
    time("status new: list_transactions(3000)", || {
        vault.list_transactions(BookingFilter {
            checked: Some(false),
            limit: 3000,
            ..Default::default()
        })
    });
    // Tags (#228, part 3): a third of the bookings carry one of the demo tags.
    let tags = vault.list_tags().unwrap();
    if !tags.is_empty() {
        for (i, tag) in tags.iter().enumerate() {
            let ids: Vec<i64> = all
                .transactions
                .iter()
                .skip(i)
                .step_by(3 * tags.len())
                .map(|b| b.id)
                .collect();
            vault.add_tag_to_bookings(ids, tag.id).unwrap();
        }
        time("tag_totals (all accounts, all dates)", || {
            vault.tag_totals(BookingFilter::default())
        });
        time("tag_totals (category, with subcategories)", || {
            vault.tag_totals(BookingFilter {
                category_id: Some(c1),
                include_subcategories: true,
                ..Default::default()
            })
        });
        time("tag_totals (scope Alltag: enriched pass)", || {
            vault.tag_totals(BookingFilter {
                scope: BookingScope::Everyday,
                ..Default::default()
            })
        });
        time("tag_account_counts", || {
            vault.tag_account_counts(tags[0].id)
        });
        time("list_tags", || vault.list_tags());
        // Rules that add tags (#580): a fetch runs them over the bookings whose tags
        // nobody touched, "Regeln anwenden" over all.
        time("apply_rules(fetch), no tag rule", || {
            vault.apply_rules(true)
        });
        vault
            .save_rule(Rule {
                id: 0,
                name: "Tags".into(),
                category_id: 0,
                field: RuleField::Any,
                match_kind: MatchKind::Keyword,
                pattern: "REWE|Aral|Shell".into(),
                case_sensitive: false,
                min_amount_cents: None,
                max_amount_cents: None,
                account_ids: Vec::new(),
                priority: 1000,
                enabled: true,
                match_count: 0,
                tag_ids: vec![tags[0].id],
            })
            .unwrap();
        time("apply_rules(fetch), one tag rule", || {
            vault.apply_rules(true)
        });
        time("apply_rules(all), one tag rule", || {
            vault.apply_rules(false)
        });
        time("rule_tag_counts (rule list, demo rules)", || {
            vault.rule_tag_counts()
        });
    }
    // Local read-only interface (#136): keyset pages enrich only their rows.
    let accounts: Vec<i64> = vault
        .list_accounts()
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    vault
        .set_local_api_scope(girocrab_core::local_api::ShareScope {
            enabled: true,
            account_ids: accounts,
            ..Default::default()
        })
        .unwrap();
    let first = vault.local_api_request(r#"{"method":"transactions","params":{"limit":100}}"#);
    let cursor: serde_json::Value = serde_json::from_str(&first).unwrap();
    let deep = format!(
        r#"{{"method":"transactions","params":{{"limit":100,"cursor":{}}}}}"#,
        cursor["result"]["next_cursor"]
    );
    time("local_api transactions(limit 100), first page", || {
        vault.local_api_request(r#"{"method":"transactions","params":{"limit":100}}"#)
    });
    time("local_api transactions(limit 100), with cursor", || {
        vault.local_api_request(&deep)
    });
    let card = vault
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|a| a.transaction_count > 0)
        .min_by_key(|a| a.transaction_count)
        .unwrap();
    let one = format!(
        r#"{{"method":"transactions","params":{{"limit":100,"account_ids":[{}]}}}}"#,
        card.id
    );
    time(
        &format!(
            "local_api transactions(limit 100), {} bookings acct",
            card.transaction_count
        ),
        || vault.local_api_request(&one),
    );
    time("local_api summary (one year)", || {
        vault.local_api_request(
            r#"{"method":"summary","params":{"date_from":"2025-01-01","date_to":"2025-12-31"}}"#,
        )
    });
    let to = jiff::ZonedDateTime::now().date();
    let three = format!(
        r#"{{"method":"summary","params":{{"date_from":"{}","date_to":"{to}"}}}}"#,
        to - jiff::Span::new().days(3 * 366)
    );
    time("local_api summary (three years, the maximum)", || {
        vault.local_api_request(&three)
    });
    category_selection(&vault, &cats);
    // Last: it writes (a document, a recurring series).
    small_results(&vault, &all.transactions);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Calls that return a handful of bookings or only sums.
fn small_results(vault: &Vault, all: &[girocrab_core::models::Booking]) {
    use girocrab_core::models::{MatchKind, PeriodGranularity, Rule, RuleField};
    // The newest booking with a plain word in its counterparty: a rule or loan
    // expression on it hits at once, newest first.
    let (b, word) = all
        .iter()
        .find_map(|b| {
            let w = b.counterparty.split_whitespace().next()?;
            (w.len() >= 3 && w.chars().all(char::is_alphanumeric)).then(|| (b, w.to_string()))
        })
        .expect("a booking with a counterparty");
    println!();
    time("count_transactions(uncategorized)", || {
        vault
            .count_transactions(BookingFilter {
                uncategorized_only: true,
                ..Default::default()
            })
            .unwrap()
    });
    time("suggest_recipients(\"m\", 5), data unchanged", || {
        vault.suggest_recipients("m".into(), 5).unwrap()
    });
    let known = vault.list_known_recipients(String::new()).unwrap();
    println!(
        "  → {} IBANs, {} outgoing bookings with an IBAN",
        known.len(),
        known.iter().map(|r| r.times_used).sum::<u32>()
    );
    // The rebuild after a write, without the write: hide and show again (an even number of
    // times, so the IBAN ends up shown), timing only the read that follows each.
    let iban = known[0].iban.clone();
    let mut rebuild: Vec<f32> = [true, false, true, false, true, false]
        .into_iter()
        .map(|hide| {
            vault.set_recipient_hidden(iban.clone(), hide).unwrap();
            let t = std::time::Instant::now();
            vault.suggest_recipients("m".into(), 5).unwrap();
            t.elapsed().as_secs_f32() * 1000.0
        })
        .collect();
    rebuild.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{:<52} {:>8.1} ms",
        "suggest_recipients(\"m\", 5), first after a write", rebuild[3]
    );
    time("cashflow_by_period(month, 12)", || {
        vault
            .cashflow_by_period(PeriodGranularity::Month, 12, None, true)
            .unwrap()
    });
    let rule = |pattern: &str| Rule {
        id: 0,
        name: String::new(),
        category_id: 0,
        field: RuleField::Counterparty,
        match_kind: MatchKind::Keyword,
        pattern: pattern.into(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: vec![],
        priority: 0,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    };
    time("preview_rule(limit 20), newest bookings match", || {
        assert!(!vault.preview_rule(rule(&word), 20).unwrap().is_empty())
    });
    time("preview_rule(limit 20), nothing matches", || {
        assert!(vault.preview_rule(rule("zzqx"), 20).unwrap().is_empty())
    });
    let loan = girocrab_core::loans::LoanSetup {
        name: "Kredit".into(),
        original_cents: None,
        anchor_date: "2020-01-01".into(),
        anchor_balance_cents: 1_000_000,
        interest_rate_ppm: None,
        match_expr: word.clone(),
    };
    time("preview_manual_loan(limit 20)", || {
        assert!(
            vault
                .preview_manual_loan(None, loan.clone(), 20)
                .unwrap()
                .match_count
                > 0
        )
    });
    let receipt = format!(
        "{} Datum {} Summe {}",
        b.counterparty,
        b.booking_date,
        girocrab_core::money::format_cents(b.amount_cents.abs(), "EUR".into())
    );
    time("auto_match_document (amount + date)", || {
        vault
            .auto_match_document("beleg.jpg".into(), vec![0xFF, 0xD8], Some(receipt.clone()))
            .unwrap()
            .expect("matched")
    });
    time("mark_booking_recurring", || {
        vault
            .mark_booking_recurring(
                b.id,
                girocrab_core::recurring::RecurrenceInterval::Monthly,
                vec![],
            )
            .unwrap()
    });
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(default)
}

/// The core calls of `AppModel.startSelectionLoad` for the busiest top-level
/// category, for the periods "all" and current year.
fn category_selection(vault: &Vault, cats: &[girocrab_core::models::Category]) {
    let today = jiff::ZonedDateTime::now().date();
    let cat = cats
        .iter()
        .filter(|c| c.parent_id.is_none())
        .max_by_key(|c| c.transaction_count)
        .unwrap();
    println!(
        "\ncategory selection: {} ({} bookings)",
        cat.name, cat.transaction_count
    );
    for (label, from, to, key_len) in [
        ("all", None, None, 4u32),
        (
            "year",
            Some(format!("{}-01-01", today.year())),
            Some(today.to_string()),
            7,
        ),
    ] {
        let mut total = 0.0;
        total += time(&format!("[{label}] salary_cycle"), || {
            vault.salary_cycle(vec![])
        });
        total += time(
            &format!("[{label}] list_transactions(category, 300)"),
            || {
                vault.list_transactions(BookingFilter {
                    category_id: Some(cat.id),
                    include_subcategories: true,
                    date_from: from.clone(),
                    date_to: to.clone(),
                    limit: 300,
                    scope: BookingScope::All,
                    whole_days: true,
                    ..Default::default()
                })
            },
        );
        total += time(&format!("[{label}] selection_flow"), || {
            vault.selection_flow(vec![], from.clone(), to.clone())
        });
        total += time(&format!("[{label}] recurring_snapshot"), || {
            vault.recurring_snapshot(vec![], today.year().into(), today.month() as u32)
        });
        total += time(&format!("[{label}] list_categories(period)"), || {
            vault.list_categories(from.clone(), to.clone())
        });
        total += time(&format!("[{label}] balance_history(365)"), || {
            vault.balance_history(vec![], 365)
        });
        total += time(&format!("[{label}] category_history_by_unit"), || {
            vault.category_history_by_unit(cat.id, vec![], from.clone(), to.clone(), key_len)
        });
        println!("{:<52} {:>8.1} ms", format!("[{label}] sum"), total);
    }
}
