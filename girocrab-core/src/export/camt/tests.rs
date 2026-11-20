// xmllint checks the exports against the ISO 20022 schemas; tests may start processes (#948).
#![allow(clippy::disallowed_types)]

use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn vault(tag: &str) -> (Arc<Vault>, PathBuf) {
    let p = crate::test_support::thread_temp_path(tag);
    let v = Vault::open(p.to_string_lossy().into(), vec![7u8; 32]).unwrap();
    (v, p)
}

fn group(db: &Connection) -> i64 {
    db.execute(
        "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G', 'tray', 1)
         ON CONFLICT DO NOTHING",
        [],
    )
    .unwrap();
    db.query_row("SELECT id FROM account_groups WHERE name = 'G'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

/// An account with a balance the bank reported at `balance_date`.
fn bank_account(v: &Vault, tag: &str, currency: &str, balance: i64, balance_date: &str) -> i64 {
    let db = v.db().unwrap();
    let g = group(&db);
    db.execute(
        "INSERT INTO bank_connections(connector, display_name) VALUES ('fints', 'Bank')",
        [],
    )
    .unwrap();
    let c = db.last_insert_rowid();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner, iban,
                              account_number, currency, kind, balance_cents, balance_date, balance_derived)
         VALUES (?1, ?2, ?3, 'Giro', 'Bank', 'Erika Muster', 'DE02120300000000202051',
                 '4711', ?4, 'checking', ?5, ?6, 0)",
        params![c, g, format!("bank-{tag}"), currency, balance, balance_date],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// An account whose balance GiroCrab computes: an optional Startsaldo plus
/// the bookings. `balance_cents` mirrors what the list shows.
fn computed_account(
    v: &Vault,
    tag: &str,
    kind: &str,
    opening: Option<i64>,
    opening_date: &str,
) -> i64 {
    let db = v.db().unwrap();
    let g = group(&db);
    db.execute(
        "INSERT INTO accounts(group_id, external_ref, name, currency, kind, balance_cents, balance_date)
         VALUES (?1, ?2, 'Konto', 'EUR', ?3, 0, '')",
        params![g, format!("computed-{tag}"), kind],
    )
    .unwrap();
    let id = db.last_insert_rowid();
    if let Some(cents) = opening {
        db.execute(
            "INSERT INTO opening_balances(account_id, opening_date, opening_cents, updated_ms)
             VALUES (?1, ?2, ?3, 0)",
            params![id, opening_date, cents],
        )
        .unwrap();
    }
    id
}

fn add(v: &Vault, account: i64, tag: &str, day: &str, cents: i64) -> i64 {
    add_in(v, account, tag, day, cents, "EUR")
}

fn add_in(v: &Vault, account: i64, tag: &str, day: &str, cents: i64, currency: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, currency,
                                  counterparty, counterparty_iban, purpose, booking_text)
         VALUES (?1, ?2, ?3, ?4, ?5, 'Gegenpartei', '', '', '')",
        params![account, tag, day, cents, currency],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// `days` from today (`-5` = five days ago).
fn day(offset: i64) -> String {
    crate::dates::iso(crate::dates::today() + crate::dates::days(offset))
}

fn export(
    v: &Vault,
    account: i64,
    from: Option<&str>,
    to: Option<&str>,
    ids: Option<Vec<i64>>,
    version: CamtVersion,
) -> String {
    v.camt053_export(Camt053Request {
        account_id: account,
        date_from: from.map(str::to_string),
        date_to: to.map(str::to_string),
        booking_ids: ids,
        version,
        created_at: "2026-03-31T12:00:00+02:00".into(),
    })
    .unwrap()
}

/// Cents and currency of the `OPBD` / `CLBD` balance.
fn balance(xml: &str, code: &str) -> (i64, String) {
    let bal = xml
        .split("<Bal>")
        .find(|b| b.contains(&format!("<Cd>{code}</Cd>")))
        .and_then(|b| b.split("</Bal>").next())
        .unwrap_or_else(|| panic!("{code} in {xml}"));
    let amt = bal.split("<Amt Ccy=\"").nth(1).unwrap();
    let currency = amt.split('"').next().unwrap().to_string();
    let text = amt.split('>').nth(1).unwrap().split('<').next().unwrap();
    let (euro, cent) = text.split_once('.').unwrap();
    let mut cents = euro.parse::<i64>().unwrap() * 100 + cent.parse::<i64>().unwrap();
    if bal.contains("<CdtDbtInd>DBIT</CdtDbtInd>") {
        cents = -cents;
    }
    (cents, currency)
}

/// `xmllint --schema` against the official ISO 20022 XSD; skipped where
/// xmllint is not installed (it ships with macOS and libxml2-utils).
fn validate(xml: &str, schema: &str) {
    let xsd = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/iso20022")
        .join(format!("{schema}.xsd"));
    let file = std::env::temp_dir().join(format!("camt-{}.xml", uuid::Uuid::new_v4()));
    std::fs::write(&file, xml).unwrap();
    let run = Command::new("xmllint")
        .args(["--noout", "--schema"])
        .arg(&xsd)
        .arg(&file)
        .output();
    let _ = std::fs::remove_file(&file);
    match run {
        Ok(o) => assert!(
            o.status.success(),
            "{schema}: {}\n{xml}",
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => eprintln!("xmllint not available ({e}); XSD check skipped"),
    }
}

/// A bank statement with every optional detail: pending entry, control
/// characters, a long purpose and counterparty, SEPA references, both GVC
/// forms and a value date.
fn rich(v: &Vault) -> i64 {
    let a = bank_account(v, "rich", "EUR", 1_000_000, &day(0));
    let one = add(v, a, "b1", &day(-20), 500_000);
    let pending = add(v, a, "pending-1", &day(-2), -50_000);
    let debit = add(v, a, "b3", &day(-5), -10_000);
    let gvc = add(v, a, "b4", &day(-1), -1_000);
    let svwz = add(v, a, "b5", &day(-4), 2_000);
    let ret = add(v, a, "b6", &day(-3), 7_000);
    let db = v.db().unwrap();
    db.execute(
        "UPDATE transactions SET purpose = ?1, purpose_code = 'SALA' WHERE id = ?2",
        params![
            format!("Rechnung\u{1}Nr. 7 & <Sonder> \u{b}{}", "x".repeat(300)),
            one
        ],
    )
    .unwrap();
    db.execute(
        "UPDATE transactions SET counterparty_iban = '1234567890' WHERE id = ?1",
        [pending],
    )
    .unwrap();
    db.execute(
        "UPDATE transactions SET counterparty = ?1, end_to_end_id = 'E2E-7', mandate_id = 'M-7',
                creditor_id = 'DE98ZZZ09999999999', booking_text = 'SEPA-Lastschrift',
                value_date = ?2, external_id = 'camt:BANKREF-77'
         WHERE id = ?3",
        params![format!("{} <&>", "N".repeat(200)), day(-6), debit],
    )
    .unwrap();
    db.execute(
        "UPDATE transactions SET purpose = 'EREF+SVWZ-9 MREF+M-9 SVWZ+Stripped purpose',
                end_to_end_id = 'SVWZ-9', mandate_id = 'M-9'
         WHERE id = ?1",
        [svwz],
    )
    .unwrap();
    db.execute(
        "UPDATE transactions SET gvc = '109', booking_text = 'Rücklastschrift' WHERE id = ?1",
        [ret],
    )
    .unwrap();
    db.execute("UPDATE transactions SET gvc = '153' WHERE id = ?1", [gvc])
        .unwrap();
    drop(db);
    a
}

#[test]
fn camt053_validates_against_iso20022_schemas() {
    let (v, _) = vault("camt-xsd");
    let a = rich(&v);
    for (version, schema) in [
        (CamtVersion::V02, "camt.053.001.02"),
        (CamtVersion::V08, "camt.053.001.08"),
    ] {
        let xml = export(&v, a, Some(&day(-30)), Some(&day(0)), None, version);
        assert!(!xml.contains('\u{1}'), "{xml}");
        validate(&xml, schema);
    }
    // Account without IBAN → <Othr><Id>
    let no_iban = bank_account(&v, "no-iban", "EUR", 0, &day(0));
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET iban = '', owner = '' WHERE id = ?1",
            [no_iban],
        )
        .unwrap();
    add(&v, no_iban, "n1", &day(-1), -100);
    let xml = export(
        &v,
        no_iban,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert!(xml.contains("<Othr><Id>4711</Id></Othr>"), "{xml}");
    // No owner: the account name stays in Nm, Ownr is omitted.
    assert!(xml.contains("<Nm>Giro</Nm>"), "{xml}");
    assert!(!xml.contains("<Ownr>"), "{xml}");
    validate(&xml, "camt.053.001.08");
}

/// #483 (1, 2, 6): pending entries and bookings after today never move the
/// balances, and a time part in `booking_date` still lands on its day.
#[test]
fn camt053_balances_ignore_pending_and_future_bookings() {
    let (v, _) = vault("camt-balances");
    // Bank balance 1.000,00 from today.
    let bank = bank_account(&v, "pending", "EUR", 100_000, &day(0));
    add(&v, bank, "b1", &day(-20), 50_000);
    add(&v, bank, "b2", &day(-5), -10_000);
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET booking_date = ?1 WHERE account_id = ?2 AND external_id = 'b2'",
            params![format!("{}T10:00:00", day(-5)), bank],
        )
        .unwrap();
    add(&v, bank, "pending-1", &day(-2), -5_000);
    let xml = export(
        &v,
        bank,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD").0, 60_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 100_000, "{xml}");

    // Computed balance: sum of all bookings, the future one included.
    let computed = computed_account(&v, "future", "checking", None, "");
    add(&v, computed, "c1", &day(-40), 100_000);
    add(&v, computed, "c2", &day(-10), -10_000);
    add(&v, computed, "c3", &day(3), -20_000);
    let xml = export(
        &v,
        computed,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD").0, 100_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 90_000, "{xml}");
    // The future booking is not part of a statement that ends today.
    assert_eq!(xml.matches("<Ntry>").count(), 1, "{xml}");
}

/// #483 (3): a bank balance is valid at its `balance_date`, not today.
#[test]
fn camt053_honours_the_balance_date() {
    let (v, _) = vault("camt-balance-date");
    let a = bank_account(&v, "old", "EUR", 100_000, &day(-10));
    add(&v, a, "b1", &day(-20), 20_000);
    add(&v, a, "b2", &day(-3), -10_000);
    let xml = export(
        &v,
        a,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD").0, 80_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 90_000, "{xml}");
}

/// #483 (4): the statement uses the account's currency, entries in another
/// one keep theirs and stay out of the balances, an empty code falls back.
#[test]
fn camt053_currency_comes_from_the_account() {
    let (v, _) = vault("camt-currency");
    let usd = bank_account(&v, "usd", "USD", 100_000, &day(0));
    add_in(&v, usd, "u1", &day(-5), -10_000, "USD");
    let xml = export(
        &v,
        usd,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD"), (110_000, "USD".into()), "{xml}");
    assert_eq!(balance(&xml, "CLBD"), (100_000, "USD".into()), "{xml}");
    assert!(xml.contains("<Ccy>USD</Ccy>"), "{xml}");

    let eur = bank_account(&v, "eur-mixed", "EUR", 100_000, &day(0));
    add_in(&v, eur, "m1", &day(-6), -1_000, "USD");
    add_in(&v, eur, "m2", &day(-5), -10_000, "EUR");
    add_in(&v, eur, "m3", &day(-4), 500, "");
    let xml = export(
        &v,
        eur,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD"), (109_500, "EUR".into()), "{xml}");
    assert_eq!(balance(&xml, "CLBD"), (100_000, "EUR".into()), "{xml}");
    assert!(xml.contains("<Amt Ccy=\"USD\">10.00</Amt>"), "{xml}");
    assert!(xml.contains("<Amt Ccy=\"EUR\">5.00</Amt>"), "{xml}");
    validate(&xml, "camt.053.001.08");
}

/// #483 (5): a loan's balance is no sum of its own bookings, a depot has
/// none either – both are refused in the core.
#[test]
fn camt053_refuses_loans_and_depots() {
    let (v, _) = vault("camt-refused");
    for kind in ["loan", "brokerage"] {
        let id = computed_account(&v, kind, kind, None, "");
        add(&v, id, "x", &day(-1), -30_000);
        let err = v
            .camt053_export(Camt053Request {
                account_id: id,
                date_from: None,
                date_to: None,
                booking_ids: None,
                version: CamtVersion::V08,
                created_at: "2026-03-31T12:00:00+02:00".into(),
            })
            .unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidInput { .. }),
            "{kind}: {err:?}"
        );
    }
}

/// #483 (#403): marked bookings are allowed – the balances stay the real
/// ones at the edges of the marked period, so a marking with a gap is refused.
#[test]
fn camt053_marked_bookings_keep_the_real_balances() {
    let (v, _) = vault("camt-selection");
    let a = bank_account(&v, "selection", "EUR", 100_000, &day(0));
    let first = add(&v, a, "b1", &day(-20), 50_000);
    let middle = add(&v, a, "b2", &day(-5), -10_000);
    let last = add(&v, a, "b3", &day(-1), -2_000);
    let same_day = add(&v, a, "b4", &day(-1), -3_000);
    // The newest bookings without a gap: real balances, OPBD + entries = CLBD.
    let xml = export(
        &v,
        a,
        None,
        None,
        Some(vec![middle, last, same_day]),
        CamtVersion::V08,
    );
    assert_eq!(xml.matches("<Ntry>").count(), 3, "{xml}");
    assert_eq!(balance(&xml, "OPBD").0, 115_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 100_000, "{xml}");
    // A marking that leaves out a booked booking between its first and last
    // day (also on the same day) would contradict itself: refused.
    for gap in [vec![first, last, same_day], vec![last]] {
        let err = v
            .camt053_export(Camt053Request {
                account_id: a,
                date_from: None,
                date_to: None,
                booking_ids: Some(gap),
                version: CamtVersion::V08,
                created_at: "2026-03-31T12:00:00+02:00".into(),
            })
            .unwrap_err();
        assert!(err.to_string().contains("lückenlose"), "{err}");
    }
}

/// A direct debit the account holder collected (own creditor id) and its
/// return: the peer stays the debtor, the holder the creditor with its id.
#[test]
fn camt053_own_direct_debit_keeps_the_roles() {
    let (v, _) = vault("camt-collect");
    let a = bank_account(&v, "collect", "EUR", 100_000, &day(0));
    let collect = add(&v, a, "c1", &day(-10), 3_000);
    let returned = add(&v, a, "c2", &day(-8), -3_000);
    {
        let db = v.db().unwrap();
        for (id, gvc) in [(collect, "171"), (returned, "109")] {
            db.execute(
                "UPDATE transactions SET counterparty = 'Max Mitglied', mandate_id = 'M-1',
                        creditor_id = 'DE11ZZZ00000000001', gvc = ?2 WHERE id = ?1",
                params![id, gvc],
            )
            .unwrap();
        }
    }
    for version in [CamtVersion::V02, CamtVersion::V08] {
        let xml = export(&v, a, None, None, None, version);
        validate(&xml, &format!("camt.053.001.{}", version.number()));
        let debtor = party_xml("Dbtr", "Max Mitglied", "", version);
        let creditor = party_xml("Cdtr", "Erika Muster", "DE11ZZZ00000000001", version);
        let entries: Vec<&str> = xml.split("<Ntry>").skip(1).collect();
        assert_eq!(entries.len(), 2, "{xml}");
        for e in entries {
            assert!(
                e.contains(&format!("<RltdPties>{debtor}{creditor}</RltdPties>")),
                "{e}"
            );
        }
    }
}

/// #483 (7–18): bank data stays as delivered, the missing references and
/// parties are written, and two exports never share a message id.
#[test]
fn camt053_writes_bank_data_and_references() {
    let (v, _) = vault("camt-details");
    let a = rich(&v);
    let xml = export(
        &v,
        a,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    // Account: name, owner only when set.
    assert!(
        xml.contains(
            "<Acct><Id><IBAN>DE02120300000000202051</IBAN></Id><Ccy>EUR</Ccy><Nm>Giro</Nm>\
         <Ownr><Nm>Erika Muster</Nm></Ownr>"
        ),
        "{xml}"
    );
    // Pagination and statement period.
    assert!(
        xml.contains("<MsgPgntn><PgNb>1</PgNb><LastPgInd>true</LastPgInd></MsgPgntn>"),
        "{xml}"
    );
    assert!(
        xml.contains(&format!(
            "<FrToDt><FrDtTm>{}T00:00:00</FrDtTm><ToDtTm>{}T23:59:59</ToDtTm></FrToDt>",
            day(-30),
            day(0)
        )),
        "{xml}"
    );
    // Bank purpose (no cleaned text, control characters dropped).
    assert!(xml.contains("RechnungNr. 7 &amp; &lt;Sonder&gt;"), "{xml}");
    assert!(!xml.contains('\u{1}'));
    // A SEPA envelope around the purpose is unwrapped.
    assert!(xml.contains("<Ustrd>Stripped purpose</Ustrd>"), "{xml}");
    // Direct debit: bank reference, references, creditor id, owner as debtor.
    let debit = xml.split("<Ntry>").find(|e| e.contains("E2E-7")).unwrap();
    assert!(
        debit.contains("<AcctSvcrRef>BANKREF-77</AcctSvcrRef>"),
        "{debit}"
    );
    assert!(
        debit.contains(
            "<Refs><AcctSvcrRef>BANKREF-77</AcctSvcrRef><EndToEndId>E2E-7</EndToEndId>\
             <MndtId>M-7</MndtId></Refs>"
        ),
        "{debit}"
    );
    assert!(debit.contains("<Cdtr><Pty><Nm>",), "{debit}");
    assert!(debit.contains(
        "<Id><PrvtId><Othr><Id>DE98ZZZ09999999999</Id><SchmeNm><Prtry>SEPA</Prtry></SchmeNm></Othr></PrvtId></Id>"
    ), "{debit}");
    assert!(
        debit.contains("<Dbtr><Pty><Nm>Erika Muster</Nm></Pty></Dbtr>"),
        "{debit}"
    );
    // Booking text as AddtlNtryInf; no GVC → ISO domain/family only.
    assert!(
        debit.contains("<AddtlNtryInf>SEPA-Lastschrift</AddtlNtryInf>"),
        "{debit}"
    );
    assert!(debit.contains(
        "<BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>IRCT</Cd><SubFmlyCd>OTHR</SubFmlyCd></Fmly></Domn></BkTxCd>"
    ), "{debit}");
    // A return carries its reason and keeps the direct debit's roles:
    // the peer stays the creditor even though the amount is a credit.
    let ret = xml.split("<Ntry>").find(|e| e.contains("NARR")).unwrap();
    assert!(
        ret.contains("<RtrInf><Rsn><Cd>NARR</Cd></Rsn></RtrInf>"),
        "{ret}"
    );
    assert!(
        ret.contains("<Dbtr><Pty><Nm>Erika Muster</Nm></Pty></Dbtr>"),
        "{ret}"
    );
    assert!(
        ret.contains("<Cdtr><Pty><Nm>Gegenpartei</Nm></Pty></Cdtr>"),
        "{ret}"
    );
    // A credit without mandate: the peer is the debtor, the owner the creditor.
    let credit = xml.split("<Ntry>").find(|e| e.contains("<Ustrd>")).unwrap();
    assert!(
        credit.contains("<Dbtr><Pty><Nm>Gegenpartei</Nm></Pty></Dbtr>"),
        "{credit}"
    );
    assert!(
        credit.contains("<Cdtr><Pty><Nm>Erika Muster</Nm></Pty></Cdtr>"),
        "{credit}"
    );
    // GVC 153: ISO domain/family plus the German DK form.
    assert!(xml.contains(
        "<BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>IRCT</Cd><SubFmlyCd>ESCT</SubFmlyCd></Fmly></Domn>\
         <Prtry><Cd>NTRF+153+000</Cd><Issr>DK</Issr></Prtry></BkTxCd>"
    ), "{xml}");
    // Value date only where the bank delivered one.
    assert_eq!(xml.matches("<ValDt>").count(), 1, "{xml}");
    // Pending entry listed as such.
    assert!(xml.contains("<Sts><Cd>PDNG</Cd></Sts>"), "{xml}");
    // A second export gets a new message id.
    let again = export(
        &v,
        a,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    let id = |x: &str| {
        x.split("<MsgId>")
            .nth(1)
            .unwrap()
            .split('<')
            .next()
            .unwrap()
            .to_string()
    };
    assert_ne!(id(&xml), id(&again));
    assert_eq!(
        id(&xml),
        xml.split("<Id>").nth(1).unwrap().split('<').next().unwrap()
    );
}

/// A GVC that is not a 3-digit DK code: the ISO domain/family classifies
/// the entry, and no proprietary code without an issuer is invented.
#[test]
fn camt053_non_numeric_gvc_uses_the_iso_code_only() {
    let (v, _) = vault("camt-gvc-text");
    let a = bank_account(&v, "gvc-text", "EUR", 0, &day(0));
    let b = add(&v, a, "b1", &day(-1), -100);
    v.db()
        .unwrap()
        .execute("UPDATE transactions SET gvc = 'CARD' WHERE id = ?1", [b])
        .unwrap();
    let xml = export(
        &v,
        a,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert!(!xml.contains("<Prtry>"), "{xml}");
    assert!(
        xml.contains(
            "<BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>IRCT</Cd><SubFmlyCd>OTHR</SubFmlyCd></Fmly></Domn></BkTxCd>"
        ),
        "{xml}"
    );
    validate(&xml, "camt.053.001.08");
}

/// A dated Startsaldo is the balance at the start of its day and covers
/// everything before it: periods before that day must take later bookings
/// off it again, so CLBD = OPBD + booked entries holds on both sides.
#[test]
fn camt053_computed_balance_around_the_startsaldo() {
    let (v, _) = vault("camt-startsaldo");
    let a = computed_account(&v, "opening", "checking", Some(50_000), &day(-10));
    add(&v, a, "b1", &day(-20), 20_000);
    add(&v, a, "b2", &day(-5), -5_000);
    // Before the Startsaldo: +200,00 lies in the period, the balance
    // before it is the Startsaldo minus that entry.
    let xml = export(
        &v,
        a,
        Some(&day(-30)),
        Some(&day(-12)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD").0, 30_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 50_000, "{xml}");
    // From the Startsaldo on: it is the opening balance, bookings count.
    let xml = export(
        &v,
        a,
        Some(&day(-10)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    assert_eq!(balance(&xml, "OPBD").0, 50_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 45_000, "{xml}");
}

/// A card stored with balance 0 gets the open cycle's amount from
/// `list_accounts`; that amount holds now, not at the stored date.
#[test]
fn camt053_anchors_a_zero_card_balance_at_today() {
    let (v, _) = vault("camt-card");
    let card = bank_account(&v, "card", "EUR", 0, &day(-20));
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET kind = 'credit_card' WHERE id = ?1",
            [card],
        )
        .unwrap();
    add(&v, card, "c1", &day(-10), -4_000);
    add(&v, card, "pending-1", &day(-5), -2_000);
    let xml = export(
        &v,
        card,
        Some(&day(-30)),
        Some(&day(0)),
        None,
        CamtVersion::V08,
    );
    // The hold is listed, but the anchor is the booked open cycle only.
    assert_eq!(xml.matches("<Sts><Cd>PDNG</Cd></Sts>").count(), 1, "{xml}");
    assert_eq!(balance(&xml, "OPBD").0, 0, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, -4_000, "{xml}");
}

/// An empty marking is a valid, empty statement – not a broken SQL list.
#[test]
fn camt053_empty_marking_is_an_empty_statement() {
    let (v, _) = vault("camt-empty-ids");
    let a = bank_account(&v, "empty-ids", "EUR", 100_000, &day(0));
    add(&v, a, "b1", &day(-1), -1_000);
    let xml = export(&v, a, None, None, Some(vec![]), CamtVersion::V08);
    assert_eq!(xml.matches("<Ntry>").count(), 0, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 100_000, "{xml}");
}

/// An end date alone bounds an empty statement instead of failing.
#[test]
fn camt053_accepts_an_end_date_without_bookings() {
    let (v, _) = vault("camt-end-only");
    let a = bank_account(&v, "end-only", "EUR", 100_000, &day(0));
    add(&v, a, "later", &day(-20), -10_000);
    let xml = export(&v, a, None, Some(&day(-40)), None, CamtVersion::V08);
    assert_eq!(xml.matches("<Ntry>").count(), 0, "{xml}");
    // The -100,00 lies after the end date and is not in the balance yet.
    assert_eq!(balance(&xml, "OPBD").0, 110_000, "{xml}");
    assert_eq!(balance(&xml, "CLBD").0, 110_000, "{xml}");
}

/// The FinTS CAMT parser reads the export back: booked entries with their
/// amounts and GVC, pending ones skipped, CLBD as written.
#[test]
fn camt053_round_trips_through_the_camt_parser() {
    let (v, _) = vault("camt-roundtrip");
    let a = rich(&v);
    for version in [CamtVersion::V02, CamtVersion::V08] {
        let xml = export(&v, a, Some(&day(-30)), Some(&day(0)), None, version);
        let (txs, report) = crate::connectors::fints::camt::parse(&xml, "");
        assert_eq!(txs.len(), 5, "{version:?}: {xml}");
        assert_eq!(report.closing_balance_cents, Some(balance(&xml, "CLBD").0));
        assert!(txs.iter().any(|t| t.gvc == "153"), "{version:?}");
        assert!(txs.iter().any(|t| t.purpose_code == "SALA"), "{version:?}");
    }
}

#[test]
fn esc_drops_xml_forbidden_characters() {
    assert_eq!(esc("a\u{0}b\u{1f}c\u{fffe}"), "abc");
    assert_eq!(esc("a\tb\nc\r\nd"), "a\tb\nc\r\nd");
    assert_eq!(esc("<&>\"'"), "&lt;&amp;&gt;&quot;&#39;");
    assert!(has_iban_shape("DE02120300000000202051"));
    assert!(!has_iban_shape("1234567890"));
    assert!(!has_iban_shape("de02120300000000202051"));
}
