//! Händler-Historie, Sparplan-/Perks-Dashboard und der optionale
//! Logo-Cache (Tier 2 des Merchant-Resolvers).

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::civil::Date;
use rusqlite::{OptionalExtension, params};

use crate::connectors::read_body_limited;
use crate::error::CoreError;
use crate::merchants::{FlowKind, MerchantResolver};
use crate::models::*;
use crate::vault::{LIVE_BOOKINGS_SQL, Vault, sql_int_list};

fn all_bookings(
    v: &Vault,
    db: &rusqlite::Connection,
    scope: BookingScope,
) -> Result<Vec<Booking>, CoreError> {
    let filter = BookingFilter {
        scope,
        ..Default::default()
    };
    v.collect_bookings(db, &filter)
}

fn month_key(date: &str) -> String {
    date.chars().take(7).collect()
}

fn last_months(n: u32) -> Vec<String> {
    last_months_ending(n, 0)
}

/// `n` months ending `offset` months before the current one (0 = up to now), oldest first.
fn last_months_ending(n: u32, offset: u32) -> Vec<String> {
    let today = crate::dates::today();
    let back = offset as i32;
    let (mut y, mut m) = (
        i32::from(today.year()) - back / 12,
        i32::from(today.month()) - back % 12,
    );
    if m <= 0 {
        m += 12;
        y -= 1;
    }
    let mut out = Vec::new();
    for _ in 0..n.max(1) {
        out.push(format!("{y:04}-{m:02}"));
        m -= 1;
        if m == 0 {
            m = 12;
            y -= 1;
        }
    }
    out.reverse();
    out
}

fn lower_contains(b: &Booking, needles: &[&str]) -> bool {
    let t = format!("{} {} {}", b.counterparty, b.purpose, b.booking_text).to_lowercase();
    needles.iter().any(|n| t.contains(n))
}

#[uniffi::export]
impl Vault {
    /// "Händler-Historie": how often and how much at this merchant.
    pub fn merchant_stats(&self, merchant_key: String) -> Result<MerchantStats, CoreError> {
        Ok(self.merchant_history_for(&merchant_key, None)?.0)
    }

    /// Total and own-account (#299) history of a merchant in one pass (#591):
    /// what the booking detail shows, at the cost of a single `merchant_stats`.
    pub fn merchant_history(
        &self,
        merchant_key: String,
        account_id: i64,
    ) -> Result<MerchantHistory, CoreError> {
        let (all, account) = self.merchant_history_for(&merchant_key, Some(account_id))?;
        Ok(MerchantHistory {
            all,
            account: account.unwrap_or_else(|| MerchantTally::default().stats(&merchant_key)),
        })
    }

    /// KPI tiles for the selected accounts (empty = all) and period. Sums in the
    /// core, so the bookings never cross the FFI boundary.
    pub fn selection_flow(
        &self,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<SelectionFlow, CoreError> {
        Ok(self
            .list_page(
                BookingFilter {
                    account_ids,
                    date_from,
                    date_to,
                    ..Default::default()
                },
                false,
            )?
            .flow)
    }

    /// The same tiles as [`selection_flow`](Self::selection_flow), but for
    /// overall money figures (#337): accounts outside the effective total set
    /// (own switch off, excluded group, archived) contribute nothing. A
    /// selection that is empty after that exclusion means "none" (zero sums),
    /// never "all accounts".
    pub fn selection_flow_for_totals(
        &self,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<SelectionFlow, CoreError> {
        // rows-free path like `selection_flow`: only the flow is used, so no
        // booking is enriched or kept
        Ok(self
            .list_page(
                BookingFilter {
                    account_ids,
                    date_from,
                    date_to,
                    respect_in_total: true,
                    ..Default::default()
                },
                false,
            )?
            .flow)
    }

    /// "Sparpläne & Perks": recurring plans with a month grid, Saveback and
    /// Round up totals, and consumption vs. investment per month.
    pub fn investment_dashboard(&self, months: u32) -> Result<InvestmentDashboard, CoreError> {
        let months_list = last_months(months);
        let first_month = months_list.first().cloned().unwrap_or_default();
        let this_month = months_list.last().cloned().unwrap_or_default();
        // Overall money figure (#337): only accounts of the effective total set
        // contribute — own switch, group and archiving, like the KPI tiles and
        // the cashflow card. The booking list itself is never narrowed by this.
        // One guard for both reads, so a merge committing in between cannot mix
        // old amounts with new eligibility.
        let all: Vec<Booking> = {
            let db = self.db()?;
            let allowed = crate::user_groups::total_account_ids(&db)?;
            all_bookings(self, &db, BookingScope::All)?
                .into_iter()
                .filter(|b| allowed.contains(&b.account_id))
                .collect()
        };

        // Monthly flows
        let mut monthly: BTreeMap<String, MonthFlow> = months_list
            .iter()
            .map(|m| {
                (
                    m.clone(),
                    MonthFlow {
                        month: m.clone(),
                        consumption_cents: 0,
                        investment_cents: 0,
                        income_cents: 0,
                    },
                )
            })
            .collect();
        for b in &all {
            if let Some(mf) = monthly.get_mut(&month_key(&b.booking_date)) {
                match b.flow {
                    FlowKind::Consumption => mf.consumption_cents += -b.amount_cents,
                    FlowKind::Investment => mf.investment_cents += -b.amount_cents,
                    FlowKind::Perk if b.amount_cents < 0 => mf.investment_cents += -b.amount_cents,
                    FlowKind::Income => mf.income_cents += b.amount_cents,
                    _ => {}
                }
            }
        }

        // Savings plans: investment outflows that look like plan executions
        let mut plans: HashMap<String, (crate::merchants::MerchantInfo, Vec<&Booking>)> =
            HashMap::new();
        let (mut sb_earned, mut sb_count, mut ru_inv, mut ru_count, mut perks_month) =
            (0i64, 0u32, 0i64, 0u32, 0i64);
        for b in &all {
            if b.flow == FlowKind::Perk {
                if lower_contains(b, &["saveback"]) {
                    if b.amount_cents > 0 {
                        sb_earned += b.amount_cents;
                        sb_count += 1;
                        if month_key(&b.booking_date) == this_month {
                            perks_month += b.amount_cents;
                        }
                    }
                } else if b.amount_cents < 0 {
                    ru_inv += -b.amount_cents;
                    ru_count += 1;
                    if month_key(&b.booking_date) == this_month {
                        perks_month += -b.amount_cents;
                    }
                }
                continue;
            }
            if b.flow == FlowKind::Investment
                && b.amount_cents < 0
                && b.booking_date.as_str() >= first_month.as_str()
            {
                plans
                    .entry(b.merchant.key.clone())
                    .or_insert_with(|| (b.merchant.clone(), Vec::new()))
                    .1
                    .push(b);
            }
        }
        let today = crate::dates::today();
        let mut out_plans: Vec<SavingsPlan> = plans
            .into_values()
            .filter(|(_, v)| v.len() >= 2 || v.iter().any(|b| lower_contains(b, &["sparplan"])))
            .map(|(m, v)| {
                let latest = v.iter().max_by_key(|b| b.booking_date.clone()).unwrap();
                let mut per_month: HashMap<String, i64> = HashMap::new();
                for b in &v {
                    *per_month.entry(month_key(&b.booking_date)).or_default() += -b.amount_cents;
                }
                let last = Date::strptime("%Y-%m-%d", &latest.booking_date).ok();
                SavingsPlan {
                    merchant: m,
                    monthly_cents: -latest.amount_cents,
                    day_of_month: last.map_or(1, |d| d.day() as u32),
                    last_date: latest.booking_date.clone(),
                    active: last
                        .map(|d| crate::dates::days_between(d, today) <= 40)
                        .unwrap_or(false),
                    total_invested_cents: v.iter().map(|b| -b.amount_cents).sum(),
                    executions: months_list
                        .iter()
                        .map(|mk| PlanExecution {
                            month: mk.clone(),
                            amount_cents: per_month.get(mk).copied().unwrap_or(0),
                        })
                        .collect(),
                    run_days: vec![last.map_or(1, |d| d.day() as u32)],
                }
            })
            .collect();
        out_plans.sort_by(|a, b| {
            b.monthly_cents
                .cmp(&a.monthly_cents)
                .then(a.merchant.name.cmp(&b.merchant.name))
        });
        let total_monthly = out_plans
            .iter()
            .filter(|p| p.active)
            .map(|p| p.monthly_cents)
            .sum();
        Ok(InvestmentDashboard {
            months: months_list,
            plans: out_plans,
            total_monthly_cents: total_monthly,
            saveback_earned_cents: sb_earned,
            saveback_count: sb_count,
            roundup_invested_cents: ru_inv,
            roundup_count: ru_count,
            perks_this_month_cents: perks_month,
            monthly: monthly.into_values().collect(),
        })
    }

    /// Savings plans of one depot, from its securities activity: the buys whose text says "Sparplan"
    /// (or the English `savings_plan`), grouped by security, in the shape of the "Sparpläne & Perks" grid
    /// (perks and flows stay empty). Only executions in the depot's currency count: the grid shows one
    /// currency and nothing is converted. The grid shows `months` months ending `end_offset_months`
    /// before the current one (0 = up to now); an earlier page is worked out as of its last month: only
    /// what ran until then counts for the rate and for "active".
    pub fn depot_savings_plans(
        &self,
        account_id: i64,
        months: u32,
        end_offset_months: u32,
    ) -> Result<InvestmentDashboard, CoreError> {
        let transactions = self.list_security_transactions(account_id)?;
        let currency: String = self.db()?.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            [account_id],
            |r| r.get(0),
        )?;
        let months_list = last_months_ending(months, end_offset_months);
        let plans = plans_from_security_transactions(
            &transactions,
            &months_list,
            crate::dates::today(),
            &currency,
        );
        let total_monthly = plans
            .iter()
            .filter(|p| p.active)
            .map(|p| p.monthly_cents)
            .sum();
        Ok(InvestmentDashboard {
            months: months_list,
            plans,
            total_monthly_cents: total_monthly,
            saveback_earned_cents: 0,
            saveback_count: 0,
            roundup_invested_cents: 0,
            roundup_count: 0,
            perks_this_month_cents: 0,
            monthly: Vec::new(),
        })
    }

    /// Tier 2: cached logo bytes (PNG/ICO) for a domain – never touches the network.
    /// None while online logos are off: "Aus" shows no favicons, not even cached ones (#422).
    pub fn get_merchant_logo_png(&self, domain: String) -> Result<Option<Vec<u8>>, CoreError> {
        let db = self.db()?;
        let raw = Vault::setting_inner(&db, "merchants.online_logos")?;
        if normalize_logo_provider(raw.as_deref()) == "off" {
            return Ok(None);
        }
        let bytes: Option<Option<Vec<u8>>> = db
            .query_row(
                "SELECT image_bytes FROM merchant_logo_cache WHERE domain = ?1",
                [&domain],
                |r| r.get(0),
            )
            .optional()?;
        Ok(bytes.flatten().filter(|b| good_logo(b)))
    }

    /// Manual domain for a merchant (inspector "edit" button). None/empty
    /// removes the override. Returns the stored, normalised domain.
    pub fn set_merchant_domain(
        &self,
        merchant_key: String,
        domain: Option<String>,
    ) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        match domain
            .map(|d| normalise_domain(&d))
            .filter(|d| d.contains('.'))
        {
            Some(d) => {
                db.execute(
                    "INSERT INTO merchant_domain_overrides(merchant_key, domain) VALUES (?1, ?2)
                     ON CONFLICT(merchant_key) DO UPDATE SET domain = excluded.domain, updated_at = strftime('%s','now')",
                    params![merchant_key, d],
                )?;
                // forget a previous negative result so the logo is fetched again
                db.execute("DELETE FROM merchant_logo_cache WHERE domain = ?1 AND (image_bytes IS NULL OR length(image_bytes) = 0)", [&d])?;
                Ok(Some(d))
            }
            None => {
                db.execute(
                    "DELETE FROM merchant_domain_overrides WHERE merchant_key = ?1",
                    [&merchant_key],
                )?;
                Ok(None)
            }
        }
    }

    pub fn clear_logo_cache(&self) -> Result<u32, CoreError> {
        Ok(self.db()?.execute("DELETE FROM merchant_logo_cache", [])? as u32)
    }

    pub fn online_logos_enabled(&self) -> Result<bool, CoreError> {
        Ok(self.online_logo_provider()? != "off")
    }

    /// Selected favicon provider: "off" (default), "duckduckgo" or "google".
    /// Legacy values from the former on/off toggle ("1"/"true") map to
    /// "google" (what they used first); anything else means "off".
    pub fn online_logo_provider(&self) -> Result<String, CoreError> {
        let db = self.db()?;
        let raw = Vault::setting_inner(&db, "merchants.online_logos")?;
        Ok(normalize_logo_provider(raw.as_deref()).to_string())
    }
}

/// Running totals of one side of `merchant_history_for`.
#[derive(Default)]
struct MerchantTally {
    count: u32,
    total: i64,
    /// (date, id, merchant name) of the newest booking
    newest: Option<(String, i64, String)>,
    /// (date, id) of the oldest booking
    oldest: Option<(String, i64)>,
}

impl MerchantTally {
    fn add(&mut self, id: i64, date: &str, amount: i64, name: &str) {
        self.count += 1;
        self.total += amount;
        // newest first by (booking_date, id), the order of `list_transactions`
        if self
            .newest
            .as_ref()
            .is_none_or(|(d, i, _)| (date, id) > (d.as_str(), *i))
        {
            self.newest = Some((date.to_string(), id, name.to_string()));
        }
        if self
            .oldest
            .as_ref()
            .is_none_or(|(d, i)| (date, id) < (d.as_str(), *i))
        {
            self.oldest = Some((date.to_string(), id));
        }
    }

    fn stats(self, key: &str) -> MerchantStats {
        let count = self.count;
        let (last_date, name) = self.newest.map(|(d, _, n)| (d, n)).unwrap_or_default();
        MerchantStats {
            key: key.to_string(),
            name,
            count,
            total_cents: self.total,
            average_cents: if count > 0 {
                self.total / count as i64
            } else {
                0
            },
            first_date: self.oldest.map(|(d, _)| d).unwrap_or_default(),
            last_date,
        }
    }
}

impl Vault {
    /// Merchant history over all accounts and, with `account_id`, over the
    /// bookings `list_transactions` shows for that account (a loan's linked
    /// bookings included). The merchant is no SQL column, so this reads every
    /// booking once, but only the columns `MerchantResolver` needs: no full
    /// enrichment, no `BookingFinisher`, no tags (#591). When the list's booking
    /// classes are cached, only the merchant's own bookings are read.
    fn merchant_history_for(
        &self,
        merchant_key: &str,
        account_id: Option<i64>,
    ) -> Result<(MerchantStats, Option<MerchantStats>), CoreError> {
        let db = self.db()?;
        let ids: Option<String> = self.cached_booking_classes(&db).map(|classes| {
            let ids: Vec<i64> = classes
                .iter()
                .filter(|(_, c)| &*c.merchant == merchant_key)
                .map(|(id, _)| *id)
                .collect();
            format!("[{}]", sql_int_list(&ids))
        });
        let only_ids = if ids.is_some() {
            " AND t.id IN (SELECT value FROM json_each(?1))"
        } else {
            ""
        };
        // (id, booking_date, amount_cents, merchant name) of the merchant's bookings
        let mut matches: Vec<(i64, String, i64, String)> = Vec::new();
        {
            let mut stmt = db.prepare(&format!(
                "SELECT t.id, t.booking_date, t.amount_cents, t.counterparty, t.purpose \
                 FROM transactions t JOIN accounts a ON a.id = t.account_id \
                 WHERE {LIVE_BOOKINGS_SQL}{only_ids}"
            ))?;
            let mut rows = stmt.query(rusqlite::params_from_iter(&ids))?;
            while let Some(r) = rows.next()? {
                let (counterparty, purpose): (String, String) = (r.get(3)?, r.get(4)?);
                let m = MerchantResolver::resolve(&counterparty, &purpose);
                if m.key == merchant_key {
                    matches.push((r.get(0)?, r.get(1)?, r.get(2)?, m.name));
                }
            }
        }
        let tally = |rows: &mut dyn Iterator<Item = &(i64, String, i64, String)>| {
            let mut t = MerchantTally::default();
            for (id, date, amount, name) in rows {
                t.add(*id, date, *amount, name);
            }
            t.stats(merchant_key)
        };
        let all = tally(&mut matches.iter());
        let Some(account_id) = account_id else {
            return Ok((all, None));
        };
        // Own account = the booking list's account filter as its own query (with
        // its CTE and joins); only the merchant's bookings are looked up in it.
        let own_ids: HashSet<i64> = if matches.is_empty() {
            HashSet::new()
        } else {
            let filter = BookingFilter {
                account_ids: vec![account_id],
                ..Default::default()
            };
            let (cte, where_sql, args, _) = Vault::filter_clauses(&db, &filter)?;
            let sql = format!(
                "{cte}SELECT t.id FROM transactions t JOIN accounts a ON a.id = t.account_id \
                 LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL \
                 WHERE {where_sql}"
            );
            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
                |r| r.get::<_, i64>(0),
            )?;
            rows.collect::<Result<_, _>>()?
        };
        let own = tally(&mut matches.iter().filter(|m| own_ids.contains(&m.0)));
        Ok((all, Some(own)))
    }
}

fn normalize_logo_provider(raw: Option<&str>) -> &'static str {
    match raw.map(|v| v.trim().to_lowercase()).as_deref() {
        Some("duckduckgo") => "duckduckgo",
        Some("google") | Some("1") | Some("true") => "google",
        _ => "off",
    }
}

/// Pixel size of a PNG / ICO / GIF (None for other formats).
pub(crate) fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() > 24 && b.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some((
            u32::from_be_bytes([b[16], b[17], b[18], b[19]]),
            u32::from_be_bytes([b[20], b[21], b[22], b[23]]),
        ));
    }
    if b.len() > 6 && b.starts_with(&[0, 0, 1, 0]) {
        let count = u16::from_le_bytes([b[4], b[5]]) as usize;
        let mut best = 0u32;
        for i in 0..count.min(32) {
            let off = 6 + i * 16;
            if off + 2 > b.len() {
                break;
            }
            let w = if b[off] == 0 { 256 } else { b[off] as u32 };
            best = best.max(w);
        }
        return Some((best, best));
    }
    if b.len() > 10 && b.starts_with(b"GIF8") {
        return Some((
            u16::from_le_bytes([b[6], b[7]]) as u32,
            u16::from_le_bytes([b[8], b[9]]) as u32,
        ));
    }
    None
}

/// A real logo, not the tiny 16×16 default globe: dimensions > 16 px when
/// known, otherwise more than 400 bytes.
pub(crate) fn good_logo(b: &[u8]) -> bool {
    looks_like_image(b)
        && match image_size(b) {
            Some((w, h)) => w > 16 && h > 16,
            None => b.len() > 400,
        }
}

/// "https://www.EnBW.com/privat" -> "enbw.com"
pub fn normalise_domain(input: &str) -> String {
    let s = input.trim().to_lowercase();
    let s = s.split("://").last().unwrap_or(&s).to_string();
    let s = s.split(['/', '?', '#']).next().unwrap_or("").to_string();
    s.trim_start_matches("www.").trim_matches('.').to_string()
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MerchantLogo {
    pub domain: String,
    pub bytes: Vec<u8>,
}

pub(crate) const OVERRIDE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS merchant_domain_overrides (
    merchant_key TEXT PRIMARY KEY, domain TEXT NOT NULL, updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')));";

/// One-time cleanup (v1): deletes cached favicons whose domain is neither a
/// curated brand nor a manual override (e.g. "paula.de" fetched for the
/// private person "Laura Schneider" by the old guesser).
pub(crate) fn purge_untrusted_logos(conn: &rusqlite::Connection) -> rusqlite::Result<u32> {
    let done: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'maintenance.logo_purge_v1'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if done.is_some() {
        return Ok(0);
    }
    let mut keep: std::collections::HashSet<String> = crate::merchants::known_brand_domains()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    keep.extend(load_overrides(conn)?.into_values());
    let domains: Vec<String> = {
        let mut st = conn.prepare("SELECT domain FROM merchant_logo_cache")?;

        st.query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut n = 0;
    for d in domains {
        if !keep.contains(&d) {
            n += conn.execute("DELETE FROM merchant_logo_cache WHERE domain = ?1", [&d])? as u32;
        }
    }
    conn.execute("INSERT OR REPLACE INTO settings(key, value, is_secret) VALUES ('maintenance.logo_purge_v1', '1', 0)", [])?;
    Ok(n)
}

pub(crate) fn load_overrides(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<HashMap<String, String>> {
    let mut st = conn.prepare("SELECT merchant_key, domain FROM merchant_domain_overrides")?;

    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<HashMap<_, _>, _>>()
}

#[cfg(test)]
mod logo_tests {
    use super::*;

    fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
        let mut b = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.resize(len, 0);
        b
    }

    #[test]
    fn rejects_default_globe_and_normalises_domains() {
        assert!(!good_logo(&png(16, 16, 726)), "16x16 globe is rejected");
        assert!(good_logo(&png(128, 128, 2000)));
        assert_eq!(
            normalise_domain("https://www.EnBW.com/privat?x=1"),
            "enbw.com"
        );
        assert_eq!(normalise_domain(" vinzenzmurr.de "), "vinzenzmurr.de");
    }

    #[test]
    fn online_logos_are_off_until_enabled() {
        let p_dir = crate::test_support::TempDir::new("girocrab-logos");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
        assert!(!v.online_logos_enabled().unwrap());
        // Off means no request: an uncached domain comes back empty and is not cached.
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(rt.block_on(v.fetch_logo("lidl.de".into())).unwrap(), None);
        let cached: i64 = v
            .db()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM merchant_logo_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 0);
        v.set_setting("merchants.online_logos".into(), "1".into())
            .unwrap();
        assert!(v.online_logos_enabled().unwrap());
        assert_eq!(v.online_logo_provider().unwrap(), "google");
        v.set_setting("merchants.online_logos".into(), "duckduckgo".into())
            .unwrap();
        assert_eq!(v.online_logo_provider().unwrap(), "duckduckgo");
        v.set_setting("merchants.online_logos".into(), "0".into())
            .unwrap();
        assert_eq!(v.online_logo_provider().unwrap(), "off");
    }

    #[test]
    fn cached_logos_are_hidden_while_online_logos_are_off() {
        let p_dir = crate::test_support::TempDir::new("girocrab-logos-off");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
        let logo = png(128, 128, 2000);
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO merchant_logo_cache(domain, image_bytes, source, updated_at) VALUES ('rewe.de', ?1, 'google', strftime('%s','now'))",
                [&logo],
            )
            .unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Off (the default): neither the cache read nor the fetch hands out the cached logo (#422).
        assert_eq!(v.get_merchant_logo_png("rewe.de".into()).unwrap(), None);
        assert_eq!(rt.block_on(v.fetch_logo("rewe.de".into())).unwrap(), None);
        let none = rt
            .block_on(v.fetch_merchant_logo(vec!["rewe.de".into()]))
            .unwrap();
        assert!(none.is_none());
        // On: the cache answers again, without a request.
        v.set_setting("merchants.online_logos".into(), "duckduckgo".into())
            .unwrap();
        assert_eq!(
            v.get_merchant_logo_png("rewe.de".into()).unwrap(),
            Some(logo.clone())
        );
        assert_eq!(
            rt.block_on(v.fetch_logo("rewe.de".into())).unwrap(),
            Some(logo)
        );
    }

    #[test]
    fn merchant_stats_in_account_splits_the_total() {
        let p_dir = crate::test_support::TempDir::new("girocrab-mstats");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let key = all[0].merchant.key.clone();
        let total = v.merchant_stats(key.clone()).unwrap();
        let mut accounts: Vec<i64> = all
            .iter()
            .filter(|t| t.merchant.key == key)
            .map(|t| t.account_id)
            .collect();
        accounts.sort_unstable();
        accounts.dedup();
        let (mut count, mut sum) = (0, 0);
        for id in &accounts {
            let s = v.merchant_history(key.clone(), *id).unwrap().account;
            assert!(s.count >= 1 && s.count <= total.count);
            count += s.count;
            sum += s.total_cents;
        }
        assert_eq!((count, sum), (total.count, total.total_cents));
        // an account without bookings of the merchant: empty history
        let none = v.merchant_history(key, -1).unwrap().account;
        assert_eq!((none.count, none.total_cents), (0, 0));
    }

    #[test]
    fn merchant_history_matches_the_booking_list() {
        let p_dir = crate::test_support::TempDir::new("girocrab-mhist");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        type Expected = (String, u32, i64, String, String);
        // what `merchant_stats` computed before #591 from the enriched list
        // (newest first), per merchant key
        let expected = |account_ids: Vec<i64>| {
            let txs = v
                .list_transactions(BookingFilter {
                    account_ids,
                    ..Default::default()
                })
                .unwrap()
                .transactions;
            let mut by_key: HashMap<String, Expected> = HashMap::new();
            for t in &txs {
                let e = by_key.entry(t.merchant.key.clone()).or_insert_with(|| {
                    let newest = t.booking_date.clone();
                    (t.merchant.name.clone(), 0, 0, String::new(), newest)
                });
                e.1 += 1;
                e.2 += t.amount_cents;
                e.3 = t.booking_date.clone();
            }
            by_key
        };
        let got = |s: &MerchantStats| -> Expected {
            assert_eq!(
                s.average_cents,
                if s.count > 0 {
                    s.total_cents / s.count as i64
                } else {
                    0
                }
            );
            (
                s.name.clone(),
                s.count,
                s.total_cents,
                s.first_date.clone(),
                s.last_date.clone(),
            )
        };
        let empty = |key: &str| -> Expected {
            let none = v.merchant_history(key.to_string(), -1).unwrap().account;
            got(&none)
        };
        let total = expected(Vec::new());
        // every account, loans included (their history pulls in linked bookings)
        let accounts: Vec<i64> = v.list_accounts().unwrap().iter().map(|a| a.id).collect();
        assert!(total.len() > 5 && accounts.len() > 1);
        // the demo loan's view holds bookings of other accounts
        assert!(accounts.iter().any(|id| {
            let filter = BookingFilter {
                account_ids: vec![*id],
                ..Default::default()
            };
            let txs = v.list_transactions(filter).unwrap().transactions;
            txs.iter().any(|t| t.account_id != *id)
        }));
        for (key, want) in &total {
            assert_eq!(&got(&v.merchant_stats(key.clone()).unwrap()), want, "{key}");
        }
        for id in &accounts {
            let own = expected(vec![*id]);
            for (key, want) in &total {
                let h = v.merchant_history(key.clone(), *id).unwrap();
                assert_eq!(&got(&h.all), want, "{key}");
                let want_own = own.get(key).cloned().unwrap_or_else(|| empty(key));
                assert_eq!(got(&h.account), want_own, "{key} in {id}");
            }
        }
        // after a write the booking classes are gone: every booking is resolved again
        v.db()
            .unwrap()
            .execute("UPDATE transactions SET note = note", [])
            .unwrap();
        for (key, want) in &total {
            assert!(v.cached_booking_classes(&v.db().unwrap()).is_none());
            assert_eq!(&got(&v.merchant_stats(key.clone()).unwrap()), want, "{key}");
        }
    }

    #[test]
    fn domain_override_applies_to_bookings() {
        let p_dir = crate::test_support::TempDir::new("girocrab-ovr");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let page = v
            .list_transactions(BookingFilter {
                search: Some("Konditorei König".into()),
                ..Default::default()
            })
            .unwrap();
        let key = page.transactions[0].merchant.key.clone();
        assert_eq!(
            v.set_merchant_domain(key.clone(), Some("https://www.baeckerei-koenig.de/".into()))
                .unwrap()
                .as_deref(),
            Some("baeckerei-koenig.de")
        );
        let page = v
            .list_transactions(BookingFilter {
                search: Some("Konditorei König".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            page.transactions
                .iter()
                .all(
                    |t| t.merchant_domain.as_deref() == Some("baeckerei-koenig.de")
                        && t.merchant.domain_source == "override"
                )
        );
        let enbw = v
            .list_transactions(BookingFilter {
                search: Some("EnBW".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            enbw.total_count > 0
                && enbw
                    .transactions
                    .iter()
                    .any(|t| t.merchant_domain.as_deref() == Some("enbw.com"))
        );
        let _ = std::fs::remove_file(p);
    }
}

fn looks_like_image(b: &[u8]) -> bool {
    b.len() > 64
        && b.len() < 512 * 1024
        && (b.starts_with(&[0x89, b'P', b'N', b'G'])
            || b.starts_with(&[0, 0, 1, 0])
            || b.starts_with(&[0xFF, 0xD8])
            || b.starts_with(b"GIF8")
            || (b.len() > 12 && &b[8..12] == b"WEBP"))
}

#[uniffi::export(async_runtime = "tokio")]
impl Vault {
    /// Tier 2 (opt-in): None while "Logos aus dem Internet laden" is off, even for
    /// a cached favicon (#422). Otherwise returns the cached favicon or fetches it
    /// once from the selected provider's (Google or DuckDuckGo) favicon service and
    /// caches it in the encrypted vault. Only the domain is sent. Failures are
    /// cached for 7 days.
    pub async fn fetch_logo(&self, domain: String) -> Result<Option<Vec<u8>>, CoreError> {
        let domain = domain.trim().to_lowercase();
        if domain.is_empty()
            || !domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Ok(None);
        }
        let provider: &str;
        {
            let db = self.db()?;
            // Checked before the cache: "Aus" also hides favicons cached earlier (#422).
            let raw = Vault::setting_inner(&db, "merchants.online_logos")?;
            provider = normalize_logo_provider(raw.as_deref());
            if provider == "off" {
                return Ok(None);
            }
            let row: Option<(Option<Vec<u8>>, i64)> = db
                .query_row(
                    "SELECT image_bytes, updated_at FROM merchant_logo_cache WHERE domain = ?1",
                    [&domain],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((bytes, fetched)) = row {
                let fresh_negative = jiff::Timestamp::now().as_second() - fetched < 7 * 86400;
                match bytes {
                    Some(b) if good_logo(&b) => return Ok(Some(b)),
                    _ if fresh_negative => return Ok(None),
                    _ => {}
                }
            }
        }
        let client =
            crate::connectors::client_builder(std::time::Duration::from_secs(8)).build()?;
        // Only the provider the user picked is contacted – no silent fallback.
        let sources = [if provider == "duckduckgo" {
            (
                "duckduckgo",
                format!("https://icons.duckduckgo.com/ip3/{domain}.ico"),
            )
        } else {
            (
                "google",
                format!("https://www.google.com/s2/favicons?domain={domain}&sz=128"),
            )
        }];
        // Favicons are a few KB; a larger body counts as no logo.
        const MAX_LOGO_BYTES: usize = 1024 * 1024;
        let mut found: Option<(Vec<u8>, &str)> = None;
        for (name, url) in sources.iter() {
            if let Ok(resp) = client.get(url).send().await
                && resp.status().is_success()
                && let Ok(bytes) = read_body_limited(resp, MAX_LOGO_BYTES).await
                && good_logo(&bytes)
            {
                found = Some((bytes, name));
                break;
            }
        }
        let db = self.db()?;
        db.execute(
            "INSERT INTO merchant_logo_cache(domain, image_bytes, source, updated_at) VALUES (?1, ?2, ?3, strftime('%s','now'))
             ON CONFLICT(domain) DO UPDATE SET image_bytes = excluded.image_bytes, source = excluded.source, updated_at = excluded.updated_at",
            params![domain, found.as_ref().map(|f| f.0.clone()), found.as_ref().map(|f| f.1).unwrap_or("")],
        )?;
        Ok(found.map(|f| f.0))
    }

    /// Tries every candidate domain in order ("enbw.de" -> "enbw.com") via
    /// `fetch_logo`: cache, then the selected provider; the tiny default globe
    /// is rejected so the next candidate is tried. Returns the first real logo
    /// and its domain; None while online logos are off.
    pub async fn fetch_merchant_logo(
        &self,
        candidates: Vec<String>,
    ) -> Result<Option<MerchantLogo>, CoreError> {
        for c in candidates {
            let d = normalise_domain(&c);
            if d.is_empty() {
                continue;
            }
            if let Some(bytes) = self.fetch_logo(d.clone()).await? {
                return Ok(Some(MerchantLogo { domain: d, bytes }));
            }
        }
        Ok(None)
    }
}

/// What a plan invests per month: the sum of the executions of its latest month, so two executions of
/// 50 € a month are 100 €, not 50 €. A running month that has fewer executions than the one before
/// (the plan has not run again yet) is not taken: its predecessor is.
fn monthly_rate(executions: &[&SecurityTransaction], today: Date) -> (i64, Vec<u32>) {
    let mut per_month: BTreeMap<String, (i64, Vec<u32>)> = BTreeMap::new();
    for t in executions {
        let e = per_month.entry(month_key(&t.booking_date)).or_default();
        e.0 += t.value_cents.abs();
        if let Ok(d) = Date::strptime("%Y-%m-%d", &t.booking_date) {
            e.1.push(d.day() as u32);
        }
    }
    for (_, days) in per_month.values_mut() {
        days.sort_unstable();
        days.dedup();
    }
    let mut newest = per_month.iter().rev();
    let Some((latest_month, (latest_sum, latest_days))) = newest.next() else {
        return (0, Vec::new());
    };
    if let Some((_, (previous_sum, previous_days))) = newest.next()
        && *latest_month == today.strftime("%Y-%m").to_string()
        && latest_days.len() < previous_days.len()
    {
        return (*previous_sum, previous_days.clone());
    }
    (*latest_sum, latest_days.clone())
}

/// Last day of a `YYYY-MM` month.
fn last_day_of_month(month: &str) -> Option<Date> {
    let first = Date::strptime("%Y-%m-%d", format!("{month}-01")).ok()?;
    Some(first.last_of_month())
}

/// The text of a savings plan execution: "Sparplan ausgeführt" or the bank's event code
/// (`savings_plan_execution`, "Savings plan").
fn is_savings_plan_text(text: &str) -> bool {
    let t = text.to_lowercase();
    ["sparplan", "savings_plan", "savings plan", "savings-plan"]
        .iter()
        .any(|k| t.contains(k))
}

/// Savings plans from the securities activity of a depot: buys whose text names a savings plan,
/// one plan per security (ISIN, else name). The rate is the latest execution, a plan is active when
/// it ran within the last 40 days. Largest rate first, like the booking based grid. Only executions in
/// `currency` (the depot's; a transaction without one counts as EUR) are summed: nothing is converted.
fn plans_from_security_transactions(
    transactions: &[SecurityTransaction],
    months_list: &[String],
    today: Date,
    currency: &str,
) -> Vec<SavingsPlan> {
    let first_month = months_list.first().cloned().unwrap_or_default();
    let last_month = months_list.last().cloned().unwrap_or_default();
    // a page that ends before the current month is worked out as of its last day ("active")
    let as_of = last_day_of_month(&last_month).map_or(today, |end| end.min(today));
    let mut by_security: HashMap<String, Vec<&SecurityTransaction>> = HashMap::new();
    for t in transactions {
        if month_key(&t.booking_date).as_str() > last_month.as_str() {
            continue;
        }
        let own_currency = if t.currency.is_empty() {
            "EUR"
        } else {
            t.currency.as_str()
        };
        if t.direction == SecurityDirection::Buy
            && t.value_cents != 0
            && own_currency.eq_ignore_ascii_case(currency)
            && is_savings_plan_text(&t.booking_text)
        {
            let key = if t.isin.is_empty() {
                t.name.to_lowercase()
            } else {
                t.isin.clone()
            };
            by_security.entry(key).or_default().push(t);
        }
    }
    let mut plans: Vec<SavingsPlan> = by_security
        .into_iter()
        .filter(|(_, v)| {
            v.iter()
                .any(|t| t.booking_date.as_str() >= first_month.as_str())
        })
        .map(|(key, v)| {
            let latest = v.iter().max_by_key(|t| t.booking_date.clone()).unwrap();
            let mut merchant = MerchantResolver::resolve(&latest.name, "Sparplan");
            merchant.key = format!("security:{key}");
            let mut per_month: HashMap<String, i64> = HashMap::new();
            for t in &v {
                *per_month.entry(month_key(&t.booking_date)).or_default() += t.value_cents.abs();
            }
            let last = Date::strptime("%Y-%m-%d", &latest.booking_date).ok();
            let (monthly_cents, run_days) = monthly_rate(&v, today);
            SavingsPlan {
                merchant,
                monthly_cents,
                day_of_month: last.map_or(1, |d| d.day() as u32),
                last_date: latest.booking_date.clone(),
                active: last
                    .map(|d| crate::dates::days_between(d, as_of) <= 40)
                    .unwrap_or(false),
                total_invested_cents: v.iter().map(|t| t.value_cents.abs()).sum(),
                executions: months_list
                    .iter()
                    .map(|mk| PlanExecution {
                        month: mk.clone(),
                        amount_cents: per_month.get(mk).copied().unwrap_or(0),
                    })
                    .collect(),
                run_days,
            }
        })
        .collect();
    plans.sort_by(|a, b| {
        b.monthly_cents
            .cmp(&a.monthly_cents)
            .then(a.merchant.name.cmp(&b.merchant.name))
    });
    plans
}

#[cfg(test)]
mod depot_plan_tests {
    use super::*;

    fn buy(date: &str, isin: &str, name: &str, cents: i64, text: &str) -> SecurityTransaction {
        SecurityTransaction {
            booking_date: date.into(),
            isin: isin.into(),
            name: name.into(),
            direction: SecurityDirection::Buy,
            value_cents: cents,
            booking_text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn plans_come_from_savings_plan_buys_grouped_by_security() {
        let months: Vec<String> = ["2026-09", "2026-10", "2026-11"]
            .iter()
            .map(|m| m.to_string())
            .collect();
        let today = jiff::civil::date(2026, 11, 17);
        let tx = vec![
            buy("2026-09-02", "IE1", "Alpha", 5000, "Sparplan ausgeführt"),
            buy("2026-11-02", "IE1", "Alpha", 7500, "Sparplan ausgeführt"),
            buy("2026-10-16", "IE2", "Beta", 10000, "Sparplan ausgeführt"),
            // a one-off order, a dividend and a sell are no plan
            buy("2026-10-05", "IE3", "Gamma", 20000, "Kauf"),
            SecurityTransaction {
                direction: SecurityDirection::Dividend,
                ..buy("2026-10-06", "IE1", "Alpha", 300, "Sparplan")
            },
            // a plan that stopped long before the grid
            buy("2025-01-02", "IE4", "Delta", 2500, "Sparplan ausgeführt"),
        ];
        let plans = plans_from_security_transactions(&tx, &months, today, "EUR");
        assert_eq!(plans.len(), 2);
        // largest rate first: Beta 100 €, then Alpha with its latest 75 €
        assert_eq!(plans[0].merchant.name, "Beta");
        assert_eq!(plans[0].monthly_cents, 10000);
        assert_eq!(plans[1].merchant.name, "Alpha");
        assert_eq!(plans[1].monthly_cents, 7500);
        assert_eq!(plans[1].total_invested_cents, 12500);
        assert!(plans[1].active);
        assert_eq!(
            plans[1]
                .executions
                .iter()
                .map(|e| e.amount_cents)
                .collect::<Vec<_>>(),
            vec![5000, 0, 7500]
        );
    }

    #[test]
    fn english_event_texts_count_and_other_currencies_do_not() {
        let months: Vec<String> = ["2026-10", "2026-11"]
            .iter()
            .map(|m| m.to_string())
            .collect();
        let today = jiff::civil::date(2026, 11, 17);
        let usd = |mut t: SecurityTransaction| {
            t.currency = "USD".into();
            t
        };
        let tx = vec![
            buy("2026-11-02", "IE1", "Alpha", 5000, "savings_plan_execution"),
            buy("2026-10-02", "IE1", "Alpha", 5000, "Savings plan"),
            // the same security bought in dollars: not mixed into the euro plan
            usd(buy(
                "2026-11-03",
                "IE1",
                "Alpha",
                9900,
                "Sparplan ausgeführt",
            )),
            // only dollars: no plan of a euro depot
            usd(buy(
                "2026-11-04",
                "US1",
                "Beta",
                4000,
                "Sparplan ausgeführt",
            )),
        ];
        let plans = plans_from_security_transactions(&tx, &months, today, "EUR");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].merchant.name, "Alpha");
        assert_eq!(plans[0].monthly_cents, 5000);
        assert_eq!(plans[0].total_invested_cents, 10000);
        // a dollar depot shows its own executions only
        let usd_plans = plans_from_security_transactions(&tx, &months, today, "USD");
        assert_eq!(usd_plans.len(), 2);
        assert!(usd_plans.iter().all(|p| p.total_invested_cents <= 9900));
    }

    #[test]
    fn the_rate_is_what_a_month_invests_not_one_execution() {
        let months: Vec<String> = ["2026-09", "2026-10", "2026-11"]
            .iter()
            .map(|m| m.to_string())
            .collect();
        let today = jiff::civil::date(2026, 11, 17);
        let text = "Sparplan ausgeführt";
        let tx = vec![
            // twice a month, 50 € each: 100 € a month
            buy("2026-10-01", "IE1", "Alpha", 5000, text),
            buy("2026-10-15", "IE1", "Alpha", 5000, text),
            buy("2026-11-01", "IE1", "Alpha", 5000, text),
            buy("2026-11-15", "IE1", "Alpha", 5000, text),
            // the running month has only its first execution yet: the month before counts
            buy("2026-10-03", "IE2", "Beta", 2000, text),
            buy("2026-10-17", "IE2", "Beta", 2000, text),
            buy("2026-11-03", "IE2", "Beta", 2000, text),
            // a plan that ran once in a month and stays at that
            buy("2026-10-10", "IE3", "Gamma", 7000, text),
            buy("2026-11-10", "IE3", "Gamma", 7000, text),
        ];
        let plans = plans_from_security_transactions(&tx, &months, today, "EUR");
        let rate = |name: &str| {
            plans
                .iter()
                .find(|p| p.merchant.name == name)
                .unwrap()
                .monthly_cents
        };
        assert_eq!(rate("Alpha"), 10000);
        assert_eq!(rate("Beta"), 4000);
        assert_eq!(rate("Gamma"), 7000);
        let days = |name: &str| {
            plans
                .iter()
                .find(|p| p.merchant.name == name)
                .unwrap()
                .run_days
                .clone()
        };
        // the days of the month the rate comes from (Beta: October, its running November has one run)
        assert_eq!(days("Alpha"), vec![1, 15]);
        assert_eq!(days("Beta"), vec![3, 17]);
        assert_eq!(days("Gamma"), vec![10]);
        // the same running month, one execution fewer is no gap once the month is over
        let december = jiff::civil::date(2026, 12, 2);
        let later = plans_from_security_transactions(&tx, &months, december, "EUR");
        assert_eq!(
            later
                .iter()
                .find(|p| p.merchant.name == "Beta")
                .unwrap()
                .monthly_cents,
            2000
        );
    }

    #[test]
    fn an_earlier_page_is_worked_out_as_of_its_last_month() {
        // pages of three months, the current month last
        let now = last_months_ending(3, 0);
        let before = last_months_ending(3, 3);
        assert_eq!(now.len(), 3);
        assert_eq!(before.len(), 3);
        assert!(before.last().unwrap() < now.first().unwrap());
        // over a year boundary and a whole year back
        assert_eq!(
            last_months_ending(2, 12).last().unwrap(),
            &last_months_ending(1, 12)[0]
        );
        let text = "Sparplan ausgeführt";
        let months = ["2026-03", "2026-04", "2026-05"].map(String::from).to_vec();
        let today = jiff::civil::date(2026, 11, 17);
        let tx = vec![
            buy("2026-04-10", "IE1", "Alpha", 5000, text),
            buy("2026-05-10", "IE1", "Alpha", 5000, text),
            // later executions do not belong to this page
            buy("2026-09-10", "IE1", "Alpha", 9000, text),
            // a plan that began after the page
            buy("2026-10-10", "IE2", "Beta", 3000, text),
        ];
        let plans = plans_from_security_transactions(&tx, &months, today, "EUR");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].merchant.name, "Alpha");
        assert_eq!(plans[0].monthly_cents, 5000);
        assert_eq!(plans[0].total_invested_cents, 10000);
        // ran in May: on 31 May it was active, though today it would not be
        assert!(plans[0].active);
        assert_eq!(last_day_of_month("2026-02"), crate::dates::ymd(2026, 2, 28));
        assert_eq!(
            last_day_of_month("2026-12"),
            crate::dates::ymd(2026, 12, 31)
        );
    }
}
