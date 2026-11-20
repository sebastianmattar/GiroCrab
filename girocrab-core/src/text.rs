//! Text decoding for bank data, plus small parsers for text cells of
//! imported files.
//!
//! FinTS 3.0 envelopes are ISO-8859-1 (in practice Windows-1252), embedded
//! camt.052 XML is UTF-8, MT940 varies by bank. Decoding the wrong way round
//! produces "M�ller" (Latin-1 read as UTF-8) or "MÃ¼ller" (UTF-8 read as
//! Latin-1). `decode_bank_bytes` picks the right decoding, `fix_mojibake`
//! repairs already broken strings (also used to clean the database).

use rusqlite::Connection;

/// Windows-1252 code page for 0x80–0x9F (None = undefined, kept as C1 control).
const CP1252_HIGH: [Option<char>; 32] = [
    Some('€'),
    None,
    Some('‚'),
    Some('ƒ'),
    Some('„'),
    Some('…'),
    Some('†'),
    Some('‡'),
    Some('ˆ'),
    Some('‰'),
    Some('Š'),
    Some('‹'),
    Some('Œ'),
    None,
    Some('Ž'),
    None,
    None,
    Some('‘'),
    Some('’'),
    Some('“'),
    Some('”'),
    Some('•'),
    Some('–'),
    Some('—'),
    Some('˜'),
    Some('™'),
    Some('š'),
    Some('›'),
    Some('œ'),
    None,
    Some('ž'),
    Some('Ÿ'),
];

fn cp1252_char(b: u8) -> char {
    if (0x80..=0x9F).contains(&b) {
        CP1252_HIGH[(b - 0x80) as usize].unwrap_or(b as char)
    } else {
        b as char
    }
}

/// Inverse of `cp1252_char` (None if the char has no single-byte form).
fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u < 0x80 || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    if (0x80..=0x9F).contains(&u) {
        return Some(u as u8);
    }
    CP1252_HIGH
        .iter()
        .position(|x| *x == Some(c))
        .map(|i| 0x80 + i as u8)
}

/// Typical UTF-8-read-as-Latin-1 sequences.
const MOJIBAKE: &[(&str, &str)] = &[
    ("Ã¤", "ä"),
    ("Ã¶", "ö"),
    ("Ã¼", "ü"),
    ("ÃŸ", "ß"),
    ("Ã„", "Ä"),
    ("Ã–", "Ö"),
    ("Ãœ", "Ü"),
    ("Ã©", "é"),
    ("Ã¨", "è"),
    ("Ã¡", "á"),
    ("Ã ", "à"),
    ("Ã§", "ç"),
    ("Ã±", "ñ"),
    ("Ã³", "ó"),
    ("Ãº", "ú"),
    ("â‚¬", "€"),
    ("â€“", "–"),
    ("â€”", "—"),
    ("â€ž", "„"),
    ("â€œ", "“"),
    ("â€™", "’"),
    ("â€¦", "…"),
    ("Â§", "§"),
    ("Â°", "°"),
    ("Â ", " "),
];

pub fn has_mojibake(s: &str) -> bool {
    // "Ãƒ"/"Ã‚" = doubly encoded UTF-8
    s.contains("Ãƒ") || s.contains("Ã‚") || MOJIBAKE.iter().any(|(bad, _)| s.contains(bad))
}

/// No replacement character and no UTF-8/Latin-1 mojibake.
pub(crate) fn is_clean(s: &str) -> bool {
    !s.contains('\u{FFFD}') && !has_mojibake(s)
}

/// Repairs "MÃ¼ller" -> "Müller", "StraÃŸe" -> "Straße" (also doubly broken text).
pub fn fix_mojibake(s: &str) -> String {
    if !has_mojibake(s) {
        return s.to_string();
    }
    let mut cur = s.to_string();
    for _ in 0..2 {
        // whole-string round trip: chars -> cp1252 bytes -> UTF-8
        let bytes: Option<Vec<u8>> = cur.chars().map(cp1252_byte).collect();
        match bytes.and_then(|b| String::from_utf8(b).ok()) {
            Some(fixed) if fixed != cur => cur = fixed,
            _ => break,
        }
        if !has_mojibake(&cur) {
            return cur;
        }
    }
    // mixed strings (some chars not cp1252-encodable): targeted replacements
    for (bad, good) in MOJIBAKE {
        cur = cur.replace(bad, good);
    }
    cur
}

/// Decodes bank bytes: valid UTF-8 without mojibake stays UTF-8; anything
/// else is ISO-8859-1 / Windows-1252 (never lossy, never "�").
pub fn decode_bank_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) if !has_mojibake(s) => s.to_string(),
        Ok(s) => fix_mojibake(s),
        Err(_) => fix_mojibake(&bytes.iter().map(|&b| cp1252_char(b)).collect::<String>()),
    }
}

/// XML payloads (camt): honour the declared encoding, default UTF-8.
pub fn decode_xml_bytes(bytes: &[u8]) -> String {
    let head = decode_bank_bytes(&bytes[..bytes.len().min(120)]).to_lowercase();
    if head.contains("encoding=\"iso-8859")
        || head.contains("encoding='iso-8859")
        || head.contains("windows-1252")
    {
        fix_mojibake(&bytes.iter().map(|&b| cp1252_char(b)).collect::<String>())
    } else {
        let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes); // BOM
        decode_bank_bytes(b)
    }
}

/// Known German words/names whose umlaut was replaced by a literal `?`.
const QMARK_FIXES: &[(&str, &str)] = &[
    ("M?ller", "Müller"),
    ("Sch?fer", "Schäfer"),
    ("Schr?der", "Schröder"),
    ("K?hler", "Köhler"),
    ("K?pper", "Köpper"),
    ("K?nig", "König"),
    ("B?cker", "Bäcker"),
    ("G?ttingen", "Göttingen"),
    ("Geb?hr", "Gebühr"),
    ("?berweisung", "Überweisung"),
    ("Ausf?hrung", "Ausführung"),
    ("L?schung", "Löschung"),
    ("R?ck", "Rück"),
    ("r?ck", "rück"),
    ("Pr?f", "Prüf"),
    ("pr?f", "prüf"),
    ("Gr??e", "Grüße"),
    ("gr??e", "grüße"),
    ("F?r", "Für"),
    ("f?r", "für"),
];

/// Repairs `?` standing in for an umlaut inside known German words
/// ("Anna M?ller" -> "Anna Müller"). Works token-wise so URLs and
/// query strings (`page?id=1`) stay untouched.
pub fn fix_question_marks(s: &str) -> String {
    if !s.contains('?') || s.contains("://") || s.contains("www.") {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let is_tok = |c: char| c.is_alphabetic() || c == '?';
        if !is_tok(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_tok(chars[i]) {
            i += 1;
        }
        let tok: String = chars[start..i].iter().collect();
        let url_ctx = start > 0 && matches!(chars[start - 1], '/' | '=' | '&' | '.');
        if tok.contains('?') && !url_ctx {
            let mut t = tok;
            for (bad, good) in QMARK_FIXES {
                if t.contains(bad) {
                    t = t.replace(bad, good);
                }
            }
            out.push_str(&t);
        } else {
            out.push_str(&tok);
        }
    }
    out
}

/// German syllables/words whose umlaut/ß was stored as U+FFFD by an earlier lossy decode.
/// Longer patterns first; case variants (as-is, lower, Capitalised, UPPER) are tried for each.
const FFFD_FIXES: &[(&str, &str)] = &[
    ("Gr\u{FFFD}\u{FFFD}e", "Grüße"),
    ("stra\u{FFFD}e", "straße"),
    ("Stra\u{FFFD}e", "Straße"),
    ("\u{FFFD}berweisung", "Überweisung"),
    ("\u{FFFD}bertrag", "Übertrag"),
    ("Ausf\u{FFFD}hrung", "Ausführung"),
    ("Geb\u{FFFD}hr", "Gebühr"),
    ("G\u{FFFD}ttingen", "Göttingen"),
    ("Schr\u{FFFD}der", "Schröder"),
    ("Sch\u{FFFD}fer", "Schäfer"),
    ("M\u{FFFD}ller", "Müller"),
    ("K\u{FFFD}hler", "Köhler"),
    ("K\u{FFFD}pper", "Köpper"),
    ("K\u{FFFD}nig", "König"),
    ("B\u{FFFD}cker", "Bäcker"),
    ("J\u{FFFD}ger", "Jäger"),
    ("Kr\u{FFFD}ger", "Krüger"),
    ("H\u{FFFD}hold", "Höhold"),
    ("R\u{FFFD}ck", "Rück"),
    ("L\u{FFFD}sch", "Lösch"),
    ("pr\u{FFFD}f", "prüf"),
    ("h\u{FFFD}fe", "höfe"),
    ("f\u{FFFD}r", "für"),
];

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Repairs U+FFFD inside known German words, preserving case.
pub fn fix_replacement_chars(s: &str) -> String {
    if !s.contains('\u{FFFD}') {
        return s.to_string();
    }
    let mut cur = s.to_string();
    for (bad, good) in FFFD_FIXES {
        let variants = [
            (bad.to_string(), good.to_string()),
            (bad.to_lowercase(), good.to_lowercase()),
            (
                capitalize_first(&bad.to_lowercase()),
                capitalize_first(&good.to_lowercase()),
            ),
            (bad.to_uppercase(), good.to_uppercase()),
        ];
        for (b, g) in variants {
            if cur.contains(&b) {
                cur = cur.replace(&b, &g);
            }
        }
    }
    cur
}

/// Full cleanup for bank-provided text: mojibake, U+FFFD and `?`-for-umlaut.
pub fn clean_bank_text(s: &str) -> String {
    fix_question_marks(&fix_replacement_chars(&fix_mojibake(s)))
}

/// Startup cleanup: repairs mojibake already stored in the vault.
/// Returns the number of changed rows.
pub(crate) fn repair_stored_text(conn: &Connection) -> rusqlite::Result<u32> {
    let mut changed = 0u32;
    let targets: &[(&str, &[&str])] = &[
        ("transactions", &["counterparty", "purpose", "booking_text"]),
        ("accounts", &["name", "owner", "bank_name"]),
        ("bank_connections", &["display_name"]),
        ("booking_documents", &["title", "file_name"]),
    ];
    for (table, cols) in targets {
        let filter = cols
            .iter()
            .map(|c| {
                format!("{c} LIKE '%Ã%' OR {c} LIKE '%â€%' OR {c} LIKE '%Â%' OR {c} LIKE '%?%' OR {c} LIKE '%\u{FFFD}%'")
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!("SELECT id, {} FROM {table} WHERE {filter}", cols.join(", "));
        let rows: Vec<(i64, Vec<String>)> = {
            let mut st = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(_) => continue, // table may not exist yet
            };

            st.query_map([], |r| {
                let id: i64 = r.get(0)?;
                let vals = (0..cols.len())
                    .map(|i| r.get::<_, String>(i + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((id, vals))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (id, vals) in rows {
            let fixed: Vec<String> = vals.iter().map(|v| clean_bank_text(v)).collect();
            if fixed != vals {
                let set = cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{c} = ?{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut args: Vec<&dyn rusqlite::ToSql> =
                    fixed.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                args.push(&id);
                conn.execute(
                    &format!(
                        "UPDATE OR IGNORE {table} SET {set} WHERE id = ?{}",
                        cols.len() + 1
                    ),
                    args.as_slice(),
                )?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Decodes HTML/XML character references: the five XML entities, `&nbsp;`,
/// `&euro;`, `&minus;`, the German umlauts and `&szlig;`, and numeric ones (`&#65;`, `&#x42;`). Anything else
/// (`&bogus;`, a bare `&`) stays as it is. Used by the web/XML connectors.
pub(crate) fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let ch = after.find(';').filter(|e| *e <= 8).and_then(|end| {
            let ent = &after[..end];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "euro" => Some('€'),
                "minus" => Some('-'),
                "auml" => Some('ä'),
                "ouml" => Some('ö'),
                "uuml" => Some('ü'),
                "Auml" => Some('Ä'),
                "Ouml" => Some('Ö'),
                "Uuml" => Some('Ü'),
                "szlig" => Some('ß'),
                _ => ent
                    .strip_prefix("#x")
                    .or_else(|| ent.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match ch {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether a cell of an imported file means "yes": `1`, `true`, `yes`/`y`,
/// `ja`/`j`, `x` or a check mark, in any case. Anything else is "no".
pub(crate) fn parse_bool_loose(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "ja" | "j" | "x" | "✓" | "✔"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_bools() {
        for yes in ["1", "true", "Yes", "y", " JA ", "j", "X", "✓", "✔"] {
            assert!(parse_bool_loose(yes), "{yes:?}");
        }
        for no in ["", "0", "false", "nein", "n", "-", "ok"] {
            assert!(!parse_bool_loose(no), "{no:?}");
        }
    }

    #[test]
    fn decodes_html_entities() {
        assert_eq!(
            decode_html_entities("A &amp; B &lt;&#65;&#x42;&#X43;&gt; &bogus; & x"),
            "A & B <ABC> &bogus; & x"
        );
        assert_eq!(decode_html_entities("1.012,34&nbsp;&euro;"), "1.012,34 €");
        assert_eq!(decode_html_entities("&minus;5,00"), "-5,00");
        assert_eq!(decode_html_entities("plain"), "plain");
    }

    #[test]
    fn decodes_latin1_utf8_and_mojibake() {
        assert_eq!(
            decode_bank_bytes(b"M\xfcller Stra\xdfe \xe4\xf6\xdc"),
            "Müller Straße äöÜ"
        );
        assert_eq!(
            decode_bank_bytes("Müller Straße".as_bytes()),
            "Müller Straße"
        );
        assert_eq!(
            decode_bank_bytes(b"\x80 5,00"),
            "€ 5,00",
            "Windows-1252 euro sign"
        );
        assert_eq!(fix_mojibake("MÃ¼ller"), "Müller");
        assert_eq!(fix_mojibake("StraÃŸe"), "Straße");
        assert_eq!(
            fix_mojibake("Ã„rztehaus Ã–sterreich Ãœbung"),
            "Ärztehaus Österreich Übung"
        );
        assert_eq!(fix_mojibake("MÃƒÂ¼ller"), "Müller", "double encoded");
        assert_eq!(
            fix_mojibake("Schöne Grüße"),
            "Schöne Grüße",
            "correct text untouched"
        );
        assert_eq!(
            decode_xml_bytes(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Nm>Bäckerei</Nm>".as_bytes()
            ),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Nm>Bäckerei</Nm>"
        );
    }

    #[test]
    fn repairs_question_marks_in_german_words() {
        assert_eq!(clean_bank_text("Anna M?ller"), "Anna Müller");
        assert_eq!(
            clean_bank_text("M?ller f?r Gr??e, ?berweisung R?ck"),
            "Müller für Grüße, Überweisung Rück"
        );
        assert_eq!(clean_bank_text("Wieso? Ja?"), "Wieso? Ja?");
        assert_eq!(clean_bank_text("https://x.de/a?f=1"), "https://x.de/a?f=1");
        assert_eq!(clean_bank_text("x.de/seite?r=1"), "x.de/seite?r=1");
    }

    #[test]
    fn repairs_replacement_chars() {
        assert_eq!(clean_bank_text("Steven H\u{FFFD}hold"), "Steven Höhold");
        assert_eq!(
            clean_bank_text("M\u{FFFD}ller f\u{FFFD}r Gr\u{FFFD}\u{FFFD}e, Hauptstra\u{FFFD}e"),
            "Müller für Grüße, Hauptstraße"
        );
        assert_eq!(clean_bank_text("\u{FFFD}BERWEISUNG"), "ÜBERWEISUNG");
    }

    #[test]
    fn fints_latin1_envelope_with_utf8_camt_inside() {
        use crate::connectors::fints::{camt, syntax};
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Document><BkToCstmrAcctRpt><Rpt><Ntry><Amt Ccy=\"EUR\">5.00</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts><Cd>BOOK</Cd></Sts><BookgDt><Dt>2026-11-02</Dt></BookgDt><NtryDtls><TxDtls><RltdPties><Cdtr><Nm>Bäckerei Müller</Nm></Cdtr></RltdPties><RmtInf><Ustrd>Brötchen für Grüße</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry></Rpt></BkToCstmrAcctRpt></Document>";
        let mut msg: Vec<u8> =
            b"HIRMS:3:2+0020::Auftrag ausgef\xfchrt'HICAZ:4:1:3+DE1+urn+@".to_vec();
        msg.extend(format!("{}@", xml.len()).into_bytes());
        msg.extend(xml.as_bytes());
        msg.extend(b"'");
        let segs = syntax::parse(&msg).unwrap();
        assert_eq!(
            segs[0].get(1, 2),
            "Auftrag ausgeführt",
            "Latin-1 envelope text"
        );
        let syntax::Value::Binary(raw) = &segs[1].element(3)[0] else {
            panic!("binary")
        };
        let (txs, _) = camt::parse(&decode_xml_bytes(raw), "acc");
        assert_eq!(txs[0].counterparty, "Bäckerei Müller");
        assert_eq!(txs[0].purpose, "Brötchen für Grüße");
    }

    #[test]
    fn repairs_stored_mojibake_on_open() {
        let p_dir = crate::test_support::TempDir::new("girocrab-moji");
        let p = p_dir.join("v.db");
        let path = p.to_string_lossy().to_string();
        {
            let v = crate::vault::Vault::open(path.clone(), vec![2u8; 32]).unwrap();
            v.seed_demo_data().unwrap();
            let db = v.db().unwrap();
            db.execute("UPDATE transactions SET counterparty = 'BÃ¤ckerei MÃ¼ller', purpose = 'StraÃŸe' WHERE id = (SELECT MIN(id) FROM transactions)", []).unwrap();
        }
        let v = crate::vault::Vault::open(path, vec![2u8; 32]).unwrap();
        let (cp, pu): (String, String) = v.db().unwrap().query_row("SELECT counterparty, purpose FROM transactions WHERE id = (SELECT MIN(id) FROM transactions)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((cp.as_str(), pu.as_str()), ("Bäckerei Müller", "Straße"));
        let _ = std::fs::remove_file(p);
    }
}
