use std::sync::Arc;

use rusqlite::params;

use super::{CategorySuggestion, SuggestionBasis};
use crate::models::{MatchKind, Rule, RuleField};
use crate::test_support::TempDir;
use crate::vault::Vault;

const OWN_IBAN: &str = "DE89370400440532013000";
const ANNA_1: &str = "DE02120300000000202051";
const ANNA_2: &str = "DE44500105175407324931";
const ANNA_3: &str = "DE75512108001245126199";

struct Fixture {
    _dir: TempDir,
    v: Arc<Vault>,
    account: i64,
}

/// One booking to seed; `b(…)` gives an uncategorised card payment.
#[derive(Clone, Copy)]
struct B<'a> {
    date: &'a str,
    cents: i64,
    counterparty: &'a str,
    iban: &'a str,
    purpose: &'a str,
    booking_text: &'a str,
    category: Option<&'a str>,
    source: &'a str,
    rule_id: Option<i64>,
    checked: bool,
}

fn b(counterparty: &str, cents: i64) -> B<'_> {
    B {
        date: "2026-10-01",
        cents,
        counterparty,
        iban: "",
        purpose: "",
        booking_text: "",
        category: None,
        source: "none",
        rule_id: None,
        checked: false,
    }
}

impl<'a> B<'a> {
    fn manual(self, category: &'a str) -> Self {
        B {
            category: Some(category),
            source: "manual",
            ..self
        }
    }
    /// Categorised by the rule `rule_id` (made with `Fixture::rule`).
    fn by_rule(self, rule_id: i64, category: &'a str, checked: bool) -> Self {
        B {
            category: Some(category),
            source: "rule",
            rule_id: Some(rule_id),
            checked,
            ..self
        }
    }
    fn iban(self, iban: &'a str) -> Self {
        B { iban, ..self }
    }
    fn on(self, date: &'a str) -> Self {
        B { date, ..self }
    }
    fn purpose(self, purpose: &'a str) -> Self {
        B { purpose, ..self }
    }
    fn text(self, booking_text: &'a str) -> Self {
        B {
            booking_text,
            ..self
        }
    }
}

fn vault() -> Fixture {
    let dir = TempDir::new("suggestions");
    let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
    let db = v.db().unwrap();
    db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
        .unwrap();
    db.execute(
        "INSERT INTO accounts(group_id, external_ref, name, kind, iban)
         VALUES (last_insert_rowid(), 'A1', 'Giro', 'checking', ?1)",
        [OWN_IBAN],
    )
    .unwrap();
    let account = db.last_insert_rowid();
    drop(db);
    Fixture {
        _dir: dir,
        v,
        account,
    }
}

impl Fixture {
    fn category(&self, name: &str) -> i64 {
        let found = self.v.db().unwrap().query_row(
            "SELECT id FROM categories WHERE name = ?1",
            [name],
            |r| r.get(0),
        );
        found.unwrap_or_else(|_| {
            self.v
                .create_category(name.into(), None, "tag".into(), String::new())
                .unwrap()
        })
    }

    fn rule(
        &self,
        category: &str,
        match_kind: MatchKind,
        pattern: &str,
        amount: (Option<i64>, Option<i64>),
    ) -> i64 {
        self.v
            .save_rule(Rule {
                id: 0,
                name: pattern.into(),
                category_id: self.category(category),
                field: RuleField::Any,
                match_kind,
                pattern: pattern.into(),
                case_sensitive: false,
                min_amount_cents: amount.0,
                max_amount_cents: amount.1,
                account_ids: vec![],
                priority: 0,
                enabled: true,
                match_count: 0,
                tag_ids: Vec::new(),
            })
            .unwrap()
    }

    fn add(&self, bk: B) -> i64 {
        let category = bk.category.map(|c| self.category(c));
        let db = self.v.db().unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty,
                                      counterparty_iban, purpose, booking_text, category_id, category_source,
                                      rule_id, checked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                self.account,
                format!("t{n}"),
                bk.date,
                bk.cents,
                bk.counterparty,
                bk.iban,
                bk.purpose,
                bk.booking_text,
                category,
                bk.source,
                bk.rule_id,
                bk.checked
            ],
        )
        .unwrap();
        db.last_insert_rowid()
    }

    fn add_n(&self, n: usize, bk: B) {
        for _ in 0..n {
            self.add(bk);
        }
    }

    fn suggest(&self, id: i64) -> Option<CategorySuggestion> {
        let mut all = self.v.suggest_categories(vec![id]).unwrap();
        assert!(all.len() <= 1);
        all.pop()
    }
}

#[test]
fn merchant_history_suggests_the_hand_picked_category() {
    let f = vault();
    f.add_n(3, b("REWE Markt GmbH", -23_40).manual("Lebensmittel"));
    let id = f.add(b("REWE SAGT DANKE 4711", -8_15));
    assert_eq!(
        f.suggest(id),
        Some(CategorySuggestion {
            booking_id: id,
            category_id: f.category("Lebensmittel"),
            basis: SuggestionBasis::Merchant,
            support: 3,
            considered: 3,
        })
    );
}

#[test]
fn iban_tells_two_payees_of_the_same_name_apart() {
    let f = vault();
    f.add_n(2, b("Anna Schmidt", -500_00).iban(ANNA_1).manual("Miete"));
    f.add_n(
        3,
        b("Anna Schmidt", -30_00).iban(ANNA_2).manual("Geschenke"),
    );
    let first = f.add(b("Anna Schmidt", -500_00).iban("DE02 1203 0000 0000 2020 51"));
    let s = f.suggest(first).unwrap();
    assert_eq!(s.category_id, f.category("Miete"));
    assert_eq!(
        (s.basis, s.support, s.considered),
        (SuggestionBasis::Iban, 2, 2)
    );
    let second = f.add(b("Anna Schmidt", -25_00).iban(ANNA_2));
    assert_eq!(
        f.suggest(second).unwrap().category_id,
        f.category("Geschenke")
    );
    // a third IBAN: only the name is left, and it is split 2 : 3
    let third = f.add(b("Anna Schmidt", -25_00).iban(ANNA_3));
    assert_eq!(f.suggest(third), None);
}

#[test]
fn a_shared_iban_never_decides_on_its_own() {
    // one collecting account for several payees (payment service, card acquirer)
    let f = vault();
    f.add_n(
        3,
        b("Stadtwerke Muster", -80_00)
            .iban(ANNA_1)
            .manual("Energie"),
    );
    let id = f.add(b("Fitnessstudio Kraftwerk", -39_90).iban(ANNA_1));
    assert_eq!(f.suggest(id), None);
}

#[test]
fn a_payment_service_with_nothing_behind_it_suggests_nothing() {
    let f = vault();
    let paypal =
        b("PayPal Europe S.a.r.l. et Cie S.C.A", -12_00).purpose("Ihr Einkauf bei Kleiner Laden");
    let m = crate::merchants::MerchantResolver::resolve(paypal.counterparty, paypal.purpose);
    assert_eq!(m.kind, crate::merchants::MerchantKind::PaymentService);
    f.add_n(3, paypal.manual("Shopping"));
    let id = f.add(paypal);
    assert_eq!(f.suggest(id), None);
}

#[test]
fn checked_alone_is_no_history() {
    let f = vault();
    let rule = f.rule("Streaming", MatchKind::Keyword, "Spotify", (None, None));
    let spotify = b("Spotify AB", -10_99);
    f.add_n(3, spotify.by_rule(rule, "Streaming", false));
    let id = f.add(spotify);
    assert_eq!(f.suggest(id), None, "unchecked rule bookings do not vote");
    f.add_n(2, spotify.by_rule(rule, "Streaming", true));
    let s = f.suggest(id).unwrap();
    assert_eq!((s.support, s.considered), (2, 2));
}

#[test]
fn a_checked_rule_booking_counts_half() {
    // 3 : 2 by count would be too mixed (60 %), 3 : 1 by weight is clear (75 %)
    let f = vault();
    f.add_n(3, b("Bäckerei Krause", -4_20).manual("Bäcker"));
    let rule = f.rule("Restaurant", MatchKind::Keyword, "Krause", (None, None));
    f.add_n(
        2,
        b("Bäckerei Krause", -4_20).by_rule(rule, "Restaurant", true),
    );
    let id = f.add(b("Bäckerei Krause", -3_80));
    let s = f.suggest(id).unwrap();
    assert_eq!(s.category_id, f.category("Bäcker"));
    assert_eq!((s.support, s.considered), (3, 5));
}

#[test]
fn too_little_or_too_mixed_history_suggests_nothing() {
    let f = vault();
    f.add(b("Buchhandlung Seitenweise", -18_00).manual("Bücher"));
    let id = f.add(b("Buchhandlung Seitenweise", -9_00));
    assert_eq!(f.suggest(id), None, "one booking is not enough");
    f.add(b("Buchhandlung Seitenweise", -18_00).manual("Bücher"));
    f.add_n(2, b("Buchhandlung Seitenweise", -18_00).manual("Geschenke"));
    assert_eq!(f.suggest(id), None, "2 : 2 is no decision");
}

#[test]
fn a_refund_does_not_take_the_purchase_category() {
    let f = vault();
    f.add_n(3, b("Zalando SE", -59_95).manual("Kleidung"));
    let refund = f.add(b("Zalando SE", 59_95));
    assert_eq!(f.suggest(refund), None);
}

#[test]
fn a_recent_choice_beats_an_old_habit() {
    let f = vault();
    f.add_n(3, b("Netflix", -7_99).on("2018-02-01").manual("Freizeit"));
    f.add_n(3, b("Netflix", -13_99).on("2026-09-01").manual("Streaming"));
    let id = f.add(b("Netflix", -13_99).on("2026-11-01"));
    let s = f.suggest(id).unwrap();
    assert_eq!(s.category_id, f.category("Streaming"));
    assert_eq!((s.support, s.considered), (3, 6));
}

#[test]
fn only_uncategorised_bookings_get_one() {
    let f = vault();
    f.add_n(3, b("Edeka Center", -31_00).manual("Lebensmittel"));
    let manual = f.add(b("Edeka Center", -31_00).manual("Haushalt"));
    let rule = f.rule("Haushalt", MatchKind::Keyword, "Edeka", (None, None));
    let by_rule = f.add(b("Edeka Center", -31_00).by_rule(rule, "Haushalt", false));
    assert!(
        f.v.suggest_categories(vec![manual, by_rule, 999_999])
            .unwrap()
            .is_empty()
    );
    assert!(f.v.suggest_categories(vec![]).unwrap().is_empty());
}

#[test]
fn splits_own_transfers_and_deleted_categories_are_left_out() {
    let f = vault();
    // split bookings: only one plain booking remains as history
    for _ in 0..3 {
        let id = f.add(b("Baumarkt Hammer", -120_00).manual("Haushalt"));
        f.v.db()
            .unwrap()
            .execute(
                "INSERT INTO transaction_splits(transaction_id, category_id, amount_cents) VALUES (?1, NULL, -6000)",
                [id],
            )
            .unwrap();
    }
    f.add(b("Baumarkt Hammer", -20_00).manual("Haushalt"));
    let id = f.add(b("Baumarkt Hammer", -15_00));
    assert_eq!(f.suggest(id), None);

    // transfers between own accounts (counterparty = own IBAN, spaced or not)
    f.add_n(
        3,
        b("Max Mustermann", -100_00).iban(OWN_IBAN).manual("Sparen"),
    );
    let transfer = f.add(b("Max Mustermann", -100_00).iban("DE89 3704 0044 0532 0130 00"));
    assert_eq!(f.suggest(transfer), None);

    // a deleted category does not vote
    f.add_n(3, b("Kiosk Ecke", -3_00).manual("Snacks"));
    let kiosk = f.add(b("Kiosk Ecke", -2_50));
    assert!(f.suggest(kiosk).is_some());
    f.v.db()
        .unwrap()
        .execute(
            "UPDATE categories SET deleted_ms = 1 WHERE name = 'Snacks'",
            [],
        )
        .unwrap();
    assert_eq!(f.suggest(kiosk), None);
}

#[test]
fn one_call_answers_a_whole_page_in_order() {
    let f = vault();
    f.add_n(2, b("Aral Tankstelle", -70_00).manual("Tanken"));
    f.add_n(2, b("Stadtbibliothek", -2_00).manual("Bildung"));
    let fuel = f.add(b("Aral Tankstelle", -65_00));
    let nothing = f.add(b("Unbekannt GmbH", -1_00));
    let library = f.add(b("Stadtbibliothek", -2_00));
    let got: Vec<(i64, i64)> =
        f.v.suggest_categories(vec![library, nothing, fuel, library])
            .unwrap()
            .into_iter()
            .map(|s| (s.booking_id, s.category_id))
            .collect();
    assert_eq!(
        got,
        vec![
            (library, f.category("Bildung")),
            (fuel, f.category("Tanken"))
        ]
    );
}

#[test]
fn a_booking_to_the_same_iban_with_another_category_vetoes_the_name() {
    // one rent payment to this IBAN outweighs four gifts to a namesake
    let f = vault();
    f.add(b("Anna Schmidt", -500_00).iban(ANNA_1).manual("Miete"));
    f.add_n(
        4,
        b("Anna Schmidt", -30_00).iban(ANNA_2).manual("Geschenke"),
    );
    let rent = f.add(b("Anna Schmidt", -500_00).iban(ANNA_1));
    assert_eq!(f.suggest(rent), None);
    // the same IBAN agreeing with the name does not hold it back
    let gift = f.add(b("Anna Schmidt", -30_00).iban(ANNA_3));
    assert_eq!(
        f.suggest(gift).unwrap().category_id,
        f.category("Geschenke")
    );
}

#[test]
fn a_rule_vote_only_counts_inside_the_rules_limits() {
    let f = vault();
    let rent = f.rule(
        "Miete",
        MatchKind::Keyword,
        "Wohnung",
        (Some(-500_00), Some(-500_00)),
    );
    f.add_n(
        4,
        b("Anna Schmidt", -500_00)
            .purpose("Wohnung")
            .by_rule(rent, "Miete", true),
    );
    let small = f.add(b("Anna Schmidt", -25_00));
    assert_eq!(f.suggest(small), None, "the rule left -25 € out on purpose");
    let reworded = f.add(b("Anna Schmidt", -500_00).purpose("Miete Dezember"));
    let s = f.suggest(reworded).unwrap();
    assert_eq!(s.category_id, f.category("Miete"));
    assert_eq!((s.support, s.considered), (4, 4));
}

#[test]
fn bookings_of_expression_rules_never_vote() {
    let f = vault();
    let gym = f.rule(
        "Sport",
        MatchKind::Expression,
        "name:Kraftwerk",
        (None, None),
    );
    f.add_n(3, b("Kraftwerk Gym", -39_90).by_rule(gym, "Sport", true));
    let id = f.add(b("Kraftwerk Gym", -39_90));
    assert_eq!(f.suggest(id), None);
}

#[test]
fn a_transfer_by_its_wording_gets_none() {
    let f = vault();
    f.add_n(3, b("Mama", -50_00).iban(ANNA_1).manual("Familie"));
    let transfer = f.add(
        b("Mama", -50_00)
            .iban(ANNA_1)
            .text("Übertrag / Überweisung"),
    );
    assert_eq!(f.suggest(transfer), None);
    let payment = f.add(b("Mama", -50_00).iban(ANNA_1));
    assert!(f.suggest(payment).is_some());
}

#[test]
fn a_blank_account_iban_hides_no_card_payment() {
    let f = vault();
    f.v.db()
        .unwrap()
        .execute(
            "INSERT INTO accounts(group_id, external_ref, name, kind, iban)
             VALUES ((SELECT id FROM account_groups), 'A2', 'Karte', 'credit_card', ' ')",
            [],
        )
        .unwrap();
    f.add_n(2, b("Eiscafé Venezia", -6_50).manual("Restaurant"));
    let id = f.add(b("Eiscafé Venezia", -4_00));
    assert!(f.suggest(id).is_some());
}

#[test]
fn the_history_is_read_again_only_after_a_change() {
    let f = vault();
    f.add_n(2, b("Kino Lichtspiele", -12_00).manual("Freizeit"));
    let id = f.add(b("Kino Lichtspiele", -12_00));
    let cached = || f.v.suggestions.current().unwrap();
    assert!(f.suggest(id).is_some());
    let first = cached();
    assert!(f.suggest(id).is_some());
    assert!(
        std::sync::Arc::ptr_eq(&first, &cached()),
        "unchanged data: no new scan"
    );
    f.add_n(3, b("Kino Lichtspiele", -12_00).manual("Kultur"));
    assert_eq!(f.suggest(id), None, "the new bookings count at once");
    assert!(!std::sync::Arc::ptr_eq(&first, &cached()));
}

#[test]
fn words_leave_out_numbers_short_words_and_bank_wording() {
    let w = super::similar::words(&[
        "SEPA-Lastschrift",
        "Abschlag Fernwärme 11/2026 KD 4711 EREF+AB12",
    ]);
    assert_eq!(w, vec!["abschlag", "fernwärme"]);
}

#[test]
fn similar_text_helps_a_merchant_without_history() {
    let f = vault();
    f.add_n(
        3,
        b("Stadtwerke Nord", -80_00)
            .purpose("Abschlag Fernwärme Nord")
            .manual("Energie"),
    );
    f.add_n(
        3,
        b("Buchladen Seite", -15_00)
            .purpose("Roman Taschenbuch")
            .manual("Bücher"),
    );
    let new_payee = f.add(b("Fernwärme Nord GmbH", -80_00).purpose("Abschlag"));
    let s = f.suggest(new_payee).unwrap();
    assert_eq!(s.category_id, f.category("Energie"));
    assert_eq!(
        (s.basis, s.support, s.considered),
        (SuggestionBasis::Similar, 3, 3)
    );
    // a refund does not take the purchases' category
    let refund = f.add(b("Fernwärme Nord GmbH", 80_00).purpose("Abschlag"));
    assert_eq!(f.suggest(refund), None);
}

#[test]
fn similar_text_needs_two_close_bookings() {
    let f = vault();
    f.add(
        b("Stadtwerke Nord", -80_00)
            .purpose("Abschlag Fernwärme Nord")
            .manual("Energie"),
    );
    let id = f.add(b("Fernwärme Nord GmbH", -80_00).purpose("Abschlag"));
    assert_eq!(f.suggest(id), None, "one similar booking is not enough");
    f.add(
        b("Pizzeria Nord", -20_00)
            .purpose("Lieferung")
            .manual("Restaurant"),
    );
    f.add(
        b("Stadtwerke Nord", -80_00)
            .purpose("Abschlag Fernwärme Nord")
            .manual("Energie"),
    );
    assert!(f.suggest(id).is_some());
}

#[test]
fn similar_text_never_overrides_a_mixed_merchant() {
    let f = vault();
    f.add_n(
        3,
        b("Stadtwerke Nord", -80_00)
            .purpose("Abschlag Fernwärme Nord")
            .manual("Energie"),
    );
    // the merchant itself has history, only too mixed: no stage 3
    f.add(
        b("Fernwärme Nord GmbH", -80_00)
            .purpose("Abschlag")
            .manual("Wohnen"),
    );
    f.add(
        b("Fernwärme Nord GmbH", -80_00)
            .purpose("Abschlag")
            .manual("Energie"),
    );
    let id = f.add(b("Fernwärme Nord GmbH", -80_00).purpose("Abschlag"));
    assert_eq!(f.suggest(id), None);
}
