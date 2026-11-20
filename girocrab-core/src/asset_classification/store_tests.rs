use super::*;
use crate::{
    connectors::{FetchResult, RemoteAccount, RemoteHolding, store_fetch},
    models::{AccountKind, ConnectorKind},
    vault::Vault,
};
use std::sync::Arc;
fn setup(name: &str) -> (crate::test_support::TempDir, Arc<Vault>, i64) {
    let dir = crate::test_support::TempDir::new(name);
    let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7; 32]).unwrap();
    let id = v
        .add_bank_connection(ConnectorKind::Fnz, "Test".into(), "test".into())
        .unwrap();
    (dir, v, id)
}
fn fetch(v: &Vault, id: i64, asset: Option<AssetClass>) -> i64 {
    store_fetch(
        v,
        ConnectorKind::Fnz,
        id,
        FetchResult {
            accounts: vec![RemoteAccount {
                external_ref: "depot".into(),
                name: "Depot".into(),
                owner: "Test".into(),
                iban: String::new(),
                account_number: format!("D{id}"),
                currency: "EUR".into(),
                kind: AccountKind::Brokerage,
                balance_cents: 10000,
                balance_date: "2026-11-15".into(),
            }],
            holdings: vec![RemoteHolding {
                account_ref: "depot".into(),
                holding: Holding {
                    isin: "US0378331005".into(),
                    name: "Apple".into(),
                    value_cents: 10000,
                    quantity: 1.0,
                    ..Default::default()
                },
                classification: ClassificationValues {
                    asset_class: asset,
                    instrument: None,
                },
                direct_crypto: false,
            }],
            ..Default::default()
        },
    )
    .unwrap();
    v.list_accounts().unwrap()[0].id
}
#[test]
fn overrides_survive_refresh_and_reset_axes_independently() {
    let (_d, v, id) = setup("classification-refresh");
    let account = fetch(&v, id, Some(AssetClass::Bond));
    let first = v.classified_assets(account).unwrap();
    let key = first[0].classification.security_key.clone().unwrap();
    assert_eq!(
        first[0].classification.asset_class_source,
        ClassificationSource::Import
    );
    v.set_asset_class_override(key.clone(), Some(AssetClass::MultiAsset))
        .unwrap();
    v.set_instrument_override(key.clone(), Some(InstrumentKind::Fund))
        .unwrap();
    fetch(&v, id, Some(AssetClass::Equity));
    let c = v
        .classified_assets(account)
        .unwrap()
        .remove(0)
        .classification;
    assert_eq!(
        (c.asset_class, c.instrument),
        (AssetClass::MultiAsset, InstrumentKind::Fund)
    );
    v.set_asset_class_override(key.clone(), None).unwrap();
    let c = v
        .classified_assets(account)
        .unwrap()
        .remove(0)
        .classification;
    assert_eq!(c.asset_class_source, ClassificationSource::Import);
    assert_eq!(c.instrument_source, ClassificationSource::User);
    let db = v.db().unwrap();
    let reset:(Option<String>,i64)=db.query_row("SELECT asset_class_override,asset_class_ms FROM security_classifications WHERE security_key=?1",[key],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(reset.0, None);
    assert!(reset.1 > 0);
    assert_eq!(
        db.query_row("SELECT count(*) FROM holding_classifications", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn merge_is_per_field_and_resets_do_not_resurrect() {
    let (_da, a, aid) = setup("classification-merge-a");
    let (_db, b, bid) = setup("classification-merge-b");
    let account = fetch(&a, aid, None);
    fetch(&b, bid, None);
    let key = "isin:US0378331005".to_string();
    a.set_asset_class_override(key.clone(), Some(AssetClass::Bond))
        .unwrap();
    b.set_instrument_override(key.clone(), Some(InstrumentKind::Fund))
        .unwrap();
    a.merge_peer_snapshot(b.file_path()).unwrap();
    b.merge_peer_snapshot(a.file_path()).unwrap();
    let c = a
        .classified_assets(account)
        .unwrap()
        .remove(0)
        .classification;
    assert_eq!(
        (c.asset_class, c.instrument),
        (AssetClass::Bond, InstrumentKind::Fund)
    );
    a.set_asset_class_override(key.clone(), None).unwrap();
    b.merge_peer_snapshot(a.file_path()).unwrap();
    a.merge_peer_snapshot(b.file_path()).unwrap();
    let c = a
        .classified_assets(account)
        .unwrap()
        .remove(0)
        .classification;
    assert_eq!(
        (c.asset_class, c.asset_class_source),
        (AssetClass::Equity, ClassificationSource::Rule)
    );
    assert_eq!(
        (c.instrument, c.instrument_source),
        (InstrumentKind::Fund, ClassificationSource::User)
    );
    // Equal timestamp ties also converge; reset takes precedence over an old value.
    a.db()
        .unwrap()
        .execute(
            "UPDATE security_classifications SET asset_class_ms=42,asset_class_override=NULL",
            [],
        )
        .unwrap();
    b.db()
        .unwrap()
        .execute(
            "UPDATE security_classifications SET asset_class_ms=42,asset_class_override='bond'",
            [],
        )
        .unwrap();
    a.merge_peer_snapshot(b.file_path()).unwrap();
    b.merge_peer_snapshot(a.file_path()).unwrap();
    for v in [a, b] {
        assert_eq!(
            v.db()
                .unwrap()
                .query_row(
                    "SELECT asset_class_override FROM security_classifications",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )
                .unwrap(),
            None
        );
    }
}
#[test]
fn invalid_and_blank_identity_cannot_create_shared_overrides() {
    let (_d, v, id) = setup("classification-invalid");
    fetch(&v, id, None);
    for key in [
        "",
        "isin:",
        "isin:US0378331006",
        "crypto:UNKNOWN",
        "asset:missing",
        "name:Apple",
    ] {
        assert!(
            v.set_asset_class_override(key.into(), Some(AssetClass::Other))
                .is_err()
        );
    }
    assert_eq!(
        v.db()
            .unwrap()
            .query_row("SELECT count(*) FROM security_classifications", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}
#[test]
fn same_isin_in_two_depots_shares_choice_but_not_values() {
    let (_d, v, id) = setup("classification-shared");
    let account = fetch(&v, id, None);
    let second = v
        .add_bank_connection(ConnectorKind::Fnz, "Other".into(), "other".into())
        .unwrap();
    fetch(&v, second, None);
    v.set_asset_class_override("isin:US0378331005".into(), Some(AssetClass::Other))
        .unwrap();
    let accounts = v.list_accounts().unwrap();
    assert_eq!(accounts.len(), 2);
    let rows: Vec<_> = accounts
        .iter()
        .flat_map(|a| v.classified_assets(a.id).unwrap())
        .collect();
    assert_eq!(
        rows.iter().map(|r| r.holding.value_cents).sum::<i64>(),
        20000
    );
    assert!(
        rows.iter()
            .all(|r| r.classification.asset_class == AssetClass::Other)
    );
    assert_eq!(v.list_holdings(account).unwrap().len(), 1);
}
#[test]
fn tangible_choices_use_stable_uuid_and_sold_items_are_excluded() {
    let (_d, v, _) = setup("classification-tangible");
    v.seed_demo_data().unwrap();
    let demo_items: Vec<_> = v
        .list_tangible_account_ids()
        .unwrap()
        .into_iter()
        .flat_map(|id| v.classified_assets(id).unwrap())
        .collect();
    let house = demo_items
        .iter()
        .find(|r| r.holding.name == "Einfamilienhaus")
        .unwrap();
    assert_eq!(house.classification.asset_class, AssetClass::RealEstate);
    assert_eq!(
        house.classification.asset_class_source,
        ClassificationSource::User
    );
    assert_eq!(
        house.classification.instrument,
        InstrumentKind::DirectHolding
    );
    for id in v.list_tangible_account_ids().unwrap() {
        for row in v.classified_assets(id).unwrap() {
            let key = row.classification.security_key.clone().unwrap();
            assert!(key.starts_with("asset:"));
            assert_eq!(row.classification.instrument, InstrumentKind::DirectHolding);
            v.set_asset_class_override(key, Some(AssetClass::RealEstate))
                .unwrap();
            assert!(
                v.classified_assets(id)
                    .unwrap()
                    .iter()
                    .find(|r| r.asset_item_id == row.asset_item_id)
                    .unwrap()
                    .classification
                    .asset_class
                    == AssetClass::RealEstate
            );
        }
    }
}
#[test]
fn exact_import_labels_do_not_conflate_etf_with_equity() {
    assert_eq!(
        from_labels("", "ETF"),
        ClassificationValues {
            asset_class: None,
            instrument: Some(InstrumentKind::Etf)
        }
    );
    assert_eq!(
        from_labels("Aktien", ""),
        ClassificationValues {
            asset_class: Some(AssetClass::Equity),
            instrument: None
        }
    );
    assert_eq!(
        from_labels("Funds", "Mystery"),
        ClassificationValues::default()
    );
}
