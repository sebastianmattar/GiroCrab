//! Credit card transactions (HKKKU / HIKKU version 1, the DK successor of the
//! proprietary DKKKU). Banks serve card accounts through it when the card has
//! no statement access via HKKAZ/HKCAZ (Deutsche Bank: Miles & More cards).
//!
//! HIKKU: card number, account id, balance (C|D : amount : currency : date),
//! last statement date, next statement date, then one data element group per
//! booking. Components of a booking (0-based):
//!
//! | 0 | card number | 9 | booked amount (account currency) |
//! | 1 | receipt date | 10 | booked currency |
//! | 2 | booking date | 11 | booked C/D mark |
//! | 3 | statement date | 12–19 | description lines |
//! | 4 | value date | 20 | country |
//! | 5 | original amount | 21 | merchant name |
//! | 6 | original currency | 22 | terminal |
//! | 7 | original C/D mark | 23 | billed (J/N) |
//! | 8 | exchange rate | 24 | booking reference |
//!
//! Original amount, currency and rate are empty for payments in the account
//! currency.
//!
//! DIKKU version 2 (answer to the proprietary DKKKU, comdirect, #807) has the same
//! head but 22 components per booking (`DIKKU_V2`). `shape` logs the kind of every
//! field, never the content, so a layout deviation shows up in the debug log.

use super::syntax::{Segment, Value};
use crate::connectors::RemoteTransaction;
use crate::money::parse_cents;

/// Booked balance of the card account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardBalance {
    pub cents: i64,
    pub currency: String,
    /// `YYYY-MM-DD`, "" if the bank sent none.
    pub date: String,
}

fn iso_date(d: &str) -> Option<String> {
    (d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]))
}

/// Splits a group at unescaped ':' and removes the '?' escapes.
fn split_group(raw: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '?' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// Components of one booking: a regular data element group, or the same group
/// wrapped in a binary element.
fn components(el: &[Value]) -> Vec<String> {
    match el {
        [Value::Binary(b)] => split_group(&crate::text::decode_bank_bytes(b)),
        _ => el.iter().map(|v| v.text().to_string()).collect(),
    }
}

fn signed(amount: &str, mark: &str) -> Option<i64> {
    let cents = parse_cents(amount).ok()?;
    Some(if mark == "D" { -cents } else { cents })
}

/// Where the fields of one booking group are. HIKKU v1 has 25 components (see the
/// module docs). DIKKU v2 has 22, measured at comdirect (#807): no statement date,
/// so amounts sit one position earlier, nine description lines, then billed flag and
/// booking reference; no country, merchant or terminal.
struct Layout {
    receipt_date: usize,
    booking_date: usize,
    value_date: usize,
    /// original amount, currency, C/D mark, then exchange rate
    original: usize,
    /// booked amount, currency, C/D mark
    booked: usize,
    lines: std::ops::RangeInclusive<usize>,
    merchant: Option<usize>,
}

const HIKKU_V1: Layout = Layout {
    receipt_date: 1,
    booking_date: 2,
    value_date: 4,
    original: 5,
    booked: 9,
    lines: 12..=19,
    merchant: Some(21),
};

const DIKKU_V2: Layout = Layout {
    receipt_date: 1,
    booking_date: 2,
    value_date: 3,
    original: 4,
    booked: 8,
    lines: 11..=19,
    merchant: None,
};

fn layout(s: &Segment) -> &'static Layout {
    if s.id() == "DIKKU" {
        &DIKKU_V2
    } else {
        &HIKKU_V1
    }
}

fn booking(
    l: &Layout,
    c: &[String],
    account_ref: &str,
    fallback_currency: &str,
) -> Option<RemoteTransaction> {
    let get = |i: usize| c.get(i).map(|s| s.trim()).unwrap_or("");
    let (o, b) = (l.original, l.booked);
    // Booked amount in the account currency; some banks only fill the original.
    let (amount, currency) = match signed(get(b), get(b + 2)) {
        Some(a) => (a, get(b + 1)),
        None => (signed(get(o), get(o + 2))?, get(o + 1)),
    };
    let booking_date = iso_date(get(l.booking_date)).or_else(|| iso_date(get(l.receipt_date)))?;
    let value_date = iso_date(get(l.value_date)).unwrap_or_else(|| booking_date.clone());
    let lines: Vec<&str> = l.lines.clone().map(get).filter(|l| !l.is_empty()).collect();
    let merchant = l.merchant.map(get).unwrap_or("");
    let counterparty = if merchant.is_empty() {
        lines.first().copied().unwrap_or("")
    } else {
        merchant
    };
    let mut purpose: Vec<String> = lines
        .iter()
        .filter(|l| !l.eq_ignore_ascii_case(counterparty))
        .map(|l| l.to_string())
        .collect();
    // Foreign currency: keep what was actually charged abroad.
    let (orig_amount, orig_currency) = (get(o), get(o + 1));
    if !orig_amount.is_empty() && !orig_currency.is_empty() && orig_currency != currency {
        let rate = get(o + 3);
        purpose.push(if rate.is_empty() {
            format!("{orig_amount} {orig_currency}")
        } else {
            format!("{orig_amount} {orig_currency}, Kurs {rate}")
        });
    }
    Some(RemoteTransaction {
        account_ref: account_ref.to_string(),
        booking_date,
        value_date,
        amount_cents: amount,
        currency: if currency.is_empty() {
            fallback_currency.to_string()
        } else {
            currency.to_string()
        },
        counterparty: counterparty.to_string(),
        purpose: purpose.join(" "),
        booking_text: "Kartenumsatz".into(),
        ..Default::default()
    })
}

/// Bookings and balance of all HIKKU / DIKKU segments of one reply. The balance is
/// taken from the first segment that carries one.
pub fn parse<'a>(
    segments: impl Iterator<Item = &'a Segment>,
    account_ref: &str,
    fallback_currency: &str,
) -> (Vec<RemoteTransaction>, Option<CardBalance>) {
    let mut txs = Vec::new();
    let mut balance = None;
    for s in segments {
        if balance.is_none()
            && let Some(cents) = signed(s.get(3, 1), s.get(3, 0))
        {
            balance = Some(CardBalance {
                cents,
                currency: s.get(3, 2).to_string(),
                date: iso_date(s.get(3, 3)).unwrap_or_default(),
            });
        }
        let l = layout(s);
        for el in s.elements.iter().skip(6) {
            let c = components(el);
            if let Some(t) = booking(l, &c, account_ref, fallback_currency) {
                txs.push(t);
            }
        }
    }
    (txs, balance)
}

/// Kind of one field, never its content: `-` empty, `d` date (YYYYMMDD),
/// `n` amount, `#` other digits, `c` currency, `m` C/D mark, `j` J/N, `t` text.
fn kind(s: &str) -> char {
    let s = s.trim();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if s.is_empty() {
        '-'
    } else if iso_date(s).is_some() {
        'd'
    } else if digits(s) {
        '#'
    } else if s.contains(',') && parse_cents(s).is_ok() {
        'n'
    } else if s == "C" || s == "D" {
        'm'
    } else if s == "J" || s == "N" {
        'j'
    } else if s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()) {
        'c'
    } else {
        't'
    }
}

/// Layout of a card-bookings segment for the debug log, without any content:
/// the kinds of the head elements (`:` between components) and of the components
/// of the first booking group, e.g. `head=[#|#|m:n:c:d|d|d] group1(25)=[#d d-d…]`.
pub fn shape(s: &Segment) -> String {
    let head: Vec<String> = s
        .elements
        .iter()
        .skip(1)
        .take(5)
        .map(|el| el.iter().map(|v| kind(v.text())).collect())
        .map(|k: String| k.chars().map(String::from).collect::<Vec<_>>().join(":"))
        .collect();
    let groups = s.elements.len().saturating_sub(6);
    let first = s
        .elements
        .get(6)
        .map(|el| {
            let c = components(el);
            format!(
                " group1({})=[{}]",
                c.len(),
                c.iter().map(|v| kind(v)).collect::<String>()
            )
        })
        .unwrap_or_default();
    format!("head=[{}] groups={groups}{first}", head.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::fints::syntax::parse_response;

    fn parsed(raw: &str) -> (Vec<RemoteTransaction>, Option<CardBalance>) {
        let segs = parse_response(raw.as_bytes()).unwrap();
        parse(segs.iter().filter(|s| s.id() == "HIKKU"), "card", "EUR")
    }

    /// One booking group of 25 components from (position, value) pairs.
    fn group(fields: &[(usize, &str)]) -> String {
        let mut c = vec![String::new(); 25];
        for (i, v) in fields {
            c[*i] = v.replace(':', "?:");
        }
        c.join(":")
    }

    // Invented data.
    const CARD: &str = "5232000011112222";

    fn purchase() -> String {
        group(&[
            (0, CARD),
            (1, "20261001"),
            (2, "20261002"),
            (4, "20261002"),
            (9, "12,50"),
            (10, "EUR"),
            (11, "D"),
            (12, "KAFFEEBAR AM MARKT"),
            (13, "Filiale: Mitte"),
            (20, "DE"),
            (21, "KAFFEEBAR"),
            (23, "N"),
            (24, "REF0001"),
        ])
    }

    fn foreign() -> String {
        group(&[
            (0, CARD),
            (1, "20261003"),
            (2, "20261004"),
            (4, "20261004"),
            (5, "20,00"),
            (6, "USD"),
            (7, "D"),
            (8, "1,0852"),
            (9, "18,43"),
            (10, "EUR"),
            (11, "D"),
            (12, "ONLINE SHOP"),
            (20, "US"),
            (21, "ONLINE SHOP"),
            (23, "J"),
            (24, "REF0002"),
        ])
    }

    fn refund() -> String {
        group(&[
            (0, CARD),
            (2, "20261005"),
            (9, "1.234,00"),
            (10, "EUR"),
            (11, "C"),
            (12, "GUTSCHRIFT"),
            (23, "J"),
            (24, "REF0003"),
        ])
    }

    #[test]
    fn parses_balance_and_bookings() {
        let raw = format!(
            "HIKKU:4:1:3+{CARD}++D:655,56:EUR:20261004+20261003+20261103+{}+{}+{}'",
            purchase(),
            foreign(),
            refund()
        );
        let (txs, balance) = parsed(&raw);
        assert_eq!(
            balance,
            Some(CardBalance {
                cents: -65_556,
                currency: "EUR".into(),
                date: "2026-10-04".into()
            })
        );
        assert_eq!(txs.len(), 3);

        let t = &txs[0];
        assert_eq!((t.amount_cents, t.currency.as_str()), (-1_250, "EUR"));
        assert_eq!(
            (t.booking_date.as_str(), t.value_date.as_str()),
            ("2026-10-02", "2026-10-02")
        );
        assert_eq!(t.counterparty, "KAFFEEBAR");
        assert_eq!(t.purpose, "KAFFEEBAR AM MARKT Filiale: Mitte");
        assert_eq!(t.account_ref, "card");

        // foreign currency: booked amount counts, the original goes into the purpose
        let t = &txs[1];
        assert_eq!(t.amount_cents, -1_843);
        assert_eq!(t.counterparty, "ONLINE SHOP");
        assert_eq!(t.purpose, "20,00 USD, Kurs 1,0852");

        // refund: credit, no receipt/value date, no merchant name
        let t = &txs[2];
        assert_eq!(t.amount_cents, 123_400);
        assert_eq!(t.value_date, "2026-10-05");
        assert_eq!(t.counterparty, "GUTSCHRIFT");
        assert_eq!(t.purpose, "");
    }

    /// One DIKKU v2 booking group (22 components) from (position, value) pairs.
    fn group22(fields: &[(usize, &str)]) -> String {
        let mut c = vec![String::new(); 22];
        for (i, v) in fields {
            c[*i] = v.replace(':', "?:");
        }
        c.join(":")
    }

    /// Invented DIKKU v2 answer with exactly the shape comdirect sent (#807):
    /// head `#|-|m:n:c:d:#|-|-`, group `#dd-ncmnncmt--------j#`.
    fn dikku_v2() -> String {
        let purchase = group22(&[
            (0, CARD),
            (1, "20260905"),
            (2, "20260907"),
            (4, "23,90"),
            (5, "EUR"),
            (6, "D"),
            (7, "1,00000"),
            (8, "23,90"),
            (9, "EUR"),
            (10, "D"),
            (11, "BAECKEREI SONNE BERLIN"),
            (20, "J"),
            (21, "123456789"),
        ]);
        let foreign = group22(&[
            (0, CARD),
            (1, "20260910"),
            (2, "20260912"),
            (4, "50,00"),
            (5, "USD"),
            (6, "D"),
            (7, "1,08520"),
            (8, "46,07"),
            (9, "EUR"),
            (10, "D"),
            (11, "BOOKSHOP NEW YORK"),
            (20, "N"),
            (21, "123456790"),
        ]);
        format!("DIKKU:4:2:3+{CARD}++D:69,97:EUR:20261117:1200+++{purchase}+{foreign}'")
    }

    #[test]
    fn dikku_v2_fixture_has_the_measured_shape() {
        let raw = dikku_v2();
        let segs = parse_response(raw.as_bytes()).unwrap();
        assert_eq!(
            shape(&segs[0]),
            "head=[#|-|m:n:c:d:#|-|-] groups=2 group1(22)=[#dd-ncmnncmt--------j#]"
        );
    }

    #[test]
    fn dikku_v2_bookings_and_balance() {
        let raw = dikku_v2();
        let segs = parse_response(raw.as_bytes()).unwrap();
        let (txs, balance) = parse(segs.iter(), "card", "EUR");
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0].amount_cents, -2390);
        assert_eq!(txs[0].booking_date, "2026-09-07");
        assert_eq!(
            txs[0].value_date, "2026-09-07",
            "no value date: booking date"
        );
        assert_eq!(txs[0].counterparty, "BAECKEREI SONNE BERLIN");
        assert_eq!(txs[0].purpose, "", "same currency: no conversion note");
        assert_eq!(txs[1].amount_cents, -4607);
        assert_eq!(txs[1].currency, "EUR");
        assert_eq!(txs[1].purpose, "50,00 USD, Kurs 1,08520");
        let b = balance.unwrap();
        assert_eq!(
            (b.cents, b.currency.as_str(), b.date.as_str()),
            (-6997, "EUR", "2026-11-17")
        );
    }

    #[test]
    fn hikku_v1_layout_is_unchanged() {
        // the same groups under the v1 id are not misread as v2
        let raw = format!(
            "HIKKU:4:1:3+{CARD}++D:655,56:EUR:20261004+20261003+20261103+{}'",
            purchase()
        );
        let segs = parse_response(raw.as_bytes()).unwrap();
        let (txs, _) = parse(segs.iter(), "card", "EUR");
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].amount_cents, -1250);
        assert_eq!(txs[0].counterparty, "KAFFEEBAR");
    }

    #[test]
    fn shape_shows_field_kinds_but_no_content() {
        let raw = format!(
            "HIKKU:4:1:3+{CARD}++D:655,56:EUR:20261004+20261003+20261103+{}'",
            purchase()
        );
        let segs = parse_response(raw.as_bytes()).unwrap();
        let s = shape(&segs[0]);
        assert_eq!(
            s,
            "head=[#|-|m:n:c:d|d|d] groups=1 group1(25)=[#dd-d----ncmtt------tt-jt]"
        );
        for secret in [CARD, "655,56", "KAFFEEBAR", "20261004", "REF0001"] {
            assert!(!s.contains(secret), "{secret} leaked into {s}");
        }
    }

    #[test]
    fn no_bookings_and_no_balance() {
        let (txs, balance) = parsed("HIKKU:4:1:3+5232000011112222'");
        assert!(txs.is_empty());
        assert_eq!(balance, None);
    }

    #[test]
    fn several_segments_are_merged_balance_from_the_first() {
        let raw = format!(
            "HIKKU:4:1:3+{CARD}++D:10,00:EUR:20261004+++{}'HIKKU:5:1:3+{CARD}++D:99,00:EUR:20261004+++{}'",
            purchase(),
            refund()
        );
        let (txs, balance) = parsed(&raw);
        assert_eq!(txs.len(), 2);
        assert_eq!(balance.unwrap().cents, -1_000);
    }

    #[test]
    fn booking_wrapped_in_a_binary_element() {
        let p = purchase();
        let raw = format!("HIKKU:4:1:3+{CARD}+++++@{}@{p}'", p.len());
        let (txs, _) = parsed(&raw);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].purpose, "KAFFEEBAR AM MARKT Filiale: Mitte");
    }

    #[test]
    fn incomplete_groups_are_skipped() {
        let (txs, _) = parsed("HIKKU:4:1:3+5232000011112222+++++5232000011112222:20261001'");
        assert!(txs.is_empty());
    }
}
