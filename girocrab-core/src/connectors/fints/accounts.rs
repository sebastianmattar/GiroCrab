//! Accounts: the account list (UPD merged with HKSPA), balances (HKSAL), depot
//! holdings (HKWPD) and the KTI/KTV account references of account-bound
//! requests.

use std::collections::HashSet;

use super::syntax::{SegmentBuilder, escape};
use super::{CONSORSBANK_BLZ, COUNTRY, FinTsConnector, SepaAccount, UpdAccount, fints_date, mt535};
use crate::connectors::{ConnectContext, today};
use crate::error::{CoreError, CoreResult};
use crate::i18n::trf;
use crate::iban::normalize_iban;
use crate::models::AccountKind;
use crate::money::parse_cents;

pub(super) fn kind_from_code(code: u32) -> AccountKind {
    // FinTS Kontoart: 1-9 Kontokorrent, 10-19 Sparkonto, 20-29 Festgeld,
    // 30-39 Depot, 40-49 Darlehen, 50-59 Kreditkarte, 60-69 Fonds-Depot,
    // 70-79 Bausparvertrag, 80-89 Versicherung, 90-99 Sonstige
    match code {
        10..=29 | 70..=79 => AccountKind::Savings,
        30..=39 | 60..=69 => AccountKind::Brokerage,
        40..=49 => AccountKind::Loan,
        50..=59 => AccountKind::CreditCard,
        _ => AccountKind::Checking,
    }
}

/// Accounts from the UPD (HIUPD) merged with the SEPA account list (HISPA), which adds
/// BIC and IBAN. Returns (account, upd); HISPA accounts missing from the UPD follow
/// with `None`. `blz` fills in a bank code the UPD leaves empty.
///
/// An account with an IBAN of its own is paired by that IBAN first. An account without
/// one may take the IBAN of a HISPA entry with the same account number, but never an
/// IBAN that another UPD account carries itself or that an earlier account already
/// took: comdirect reports its depot (no IBAN) with the account number of the
/// Girokonto, and the depot then shared the Girokonto's key – one account with the
/// positions and the depot value as balance (#805).
pub(super) fn merge_upd_with_sepa(
    upd: &[UpdAccount],
    sepa: Vec<SepaAccount>,
    blz: &str,
) -> Vec<(SepaAccount, Option<UpdAccount>)> {
    // IBANs the bank assigns to UPD accounts themselves: never handed to another one.
    let mut claimed: HashSet<String> = upd
        .iter()
        .filter(|u| !u.iban.is_empty())
        .map(|u| u.iban.clone())
        .collect();
    let mut out: Vec<(SepaAccount, Option<UpdAccount>)> = Vec::new();
    for u in upd {
        // Only accounts of this bank with at least one usable transaction
        let usable = ["HKSAL", "HKCAZ", "HKKAZ", "HKWPD", "HKKKU", "DKKKU"]
            .iter()
            .any(|t| u.allows(t));
        if crate::depot_debug::enabled() {
            let mut allowed: Vec<&str> = u.allowed.iter().map(String::as_str).collect();
            allowed.sort_unstable();
            crate::depot_debug::log(&format!(
                "fints UPD account: kind_code={} has_iban={} usable={usable} allowed=[{}]",
                u.kind_code,
                !u.iban.is_empty(),
                allowed.join(",")
            ));
        }
        if !usable {
            continue;
        }
        let by_iban = sepa.iter().find(|s| !u.iban.is_empty() && s.iban == u.iban);
        let s = by_iban.or_else(|| {
            sepa.iter().find(|s| {
                !u.account_number.is_empty()
                    && s.account_number == u.account_number
                    && !claimed.contains(&s.iban)
            })
        });
        let iban = s.map(|s| s.iban.clone()).unwrap_or_else(|| u.iban.clone());
        if !iban.is_empty() {
            claimed.insert(iban.clone());
        }
        let acc = SepaAccount {
            iban,
            bic: s.map(|s| s.bic.clone()).unwrap_or_default(),
            account_number: u.account_number.clone(),
            sub_account: u.sub_account.clone(),
            blz: if u.blz.is_empty() {
                blz.to_string()
            } else {
                u.blz.clone()
            },
        };
        out.push((acc, Some(u.clone())));
    }
    for s in sepa {
        if !out.iter().any(|(a, _)| a.iban == s.iban) {
            out.push((s, None));
        }
    }
    if crate::depot_debug::enabled() {
        let mut labels: Vec<String> = out.iter().map(|(a, _)| account_label(a)).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        if labels.len() != before {
            crate::depot_debug::log(&format!(
                "fints accounts: {} of {before} share a key with another account",
                before - labels.len()
            ));
        }
    }
    out
}

/// Stable per-account key (`external_ref`). Loans (Darlehen) have no IBAN and often share
/// one account number, distinguished only by the Unterkontomerkmal – it must be part of
/// the key, otherwise every sub-account collapses into one account with all bookings.
pub(super) fn account_label(acc: &SepaAccount) -> String {
    if !acc.iban.is_empty() {
        acc.iban.clone()
    } else if acc.sub_account.is_empty() {
        acc.account_number.clone()
    } else {
        format!("{}-{}", acc.account_number, acc.sub_account)
    }
}

impl FinTsConnector {
    /// Accounts from UPD merged with HKSPA (BIC/IBAN). Returns (account, upd).
    pub(super) async fn account_list(
        &mut self,
        ctx: &ConnectContext,
        errors: &mut Vec<String>,
    ) -> Vec<(SepaAccount, Option<UpdAccount>)> {
        let mut sepa: Vec<SepaAccount> = Vec::new();
        if self.bank_supports("HKSPA") {
            let v = self.version("HKSPA");
            match self
                .request(ctx, "HKSPA", SegmentBuilder::new("HKSPA", 3, v).build())
                .await
            {
                Ok(reply) => {
                    for hispa in reply.all("HISPA") {
                        for el in hispa.elements.iter().skip(1) {
                            let t = |i: usize| {
                                el.get(i).map(|v| v.text().to_string()).unwrap_or_default()
                            };
                            if !t(1).is_empty() {
                                sepa.push(SepaAccount {
                                    iban: t(1),
                                    bic: t(2),
                                    account_number: t(3),
                                    sub_account: t(4),
                                    blz: t(6),
                                });
                            }
                        }
                    }
                }
                Err(e) => errors.push(trf("Kontenliste (HKSPA) – {0}", &[&e])),
            }
        }
        merge_upd_with_sepa(&self.upd, sepa, &self.blz)
    }

    /// The order's source account from the bank's account list. `None` when the
    /// UPD does not allow `seg_id` for it; the caller words that refusal.
    pub(super) async fn source_account(
        &mut self,
        ctx: &ConnectContext,
        iban: &str,
        seg_id: &str,
    ) -> CoreResult<Option<SepaAccount>> {
        let mut errors = Vec::new();
        let accounts = self.account_list(ctx, &mut errors).await;
        let want = normalize_iban(iban);
        let Some((acc, upd)) = accounts
            .into_iter()
            .find(|(a, _)| normalize_iban(&a.iban) == want)
        else {
            // A failed HKSPA is the likely reason the account is missing.
            let reason = if errors.is_empty() {
                String::new()
            } else {
                format!(" ({})", errors.join(" / "))
            };
            return Err(CoreError::NotFound {
                what: trf("Konto {0} bei der Bank{1}", &[&iban, &reason]),
            });
        };
        Ok(upd.is_none_or(|u| u.allows(seg_id)).then_some(acc))
    }

    pub(super) async fn balance(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
    ) -> CoreResult<Option<(i64, String, String)>> {
        let v = self.version("HKSAL");
        let acct = if v >= 7 {
            Self::kti(acc)
        } else {
            Self::ktv(acc)
        };
        let r = self
            .request(
                ctx,
                "HKSAL",
                SegmentBuilder::new("HKSAL", 3, v)
                    .raw(&acct)
                    .raw("N")
                    .build(),
            )
            .await?;
        Ok(r.first("HISAL").and_then(|s| {
            // element 4: booked balance  C|D : amount : currency : date
            let c = parse_cents(s.get(4, 1)).ok()?;
            let sign = if s.get(4, 0) == "D" { -1 } else { 1 };
            let cur = if s.get(4, 2).is_empty() {
                "EUR".to_string()
            } else {
                s.get(4, 2).to_string()
            };
            let d = s.get(4, 3);
            let date = fints_date(d).unwrap_or_else(today);
            Some((sign * c, cur, date))
        }))
    }

    /// HKWPD: holdings statement of a depot (MT535), all pages.
    pub(super) async fn holdings(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
    ) -> CoreResult<mt535::Statement> {
        let v = self.version("HKWPD");
        let mut data = Vec::<u8>::new();
        let mut touchdown: Option<String> = None;
        crate::depot_debug::log(&format!(
            "fints HKWPD: bpd HIWPDS versions={:?} (bank_supports={}), sending v{v} with KTV",
            self.st.versions.get("HKWPD"),
            self.bank_supports("HKWPD")
        ));
        for page in 0..20 {
            let r = match self
                .request(ctx, "HKWPD", Self::hkwpd(v, acc, touchdown.as_deref()))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    crate::depot_debug::log(&format!(
                        "fints HKWPD page {page}: request error (kind of error only): {}",
                        e.to_string().chars().take(4).collect::<String>()
                    ));
                    return Err(e);
                }
            };
            let codes: Vec<String> = r
                .codes
                .iter()
                .map(|c| format!("{}@{}", c.0, c.1.map(|n| n.to_string()).unwrap_or_default()))
                .collect();
            let hiwpd: Vec<String> = r
                .all("HIWPD")
                .map(|s| {
                    format!(
                        "v{} bin_len={:?} elems={}",
                        s.version(),
                        s.binary(1).map(|b| b.len()),
                        s.elements.len()
                    )
                })
                .collect();
            crate::depot_debug::log(&format!(
                "fints HKWPD page {page}: codes={codes:?} HIWPD segments={hiwpd:?}"
            ));
            for s in r.all("HIWPD") {
                if let Some(b) = s.binary(1) {
                    data.extend_from_slice(b);
                    data.extend_from_slice(b"\r\n");
                }
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                break;
            }
        }
        Ok(mt535::parse(&crate::text::decode_bank_bytes(&data)))
    }

    /// Depot (KTV), then currency, price quality and max. entries (all
    /// optional, left to the bank) and the touchdown point.
    pub(super) fn hkwpd(version: u32, acc: &SepaAccount, touchdown: Option<&str>) -> Vec<u8> {
        let mut b = SegmentBuilder::new("HKWPD", 3, version).raw(&Self::ktv(acc));
        if let Some(t) = touchdown {
            b = b.raw("").raw("").raw("").de(&[t]);
        }
        b.build()
    }

    /// KTI: IBAN:BIC, or (without IBAN, e.g. loans) ::Konto:Unterkonto:280:BLZ.
    /// Consorsbank rejects the IBAN:BIC-only form ("9010 Fehler im
    /// Segmentaufbau") and wants all of IBAN:BIC:Konto:Unterkonto:280:BLZ.
    pub(super) fn kti(acc: &SepaAccount) -> String {
        if acc.blz == CONSORSBANK_BLZ && !acc.iban.is_empty() && !acc.account_number.is_empty() {
            return format!(
                "{}:{}:{}:{}:{COUNTRY}:{}",
                escape(&acc.iban),
                escape(&acc.bic),
                escape(&acc.account_number),
                escape(&acc.sub_account),
                acc.blz
            );
        }
        if acc.iban.is_empty() {
            format!(
                "::{}:{}:{COUNTRY}:{}",
                escape(&acc.account_number),
                escape(&acc.sub_account),
                acc.blz
            )
        } else {
            format!("{}:{}", escape(&acc.iban), escape(&acc.bic))
        }
    }

    pub(super) fn ktv(acc: &SepaAccount) -> String {
        format!(
            "{}:{}:{COUNTRY}:{}",
            escape(&acc.account_number),
            escape(&acc.sub_account),
            acc.blz
        )
    }
}
