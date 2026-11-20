//! Excel CSV export of rows the app prepared.

/// One CSV line, already in output order.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExportCsvRow {
    pub date: String,
    pub account_name: String,
    pub counterparty: String,
    /// Counterparty IBAN (column "IBAN").
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub counterparty_iban: String,
    pub purpose: String,
    pub category: String,
    pub note: String,
    /// Tag names, comma-separated (column "Tags", #228).
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub tags: String,
    pub amount_cents: i64,
    pub currency: String,
    /// End-of-day balance, set on the last row of a day; `None` = empty cell.
    pub day_balance_cents: Option<i64>,
    /// Balance after this booking (#577); `None` = empty cell.
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub balance_cents: Option<i64>,
}

/// `# Konto;Saldo;Währung;Datum` line above the header.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExportCsvBalanceLine {
    pub name: String,
    pub balance_cents: i64,
    pub currency: String,
    pub date: String,
}

/// Optional CSV columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExportCsvColumns {
    pub category: bool,
    pub note: bool,
    pub daily_balance: bool,
    /// "Saldo" after each booking (#577).
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub running_balance: bool,
    /// Counterparty IBAN after "Name".
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub iban: bool,
    /// Tag names after "Notiz".
    #[uniffi(default)]
    #[cfg_attr(feature = "serde", serde(default))]
    pub tags: bool,
}

/// Excel CSV (UTF-8 BOM, semicolon, decimal comma, CRLF).
pub fn export_csv(
    rows: Vec<ExportCsvRow>,
    columns: ExportCsvColumns,
    balance_lines: Vec<ExportCsvBalanceLine>,
) -> String {
    let mut out = String::from("\u{FEFF}");
    for l in &balance_lines {
        out.push_str(&format!(
            "# {};{};{};{}\r\n",
            csv_text(&l.name),
            csv_amount(l.balance_cents),
            csv_text(&l.currency),
            csv_text(&l.date)
        ));
    }
    let mut header = vec!["Datum", "Konto", "Name"];
    if columns.iban {
        header.push("IBAN");
    }
    header.push("Verwendungszweck");
    if columns.category {
        header.push("Kategorie");
    }
    if columns.note {
        header.push("Notiz");
    }
    if columns.tags {
        header.push("Tags");
    }
    header.extend(["Betrag", "Währung"]);
    if columns.running_balance {
        header.push("Saldo");
    }
    if columns.daily_balance {
        header.push("Tagessaldo");
    }
    out.push_str(&header.join(";"));
    out.push_str("\r\n");
    for r in &rows {
        let mut cols = vec![
            csv_text(&r.date),
            csv_text(&r.account_name),
            csv_text(&r.counterparty),
        ];
        if columns.iban {
            cols.push(csv_text(&r.counterparty_iban));
        }
        cols.push(csv_text(&r.purpose));
        if columns.category {
            cols.push(csv_text(&r.category));
        }
        if columns.note {
            cols.push(csv_text(&r.note));
        }
        if columns.tags {
            cols.push(csv_text(&r.tags));
        }
        cols.push(csv_amount(r.amount_cents));
        cols.push(csv_text(&r.currency));
        if columns.running_balance {
            cols.push(r.balance_cents.map(csv_amount).unwrap_or_default());
        }
        if columns.daily_balance {
            cols.push(r.day_balance_cents.map(csv_amount).unwrap_or_default());
        }
        out.push_str(&cols.join(";"));
        out.push_str("\r\n");
    }
    out
}

/// Quotes a CSV field (semicolon dialect); line breaks become spaces.
pub(crate) fn csv_field(v: &str) -> String {
    let s = v.replace(['\r', '\n'], " ");
    if s.contains([';', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// [`csv_field`] with the formula guard for free text: a leading `=`, `+`,
/// `-`, `@`, tab or CR makes Excel/Numbers evaluate the cell (`-2+3`,
/// `=HYPERLINK(…)`), so it is prefixed with `'`.
pub(crate) fn csv_text(v: &str) -> String {
    if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        csv_field(&format!("'{v}"))
    } else {
        csv_field(v)
    }
}

/// Formats cents as `-1234,56` (German decimal comma, no grouping).
pub(crate) fn csv_amount(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let a = cents.unsigned_abs();
    format!("{sign}{},{:02}", a / 100, a % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_formula_guard_and_escaping() {
        assert_eq!(csv_text("-2+3"), "'-2+3");
        assert_eq!(csv_text("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(csv_text("@x;y"), "\"'@x;y\"");
        assert_eq!(csv_text("\tx"), "'\tx");
        assert_eq!(csv_text("Miete"), "Miete");
        assert_eq!(csv_field("-12,50"), "-12,50");
        assert_eq!(csv_amount(-459), "-4,59");
        assert_eq!(csv_amount(250_000), "2500,00");
    }

    #[test]
    fn csv_columns_and_balance_lines() {
        let row = ExportCsvRow {
            date: "2025-03-02".into(),
            account_name: "Giro".into(),
            counterparty: "Supermarkt <Nord>".into(),
            counterparty_iban: "DE02120300000000202051".into(),
            purpose: "Einkauf".into(),
            category: "Lebensmittel".into(),
            note: "Woche".into(),
            tags: "Urlaub, Bar".into(),
            amount_cents: -4_599,
            currency: "EUR".into(),
            day_balance_cents: Some(1_000_000),
            balance_cents: Some(2_500),
        };
        let line = ExportCsvBalanceLine {
            name: "=HYPERLINK(\"x\")".into(),
            balance_cents: 1_000_000,
            currency: "EUR".into(),
            date: "2025-03-31".into(),
        };
        let all = ExportCsvColumns {
            category: true,
            note: true,
            daily_balance: true,
            running_balance: true,
            iban: true,
            tags: true,
        };
        let csv = export_csv(vec![row.clone()], all, vec![line]);
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("# \"'=HYPERLINK(\"\"x\"\")\";10000,00;EUR;2025-03-31\r\n"));
        assert!(csv.contains(
            "Datum;Konto;Name;IBAN;Verwendungszweck;Kategorie;Notiz;Tags;Betrag;Währung;Saldo;Tagessaldo\r\n"
        ));
        assert!(csv.contains(
            "2025-03-02;Giro;Supermarkt <Nord>;DE02120300000000202051;Einkauf;Lebensmittel;Woche;Urlaub, Bar;-45,99;EUR;25,00;10000,00\r\n"
        ));
        let lean = export_csv(vec![row], ExportCsvColumns::default(), vec![]);
        assert!(lean.contains("Datum;Konto;Name;Verwendungszweck;Betrag;Währung\r\n"));
        assert!(!lean.contains("Woche"));
    }

    /// Date and IBAN come from bank data unchecked: they get the formula guard too.
    #[test]
    fn csv_guards_bank_supplied_dates_and_ibans() {
        let row = ExportCsvRow {
            date: "=1+1".into(),
            account_name: "Giro".into(),
            counterparty: "Laden".into(),
            counterparty_iban: "=HYPERLINK(\"http://x.invalid\",\"a\")".into(),
            purpose: "Einkauf".into(),
            category: String::new(),
            note: String::new(),
            tags: String::new(),
            amount_cents: -100,
            currency: "EUR".into(),
            day_balance_cents: None,
            balance_cents: None,
        };
        let line = ExportCsvBalanceLine {
            name: "Giro".into(),
            balance_cents: 0,
            currency: "EUR".into(),
            date: "@now".into(),
        };
        let cols = ExportCsvColumns {
            iban: true,
            ..ExportCsvColumns::default()
        };
        let csv = export_csv(vec![row], cols, vec![line]);
        assert!(csv.contains("# Giro;0,00;EUR;'@now\r\n"), "{csv}");
        assert!(
            csv.contains("'=1+1;Giro;Laden;\"'=HYPERLINK(\"\"http://x.invalid\"\",\"\"a\"\")\";"),
            "{csv}"
        );
    }
}
