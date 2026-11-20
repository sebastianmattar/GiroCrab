//! Depot, Sachwerte and Vermögen view-models without Slint: KPI tiles, composition and the
//! position / activity tables. A figure the core does not deliver is `None` and shows as
//! "Nicht verfügbar"; it is never replaced by 0. Pure functions, unit-tested on every platform.

use std::collections::BTreeMap;

use girocrab_core::asset_classification::AssetClass;
use girocrab_core::{Account, AccountKind, Holding, SecurityDirection, SecurityTransaction};

pub const UNAVAILABLE: &str = "Nicht verfügbar";

/// A KPI tile: `value` is the finished text, `negative` colours it.
#[derive(Debug, Clone, PartialEq)]
pub struct Kpi {
    pub label: String,
    pub value: String,
    pub negative: bool,
}

/// One bar of the composition: `share` 0.0..=1.0 of the positive total.
#[derive(Debug, Clone, PartialEq)]
pub struct Share {
    pub label: String,
    pub value: String,
    pub share: f32,
    pub color_hex: String,
}

/// A table row: up to six cells plus an id the view can act on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cells {
    pub id: i64,
    pub cells: [String; 6],
    pub negative: bool,
}

/// Period of the header selector. It limits distributions, fees/taxes and the activity table;
/// the position value is always today's snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    All,
    Year,
    ThisYear,
    Quarter,
    Month,
}

impl Period {
    pub const LABELS: [&'static str; 5] = [
        "Gesamt",
        "12 Monate",
        "Seit Jahresbeginn",
        "3 Monate",
        "1 Monat",
    ];

    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Year,
            2 => Self::ThisYear,
            3 => Self::Quarter,
            4 => Self::Month,
            _ => Self::All,
        }
    }

    /// First day (ISO) of the period ending `today`; `None` = no lower bound.
    pub fn start(self, today: &str) -> Option<String> {
        let (y, m, d) = parse_date(today)?;
        match self {
            Self::All => None,
            Self::Year => Some(fmt_date(y - 1, m, d.min(days_in_month(y - 1, m)))),
            Self::ThisYear => Some(fmt_date(y, 1, 1)),
            Self::Quarter => Some(shift_months(y, m, d, 3)),
            Self::Month => Some(shift_months(y, m, d, 1)),
        }
    }
}

fn parse_date(iso: &str) -> Option<(i32, u32, u32)> {
    let mut p = iso.get(..10)?.split('-');
    let (y, m, d) = (p.next()?, p.next()?, p.next()?);
    let (y, m, d): (i32, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

pub(crate) fn fmt_date(y: i32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn shift_months(y: i32, m: u32, d: u32, back: u32) -> String {
    let total = y * 12 + m as i32 - 1 - back as i32;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    fmt_date(ny, nm, d.min(days_in_month(ny, nm)))
}

/// Today's date as ISO text (UTC), from the system clock.
pub fn today_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400) as i32 + i32::from(m <= 2);
    fmt_date(y, m, d)
}

pub fn class_label(c: AssetClass) -> &'static str {
    match c {
        AssetClass::Equity => "Aktien",
        AssetClass::Bond => "Anleihen",
        AssetClass::RealEstate => "Immobilien",
        AssetClass::Commodity => "Rohstoffe",
        AssetClass::Crypto => "Krypto",
        AssetClass::Cash => "Liquidität",
        AssetClass::MultiAsset => "Mischfonds",
        AssetClass::Other => "Sonstiges",
        AssetClass::Unclassified => "Nicht klassifiziert",
    }
}

fn class_color(c: AssetClass) -> &'static str {
    match c {
        AssetClass::Equity => "#3B82F6",
        AssetClass::Bond => "#10B981",
        AssetClass::RealEstate => "#F59E0B",
        AssetClass::Commodity => "#EAB308",
        AssetClass::Crypto => "#8B5CF6",
        AssetClass::Cash => "#14B8A6",
        AssetClass::MultiAsset => "#EC4899",
        AssetClass::Other => "#64748B",
        AssetClass::Unclassified => "#94A3B8",
    }
}

/// Bars of `(label, cents, colour)` sorted by size; only positive values take a share.
pub fn composition(parts: Vec<(String, i64, String)>, money: &dyn Fn(i64) -> String) -> Vec<Share> {
    let total: i64 = parts.iter().map(|p| p.1.max(0)).sum();
    let mut parts: Vec<_> = parts.into_iter().filter(|p| p.1 > 0).collect();
    parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    parts
        .into_iter()
        .map(|(label, cents, color_hex)| Share {
            label,
            value: money(cents),
            share: if total > 0 {
                cents as f32 / total as f32
            } else {
                0.0
            },
            color_hex,
        })
        .collect()
}

fn signed(cents: i64, money: &dyn Fn(i64) -> String) -> (String, bool) {
    let text = money(cents);
    (if cents > 0 { format!("+{text}") } else { text }, cents < 0)
}

fn kpi(label: &str, cents: Option<i64>, with_sign: bool, money: &dyn Fn(i64) -> String) -> Kpi {
    match cents {
        Some(c) if with_sign => {
            let (value, negative) = signed(c, money);
            Kpi {
                label: label.into(),
                value,
                negative,
            }
        }
        Some(c) => Kpi {
            label: label.into(),
            value: money(c),
            negative: false,
        },
        None => Kpi {
            label: label.into(),
            value: UNAVAILABLE.into(),
            negative: false,
        },
    }
}

/// Unrealised profit/loss: the bank's figure per position, else value minus cost where a
/// purchase price is known. `None` as soon as one position has neither.
pub fn unrealized_pl(holdings: &[Holding]) -> Option<i64> {
    if holdings.is_empty() {
        return None;
    }
    holdings.iter().try_fold(0i64, |sum, h| {
        let pl = h.profit_loss_purchase_cents.or_else(|| {
            (h.purchase_price_cents > 0).then(|| {
                h.value_cents - (h.purchase_price_cents as f64 * h.quantity).round() as i64
            })
        })?;
        Some(sum + pl)
    })
}

/// Change against the previous day; `None` unless every position reports it.
pub fn day_change(holdings: &[Holding]) -> Option<i64> {
    if holdings.is_empty() {
        return None;
    }
    holdings.iter().try_fold(0i64, |sum, h| {
        let d = h
            .profit_loss_prev_day_cents
            .or_else(|| h.prev_day_value_cents.map(|p| h.value_cents - p))?;
        Some(sum + d)
    })
}

fn in_period(date: &str, start: &Option<String>) -> bool {
    match start {
        Some(s) => date.get(..10).is_some_and(|d| d >= s.as_str()),
        None => true,
    }
}

/// Sum of dividend payouts in the period; `None` without any securities transaction (no data).
pub fn distributions(txs: &[SecurityTransaction], start: &Option<String>) -> Option<i64> {
    if txs.is_empty() {
        return None;
    }
    Some(
        txs.iter()
            .filter(|t| t.direction == SecurityDirection::Dividend)
            .filter(|t| in_period(&t.booking_date, start))
            .map(|t| t.value_cents.abs())
            .sum(),
    )
}

/// Fees plus taxes in the period (positive = paid); `None` without any transaction.
pub fn fees_and_taxes(txs: &[SecurityTransaction], start: &Option<String>) -> Option<i64> {
    if txs.is_empty() {
        return None;
    }
    Some(
        txs.iter()
            .filter(|t| in_period(&t.booking_date, start))
            .map(|t| {
                let fee = match t.direction {
                    SecurityDirection::Fee if t.fees_cents == 0 => t.value_cents.abs(),
                    _ => t.fees_cents.abs(),
                };
                let tax = match t.direction {
                    SecurityDirection::Tax if t.taxes_cents == 0 => t.value_cents.abs(),
                    _ => t.taxes_cents.abs(),
                };
                fee + tax
            })
            .sum(),
    )
}

pub fn quantity_text(q: f64) -> String {
    if (q - q.round()).abs() < 1e-9 {
        format!("{}", q.round() as i64)
    } else {
        let t = format!("{q:.4}");
        t.trim_end_matches('0')
            .trim_end_matches('.')
            .replace('.', ",")
    }
}

impl Cells {
    /// Replaces the personal cells (`cols`) by the mask in privacy mode; amounts and dates are
    /// masked or kept by the caller.
    pub fn mask(&mut self, cols: &[usize], privacy: bool) {
        if privacy {
            for &c in cols {
                if let Some(cell) = self.cells.get_mut(c) {
                    *cell = crate::daily::mask(cell, true);
                }
            }
        }
    }
}

pub const POSITION_COLUMNS: [&str; 5] = ["Position", "Menge", "Kurs", "Wert", "G/V"];
pub const ACTIVITY_COLUMNS: [&str; 5] = ["Datum", "Art", "Wertpapier", "Menge", "Betrag"];

pub fn direction_label(d: SecurityDirection) -> &'static str {
    match d {
        SecurityDirection::Buy => "Kauf",
        SecurityDirection::Sell => "Verkauf",
        SecurityDirection::Dividend => "Ausschüttung",
        SecurityDirection::Fee => "Gebühr",
        SecurityDirection::Tax => "Steuer",
        SecurityDirection::Other => "Sonstiges",
    }
}

/// A position as a table row (value and G/V respect privacy through the caller's `money`).
pub fn position_row(
    id: i64,
    h: &Holding,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Cells {
    let _ = date;
    let pl = unrealized_pl(std::slice::from_ref(h));
    let (mut pl_text, neg) = match pl {
        Some(c) => signed(c, money),
        None => ("–".into(), false),
    };
    if let Some(rel) = h.profit_loss_purchase_rel {
        pl_text = format!("{pl_text} ({rel:+.1} %)").replace('.', ",");
    }
    // Native holding currency (#625): shown when it differs from the account currency.
    let native = |cents: Option<i64>, fallback: i64| -> String {
        match (cents, h.currency.is_empty()) {
            (Some(c), false) => format!("{} {}", native_amount(c), h.currency),
            _ => money(fallback),
        }
    };
    let mut price = native(h.native_price_cents, h.current_price_cents);
    if h.purchase_price_cents != 0 || h.native_purchase_price_cents.is_some() {
        let buy = native(h.native_purchase_price_cents, h.purchase_price_cents);
        price = format!("{price} (Kauf {buy})");
    }
    let ids = [h.isin.as_str(), h.wkn.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" / ");
    let name = if ids.is_empty() {
        h.name.clone()
    } else {
        format!("{} · {ids}", h.name)
    };
    Cells {
        id,
        cells: [
            name,
            quantity_text(h.quantity),
            price,
            money(h.value_cents),
            pl_text,
            String::new(),
        ],
        negative: neg,
    }
}

/// Plain amount with German separators, without a currency symbol.
fn native_amount(cents: i64) -> String {
    let abs = cents.unsigned_abs();
    let mut int = (abs / 100).to_string();
    let mut grouped = String::new();
    while int.len() > 3 {
        let tail = int.split_off(int.len() - 3);
        grouped = format!(".{tail}{grouped}");
    }
    format!(
        "{}{int}{grouped},{:02}",
        if cents < 0 { "-" } else { "" },
        abs % 100
    )
}

pub fn activity_row(
    t: &SecurityTransaction,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Cells {
    let amount = match t.direction {
        SecurityDirection::Buy => -t.value_cents.abs(),
        SecurityDirection::Fee | SecurityDirection::Tax => -t.value_cents.abs(),
        _ => t.value_cents.abs(),
    };
    Cells {
        id: t.id,
        cells: [
            date(&t.booking_date),
            direction_label(t.direction).into(),
            t.name.clone(),
            if t.quantity == 0.0 {
                String::new()
            } else {
                quantity_text(t.quantity)
            },
            money(amount),
            String::new(),
        ],
        negative: amount < 0,
    }
}

/// Which table the depot view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DepotTab {
    #[default]
    Positions,
    Activities,
}

impl DepotTab {
    pub fn from_index(i: i32) -> Self {
        if i == 1 {
            Self::Activities
        } else {
            Self::Positions
        }
    }
}

/// Everything the depot / Sachwerte view shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DepotView {
    pub title: String,
    pub total: String,
    pub note: String,
    pub kpis: Vec<Kpi>,
    pub composition: Vec<Share>,
    pub positions: Vec<Cells>,
    pub activities: Vec<Cells>,
}

/// Input of the depot view, loaded off the UI thread.
pub struct DepotData {
    pub account: Account,
    pub holdings: Vec<(Holding, AssetClass)>,
    pub transactions: Vec<SecurityTransaction>,
}

pub fn depot_view(
    data: &DepotData,
    period: Period,
    today: &str,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> DepotView {
    let start = period.start(today);
    let holdings: Vec<Holding> = data.holdings.iter().map(|h| h.0.clone()).collect();
    let total: i64 = holdings.iter().map(|h| h.value_cents).sum();
    let has_positions = !holdings.is_empty();
    let tangible = data.account.kind != AccountKind::Brokerage;

    let mut by_class: BTreeMap<&'static str, (i64, AssetClass)> = BTreeMap::new();
    for (h, c) in &data.holdings {
        by_class.entry(class_label(*c)).or_insert((0, *c)).0 += h.value_cents;
    }
    let comp = composition(
        by_class
            .into_iter()
            .map(|(l, (v, c))| (l.to_string(), v, class_color(c).to_string()))
            .collect(),
        money,
    );

    let mut positions: Vec<&(Holding, AssetClass)> = data.holdings.iter().collect();
    positions.sort_by_key(|a| std::cmp::Reverse(a.0.value_cents));
    let positions = positions
        .iter()
        .enumerate()
        .map(|(i, h)| position_row(i as i64, &h.0, money, date))
        .collect();

    let mut txs: Vec<&SecurityTransaction> = data
        .transactions
        .iter()
        .filter(|t| in_period(&t.booking_date, &start))
        .collect();
    txs.sort_by(|a, b| b.booking_date.cmp(&a.booking_date).then(b.id.cmp(&a.id)));
    let activities = txs.iter().map(|t| activity_row(t, money, date)).collect();

    DepotView {
        title: data.account.name.clone(),
        total: if has_positions {
            money(total)
        } else {
            money(data.account.balance_cents)
        },
        note: if tangible {
            "Zeitraum: gilt für Aktivitäten, soweit vorhanden.".into()
        } else {
            "Zeitraum: gilt für Ausschüttungen, Gebühren/Steuern und Aktivitäten; der Wert ist der aktuelle Stand."
                .into()
        },
        kpis: vec![
            kpi(
                "Nicht realisierter G/V",
                unrealized_pl(&holdings),
                true,
                money,
            ),
            kpi("Tagesveränderung", day_change(&holdings), true, money),
            kpi(
                "Ausschüttungen",
                distributions(&data.transactions, &start),
                false,
                money,
            ),
            kpi(
                "Gebühren & Steuern",
                fees_and_taxes(&data.transactions, &start),
                false,
                money,
            ),
        ],
        composition: comp,
        positions,
        activities,
    }
}

// ------------------------------------------------------------------------------ Vermögen

/// Tabs of the Vermögen view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WealthTab {
    #[default]
    All,
    Cash,
    Securities,
    Tangibles,
}

impl WealthTab {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Cash,
            2 => Self::Securities,
            3 => Self::Tangibles,
            _ => Self::All,
        }
    }
}

/// A position of a depot or a Sachwerte account.
#[derive(Debug, Clone)]
pub struct WealthPosition {
    pub holding: Holding,
    pub tangible: bool,
}

/// Input of the Vermögen view.
pub struct WealthData {
    pub accounts: Vec<Account>,
    pub tangible_account_ids: Vec<i64>,
    /// Positions of depots and Sachwerte accounts: (account id, position).
    pub positions: Vec<(i64, WealthPosition)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WealthView {
    pub net_worth: String,
    pub note: String,
    pub composition: Vec<Share>,
    pub columns: Vec<String>,
    pub rows: Vec<Cells>,
}

fn is_tangible(data: &WealthData, a: &Account) -> bool {
    data.tangible_account_ids.contains(&a.id)
}

/// Cash accounts: everything that is neither a depot, a Sachwerte account nor a loan.
fn is_cash(data: &WealthData, a: &Account) -> bool {
    a.kind != AccountKind::Brokerage && a.kind != AccountKind::Loan && !is_tangible(data, a)
}

pub fn wealth_view(data: &WealthData, tab: WealthTab, money: &dyn Fn(i64) -> String) -> WealthView {
    // Only EUR accounts that count towards the total; foreign currencies are not converted.
    let counted: Vec<&Account> = data
        .accounts
        .iter()
        .filter(|a| a.in_total && a.currency == "EUR" && a.archived_unix.is_none())
        .collect();
    let skipped = data
        .accounts
        .iter()
        .filter(|a| a.in_total && a.currency != "EUR" && a.archived_unix.is_none())
        .count();

    let value_of = |a: &Account| -> i64 {
        let held: Vec<&WealthPosition> = data
            .positions
            .iter()
            .filter(|p| p.0 == a.id)
            .map(|p| &p.1)
            .collect();
        if held.is_empty() {
            a.balance_cents
        } else {
            held.iter().map(|p| p.holding.value_cents).sum()
        }
    };

    let cash: i64 = counted
        .iter()
        .filter(|a| is_cash(data, a))
        .map(|a| a.balance_cents)
        .sum();
    let securities: i64 = counted
        .iter()
        .filter(|a| a.kind == AccountKind::Brokerage && !is_tangible(data, a))
        .map(|a| value_of(a))
        .sum();
    let tangibles: i64 = counted
        .iter()
        .filter(|a| is_tangible(data, a))
        .map(|a| value_of(a))
        .sum();
    let debts: i64 = counted
        .iter()
        .filter(|a| a.kind == AccountKind::Loan)
        .map(|a| -a.balance_cents.abs())
        .sum();
    let net = cash + securities + tangibles + debts;

    let comp = composition(
        vec![
            ("Cash".into(), cash, "#14B8A6".into()),
            ("Wertpapiere".into(), securities, "#3B82F6".into()),
            ("Sachwerte".into(), tangibles, "#F59E0B".into()),
        ],
        money,
    );

    let mut note = String::new();
    if debts != 0 {
        note.push_str(&format!("Abzüglich Darlehen: {}. ", money(debts.abs())));
    }
    if skipped > 0 {
        note.push_str(&format!(
            "{skipped} Konto/Konten in Fremdwährung sind nicht enthalten."
        ));
    }

    let account_rows = |filter: &dyn Fn(&Account) -> bool| -> Vec<Cells> {
        let mut v: Vec<&&Account> = counted.iter().filter(|a| filter(a)).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.balance_cents));
        v.iter()
            .map(|a| Cells {
                id: a.id,
                cells: [
                    a.name.clone(),
                    [
                        crate::nav::kind_label(a.kind),
                        a.bank_name.as_str(),
                        a.owner.as_str(),
                    ]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · "),
                    String::new(),
                    money(a.balance_cents),
                    String::new(),
                    String::new(),
                ],
                negative: a.balance_cents < 0,
            })
            .collect()
    };

    // Positions of one kind, merged by ISIN (or name) across accounts.
    let position_rows = |tangible: bool| -> Vec<Cells> {
        let mut merged: BTreeMap<String, Holding> = BTreeMap::new();
        for (aid, p) in &data.positions {
            if p.tangible != tangible || !counted.iter().any(|a| a.id == *aid) {
                continue;
            }
            let key = if p.holding.isin.is_empty() {
                p.holding.name.clone()
            } else {
                p.holding.isin.clone()
            };
            merged
                .entry(key)
                .and_modify(|m| {
                    m.quantity += p.holding.quantity;
                    m.value_cents += p.holding.value_cents;
                    m.purchase_price_cents = 0; // no meaningful average of unrelated lots
                    m.profit_loss_purchase_cents = match (
                        m.profit_loss_purchase_cents,
                        p.holding.profit_loss_purchase_cents,
                    ) {
                        (Some(a), Some(b)) => Some(a + b),
                        _ => None,
                    };
                    m.profit_loss_prev_day_cents = None;
                    m.prev_day_value_cents = None;
                })
                .or_insert_with(|| p.holding.clone());
        }
        let mut v: Vec<Holding> = merged.into_values().collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.value_cents));
        v.iter()
            .enumerate()
            .map(|(i, h)| position_row(i as i64, h, money, &|d| d.to_string()))
            .collect()
    };

    let (columns, rows): (Vec<&str>, Vec<Cells>) = match tab {
        WealthTab::Cash => (
            vec!["Konto", "Art", "", "Saldo", ""],
            account_rows(&|a| is_cash(data, a)),
        ),
        WealthTab::Securities => (POSITION_COLUMNS.to_vec(), position_rows(false)),
        WealthTab::Tangibles => (POSITION_COLUMNS.to_vec(), position_rows(true)),
        WealthTab::All => {
            let mut rows = account_rows(&|a| is_cash(data, a));
            rows.extend(position_rows(false));
            rows.extend(position_rows(true));
            (
                vec!["Bezeichnung", "Menge / Art", "Kurs", "Wert", "G/V"],
                rows,
            )
        }
    };

    WealthView {
        net_worth: money(net),
        note,
        composition: comp,
        columns: columns.into_iter().map(String::from).collect(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eur(c: i64) -> String {
        format!("{c}€")
    }
    fn plain(d: &str) -> String {
        d.to_string()
    }

    fn holding(name: &str, isin: &str, qty: f64, value: i64) -> Holding {
        Holding {
            isin: isin.into(),
            wkn: String::new(),
            name: name.into(),
            quantity: qty,
            current_price_cents: if qty > 0.0 {
                (value as f64 / qty) as i64
            } else {
                0
            },
            purchase_price_cents: 0,
            value_cents: value,
            prev_day_price_cents: None,
            prev_day_value_cents: None,
            profit_loss_purchase_cents: None,
            profit_loss_purchase_rel: None,
            profit_loss_prev_day_cents: None,
            profit_loss_prev_day_rel: None,
            custody_type: String::new(),
            ..Default::default()
        }
    }

    fn tx(id: i64, date: &str, dir: SecurityDirection, value: i64) -> SecurityTransaction {
        SecurityTransaction {
            id,
            account_id: 1,
            external_id: String::new(),
            booking_date: date.into(),
            settlement_date: date.into(),
            isin: "DE1".into(),
            wkn: String::new(),
            name: "Fonds".into(),
            direction: dir,
            quantity: 2.0,
            price_cents: 100,
            currency: "EUR".into(),
            value_cents: value,
            fees_cents: 0,
            taxes_cents: 0,
            accrued_interest_cents: 0,
            fx_rate: None,
            booking_text: String::new(),
            cash_transaction_id: None,
        }
    }

    fn account(id: i64, kind: AccountKind, balance: i64) -> Account {
        crate::nav::testing::account(id, &format!("Konto {id}"), "Bank", kind, balance)
    }

    #[test]
    fn periods_start_on_the_right_day() {
        let t = "2026-11-16";
        assert_eq!(Period::All.start(t), None);
        assert_eq!(Period::Year.start(t).as_deref(), Some("2025-11-16"));
        assert_eq!(Period::ThisYear.start(t).as_deref(), Some("2026-01-01"));
        assert_eq!(Period::Quarter.start(t).as_deref(), Some("2026-08-16"));
        assert_eq!(Period::Month.start(t).as_deref(), Some("2026-10-16"));
        // Month ends and year wrap.
        assert_eq!(
            Period::Month.start("2026-03-31").as_deref(),
            Some("2026-02-28")
        );
        assert_eq!(
            Period::Quarter.start("2026-02-10").as_deref(),
            Some("2025-11-10")
        );
        assert_eq!(
            Period::Year.start("2024-02-29").as_deref(),
            Some("2023-02-28")
        );
        assert_eq!(Period::Month.start("kaputt"), None);
        assert_eq!(Period::from_index(2), Period::ThisYear);
        assert_eq!(Period::from_index(-1), Period::All);
    }

    #[test]
    fn today_is_a_plausible_iso_date() {
        let t = today_iso();
        assert_eq!(t.len(), 10);
        assert!(parse_date(&t).is_some());
        assert!(t.as_str() >= "2025-01-01");
    }

    #[test]
    fn missing_figures_are_unavailable_not_zero() {
        let hs = vec![holding("A", "DE1", 1.0, 1000)];
        assert_eq!(unrealized_pl(&hs), None);
        assert_eq!(day_change(&hs), None);
        assert_eq!(distributions(&[], &None), None);
        assert_eq!(fees_and_taxes(&[], &None), None);
        assert_eq!(unrealized_pl(&[]), None);
        let k = kpi("X", None, true, &eur);
        assert_eq!(k.value, UNAVAILABLE);
        assert!(!k.negative);
    }

    #[test]
    fn unrealized_pl_uses_bank_figure_or_cost_basis() {
        let mut a = holding("A", "DE1", 2.0, 1200);
        a.profit_loss_purchase_cents = Some(-50);
        let mut b = holding("B", "DE2", 2.0, 1000);
        b.purchase_price_cents = 400; // cost 800 -> +200
        assert_eq!(unrealized_pl(&[a.clone(), b.clone()]), Some(150));
        // One position with neither: no partial sum.
        let c = holding("C", "DE3", 1.0, 100);
        assert_eq!(unrealized_pl(&[a, b, c]), None);
    }

    #[test]
    fn day_change_needs_every_position() {
        let mut a = holding("A", "DE1", 1.0, 1100);
        a.prev_day_value_cents = Some(1000);
        let mut b = holding("B", "DE2", 1.0, 480);
        b.profit_loss_prev_day_cents = Some(-20);
        assert_eq!(day_change(&[a.clone(), b]), Some(80));
        assert_eq!(day_change(&[a, holding("C", "DE3", 1.0, 5)]), None);
    }

    #[test]
    fn distributions_and_fees_follow_the_period() {
        let mut fee = tx(3, "2026-09-20", SecurityDirection::Fee, -500);
        fee.fees_cents = 0;
        let mut buy = tx(4, "2026-11-01", SecurityDirection::Buy, -10_000);
        buy.fees_cents = 99;
        buy.taxes_cents = 10;
        let txs = vec![
            tx(1, "2025-03-01", SecurityDirection::Dividend, 700),
            tx(2, "2026-11-05", SecurityDirection::Dividend, 300),
            fee,
            buy,
        ];
        assert_eq!(distributions(&txs, &None), Some(1000));
        let start = Period::Month.start("2026-11-16");
        assert_eq!(distributions(&txs, &start), Some(300));
        assert_eq!(fees_and_taxes(&txs, &None), Some(500 + 99 + 10));
        assert_eq!(fees_and_taxes(&txs, &start), Some(99 + 10));
        // Transactions exist but none in the period: a real zero, not "n/a".
        let later = Period::Month.start("2027-06-01");
        assert_eq!(distributions(&txs, &later), Some(0));
    }

    #[test]
    fn composition_orders_and_ignores_negatives() {
        let c = composition(
            vec![
                ("B".into(), 300, "#111111".into()),
                ("A".into(), 700, "#222222".into()),
                ("Schuld".into(), -50, "#333333".into()),
            ],
            &eur,
        );
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].label, "A");
        assert!((c[0].share - 0.7).abs() < 1e-6);
        assert_eq!(c[1].value, "300€");
        assert!(composition(vec![], &eur).is_empty());
    }

    #[test]
    fn depot_view_has_four_kpis_and_tables() {
        let mut h1 = holding("Welt-ETF", "DE1", 10.0, 5000);
        h1.profit_loss_purchase_cents = Some(400);
        let h2 = holding("Anleihe", "DE2", 1.0, 1000);
        let data = DepotData {
            account: account(1, AccountKind::Brokerage, 6000),
            holdings: vec![(h1, AssetClass::Equity), (h2, AssetClass::Bond)],
            transactions: vec![
                tx(1, "2026-11-01", SecurityDirection::Buy, 5000),
                tx(2, "2026-11-10", SecurityDirection::Dividend, 25),
            ],
        };
        let v = depot_view(&data, Period::All, "2026-11-16", &eur, &plain);
        assert_eq!(v.total, "6000€");
        assert_eq!(v.kpis.len(), 4);
        // G/V is incomplete (one position without it), day change missing: both unavailable.
        assert_eq!(v.kpis[0].value, UNAVAILABLE);
        assert_eq!(v.kpis[1].value, UNAVAILABLE);
        assert_eq!(v.kpis[2].value, "25€");
        assert_eq!(v.kpis[3].value, "0€");
        assert_eq!(v.composition.len(), 2);
        assert_eq!(v.composition[0].label, "Aktien");
        assert_eq!(v.positions.len(), 2);
        assert!(v.positions[0].cells[0].starts_with("Welt-ETF"));
        assert_eq!(v.positions[0].cells[1], "10");
        // Activities: newest first, a purchase is an outflow.
        assert_eq!(v.activities[0].cells[1], "Ausschüttung");
        assert_eq!(v.activities[1].cells[1], "Kauf");
        assert!(v.activities[1].negative);
    }

    #[test]
    fn depot_period_limits_activities() {
        let data = DepotData {
            account: account(1, AccountKind::Brokerage, 0),
            holdings: vec![],
            transactions: vec![
                tx(1, "2024-01-01", SecurityDirection::Buy, 100),
                tx(2, "2026-11-10", SecurityDirection::Buy, 100),
            ],
        };
        assert_eq!(
            depot_view(&data, Period::All, "2026-11-16", &eur, &plain)
                .activities
                .len(),
            2
        );
        assert_eq!(
            depot_view(&data, Period::Month, "2026-11-16", &eur, &plain)
                .activities
                .len(),
            1
        );
        // No positions: the header falls back to the account balance.
        let v = depot_view(&data, Period::All, "2026-11-16", &eur, &plain);
        assert_eq!(v.total, "0€");
        assert!(v.composition.is_empty());
    }

    #[test]
    fn tangible_account_without_transactions_shows_unavailable_flows() {
        let data = DepotData {
            account: account(2, AccountKind::Cash, 9000),
            holdings: vec![(holding("Goldmünze", "", 3.0, 9000), AssetClass::Commodity)],
            transactions: vec![],
        };
        let v = depot_view(&data, Period::All, "2026-11-16", &eur, &plain);
        assert_eq!(v.kpis[2].value, UNAVAILABLE);
        assert_eq!(v.kpis[3].value, UNAVAILABLE);
        assert_eq!(v.composition[0].label, "Rohstoffe");
    }

    #[test]
    fn privacy_masks_only_the_named_cells() {
        let mut c = Cells {
            id: 1,
            cells: [
                "Name".into(),
                "Art".into(),
                "".into(),
                "5".into(),
                "".into(),
                "".into(),
            ],
            negative: false,
        };
        c.mask(&[0, 2, 9], false);
        assert_eq!(c.cells[0], "Name");
        c.mask(&[0, 2, 9], true);
        assert_eq!(c.cells[0], crate::daily::MASK);
        assert_eq!(c.cells[1], "Art");
        // An empty cell stays empty.
        assert_eq!(c.cells[2], "");
    }

    #[test]
    fn quantities_read_german() {
        assert_eq!(quantity_text(10.0), "10");
        assert_eq!(quantity_text(0.5), "0,5");
        assert_eq!(quantity_text(1.23456), "1,2346");
    }

    fn wealth_data() -> WealthData {
        let mut loan = account(4, AccountKind::Loan, -20_000);
        loan.initial_balance_cents = Some(50_000);
        let mut usd = account(5, AccountKind::Checking, 777);
        usd.currency = "USD".into();
        let mut off = account(6, AccountKind::Checking, 999);
        off.in_total = false;
        WealthData {
            accounts: vec![
                account(1, AccountKind::Checking, 10_000),
                account(2, AccountKind::Brokerage, 0),
                account(3, AccountKind::Cash, 3000),
                loan,
                usd,
                off,
            ],
            tangible_account_ids: vec![3],
            positions: vec![
                (
                    2,
                    WealthPosition {
                        holding: holding("Welt-ETF", "DE1", 4.0, 8000),
                        tangible: false,
                    },
                ),
                (
                    2,
                    WealthPosition {
                        holding: holding("Welt-ETF", "DE1", 1.0, 2000),
                        tangible: false,
                    },
                ),
                (
                    3,
                    WealthPosition {
                        holding: holding("Gold", "", 2.0, 3000),
                        tangible: true,
                    },
                ),
            ],
        }
    }

    #[test]
    fn wealth_header_sums_classes_and_subtracts_loans() {
        let v = wealth_view(&wealth_data(), WealthTab::All, &eur);
        // 10000 cash + 10000 securities + 3000 tangibles - 20000 loan.
        assert_eq!(v.net_worth, "3000€");
        let labels: Vec<&str> = v.composition.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Cash", "Wertpapiere", "Sachwerte"]);
        assert!(v.note.contains("Darlehen: 20000€"));
        assert!(v.note.contains("1 Konto/Konten in Fremdwährung"));
    }

    #[test]
    fn wealth_tabs_filter_rows_and_merge_positions() {
        let d = wealth_data();
        let cash = wealth_view(&d, WealthTab::Cash, &eur);
        assert_eq!(cash.rows.len(), 1);
        assert_eq!(cash.rows[0].cells[0], "Konto 1");
        let sec = wealth_view(&d, WealthTab::Securities, &eur);
        // Same ISIN in two lots: one aggregated row of 5 units and 10000.
        assert_eq!(sec.rows.len(), 1);
        assert_eq!(sec.rows[0].cells[1], "5");
        assert_eq!(sec.rows[0].cells[3], "10000€");
        let tan = wealth_view(&d, WealthTab::Tangibles, &eur);
        assert_eq!(tan.rows.len(), 1);
        assert_eq!(tan.rows[0].cells[0], "Gold");
        let all = wealth_view(&d, WealthTab::All, &eur);
        assert_eq!(all.rows.len(), 3);
        assert_eq!(WealthTab::from_index(3), WealthTab::Tangibles);
        assert_eq!(DepotTab::from_index(1), DepotTab::Activities);
    }

    #[test]
    fn empty_wealth_is_zero_with_no_bars() {
        let v = wealth_view(
            &WealthData {
                accounts: vec![],
                tangible_account_ids: vec![],
                positions: vec![],
            },
            WealthTab::All,
            &eur,
        );
        assert_eq!(v.net_worth, "0€");
        assert!(v.composition.is_empty());
        assert!(v.rows.is_empty());
    }
}
