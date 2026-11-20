//! Preparing a collection run: the chosen mandates become `pain.008` files,
//! one per scheme and sequence type (a message carries exactly one of each).

use std::collections::BTreeMap;

use crate::connectors::fints::pain008::{
    Collection, DebitVersion, Pain008, SequenceType, build_pain008,
};
use crate::connectors::fints::sepa::PainHeader;
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::vault::Vault;

use super::MandateScheme;

/// One debtor to collect from in this run.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CollectionItem {
    pub mandate_id: i64,
    pub amount_cents: i64,
    /// Empty = the mandate's default purpose.
    pub purpose: String,
    /// Last collection under the mandate (`FNAL`; ignored for one-off mandates).
    pub last_collection: bool,
}

/// Hands one group of a collection run to the bank (FinTS).
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DirectDebitRequest {
    pub account_id: i64,
    pub items: Vec<CollectionItem>,
    /// Due date `YYYY-MM-DD`.
    pub collection_date: String,
    /// Sequence type (`FRST`, `RCUR`, `OOFF`, `FNAL`) of the file being submitted.
    /// `items` holds only that file's mandates (`DebitFile::mandate_ids`); after an
    /// earlier file of the same run went through, mandates change their
    /// sequence type, so a position in the run would point at the wrong group.
    pub sequence: String,
    /// B2B only: the user confirmed the mandates are on file at the debtors' banks.
    pub b2b_confirmed: bool,
    /// true = the bank books one sum, false = one booking per collection.
    pub batch_booking: bool,
}

/// A ready `pain.008` message.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DebitFile {
    /// Suggested file name, e.g. `lastschrift-CORE-FRST-2026-11-30.xml`.
    pub file_name: String,
    pub scheme: MandateScheme,
    /// `FRST`, `RCUR`, `OOFF` or `FNAL`.
    pub sequence: String,
    pub count: u32,
    pub total_cents: i64,
    pub xml: String,
    /// Mandates in this file, to mark as collected once it is submitted.
    pub mandate_ids: Vec<i64>,
}

fn check_date(d: &str) -> CoreResult<()> {
    jiff::civil::Date::strptime("%Y-%m-%d", d)
        .map(|_| ())
        .map_err(|_| CoreError::invalid(tr("Das Fälligkeitsdatum ist ungültig.")))
}

/// One scheme/sequence group of a run: becomes one message and one submission.
pub(crate) struct CollectionGroup {
    pub scheme: MandateScheme,
    pub sequence: SequenceType,
    pub rows: Vec<(i64, Collection)>,
}

pub(crate) struct CollectionPlan {
    pub creditor_id: String,
    pub creditor_name: String,
    pub creditor_iban: String,
    pub groups: Vec<CollectionGroup>,
}

impl Vault {
    /// Groups `items` by scheme and sequence type (stable order: scheme code,
    /// then sequence code) after checking creditor ID, mandates and date.
    pub(crate) fn plan_collection(
        &self,
        account_id: i64,
        items: &[CollectionItem],
        collection_date: &str,
    ) -> CoreResult<CollectionPlan> {
        check_date(collection_date)?;
        if items.is_empty() {
            return Err(CoreError::invalid(tr(
                "Der Einzug enthält keine Lastschrift.",
            )));
        }
        let account = self
            .list_accounts()?
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| CoreError::invalid(tr("Das Konto gibt es nicht mehr.")))?;
        let creditor_id = self.creditor_id(account_id)?;
        if creditor_id.is_empty() {
            return Err(CoreError::invalid(tr(
                "Für dieses Konto ist keine Gläubiger-ID hinterlegt.",
            )));
        }
        let creditor_name = if account.owner.trim().is_empty() {
            account.name.clone()
        } else {
            account.owner.clone()
        };
        let mut groups: BTreeMap<(String, String), CollectionGroup> = BTreeMap::new();
        let due_compact = collection_date.replace('-', "");
        // one query instead of one per line: runs can have thousands of lines
        let by_id: std::collections::HashMap<i64, super::Mandate> = self
            .list_mandates(Some(account_id))?
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
        let mut seen = std::collections::HashSet::new();
        for item in items {
            let m = by_id
                .get(&item.mandate_id)
                .ok_or_else(|| CoreError::invalid(tr("Das Mandat gibt es nicht mehr.")))?;
            if !seen.insert(m.id) {
                return Err(CoreError::invalid(trf(
                    "Das Mandat „{0}“ steht mehrfach im Einzug.",
                    &[&m.reference],
                )));
            }
            if m.one_off && m.last_collected.is_some() {
                return Err(CoreError::invalid(trf(
                    "Das einmalige Mandat „{0}“ wurde schon eingezogen.",
                    &[&m.reference],
                )));
            }
            if !m.active {
                return Err(CoreError::invalid(trf(
                    "Das Mandat „{0}“ ist nicht aktiv.",
                    &[&m.reference],
                )));
            }
            let mut seq = SequenceType::from_code(&m.next_sequence).unwrap_or(SequenceType::First);
            if item.last_collection && seq == SequenceType::Recurring {
                seq = SequenceType::Final;
            }
            let purpose = if item.purpose.trim().is_empty() {
                m.default_purpose.clone()
            } else {
                item.purpose.clone()
            };
            let c = Collection {
                debtor_name: m.debtor_name.clone(),
                debtor_iban: m.debtor_iban.clone(),
                debtor_bic: Some(m.debtor_bic.clone()).filter(|b| !b.is_empty()),
                amount_cents: item.amount_cents,
                purpose,
                end_to_end_id: format!(
                    "{}-{}",
                    m.reference.chars().take(26).collect::<String>(),
                    due_compact
                ),
                mandate_id: m.reference.clone(),
                mandate_date: m.signed_date.clone(),
            };
            groups
                .entry((m.scheme.debit().code().to_string(), seq.code().to_string()))
                .or_insert_with(|| CollectionGroup {
                    scheme: m.scheme,
                    sequence: seq,
                    rows: Vec::new(),
                })
                .rows
                .push((m.id, c));
        }
        Ok(CollectionPlan {
            creditor_id,
            creditor_name,
            creditor_iban: account.iban,
            groups: groups.into_values().collect(),
        })
    }
}

#[uniffi::export]
impl Vault {
    /// Builds the `pain.008` files for `items` under the creditor account
    /// `account_id`, due on `collection_date` (`YYYY-MM-DD`). The creditor ID
    /// of the account must be set. Nothing is marked as collected. The order
    /// of the files is the group order `begin_direct_debit` takes.
    pub fn build_collection_files(
        &self,
        account_id: i64,
        items: Vec<CollectionItem>,
        collection_date: String,
    ) -> Result<Vec<DebitFile>, CoreError> {
        let plan = self.plan_collection(account_id, &items, &collection_date)?;
        let header = PainHeader::new(&plan.creditor_name);
        let mut files = Vec::new();
        for (n, g) in plan.groups.iter().enumerate() {
            let collections: Vec<Collection> = g.rows.iter().map(|(_, c)| c.clone()).collect();
            let message_id = format!("{}-{}", header.message_id, n + 1);
            let xml = build_pain008(&Pain008 {
                message_id: &message_id,
                created: &header.created,
                collection_date: &collection_date,
                creditor_name: header.owner,
                creditor_iban: &plan.creditor_iban,
                creditor_bic: "",
                creditor_id: &plan.creditor_id,
                scheme: g.scheme.debit(),
                sequence: g.sequence,
                collections: &collections,
                version: DebitVersion::V02,
                batch_booking: true,
            })?;
            files.push(DebitFile {
                file_name: format!(
                    "lastschrift-{}-{}-{collection_date}.xml",
                    g.scheme.debit().code(),
                    g.sequence.code()
                ),
                scheme: g.scheme,
                sequence: g.sequence.code().to_string(),
                count: collections.len() as u32,
                total_cents: collections.iter().map(|c| c.amount_cents).sum(),
                xml,
                mandate_ids: g.rows.iter().map(|(id, _)| *id).collect(),
            });
        }
        Ok(files)
    }

    /// Records that these mandates were collected with the due date `due_date`
    /// (their next collection is then `RCUR`, a one-off mandate stays `OOFF`).
    pub fn mark_collected(&self, mandate_ids: Vec<i64>, due_date: String) -> Result<(), CoreError> {
        check_date(&due_date)?;
        let db = self.db()?;
        for id in mandate_ids {
            db.execute(
                "UPDATE mandates SET last_collected = ?2 WHERE id = ?1
                   AND (last_collected IS NULL OR last_collected < ?2)",
                rusqlite::params![id, due_date],
            )?;
        }
        Ok(())
    }
}
