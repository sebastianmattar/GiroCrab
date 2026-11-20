//! SWIFT MT535 (statement of holdings) as returned by the FinTS business
//! transaction HKWPD ("Depotaufstellung anfordern" → HIWPD).
//!
//! Only what the app needs: the depot value plus a light list of positions.
//! Structure (one message per depot):
//!
//! ```text
//! :16R:GENL ... :98A::STAT//20261110 ... :16S:GENL
//! :16R:FIN
//! :35B:ISIN DE0001234567          ← identification (+ WKN line, name lines)
//! /DE/123456
//! MUSTERFONDS
//! :90B::MRKT//ACTU/EUR44,11       ← price (90A = percent of nominal)
//! :98A::PRIC//20261107            ← price date
//! :93B::AGGR//UNIT/243,           ← quantity (FAMT = nominal for bonds)
//! :19A::HOLD//EUR10718,73         ← value of the position (N = negative)
//! :16S:FIN
//! :16R:ADDINFO :19A::HOLP//EUR... ← value of this page; HOLS = whole statement
//! -
//! ```
//!
//! Amounts use a comma as decimal separator. Values are kept in cents;
//! quantities as text-exact `f64` for display only.

/// One position (Wertpapier) of the depot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Holding {
    pub isin: String,
    pub wkn: String,
    pub name: String,
    /// Units, or the nominal amount for bonds (`FAMT`).
    pub quantity: f64,
    /// Price per unit in cents of `currency` (None for percent quotes).
    pub price_cents: Option<i64>,
    /// Price date (YYYY-MM-DD).
    pub price_date: Option<String>,
    /// Market value of the position in cents.
    pub value_cents: Option<i64>,
    pub currency: String,
}

/// Parsed holdings statement.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Statement {
    pub holdings: Vec<Holding>,
    /// Total value reported by the bank: `:19A::HOLS` (statement), else the
    /// sum of `:19A::HOLP` (one per page), if any.
    pub total_cents: Option<i64>,
    /// Statement date (`:98A::STAT` / `:98C::STAT` / `PREP`), YYYY-MM-DD.
    pub date: Option<String>,
}

impl Holding {
    /// Value from the bank, else quantity × price (None on overflow / non-finite).
    fn position_value_cents(&self) -> Option<i64> {
        let p = self.price_cents?;
        let v = (p as f64 * self.quantity).round();
        (v.is_finite() && v.abs() < 9.0e18).then_some(v as i64)
    }

    /// Conversion for the vault's `holdings` table.
    pub fn to_model(&self) -> crate::models::Holding {
        crate::models::Holding {
            isin: self.isin.clone(),
            wkn: self.wkn.clone(),
            name: self.name.clone(),
            quantity: self.quantity,
            current_price_cents: self.price_cents.unwrap_or(0),
            purchase_price_cents: 0,
            value_cents: self
                .value_cents
                .or_else(|| self.position_value_cents())
                .unwrap_or(0),
            ..Default::default()
        }
    }
}

impl Statement {
    /// Depot value: the bank's total, otherwise the sum of the position values
    /// (quantity × price where the bank sent no value). None when any
    /// position has neither – a wrong total is worse than none.
    pub fn value_cents(&self) -> Option<i64> {
        if self.total_cents.is_some() {
            return self.total_cents;
        }
        self.holdings.iter().try_fold(0i64, |acc, h| {
            let v = h.value_cents.or_else(|| h.position_value_cents())?;
            acc.checked_add(v)
        })
    }

    /// Latest date in the statement (statement date or newest price date).
    pub fn latest_date(&self) -> Option<String> {
        self.holdings
            .iter()
            .filter_map(|h| h.price_date.clone())
            .chain(self.date.clone())
            .max()
    }
}

/// Parses one or more concatenated MT535 messages (several HIWPD pages).
pub fn parse(text: &str) -> Statement {
    let mut st = Statement::default();
    let mut total: Option<i64> = None;
    let mut statement_total: Option<i64> = None;
    let mut current: Option<Holding> = None;
    let (mut fin_blocks, mut skipped) = (0usize, 0usize);
    let mut tags: Vec<String> = Vec::new();
    let all_fields = fields(text);
    crate::depot_debug::log(&format!(
        "mt535 parse: {} chars, {} fields",
        text.len(),
        all_fields.len()
    ));
    for field in all_fields {
        let (tag, body) = split_tag(&field);
        if current.is_some() && !matches!(tag, "16R" | "16S") {
            tags.push(tag.to_string());
        }
        match tag {
            "16R" if body == "FIN" => {
                fin_blocks += 1;
                tags.clear();
                current = Some(Holding::default());
            }
            "16S" if body == "FIN" => {
                if let Some(h) = current.take() {
                    let named = !(h.isin.is_empty() && h.wkn.is_empty() && h.name.is_empty());
                    if named || h.quantity != 0.0 || h.value_cents.is_some() {
                        st.holdings.push(h);
                    } else {
                        skipped += 1;
                        crate::depot_debug::log(&format!(
                            "mt535 FIN block skipped (empty); tags inside={tags:?}"
                        ));
                    }
                }
            }
            "35B" => {
                if let Some(h) = current.as_mut() {
                    identification(h, body);
                }
            }
            "93B" => {
                if let (Some(h), Some(q)) = (current.as_mut(), quantity(body)) {
                    h.quantity = q;
                }
            }
            "90B" | "90A" => {
                // :MRKT//ACTU/EUR44,11 (amount per unit)
                if let (Some(h), Some((cur, cents))) = (
                    current.as_mut(),
                    after_qualifier(body).and_then(price_amount),
                ) {
                    h.price_cents = Some(cents);
                    if h.currency.is_empty() {
                        h.currency = cur;
                    }
                }
            }
            "98A" | "98C" => {
                let q = qualifier(body);
                let d = after_qualifier(body).and_then(date);
                match (q, current.as_mut()) {
                    ("PRIC", Some(h)) => h.price_date = d,
                    ("STAT" | "PREP", None)
                        if d.is_some() && (st.date.is_none() || q == "STAT") =>
                    {
                        st.date = d
                    }
                    _ => {}
                }
            }
            "19A" => {
                let q = qualifier(body);
                let amount = after_qualifier(body).and_then(signed_amount);
                match (q, current.as_mut(), amount) {
                    ("HOLD" | "MKTV" | "BOOK", Some(h), Some((cur, cents))) => {
                        if q == "HOLD" || h.value_cents.is_none() {
                            h.value_cents = Some(cents);
                        }
                        h.currency = cur;
                    }
                    ("HOLS", None, Some((_, cents))) => statement_total = Some(cents),
                    ("HOLP", None, Some((_, cents))) => {
                        total = Some(total.unwrap_or(0i64).saturating_add(cents));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    st.total_cents = statement_total.or(total);
    crate::depot_debug::log(&format!(
        "mt535 result: fin_blocks={fin_blocks} skipped={skipped} kept={} has_total={}",
        st.holdings.len(),
        st.total_cents.is_some()
    ));
    st
}

/// Splits the text into fields (":tag:content"), joining continuation lines
/// with '\n'. The end-of-message line "-" closes a message.
fn fields(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') && line.len() > 4 && line[1..].contains(':') {
            out.push(line.to_string());
        } else if line.trim() == "-" || line.trim().is_empty() {
            continue;
        } else if let Some(last) = out.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    out
}

/// ":35B:ISIN X" → ("35B", "ISIN X")
fn split_tag(field: &str) -> (&str, &str) {
    let rest = &field[1..];
    match rest.find(':') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    }
}

/// ":QUAL//..." → "QUAL"
fn qualifier(body: &str) -> &str {
    body.trim_start_matches(':')
        .split("//")
        .next()
        .unwrap_or("")
}

/// ":QUAL//rest" → "rest"
fn after_qualifier(body: &str) -> Option<&str> {
    body.split_once("//").map(|(_, r)| r)
}

fn identification(h: &mut Holding, body: &str) {
    let mut name = Vec::new();
    for (i, l) in body.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(isin) = l.strip_prefix("ISIN ") {
            h.isin = isin.split_whitespace().next().unwrap_or("").to_string();
        } else if let Some(rest) = l.strip_prefix('/') {
            // "/DE/123456", "/XX/US0378331005", "/TS/..." – country or scheme prefix
            if let Some((scheme, id)) = rest.split_once('/') {
                let id = id.trim();
                if scheme.len() == 2 && id.len() == 12 && h.isin.is_empty() {
                    h.isin = id.to_string();
                } else if h.wkn.is_empty() && !id.is_empty() {
                    h.wkn = id.to_string();
                }
            }
        } else if i == 0 && l.len() == 6 && l.chars().all(|c| c.is_ascii_alphanumeric()) {
            h.wkn = l.to_string();
        } else {
            name.push(l);
        }
    }
    h.name = name.join(" ");
}

/// ":AGGR//UNIT/243," or ":AGGR//FAMT/1000," (dot decimals tolerated)
fn quantity(body: &str) -> Option<f64> {
    let rest = after_qualifier(body)?;
    let (_, num) = rest.split_once('/')?;
    let num = num.trim();
    let neg = num.starts_with('N');
    let v: f64 = num.trim_start_matches('N').replace(',', ".").parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// "ACTU/EUR44,11" → ("EUR", 4411); percent quotes ("PRCT/101,5") → None.
fn price_amount(rest: &str) -> Option<(String, i64)> {
    let (kind, amount) = rest.split_once('/')?;
    if kind == "PRCT" || kind == "YIEL" {
        return None;
    }
    signed_amount(amount)
}

/// "EUR10718,73" / "NEUR12,5" → ("EUR", 1071873) / ("EUR", -1250)
fn signed_amount(s: &str) -> Option<(String, i64)> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('N') {
        Some(r)
            if r.len() > 3
                && r.get(..3)
                    .is_some_and(|c| c.chars().all(|c| c.is_ascii_uppercase())) =>
        {
            (true, r)
        }
        _ => (false, s),
    };
    let cur = s.get(..3)?;
    if s.len() < 4 || !cur.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let cents = decimal_cents(s.get(3..)?)?;
    Some((cur.to_string(), if neg { -cents } else { cents }))
}

/// "10718,73" → 1071873, "243," → 24300, "11,672" → 1167 (rounded half up).
fn decimal_cents(s: &str) -> Option<i64> {
    let (int, frac) = s.split_once(',').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut digits: Vec<u8> = frac.bytes().map(|b| b - b'0').collect();
    digits.resize(digits.len().max(3), 0);
    let cents = (digits[0] as i64) * 10 + digits[1] as i64;
    let round_up = digits[2] >= 5;
    int.checked_mul(100)?
        .checked_add(cents)?
        .checked_add(i64::from(round_up))
}

/// "20261107" or "20261107120000" → "2026-11-07"
fn date(s: &str) -> Option<String> {
    crate::dates::parse_yyyymmdd(s).map(crate::dates::iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented depot, public example ISINs/WKNs only.
    const SAMPLE: &str = "\r\n:16R:GENL\r\n:28E:1/ONLY\r\n:20C::SEME//NONREF\r\n:23G:NEWM\r\n:98C::PREP//20261110093000\r\n:98A::STAT//20261110\r\n:22F::STTY//CUST\r\n:97A::SAFE//12030000/0001234567\r\n:17B::ACTI//Y\r\n:16S:GENL\r\n:16R:FIN\r\n:35B:ISIN DE0009807016\r\n/DE/980701\r\nMUSTER IMMOBILIENFONDS\r\n:90B::MRKT//ACTU/EUR44,11\r\n:98A::PRIC//20261107\r\n:93B::AGGR//UNIT/243,\r\n:16R:SUBBAL\r\n:93C::TAVI//UNIT/AVAI/243,\r\n:16S:SUBBAL\r\n:19A::HOLD//EUR10718,73\r\n:70E::HOLD//1STK44,49+EUR\r\n:16S:FIN\r\n:16R:FIN\r\n:35B:ISIN LU0000000001\r\n/DE/A0B1C2\r\nBEISPIEL AKTIEN\r\nWELT\r\n:90B::MRKT//ACTU/EUR114,21\r\n:98A::PRIC//20261106\r\n:93B::AGGR//UNIT/78,\r\n:19A::HOLD//EUR8908,38\r\n:16S:FIN\r\n:16R:ADDINFO\r\n:19A::HOLP//EUR19627,11\r\n:16S:ADDINFO\r\n-";

    #[test]
    fn parses_positions_value_and_dates() {
        let st = parse(SAMPLE);
        assert_eq!(st.holdings.len(), 2);
        let h = &st.holdings[0];
        assert_eq!(h.isin, "DE0009807016");
        assert_eq!(h.wkn, "980701");
        assert_eq!(h.name, "MUSTER IMMOBILIENFONDS");
        assert_eq!(h.quantity, 243.0);
        assert_eq!(h.price_cents, Some(44_11));
        assert_eq!(h.price_date.as_deref(), Some("2026-11-07"));
        assert_eq!(h.value_cents, Some(10_718_73));
        assert_eq!(st.holdings[1].name, "BEISPIEL AKTIEN WELT");
        assert_eq!(st.total_cents, Some(19_627_11));
        assert_eq!(st.value_cents(), Some(19_627_11));
        assert_eq!(st.date.as_deref(), Some("2026-11-10"));
        assert_eq!(st.latest_date().as_deref(), Some("2026-11-10"));
    }

    #[test]
    fn without_total_sums_positions_and_falls_back_to_price() {
        let text = ":16R:FIN\n:35B:ISIN DE0000000001\n:90B::MRKT//ACTU/EUR11,672\n:93B::AGGR//UNIT/10,5\n:16S:FIN\n:16R:FIN\n:35B:ISIN DE0000000002\n:93B::AGGR//UNIT/1,\n:19A::HOLD//NEUR12,5\n:16S:FIN\n-";
        let st = parse(text);
        assert_eq!(st.total_cents, None);
        assert_eq!(st.holdings[0].price_cents, Some(11_67));
        assert_eq!(st.holdings[1].value_cents, Some(-12_50));
        // 10.5 × 11.67 € = 122.535 € → 12254 cents, minus 12.50 €
        assert_eq!(st.value_cents(), Some(12_254 - 12_50));
    }

    #[test]
    fn empty_depot_is_zero_and_percent_quotes_have_no_price() {
        assert_eq!(parse(":16R:GENL\n:16S:GENL\n-").value_cents(), Some(0));
        let st = parse(
            ":16R:FIN\n:35B:ISIN DE0000000003\n:90A::MRKT//PRCT/101,5\n:93B::AGGR//FAMT/1000,\n:16S:FIN\n-",
        );
        assert_eq!(st.holdings[0].price_cents, None);
        assert_eq!(st.holdings[0].quantity, 1000.0);
        assert_eq!(st.value_cents(), None);
    }

    #[test]
    fn comdirect_style_blocks_are_kept() {
        // no ISIN prefix, bare WKN, dot decimals, MKTV value, no price
        let text = ":16R:FIN\n:35B:/DE/865985\nAPPLE INC.\n:93B::AGGR//UNIT/12.5\n:19A::MKTV//EUR2100,5\n:16S:FIN\n:16R:FIN\n:35B:ISIN US0378331005 /DE/865985\n:93B::AGGR//UNIT/3,\n:16S:FIN\n:16R:FIN\n:35B:/XX/DE0001234567\nBUND\n:93B::AGGR//FAMT/1000,\n:90A::MRKT//PRCT/99,\n:16S:FIN\n-";
        let st = parse(text);
        assert_eq!(st.holdings.len(), 3);
        assert_eq!(st.holdings[0].wkn, "865985");
        assert_eq!(st.holdings[0].name, "APPLE INC.");
        assert_eq!(st.holdings[0].quantity, 12.5);
        assert_eq!(st.holdings[0].value_cents, Some(2100_50));
        assert_eq!(st.holdings[1].isin, "US0378331005");
        assert_eq!(st.holdings[2].isin, "DE0001234567");
    }

    #[test]
    fn several_pages_add_up() {
        let page = ":16R:ADDINFO\n:19A::HOLP//EUR100,\n:16S:ADDINFO\n-\n";
        assert_eq!(parse(&format!("{page}{page}")).total_cents, Some(200_00));
        // the statement total wins over the page totals (no double counting)
        let last = ":16R:ADDINFO\n:19A::HOLP//EUR50,\n:19A::HOLS//EUR250,\n:16S:ADDINFO\n-\n";
        assert_eq!(
            parse(&format!("{page}{page}{last}")).total_cents,
            Some(250_00)
        );
    }

    #[test]
    fn hostile_input_never_panics() {
        // multibyte char straddling the currency slice, overflow, inf quantity
        for t in [
            ":16R:FIN\n:19A::HOLD//ABÄ1,\n:16S:FIN",
            ":19A::HOLS//NABÄ1,",
            ":19A::HOLS//EUR99999999999999999999,",
            ":19A::HOLS//EUR92233720368547758,99",
            ":16R:FIN\n:93B::AGGR//UNIT/inf\n:90B::MRKT//ACTU/EUR1,\n:16S:FIN",
        ] {
            let st = parse(t);
            let _ = st.value_cents();
        }
        assert_eq!(
            parse(":19A::HOLS//EUR92233720368547758,99").total_cents,
            None
        );
    }
}
