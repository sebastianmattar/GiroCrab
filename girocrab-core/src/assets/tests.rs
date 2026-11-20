use super::*;
use std::sync::Arc;

fn temp_vault() -> (Arc<Vault>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("girocrab-assets-{}.db", uuid::Uuid::new_v4()));
    let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    (v, p)
}

fn days_ago(n: i64) -> String {
    iso(today() - crate::dates::days(n))
}

fn gold(name: &str, purchase: &str, quantity: f64, paid: Option<i64>) -> AssetItemSetup {
    AssetItemSetup {
        name: name.into(),
        price_source: "XAU".into(),
        weight_grams: TROY_OUNCE_GRAMS,
        purity_permille: 1000.0,
        interpolation: AssetInterpolation::Step,
        purchase_date: purchase.into(),
        quantity,
        purchase_price_cents: paid,
        start_value_cents: None,
        note: String::new(),
        symbol: String::new(),
    }
}

fn manual(name: &str, purchase: &str, value: i64, interp: AssetInterpolation) -> AssetItemSetup {
    AssetItemSetup {
        name: name.into(),
        price_source: MANUAL_SOURCE.into(),
        weight_grams: 0.0,
        purity_permille: 1000.0,
        interpolation: interp,
        purchase_date: purchase.into(),
        quantity: 1.0,
        purchase_price_cents: None,
        start_value_cents: Some(value),
        note: String::new(),
        symbol: String::new(),
    }
}

fn balance(v: &Vault, id: i64) -> i64 {
    v.list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap()
        .balance_cents
}

/// Stores API prices directly (no network in tests).
fn api_prices(v: &Vault, rows: &[(&str, i64)]) {
    let db = v.db().unwrap();
    let rows: Vec<(String, i64)> = rows.iter().map(|(d, c)| (d.to_string(), *c)).collect();
    store_api_prices(&db, "XAU", &rows).unwrap();
    refresh_balances(&db).unwrap();
}

#[test]
fn account_is_a_depot_without_connection() {
    let (v, _p) = temp_vault();
    let id = v.create_tangible_account("  ".into()).unwrap();
    let a = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap();
    assert_eq!(a.kind, AccountKind::Brokerage);
    assert_eq!(a.connection_id, None);
    assert_eq!(a.name, "Sachwerte");
    assert_eq!(a.balance_cents, 0);
    assert_eq!(v.list_tangible_account_ids().unwrap(), vec![id]);
    assert!(v.list_asset_items(id).unwrap().is_empty());
}

#[test]
fn gold_is_valued_by_fine_weight_and_latest_price() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Tresor".into()).unwrap();
    // Krügerrand: 33.931 g at 916.7 ‰ ≈ 1 troy ounce fine gold
    let mut k = gold("Krügerrand", &days_ago(10), 2.0, Some(6_000_00));
    k.weight_grams = 33.931;
    k.purity_permille = 916.7;
    let id = v.save_asset_item(acc, None, k).unwrap();
    // no price yet: value 0, nothing reported missing (the API wasn't asked)
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.holding.value_cents, 0);
    assert_eq!(it.missing_price_date, None);

    api_prices(&v, &[(&days_ago(10), 3_000_00), (&days_ago(3), 3_500_00)]);
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.id, id);
    let fine_oz = 33.931 * 0.9167 / TROY_OUNCE_GRAMS;
    let expected = (3_500_00f64 * fine_oz * 2.0).round() as i64;
    assert_eq!(it.holding.value_cents, expected);
    assert_eq!(it.holding.quantity, 2.0);
    // price per piece today and paid per piece
    assert_eq!(
        it.holding.current_price_cents,
        (3_500_00f64 * fine_oz).round() as i64
    );
    assert_eq!(it.holding.purchase_price_cents, 3_000_00);
    assert_eq!(it.value_date, days_ago(3));
    assert_eq!(balance(&v, acc), expected);
    // the shared position model: list_holdings returns the item
    let h = v.list_holdings(acc).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].value_cents, expected);
}

#[test]
fn missing_purchase_price_uses_the_market_value_on_that_day() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    v.save_asset_item(acc, None, gold("Barren", &days_ago(5), 1.0, None))
        .unwrap();
    api_prices(&v, &[(&days_ago(6), 2_900_00), (&days_ago(1), 3_100_00)]);
    let it = &v.list_asset_items(acc).unwrap()[0];
    // purchase day has no own price: the last one before it counts
    assert_eq!(it.holding.purchase_price_cents, 2_900_00);
    assert_eq!(it.holding.value_cents, 3_100_00);
}

#[test]
fn partial_sale_keeps_the_history() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    let id = v
        .save_asset_item(acc, None, gold("Maple Leaf", &days_ago(20), 5.0, None))
        .unwrap();
    api_prices(&v, &[(&days_ago(30), 1_000_00)]);
    v.set_asset_quantity(id, days_ago(5), 3.0).unwrap();
    assert_eq!(balance(&v, acc), 3_000_00);

    let hist = v.balance_history(vec![acc], 25).unwrap();
    let at = |d: String| hist.iter().find(|p| p.date == d).unwrap().balance_cents;
    assert_eq!(at(days_ago(24)), 0); // before the purchase
    assert_eq!(at(days_ago(20)), 5_000_00);
    assert_eq!(at(days_ago(6)), 5_000_00);
    assert_eq!(at(days_ago(5)), 3_000_00);
    assert_eq!(at(days_ago(0)), 3_000_00);

    // sold out: still listed, no position any more, value 0
    v.set_asset_quantity(id, days_ago(1), 0.0).unwrap();
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert!(it.sold);
    assert_eq!(it.quantities.len(), 3);
    assert!(v.list_holdings(acc).unwrap().is_empty());
    assert_eq!(balance(&v, acc), 0);

    // the purchase can't be undone this way, a later change can
    assert!(v.delete_asset_quantity(id, days_ago(20)).is_err());
    v.delete_asset_quantity(id, days_ago(1)).unwrap();
    assert_eq!(balance(&v, acc), 3_000_00);
    assert!(v.set_asset_quantity(id, days_ago(25), 1.0).is_err());
    assert!(v.set_asset_quantity(id, days_ago(20), 0.0).is_err());
}

#[test]
fn manual_values_step_or_linear() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Haus & Auto".into()).unwrap();
    let car = v
        .save_asset_item(
            acc,
            None,
            manual("Auto", &days_ago(10), 10_000_00, AssetInterpolation::Step),
        )
        .unwrap();
    let house = v
        .save_asset_item(
            acc,
            None,
            manual(
                "Haus",
                &days_ago(10),
                100_000_00,
                AssetInterpolation::Linear,
            ),
        )
        .unwrap();
    v.set_asset_value(car, days_ago(0), 8_000_00).unwrap();
    v.set_asset_value(house, days_ago(0), 110_000_00).unwrap();

    let hist = v.balance_history(vec![acc], 12).unwrap();
    let at = |d: String| hist.iter().find(|p| p.date == d).unwrap().balance_cents;
    assert_eq!(at(days_ago(11)), 0);
    assert_eq!(at(days_ago(10)), 110_000_00);
    // halfway: car still 10 000 (step), house 105 000 (linear)
    assert_eq!(at(days_ago(5)), 115_000_00);
    assert_eq!(at(days_ago(0)), 118_000_00);
    assert_eq!(balance(&v, acc), 118_000_00);

    // gold-only operations are refused, the start value can't be removed
    assert!(v.delete_asset_value(car, days_ago(10)).is_err());
    v.delete_asset_value(car, days_ago(0)).unwrap();
    assert_eq!(balance(&v, acc), 120_000_00);
    let items = v.list_asset_items(acc).unwrap();
    let car_item = items.iter().find(|i| i.id == car).unwrap();
    assert_eq!(car_item.setup.start_value_cents, Some(10_000_00));
    assert_eq!(car_item.holding.purchase_price_cents, 10_000_00);
}

#[test]
fn hand_typed_prices_win_and_missing_prices_are_reported() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    v.save_asset_item(acc, None, gold("Alter Barren", "1995-03-01", 1.0, None))
        .unwrap();
    // the API was asked from 1995 on but only answered from 2000 on
    {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO asset_price_fetch(symbol, from_date, to_date, fetched_unix) VALUES ('XAU', '1995-03-01', ?1, 0)",
            [days_ago(0)],
        )
        .unwrap();
    }
    api_prices(&v, &[("2000-01-03", 271_86)]);
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.missing_price_date.as_deref(), Some("1995-03-01"));

    v.set_asset_price("XAU".into(), "1995-03-01".into(), 300_00)
        .unwrap();
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.missing_price_date, None);
    assert_eq!(it.holding.purchase_price_cents, 300_00);

    // a later API answer for the same day leaves the typed price alone
    api_prices(&v, &[("1995-03-01", 999_00)]);
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.holding.purchase_price_cents, 300_00);
    assert!(v.set_asset_price("XAG".into(), days_ago(1), 1).is_err());
}

#[test]
fn fetch_plan_only_asks_for_used_symbols_and_missing_days() {
    let (v, _p) = temp_vault();
    let db = v.db().unwrap();
    // nothing to price: no request at all (privacy)
    assert!(fetch_plan(&db, false).unwrap().is_empty());
    drop(db);
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    v.save_asset_item(
        acc,
        None,
        manual("Uhr", &days_ago(9), 5_000_00, AssetInterpolation::Step),
    )
    .unwrap();
    assert!(fetch_plan(&v.db().unwrap(), false).unwrap().is_empty());
    v.save_asset_item(acc, None, gold("Barren", &days_ago(9), 1.0, None))
        .unwrap();
    let db = v.db().unwrap();
    assert_eq!(
        fetch_plan(&db, false).unwrap(),
        vec![("XAU".into(), days_ago(16), days_ago(0))]
    );
    // answered up to yesterday: only the last day and today
    db.execute(
        "INSERT INTO asset_price_fetch(symbol, from_date, to_date, fetched_unix) VALUES ('XAU', ?1, ?2, 0)",
        params![days_ago(16), days_ago(1)],
    )
    .unwrap();
    assert_eq!(
        fetch_plan(&db, false).unwrap(),
        vec![("XAU".into(), days_ago(1), days_ago(0))]
    );
    // up to date: nothing, unless forced
    db.execute("UPDATE asset_price_fetch SET to_date = ?1", [days_ago(0)])
        .unwrap();
    assert!(fetch_plan(&db, false).unwrap().is_empty());
    assert_eq!(fetch_plan(&db, true).unwrap().len(), 1);
    // an older purchase than ever asked for: the whole range again
    drop(db);
    v.save_asset_item(acc, None, gold("Münze", &days_ago(30), 1.0, None))
        .unwrap();
    assert_eq!(
        fetch_plan(&v.db().unwrap(), false).unwrap(),
        vec![("XAU".into(), days_ago(37), days_ago(0))]
    );
}

#[test]
fn parses_frankfurter_answers() {
    let rows = parse_rates(
        r#"[{"date":"2026-11-02","base":"XAU","quote":"EUR","rate":3514.01},
            {"date":"2026-11-03","base":"XAU","quote":"EUR","rate":3499.285},
            {"date":"kaputt","base":"XAU","quote":"EUR","rate":1.0}]"#,
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("2026-11-02".into(), 3_514_01),
            ("2026-11-03".into(), 3_499_29)
        ]
    );
    assert!(parse_rates("[]").unwrap().is_empty());
    assert!(parse_rates("{\"message\":\"not found\"}").is_err());
}

#[test]
fn validation_and_editing() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    let mut bad = gold("", &days_ago(1), 1.0, None);
    assert!(v.save_asset_item(acc, None, bad.clone()).is_err());
    bad.name = "x".into();
    bad.weight_grams = 0.0;
    assert!(v.save_asset_item(acc, None, bad.clone()).is_err());
    bad.weight_grams = 1.0;
    bad.purity_permille = 1001.0;
    assert!(v.save_asset_item(acc, None, bad.clone()).is_err());
    bad.purity_permille = 999.9;
    bad.purchase_date = iso(today() + crate::dates::days(1));
    assert!(v.save_asset_item(acc, None, bad.clone()).is_err());
    let mut m = manual("Bild", &days_ago(1), 0, AssetInterpolation::Step);
    m.start_value_cents = None;
    assert!(v.save_asset_item(acc, None, m).is_err());
    // not a Sachwerte account
    assert!(v.list_asset_items(acc + 100).is_err());

    // editing moves the purchase point and drops points before it
    let id = v
        .save_asset_item(acc, None, gold("Barren", &days_ago(10), 2.0, None))
        .unwrap();
    v.set_asset_quantity(id, days_ago(8), 1.0).unwrap();
    v.save_asset_item(
        acc,
        Some(id),
        gold("Barren 100 g", &days_ago(9), 4.0, Some(1)),
    )
    .unwrap();
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(it.setup.name, "Barren 100 g");
    // moving the purchase past a later change would drop it: refused
    assert!(
        v.save_asset_item(acc, Some(id), gold("Barren 100 g", &days_ago(7), 4.0, None))
            .is_err()
    );
    assert_eq!(
        it.quantities,
        vec![
            AssetQuantityPoint {
                date: days_ago(9),
                quantity: 4.0
            },
            AssetQuantityPoint {
                date: days_ago(8),
                quantity: 1.0
            },
        ]
    );
    v.delete_asset_item(id).unwrap();
    assert!(v.list_asset_items(acc).unwrap().is_empty());
}

#[test]
fn deleting_the_account_hides_its_items_and_stops_fetching() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    v.save_asset_item(acc, None, gold("Barren", &days_ago(3), 1.0, None))
        .unwrap();
    v.set_account_archived(acc, true).unwrap();
    v.delete_account(acc).unwrap();
    // the account is a tombstone now (it may come back with a sync): its
    // items stay stored but are neither listed nor fetched for
    assert!(v.list_asset_items(acc).is_err());
    let db = v.db().unwrap();
    assert!(super::load_items(&db, None, None).unwrap().is_empty());
    assert!(crate::assets::tangible_account_ids(&db).unwrap().is_empty());
    assert!(super::fetch_plan(&db, true).unwrap().is_empty());
}

/// A Sachwerte account turned into a cash account keeps its opening balance
/// and bookings through a vault merge instead of being valued at 0.
#[test]
fn sachwerte_account_changed_to_cash_keeps_its_computed_balance() {
    let (a, pa) = temp_vault();
    let acc = a.create_tangible_account("Portemonnaie".into()).unwrap();
    a.set_account_kind(acc, AccountKind::Cash).unwrap();
    a.set_account_opening_balance(acc, Some(231_20), String::new())
        .unwrap();
    a.save_manual_booking(
        acc,
        None,
        crate::offline::ManualBookingSetup {
            booking_date: days_ago(1),
            amount_cents: -26_20,
            counterparty: "Bäcker".into(),
            purpose: String::new(),
            category_id: None,
            note: String::new(),
        },
    )
    .unwrap();
    assert_eq!(balance(&a, acc), 205_00);
    assert!(a.list_tangible_account_ids().unwrap().is_empty());

    let (b, pb) = temp_vault();
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    a.merge_other_vault(pb.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    assert_eq!(balance(&a, acc), 205_00);
    let bacc = b
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.name == "Portemonnaie")
        .unwrap()
        .id;
    assert_eq!(balance(&b, bacc), 205_00);
}

/// The kind change itself revalues the account, in both directions.
#[test]
fn changing_the_kind_revalues_a_sachwerte_account_at_once() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Tresor".into()).unwrap();
    v.save_asset_item(
        acc,
        None,
        manual("Uhr", &days_ago(10), 2_000_00, AssetInterpolation::Step),
    )
    .unwrap();
    assert_eq!(balance(&v, acc), 2_000_00);

    v.set_account_kind(acc, AccountKind::Cash).unwrap();
    assert_eq!(balance(&v, acc), 0);
    v.set_account_kind(acc, AccountKind::Brokerage).unwrap();
    assert_eq!(balance(&v, acc), 2_000_00);
}

#[test]
fn vault_merge_brings_items_points_and_typed_prices() {
    let (a, pa) = temp_vault();
    let acc = a.create_tangible_account("Gold".into()).unwrap();
    let id = a
        .save_asset_item(acc, None, gold("Barren", &days_ago(10), 2.0, None))
        .unwrap();
    a.set_asset_quantity(id, days_ago(2), 1.0).unwrap();
    a.set_asset_price("XAU".into(), days_ago(10), 2_000_00)
        .unwrap();

    let (b, _pb) = temp_vault();
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let bacc = b.list_tangible_account_ids().unwrap()[0];
    let items = b.list_asset_items(bacc).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].quantities.len(), 2);
    assert_eq!(balance(&b, bacc), 2_000_00);
    // merging again changes nothing
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    assert_eq!(b.list_asset_items(bacc).unwrap().len(), 1);
    assert_eq!(balance(&b, bacc), 2_000_00);
}

#[test]
fn weight_presets_are_grouped_g_oz_kg_and_contain_the_ounce() {
    let w = asset_weight_presets();
    assert_eq!(w.len(), 19);
    // grams, then ounces, then kilos; lightest first within each group
    let group = |l: &str| match l.rsplit(' ').next() {
        Some("g") => 0,
        Some("oz") => 1,
        _ => 2,
    };
    assert!(w.windows(2).all(|p| {
        let (a, b) = (group(&p[0].label), group(&p[1].label));
        a < b || (a == b && p[0].grams < p[1].grams)
    }));
    assert_eq!(w.last().unwrap().label, "1 kg");
    assert_eq!(
        w.iter().find(|p| p.label == "1 oz").unwrap().grams,
        TROY_OUNCE_GRAMS
    );
}

/// Live: fetches real gold prices from the Frankfurter API.
/// `cargo test -p girocrab-core live_gold -- --ignored`
#[tokio::test]
#[ignore = "network"]
async fn live_gold_prices_from_frankfurter() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    v.save_asset_item(acc, None, gold("Barren", &days_ago(14), 1.0, None))
        .unwrap();
    let r = v.refresh_asset_prices(false).await.unwrap();
    assert!(r.requested);
    assert_eq!(r.error, None);
    assert!(r.prices_stored >= 5, "{r:?}");
    assert!(balance(&v, acc) > 1_000_00);
    // a second refresh the same day asks nothing
    let again = v.refresh_asset_prices(false).await.unwrap();
    assert!(!again.requested);
    let it = &v.list_asset_items(acc).unwrap()[0];
    assert!(it.holding.purchase_price_cents > 0);
}

#[test]
fn item_history_and_values_on_a_day() {
    let (v, _) = temp_vault();
    let acc = v.create_tangible_account("Gold".into()).unwrap();
    api_prices(&v, &[(&days_ago(10), 2_000_00), (&days_ago(5), 2_200_00)]);
    let id = v
        .save_asset_item(acc, None, gold("Barren", &days_ago(10), 2.0, None))
        .unwrap();
    // sold one piece 3 days ago
    v.set_asset_quantity(id, days_ago(3), 1.0).unwrap();

    let h = v.asset_item_history(id).unwrap();
    assert_eq!(h.len(), 11);
    assert_eq!(h.first().unwrap().date, days_ago(10));
    assert_eq!(h[0].balance_cents, 4_000_00);
    assert_eq!(h[5].balance_cents, 4_400_00);
    assert_eq!(h.last().unwrap().balance_cents, 2_200_00);

    assert_eq!(
        v.asset_unit_value_on(id, days_ago(7)).unwrap(),
        Some(2_000_00)
    );
    assert_eq!(v.asset_unit_value_on(id, days_ago(20)).unwrap(), None);
    let p = v
        .asset_price_on("XAU".into(), days_ago(4))
        .unwrap()
        .unwrap();
    assert_eq!((p.date, p.value_cents), (days_ago(5), 2_200_00));

    let car = v
        .save_asset_item(
            acc,
            None,
            manual("Auto", &days_ago(10), 10_000_00, AssetInterpolation::Linear),
        )
        .unwrap();
    v.set_asset_value(car, days_ago(0), 8_000_00).unwrap();
    assert_eq!(
        v.asset_unit_value_on(car, days_ago(5)).unwrap(),
        Some(9_000_00)
    );
}

#[test]
fn the_icon_is_stored_and_merged() {
    let (a, pa) = temp_vault();
    let acc = a.create_tangible_account("Haus & Hof".into()).unwrap();
    let mut s = gold("Haus", &days_ago(30), 1.0, None);
    s.symbol = "  house.fill  ".into();
    a.save_asset_item(acc, None, s).unwrap();
    assert_eq!(
        a.list_asset_items(acc).unwrap()[0].setup.symbol,
        "house.fill"
    );
    // a vault without a chosen icon keeps the default (empty)
    let mut plain = gold("Barren", &days_ago(3), 1.0, None);
    plain.symbol = String::new();
    a.save_asset_item(acc, None, plain).unwrap();
    let (b, _pb) = temp_vault();
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let merged: Vec<String> = b
        .list_asset_items(b.list_tangible_account_ids().unwrap()[0])
        .unwrap()
        .into_iter()
        .map(|i| i.setup.symbol)
        .collect();
    assert!(merged.contains(&"house.fill".to_string()));
    assert!(merged.contains(&String::new()));
}

/// Edits get their own millisecond, so "newer" is well defined in the merge tests.
fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(3));
}

#[test]
fn merge_keeps_deletions_and_takes_the_newer_edit() {
    let (a, pa) = temp_vault();
    let acc = a.create_tangible_account("Gold".into()).unwrap();
    let kept = a
        .save_asset_item(acc, None, gold("Barren", &days_ago(10), 2.0, None))
        .unwrap();
    let gone = a
        .save_asset_item(acc, None, gold("Kette", &days_ago(10), 1.0, None))
        .unwrap();
    a.set_asset_quantity(kept, days_ago(2), 1.0).unwrap();

    // B starts as a copy of A
    let (b, pb) = temp_vault();
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let bacc = b.list_tangible_account_ids().unwrap()[0];

    // A deletes an item and a point; B renames the other item later
    tick();
    a.delete_asset_item(gone).unwrap();
    a.delete_asset_quantity(kept, days_ago(2)).unwrap();
    tick();
    let bkept = b
        .list_asset_items(bacc)
        .unwrap()
        .into_iter()
        .find(|i| i.setup.name == "Barren")
        .unwrap();
    let mut renamed = bkept.setup.clone();
    renamed.name = "Barren 1 oz".into();
    b.save_asset_item(bacc, Some(bkept.id), renamed).unwrap();

    // merged back: the deletions stay, the newer name wins
    a.merge_other_vault(pb.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let items = a.list_asset_items(acc).unwrap();
    assert_eq!(items.len(), 1, "the deleted item stays deleted");
    assert_eq!(items[0].setup.name, "Barren 1 oz");
    assert_eq!(
        items[0].quantities.len(),
        1,
        "the deleted point stays deleted"
    );

    // and the other way round: B learns about A's deletions
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let items = b.list_asset_items(bacc).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].quantities.len(), 1);
}

#[test]
fn selling_is_one_step_and_never_on_the_purchase_date() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("".into()).unwrap();
    let car = v
        .save_asset_item(
            acc,
            None,
            manual("Auto", &days_ago(100), 20_000_00, AssetInterpolation::Step),
        )
        .unwrap();
    // on the purchase date: refused, and nothing changed
    assert!(
        v.sell_asset(car, days_ago(100), 1.0, Some(5_000_00))
            .is_err()
    );
    let item = &v.list_asset_items(acc).unwrap()[0];
    assert_eq!(item.values.len(), 1);
    assert_eq!(item.values[0].value_cents, 20_000_00);
    assert!(!item.sold);
    // more than held: refused
    assert!(v.sell_asset(car, days_ago(10), 2.0, None).is_err());
    // sold with a price: quantity 0 and the price as that day's value
    v.sell_asset(car, days_ago(10), 1.0, Some(15_000_00))
        .unwrap();
    let item = &v.list_asset_items(acc).unwrap()[0];
    assert!(item.sold);
    assert_eq!(item.values.last().unwrap().value_cents, 15_000_00);
    assert_eq!(balance(&v, acc), 0);

    // gold: no sale price (the price API values it), a partial sale
    let coins = v
        .save_asset_item(acc, None, gold("Münzen", &days_ago(50), 5.0, None))
        .unwrap();
    assert!(
        v.sell_asset(coins, days_ago(5), 2.0, Some(1_000_00))
            .is_err()
    );
    v.sell_asset(coins, days_ago(5), 2.0, None).unwrap();
    let coins = v
        .list_asset_items(acc)
        .unwrap()
        .into_iter()
        .find(|i| i.id == coins)
        .unwrap();
    assert_eq!(coins.holding.quantity, 3.0);
}

#[test]
fn a_value_from_an_earlier_day_is_recomputed() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("".into()).unwrap();
    let car = v
        .save_asset_item(
            acc,
            None,
            manual(
                "Auto",
                &days_ago(100),
                20_000_00,
                AssetInterpolation::Linear,
            ),
        )
        .unwrap();
    v.set_asset_value(car, days_ago(0), 10_000_00).unwrap();
    // as if the app was last open long ago: an old value of an old day
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET balance_cents = 1, balance_date = '2000-01-01' WHERE id = ?1",
            [acc],
        )
        .unwrap();
    assert_eq!(balance(&v, acc), 10_000_00);
}

#[test]
fn lookups_of_one_price_and_one_item() {
    let (v, _p) = temp_vault();
    let acc = v.create_tangible_account("".into()).unwrap();
    api_prices(&v, &[(&days_ago(9), 2_000_00), (&days_ago(3), 2_500_00)]);
    let bar = v
        .save_asset_item(acc, None, gold("Barren", &days_ago(9), 1.0, None))
        .unwrap();
    // the last price on or before the day
    assert_eq!(
        v.asset_price_on("XAU".into(), days_ago(5))
            .unwrap()
            .unwrap()
            .value_cents,
        2_000_00
    );
    assert_eq!(
        v.latest_asset_price("XAU".into()).unwrap().unwrap().date,
        days_ago(3)
    );
    assert!(
        v.asset_price_on("XAU".into(), days_ago(20))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        v.asset_unit_value_on(bar, days_ago(1)).unwrap(),
        Some(2_500_00)
    );
    // every item of every Sachwerte account in one call
    let other = v.create_tangible_account("Haus".into()).unwrap();
    v.save_asset_item(
        other,
        None,
        manual("Haus", &days_ago(50), 300_000_00, AssetInterpolation::Step),
    )
    .unwrap();
    assert_eq!(v.list_all_asset_items().unwrap().len(), 2);
}
