use super::*;
use crate::i18n::{Lang, with_language};

fn vault(dir: &crate::test_support::TempDir) -> std::sync::Arc<Vault> {
    Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7; 32]).unwrap()
}

#[test]
fn cached_booking_names_follow_language_without_a_write() {
    let dir = crate::test_support::TempDir::new("roles-language-cache");
    let v = vault(&dir);
    v.seed_demo_data().unwrap();
    let read = || {
        v.list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
    };
    let german = with_language(Lang::De, read)
        .into_iter()
        .find(|t| t.category_name == "Supermarkt")
        .unwrap();
    let english = with_language(Lang::En, read)
        .into_iter()
        .find(|t| t.id == german.id)
        .unwrap();
    assert_eq!(english.category_name, "Supermarket");
    assert_eq!(english.category_path, "Food › Supermarket");
    assert_eq!(english.flow, german.flow);
    let german_again = with_language(Lang::De, read)
        .into_iter()
        .find(|t| t.id == german.id)
        .unwrap();
    assert_eq!(german_again.category_path, german.category_path);
}

#[test]
fn custom_names_paths_budgets_and_imports_stay_literal() {
    let dir = crate::test_support::TempDir::new("roles-custom");
    let v = vault(&dir);
    v.seed_starter_categories().unwrap();
    let cats = v.list_categories(None, None).unwrap();
    let leisure = cats.iter().find(|c| c.name == "Freizeit").unwrap().id;
    let custom = v
        .create_category("Freizeit".into(), Some(leisure), "tag".into(), "".into())
        .unwrap();
    v.set_category_budget(custom, Some(10000)).unwrap();
    with_language(Lang::En, || {
        let cats = v.list_categories(None, None).unwrap();
        let c = cats.iter().find(|c| c.id == custom).unwrap();
        assert_eq!(c.name, "Freizeit");
        assert_eq!(c.path, "Leisure › Freizeit");
        let budgets = v.list_category_budget_progress(2026, 11).unwrap();
        let b = budgets.iter().find(|b| b.category_id == custom).unwrap();
        assert_eq!(b.path, c.path);
        // Saving a user name matching a translated default is a real rename.
        v.update_category(
            custom,
            "Leisure".into(),
            Some(leisure),
            "tag".into(),
            "".into(),
        )
        .unwrap();
    });
    assert_eq!(
        v.list_categories(None, None)
            .unwrap()
            .iter()
            .find(|c| c.id == custom)
            .unwrap()
            .name,
        "Leisure"
    );
    // Reusing a starter row for an import transfers ownership of its name.
    let db = v.db().unwrap();
    let mut cache = HashMap::new();
    let mut created = 0;
    let imported = crate::importers::moneymoney_import::find_or_create_category(
        &db,
        &["Freizeit".into()],
        &mut cache,
        &mut created,
    )
    .unwrap();
    assert_eq!(imported, leisure);
    drop(db);
    with_language(Lang::En, || {
        assert_eq!(
            v.list_categories(None, None)
                .unwrap()
                .iter()
                .find(|c| c.id == leisure)
                .unwrap()
                .name,
            "Freizeit"
        );
    });
}

#[test]
fn renamed_builtin_keeps_role_and_custom_name_does_not_classify() {
    let dir = crate::test_support::TempDir::new("roles-flow");
    let v = vault(&dir);
    v.seed_demo_data().unwrap();
    let category = v
        .list_categories(None, None)
        .unwrap()
        .into_iter()
        .find(|c| c.name == "Sparen & Investieren")
        .unwrap();
    v.update_category(
        category.id,
        "Mein Sparen".into(),
        None,
        category.sf_symbol,
        category.color_hex,
    )
    .unwrap();
    let custom = v
        .create_category("Sparen & Investieren".into(), None, "tag".into(), "".into())
        .unwrap();
    let booking = v
        .list_transactions(BookingFilter::default())
        .unwrap()
        .transactions
        .into_iter()
        .find(|t| t.amount_cents < 0 && t.flow == crate::merchants::FlowKind::Consumption)
        .unwrap();
    // Neutralise independent merchant/text signals; isolate category identity.
    v.db().unwrap().execute("UPDATE transactions SET counterparty='Test',purpose='',booking_text='',counterparty_iban='' WHERE id=?1", [booking.id]).unwrap();
    v.set_transaction_category(booking.id, Some(category.id))
        .unwrap();
    let read = || {
        v.list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .find(|t| t.id == booking.id)
            .unwrap()
    };
    assert_eq!(read().flow, crate::merchants::FlowKind::Investment);
    assert_eq!(read().category_name, "Mein Sparen");
    v.set_transaction_category(booking.id, Some(custom))
        .unwrap();
    with_language(Lang::En, || {
        let t = read();
        assert_eq!(t.flow, crate::merchants::FlowKind::Consumption);
        assert_eq!(t.category_name, "Sparen & Investieren");
    });
}

#[test]
fn merging_preserves_roles_and_custom_provenance() {
    let a_dir = crate::test_support::TempDir::new("roles-merge-a");
    let b_dir = crate::test_support::TempDir::new("roles-merge-b");
    let a = vault(&a_dir);
    let b = vault(&b_dir);
    a.seed_starter_categories().unwrap();
    b.create_category("Freizeit".into(), None, "tag".into(), "".into())
        .unwrap();
    crate::importers::moneymoney_import::find_or_create_category(
        &b.db().unwrap(),
        &["Umbuchungen".into()],
        &mut HashMap::new(),
        &mut 0,
    )
    .unwrap();
    for v in [&a, &b] {
        let db = v.db().unwrap();
        let id = Vault::ensure_group(&db, "Kreditkarten", "creditcard", 2).unwrap();
        if std::sync::Arc::ptr_eq(v, &a) {
            db.execute(
                "UPDATE account_groups SET name='Meine Karten' WHERE id=?1",
                [id],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO account_groups(name) VALUES ('Kreditkarten')",
            [],
        )
        .unwrap();
    }
    a.merge_other_vault(b_dir.join("v.db").to_string_lossy().into(), vec![7; 32])
        .unwrap();
    with_language(Lang::En, || {
        let cats = a.list_categories(None, None).unwrap();
        assert!(cats.iter().any(|c| c.name == "Freizeit"));
        assert!(!cats.iter().any(|c| c.name == "Leisure"));
        assert!(cats.iter().any(|c| c.name == "Income"));
        assert!(cats.iter().any(|c| c.name == "Umbuchungen"));
    });
    let db = a.db().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT builtin_role FROM categories WHERE name='Umbuchungen' AND deleted_ms IS NULL",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "import:transfers"
    );
    let groups: Vec<(String, Option<String>)> = db
        .prepare("SELECT name,builtin_role FROM account_groups ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        groups,
        vec![
            ("Meine Karten".into(), Some("Kreditkarten".into())),
            ("Kreditkarten".into(), None)
        ]
    );
}

#[test]
fn builtin_groups_do_not_capture_custom_names() {
    let dir = crate::test_support::TempDir::new("roles-groups");
    let v = vault(&dir);
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO account_groups(name) VALUES ('Darlehen & Kredite')",
        [],
    )
    .unwrap();
    let custom = db.last_insert_rowid();
    let builtin = Vault::ensure_group(&db, "Darlehen & Kredite", "house", 4).unwrap();
    assert_ne!(custom, builtin);
    db.execute(
        "UPDATE account_groups SET name='Meine Kredite' WHERE id=?1",
        [builtin],
    )
    .unwrap();
    assert_eq!(
        Vault::ensure_group(&db, "Darlehen & Kredite", "house", 4).unwrap(),
        builtin
    );
}
