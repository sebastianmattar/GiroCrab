//! Accuracy check of the suggestions on the open vault (developer tool, #664,
//! `--eval-suggestions` in the app): every hand-categorised booking is treated
//! as uncategorised and compared with what the history would suggest. The
//! report holds counts only, no names, amounts or IBANs.

use std::fmt::Write;

use super::{
    Limits, SuggestionBasis, Target, Vote, day_number, merchant_key, normalize_iban, similar,
};
use crate::error::CoreError;
use crate::merchants::MerchantResolver;
use crate::vault::Vault;

/// One booking with its true category.
struct Case {
    target: Target,
    category_id: i64,
}

#[derive(Default)]
struct Tally {
    right: [u32; 3],
    wrong: [u32; 3],
    /// The merchant had no votes left (stage 3's ground) …
    no_history: u32,
    /// … and stage 3 suggested nothing either.
    none_without_history: u32,
    /// Votes, but too few or too mixed.
    undecided: u32,
}

fn pct(n: u32, of: u32) -> f64 {
    if of == 0 {
        0.0
    } else {
        100.0 * f64::from(n) / f64::from(of)
    }
}

impl Tally {
    fn line(&self, total: u32) -> String {
        let right: u32 = self.right.iter().sum();
        let wrong: u32 = self.wrong.iter().sum();
        let shown = right + wrong;
        let similar = self.right[2] + self.wrong[2];
        format!(
            "suggested {shown} ({:.1} % of {total}), right {right}, wrong {wrong}, precision {:.1} % \
             [IBAN {}/{} · merchant {}/{} · similar {}/{} right/wrong]; \
             no merchant history {}: stage 3 suggested {similar} ({:.1} %, precision {:.1} %); undecided {}",
            pct(shown, total),
            pct(right, shown),
            self.right[0],
            self.wrong[0],
            self.right[1],
            self.wrong[1],
            self.right[2],
            self.wrong[2],
            self.no_history,
            pct(similar, self.no_history),
            pct(self.right[2], similar),
            self.undecided,
        )
    }
}

#[uniffi::export]
impl Vault {
    /// (developer) Accuracy report of the category suggestions on this vault:
    /// counts only, no names, amounts or IBANs.
    pub fn evaluate_suggestions(&self) -> Result<String, CoreError> {
        let started = std::time::Instant::now();
        let history = self.suggestion_history()?;
        let (cases, skipped) = {
            let db = self.db()?;
            let own = Vault::own_ibans(&db)?;
            let mut stmt = db.prepare(
                "SELECT t.id, t.account_id, t.booking_date, t.amount_cents, t.counterparty,
                        t.counterparty_iban, t.purpose, t.category_id, t.booking_text
                 FROM transactions t
                 JOIN accounts a ON a.id = t.account_id
                 JOIN categories c ON c.id = t.category_id
                 WHERE t.deleted_ms IS NULL AND a.deleted_ms IS NULL AND c.deleted_ms IS NULL
                   AND t.category_source = 'manual'
                   AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        (
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                        ),
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            drop(db);
            let (mut cases, mut skipped) = (Vec::new(), 0u32);
            for ((id, account_id, date), cents, counterparty, iban, purpose, category_id, text) in
                rows
            {
                let iban = normalize_iban(&iban);
                let merchant = merchant_key(&MerchantResolver::resolve(&counterparty, &purpose));
                match merchant {
                    Some(merchant) if !own.contains(&iban) => cases.push(Case {
                        target: Target {
                            id,
                            account_id,
                            amount_cents: cents,
                            day: day_number(&date),
                            iban,
                            merchant,
                            words: similar::words(&[&counterparty, &purpose, &text]),
                        },
                        category_id,
                    }),
                    _ => skipped += 1,
                }
            }
            (cases, skipped)
        };
        let total = cases.len() as u32;

        let run = |limits: Limits, earlier_only: bool| {
            let mut tally = Tally::default();
            for c in &cases {
                let t = &c.target;
                let skip = |v: &Vote| {
                    v.booking_id == t.id
                        || (earlier_only
                            && match (v.day, t.day) {
                                (Some(vote), Some(target)) => vote >= target,
                                _ => true,
                            })
                };
                let income = t.amount_cents > 0;
                let has_history = history
                    .by_merchant
                    .get(&(income, t.merchant.clone()))
                    .is_some_and(|votes| votes.iter().any(|v| !skip(v)));
                if !has_history {
                    tally.no_history += 1;
                }
                match history.suggest_with(t, limits, skip) {
                    Some(s) => {
                        let stage = match s.basis {
                            SuggestionBasis::Iban => 0,
                            SuggestionBasis::Merchant => 1,
                            SuggestionBasis::Similar => 2,
                        };
                        if s.category_id == c.category_id {
                            tally.right[stage] += 1;
                        } else {
                            tally.wrong[stage] += 1;
                        }
                    }
                    None if has_history => tally.undecided += 1,
                    None => tally.none_without_history += 1,
                }
            }
            tally
        };

        let mut out = String::new();
        let _ = writeln!(
            out,
            "Category suggestions (#664): accuracy on this vault, counts only"
        );
        let _ = writeln!(
            out,
            "hand-categorised bookings checked: {total} (left out: {skipped} without merchant key or to own accounts)"
        );
        let base = Limits::DEFAULT;
        for (earlier_only, title) in [
            (
                true,
                "A) history = only EARLIER bookings (like a new booking arriving)",
            ),
            (false, "B) history = all OTHER bookings (leave-one-out)"),
        ] {
            let _ = writeln!(out, "\n{title}");
            let off = Limits {
                similar_k: 0,
                ..base
            };
            let _ = writeln!(
                out,
                "  stages 1+2 only: {}",
                run(off, earlier_only).line(total)
            );
            for min_similarity in [0.3, 0.4, 0.5, 0.6, 0.7] {
                for similar_support in [2u32, 3, 4] {
                    let limits = Limits {
                        min_similarity,
                        similar_support,
                        ..base
                    };
                    let mark = if min_similarity == base.min_similarity
                        && similar_support == base.similar_support
                    {
                        " ← current"
                    } else {
                        ""
                    };
                    let _ = writeln!(
                        out,
                        "  stage 3 similarity≥{min_similarity:.1} support≥{similar_support}: {}{mark}",
                        run(limits, earlier_only).line(total)
                    );
                }
            }
        }
        let _ = writeln!(out, "\ntook {} ms", started.elapsed().as_millis());
        Ok(out)
    }
}
