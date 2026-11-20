//! Statement PDFs a card portal hands out next to its bookings (Amex): the app
//! downloads them in the signed-in portal page and stores them here, each at
//! most once, as a statement of the card's account. A statement is known by its
//! account and closing date: the portal's own statement ids change with every
//! login.

use rusqlite::params;

use super::portal_import::account_ref;
use crate::error::CoreError;
use crate::vault::Vault;

/// One statement PDF from a card portal.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PortalStatement {
    /// Closing date of the statement, `YYYY-MM-DD`: with the account, what
    /// tells statements apart.
    pub date: String,
    pub bytes: Vec<u8>,
}

/// `source_url` of a stored portal statement.
fn source_of(account: i64, date: &str) -> String {
    format!("{SOURCE_PREFIX}{account}:{date}")
}

const SOURCE_PREFIX: &str = "portal-statement:";

/// `DD.MM.YYYY` of an ISO date (the date as it is, if it is no ISO date).
fn german_date(date: &str) -> String {
    match date.split('-').collect::<Vec<_>>().as_slice() {
        [y, m, d] if y.len() == 4 => format!("{d}.{m}.{y}"),
        _ => date.to_string(),
    }
}

/// The account a card's statements belong to, as the booking import maps the
/// card (#243): the card's own account (key from the card switcher), else the
/// account whose number ends in the card's digits (at least four, e.g. an
/// account taken over from an import), else the login's first card account.
fn card_account(
    db: &rusqlite::Connection,
    connection_id: i64,
    base_ref: &str,
    card_key: &str,
    card_number: &str,
) -> Result<Option<i64>, CoreError> {
    let own = format!("{base_ref}-{card_key}");
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    let number = digits(card_number);
    let mut st = db.prepare(
        "SELECT id, external_ref, IFNULL(account_number, '') FROM accounts
          WHERE connection_id = ?1 AND deleted_ms IS NULL AND archived_unix IS NULL
          ORDER BY id",
    )?;
    let rows = st
        .query_map([connection_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .iter()
        .find(|(_, r, _)| !card_key.is_empty() && *r == own)
        .or_else(|| {
            rows.iter()
                .find(|(_, _, n)| number.len() >= 4 && digits(n).ends_with(&number))
        })
        .or_else(|| rows.iter().find(|(_, r, _)| r == base_ref))
        .or_else(|| rows.first())
        .map(|(id, _, _)| *id))
}

#[uniffi::export]
impl Vault {
    /// Closing dates of the portal statements the card (`card_key`, its digits
    /// `card_number`) already has (see [`Self::import_portal_statements`]): the app downloads only the
    /// others.
    pub fn portal_statement_dates(
        &self,
        connection_id: i64,
        card_key: String,
        card_number: String,
    ) -> Result<Vec<String>, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let base_ref = account_ref(kind, connection_id);
        let db = self.db()?;
        let Some(account) = card_account(&db, connection_id, &base_ref, &card_key, &card_number)?
        else {
            return Ok(Vec::new());
        };
        let mut st = db.prepare(
            "SELECT doc_date FROM booking_documents WHERE account_id = ?1 AND source_url LIKE ?2",
        )?;
        let dates = st
            .query_map(params![account, format!("{SOURCE_PREFIX}%")], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(dates)
    }

    /// Stores statement PDFs of a portal login with the account of the card
    /// `card_key` names (a hash of the portal's card token, "" = unknown;
    /// `card_number` its digits from the card switcher, "" = unknown).
    /// Returns how many were new; a statement stored before, or no PDF, is
    /// left out.
    pub fn import_portal_statements(
        &self,
        connection_id: i64,
        card_key: String,
        card_number: String,
        statements: Vec<PortalStatement>,
    ) -> Result<u32, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        let base_ref = account_ref(kind, connection_id);
        let db = self.db()?;
        let Some(account) = card_account(&db, connection_id, &base_ref, &card_key, &card_number)?
        else {
            return Ok(0);
        };
        let tx = db.unchecked_transaction()?;
        let mut added = 0;
        for s in &statements {
            if !s.bytes.starts_with(b"%PDF") || s.date.len() != 10 {
                continue;
            }
            let title = format!("Kontoauszug {name} {}", german_date(&s.date));
            let file_name = format!("{}_Kontoauszug.pdf", s.date);
            let stored = crate::documents::insert_doc(
                &tx,
                None,
                Some(account),
                &title,
                "kontoauszug",
                "application/pdf",
                &file_name,
                &s.bytes,
                &source_of(account, &s.date),
                &s.date,
            )?;
            added += u32::from(stored.is_some());
        }
        tx.commit()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ConnectorKind;

    #[test]
    fn statements_are_stored_once_with_the_card_account() {
        let dir = crate::test_support::TempDir::new("girocrab-portal-statements");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![5u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::AmericanExpress, String::new(), "Amex".into())
            .unwrap();
        // the card account comes from a booking import
        let tx = r#"{"transactions":[{"charge_date":"2026-11-03","amount":10,"description":"REWE","identifier":"A1"}]}"#;
        v.clone()
            .import_portal_capture(conn, vec![tx.into()], "[]".into())
            .unwrap();
        let pdf = |date: &str| PortalStatement {
            date: date.into(),
            bytes: format!("%PDF-1.4 {date}").into_bytes(),
        };
        let dates = || {
            let mut d = v
                .portal_statement_dates(conn, String::new(), String::new())
                .unwrap();
            d.sort();
            d
        };
        assert!(dates().is_empty());
        let added = v
            .import_portal_statements(
                conn,
                String::new(),
                String::new(),
                vec![
                    pdf("2026-10-25"),
                    pdf("2026-09-25"),
                    // no PDF: left out
                    PortalStatement {
                        date: "2026-08-25".into(),
                        bytes: b"{\"error\":1}".to_vec(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(added, 2);
        assert_eq!(dates(), vec!["2026-09-25", "2026-10-25"]);
        // fetched again after a new login (the portal's ids change): nothing new
        let again = v
            .import_portal_statements(conn, String::new(), String::new(), vec![pdf("2026-10-25")])
            .unwrap();
        assert_eq!(again, 0);
        assert_eq!(dates().len(), 2);
        let (account, title, doc_type): (Option<i64>, String, String) = v
            .db()
            .unwrap()
            .query_row(
                "SELECT account_id, title, doc_type FROM booking_documents WHERE doc_date = '2026-10-25'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(account.is_some());
        assert_eq!(title, "Kontoauszug American Express 25.10.2026");
        assert_eq!(doc_type, "kontoauszug");
    }

    #[test]
    fn a_card_reaches_the_account_with_its_digits() {
        let dir = crate::test_support::TempDir::new("girocrab-portal-statements-digits");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![5u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::AmericanExpress, String::new(), "Amex".into())
            .unwrap();
        // the login's first card and a card taken over from an import (another ref)
        for (r, number) in [("portal-x", "11111"), ("mm:business", "XXXX-XXXXXX-22222")] {
            let id = v
                .create_offline_account(
                    "Karte".into(),
                    crate::models::AccountKind::CreditCard,
                    "EUR".into(),
                    0,
                )
                .unwrap();
            v.db()
                .unwrap()
                .execute(
                    "UPDATE accounts SET connection_id = ?1, external_ref = ?2, account_number = ?3 WHERE id = ?4",
                    rusqlite::params![conn, r, number, id],
                )
                .unwrap();
        }
        let db = v.db().unwrap();
        let id_of = |r: &str| -> i64 {
            db.query_row(
                "SELECT id FROM accounts WHERE external_ref = ?1",
                [r],
                |x| x.get(0),
            )
            .unwrap()
        };
        let base = account_ref(ConnectorKind::AmericanExpress, conn);
        assert_eq!(
            card_account(&db, conn, &base, "kkkk", "••••22222").unwrap(),
            Some(id_of("mm:business"))
        );
        // too few digits, or none: the first card account
        assert_eq!(
            card_account(&db, conn, &base, "kkkk", "222").unwrap(),
            Some(id_of("portal-x"))
        );
        assert_eq!(
            card_account(&db, conn, &base, "", "").unwrap(),
            Some(id_of("portal-x"))
        );
    }
}
