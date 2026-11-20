//! Import of an existing `pain.008` file (e.g. last month's collection of a
//! club): its debtors become mandates, the creditor ID and the sequence types
//! are taken over.

use crate::connectors::fints::pain008::{SequenceType, parse_pain008, validate_creditor_id};
use crate::connectors::fints::sepa::validate_creditor_iban_matches;
use crate::error::CoreError;
use crate::i18n::tr;
use crate::iban::normalize_iban;
use crate::vault::Vault;

use super::{MandateImportResult, MandateInput, MandateScheme};

#[uniffi::export]
impl Vault {
    /// Reads a `pain.008` file and saves its debtors as mandates of the creditor
    /// account `account_id` (a known reference updates that mandate). The file's
    /// creditor IBAN must be the account's. A creditor ID the account lacks is
    /// taken from the file. `RCUR`/`FNAL` entries count as already collected
    /// once, so their next collection is again a follow-up.
    pub fn import_pain008(
        &self,
        account_id: i64,
        xml: String,
    ) -> Result<MandateImportResult, CoreError> {
        let account = self
            .list_accounts()?
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| CoreError::invalid(tr("Das Konto gibt es nicht mehr.")))?;
        let blocks = parse_pain008(&xml)?;
        let mut result = MandateImportResult::default();
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let mut known = super::mandate_index(&tx, account_id)?;
        for (n, block) in blocks.iter().enumerate() {
            if !validate_creditor_iban_matches(&block.creditor_iban, &account.iban) {
                return Err(CoreError::invalid(tr(
                    "Die Datei gehört zu einem anderen Gläubigerkonto als dem gewählten.",
                )));
            }
            if n == 0 && !block.creditor_id.is_empty() {
                let has: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM creditor_ids WHERE account_id = ?1 AND creditor_id <> '')",
                    [account_id],
                    |r| r.get(0),
                )?;
                // an invalid ID in the file is simply not taken over
                if !has && validate_creditor_id(&block.creditor_id) {
                    tx.execute(
                        "INSERT INTO creditor_ids(account_id, creditor_id, updated_ms) VALUES (?1, ?2, ?3)
                         ON CONFLICT(account_id) DO UPDATE SET creditor_id = ?2, updated_ms = ?3",
                        rusqlite::params![
                            account_id,
                            block.creditor_id.trim().to_ascii_uppercase(),
                            crate::dates::now_ms()
                        ],
                    )?;
                }
            }
            let scheme = match block.scheme.code() {
                "COR1" => MandateScheme::Cor1,
                "B2B" => MandateScheme::B2b,
                _ => MandateScheme::Core,
            };
            for (i, c) in block.collections.iter().enumerate() {
                let line = format!("{}.{}", n + 1, i + 1);
                let reference = c.mandate_id.trim().to_string();
                let existing = known.get(&reference).cloned();
                let input = MandateInput {
                    account_id,
                    debtor_name: c.debtor_name.clone(),
                    debtor_iban: normalize_iban(&c.debtor_iban),
                    debtor_bic: c.debtor_bic.clone().unwrap_or_default(),
                    reference: c.mandate_id.clone(),
                    signed_date: c.mandate_date.clone(),
                    scheme,
                    one_off: block.sequence == SequenceType::OneOff,
                    default_amount_cents: c.amount_cents,
                    default_purpose: c.purpose.clone(),
                    note: existing.as_ref().map(|m| m.1.clone()).unwrap_or_default(),
                    active: existing.as_ref().is_none_or(|m| m.2),
                };
                match super::store_mandate(&tx, existing.as_ref().map(|m| m.0), &input) {
                    Ok(id) => {
                        known.insert(reference, (id, input.note.clone(), input.active));
                        if matches!(
                            block.sequence,
                            SequenceType::Recurring | SequenceType::Final
                        ) {
                            tx.execute(
                                "UPDATE mandates SET last_collected = ?2 WHERE id = ?1
                                   AND (last_collected IS NULL OR last_collected < ?2)",
                                rusqlite::params![id, input.signed_date],
                            )?;
                        }
                        if existing.is_some() {
                            result.updated += 1;
                        } else {
                            result.created += 1;
                        }
                    }
                    Err(e) => result.failed.push(format!("{line}: {e}")),
                }
            }
        }
        tx.commit()?;
        Ok(result)
    }
}
