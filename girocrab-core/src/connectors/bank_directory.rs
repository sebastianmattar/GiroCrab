//! Unified bank search for the "Bank hinzufügen" picker.
//!
//! One query (bank name, 8-digit BLZ or German IBAN) is answered from three
//! sources, each result tagged with how GiroCrab connects to the bank:
//!
//!  * `FinTs`  – FinTS 3.0 / HBCI, no third-party registration (`fints_directory`)
//!  * `OwnApi` – the bank's own API: bunq, comdirect, Trade Republic, PayPal
//!  * `Psd2`   – PSD2 via the user's Enable Banking application: a curated
//!    catalog of popular DE/AT banks and fintechs, plus – when the
//!    user's credentials are set – the full live `GET /aspsps` list.
//!
//! The curated PSD2 names are best effort: at connect time the connector asks
//! the live ASPSP list for the exact spelling (`pick_aspsp_name`).

use std::sync::Arc;

use super::enable_banking::{EnableBankingConnector, parse_aspsps};
use super::fints_directory::{self, FinTsBankInfo, fold};
use crate::error::CoreError;
use crate::i18n::tr;
use crate::models::ConnectorKind;
use crate::vault::Vault;

/// How GiroCrab talks to a bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BankAccessMode {
    /// FinTS 3.0 / HBCI – direct, nothing to register.
    FinTs,
    /// The bank's own API (bunq, comdirect, Trade Republic, PayPal).
    OwnApi,
    /// PSD2 through Enable Banking.
    Psd2,
}

/// A bank reachable through Enable Banking.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Psd2Bank {
    /// Name shown to the user.
    pub name: String,
    /// ISO 3166 alpha-2 country the ASPSP is listed under.
    pub country: String,
    /// ASPSP name for the Enable Banking API.
    pub aspsp: String,
    /// Dedicated connector for banks that already have one (American Express,
    /// Amazon Visa: keyless web login, PSD2 optional). `None` = generic PSD2.
    pub connector: Option<ConnectorKind>,
    /// `true` when the entry came from the live `GET /aspsps` list.
    pub live: bool,
    /// Verified with a real account: what was tested (empty = not yet).
    pub tested: String,
}

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BankSearchResult {
    /// Stable id: `fints:<blz>`, `api:<connector>` or `psd2:<CC>:<aspsp>`.
    pub id: String,
    pub name: String,
    pub mode: BankAccessMode,
    pub country: String,
    /// FinTS only.
    pub blz: String,
    /// FinTS only: PIN/TAN endpoint.
    pub url: String,
    /// Own API / dedicated connector, if any.
    pub connector: Option<ConnectorKind>,
    /// PSD2 only: Enable Banking ASPSP name.
    pub aspsp: String,
    /// PSD2 only: verified with a real account (empty = not yet).
    pub tested: String,
}

fn psd2(name: &str, country: &str, aspsp: &str, connector: Option<ConnectorKind>) -> Psd2Bank {
    Psd2Bank {
        name: name.into(),
        country: country.into(),
        aspsp: aspsp.into(),
        connector,
        live: false,
        tested: String::new(),
    }
}

/// A catalog entry verified with a real account (Phase 4 of
/// docs/plans/enable-banking-psd2.md).
fn tested(mut b: Psd2Bank, note: &str) -> Psd2Bank {
    b.tested = note.to_string();
    b
}

/// Curated, searchable PSD2 catalog (works without any credentials).
pub fn psd2_catalog() -> Vec<Psd2Bank> {
    use ConnectorKind::{AmazonVisa, AmericanExpress};
    vec![
        psd2("N26", "DE", "N26", None),
        psd2("Revolut", "DE", "Revolut", None),
        tested(
            psd2("C24 Bank", "DE", "C24", None),
            tr(
                "Mit echtem Konto getestet: Girokonto, Gemeinschaftskonto und Tagesgeld-Pocket (auch ohne eigene IBAN); Abruf ohne TAN, Zustimmung 180 Tage.",
            ),
        ),
        psd2("Tomorrow", "DE", "Tomorrow", None),
        psd2("Vivid Money", "DE", "Vivid Money", None),
        psd2("Wise", "DE", "Wise", None),
        psd2("PayPal", "DE", "PayPal", None),
        psd2(
            "American Express",
            "DE",
            "American Express",
            Some(AmericanExpress),
        ),
        psd2("Amazon Visa (Zinia)", "DE", "Open Bank", Some(AmazonVisa)),
        psd2("Barclays", "DE", "Barclays", None),
        psd2("Hanseatic Bank", "DE", "Hanseatic Bank", None),
        psd2("Santander", "DE", "Santander", None),
        psd2("Commerzbank", "DE", "Commerzbank", None),
        psd2("ING Business", "DE", "ING Business", None),
        psd2("Trade Republic", "DE", "Trade Republic", None),
        psd2("Raiffeisen Bank (Österreich)", "AT", "Raiffeisen", None),
        psd2("Erste Bank (Österreich)", "AT", "Erste Bank", None),
        psd2("Bank Austria", "AT", "Bank Austria", None),
        psd2("BAWAG", "AT", "BAWAG", None),
        psd2("N26 (Österreich)", "AT", "N26", None),
        psd2("Revolut (Österreich)", "AT", "Revolut", None),
    ]
}

/// Banks with their own API: (connector, display name, extra search words).
fn own_api() -> Vec<(ConnectorKind, &'static str, &'static str)> {
    vec![
        (ConnectorKind::Bunq, "bunq", "bunq niederlande"),
        (
            ConnectorKind::Advanzia,
            "Advanzia Bank",
            "advanzia mein.advanzia.com mastercard kreditkarte",
        ),
        (ConnectorKind::Comdirect, "comdirect", "comdirect"),
        (
            ConnectorKind::TradeRepublic,
            "Trade Republic",
            "trade republic depot broker",
        ),
        (ConnectorKind::PayPal, "PayPal", "paypal"),
        (
            ConnectorKind::Easybank,
            "easybank",
            "easybank barclays kreditkarte bawag",
        ),
        (
            ConnectorKind::VolkswagenBank,
            "Volkswagen Bank",
            "volkswagen bank vw vwfs autobank tagesgeld girokonto",
        ),
        (
            ConnectorKind::Ibkr,
            "Interactive Brokers (IBKR)",
            "interactive brokers ibkr depot broker",
        ),
        (ConnectorKind::Fnz, "FNZ Bank", "fnz bank ebase finvesto"),
        (
            ConnectorKind::FordMoney,
            "Ford Money",
            "ford money fordmoney fordbank tagesgeld festgeld",
        ),
        (
            ConnectorKind::HanseaticBank,
            "Hanseatic Bank",
            "hanseatic hanseaticbank hanseatic bank genialcard goldcard kreditkarte sparbrief tagesgeld festgeld",
        ),
        (
            ConnectorKind::Bhw,
            "BHW (Bausparen)",
            "bhw mybhw bausparvertrag bausparen baufinanzierung darlehen",
        ),
        (
            ConnectorKind::Kontist,
            "Kontist",
            "kontist geschäftskonto selbstständige freelancer solaris",
        ),
        (
            ConnectorKind::OpelDirektbank,
            "Opel Direktbank",
            "opel direktbank opelbank opel-direktbank stellantis tagesgeld festgeld",
        ),
        (
            ConnectorKind::Finpension,
            "Finpension",
            "finpension saeule 3a säule 3a freizuegigkeit freizügigkeit 1e schweiz vorsorge",
        ),
        (
            ConnectorKind::Ffb,
            "FFB (FIL Fondsbank)",
            "ffb fil fondsbank fidelity fondsdepot fonds vl sparplan",
        ),
        (
            ConnectorKind::Uberspace,
            "Uberspace",
            "uberspace hosting asteroid webhosting",
        ),
        (
            ConnectorKind::AlteLeipziger,
            "Alte Leipziger",
            "alte leipziger alh bausparvertrag bauspar fin4u",
        ),
        (
            ConnectorKind::EquatePlus,
            "EquatePlus",
            "equateplus equate plus equatex mitarbeiteraktien aktienprogramme rsu sap",
        ),
        (
            ConnectorKind::Quirion,
            "quirion",
            "quirion quirin privatbank depot",
        ),
        (
            ConnectorKind::SchwaebischHall,
            "Schwäbisch Hall",
            "schwaebisch bsh bausparvertrag bauspar",
        ),
        (
            ConnectorKind::ScalableCapital,
            "Scalable Capital",
            "scalable capital depot broker overnight festgeld",
        ),
        (
            ConnectorKind::AmazonOrders,
            "Amazon Bestellungen",
            "amazon kundenkonto bestellungen einkäufe shop",
        ),
    ]
}

/// comdirect via FinTS for a name search: the bank code depends on the
/// account, so the row has none and the form asks for BLZ or IBAN.
fn comdirect_fints_result() -> BankSearchResult {
    BankSearchResult {
        id: "fints:comdirect".into(),
        name: "comdirect".into(),
        mode: BankAccessMode::FinTs,
        country: "DE".into(),
        blz: String::new(),
        url: fints_directory::COMDIRECT_FINTS_URL.into(),
        connector: None,
        aspsp: String::new(),
        tested: String::new(),
    }
}

fn matches_all(haystack: &str, words: &[&str]) -> bool {
    words.iter().all(|w| haystack.contains(w))
}

/// Extra search words for FinTS banks whose directory name lacks them
/// (former names, parent company): (BLZ, words).
const FINTS_ALIASES: &[(&str, &str)] = &[
    ("76030080", "dab bnp paribas"),
    ("50070439", "lufthansa mastercard"),
];

fn fints_result(b: FinTsBankInfo) -> BankSearchResult {
    BankSearchResult {
        id: format!("fints:{}", b.blz),
        name: b.name,
        mode: BankAccessMode::FinTs,
        country: "DE".into(),
        tested: String::new(),
        blz: b.blz,
        url: b.url,
        connector: None,
        aspsp: String::new(),
    }
}

/// Alte Leipziger Bauspar AG: not in the FinTS directory, reached via fin4u.
const ALTE_LEIPZIGER_BLZ: &str = "50012800";
/// FIL Fondsbank (FFB), no FinTS: served by its own web connector.
const FFB_BLZ: &str = "50021120";
/// Ford Bank (Ford Money), no FinTS: served by its own web connector.
const FORD_BANK_BLZ: &str = "37021548";
/// Opel Direktbank: its FinTS server offers no statements; served by its own web connector.
const OPEL_DIREKTBANK_BLZ: &str = "50025000";
/// Hanseatic Bank, no FinTS: served by its own connector (PSD2 stays in the catalog).
const HANSEATIC_BLZ: &str = "20120700";
const SCHWAEBISCH_HALL_BLZ: &str = "62220000";
/// BHW Bausparkasse, no public FinTS: served by its own connector (myBHW portal).
const BHW_BLZ: &str = "25410200";

fn own_result(kind: ConnectorKind, name: &str) -> BankSearchResult {
    BankSearchResult {
        id: format!("api:{}", kind.as_str()),
        name: name.into(),
        mode: BankAccessMode::OwnApi,
        country: String::new(),
        blz: String::new(),
        url: String::new(),
        connector: Some(kind),
        aspsp: String::new(),
        tested: String::new(),
    }
}

fn psd2_result(b: Psd2Bank) -> BankSearchResult {
    BankSearchResult {
        id: format!("psd2:{}:{}", b.country, b.aspsp),
        name: b.name,
        mode: BankAccessMode::Psd2,
        country: b.country,
        blz: String::new(),
        url: String::new(),
        connector: b.connector,
        aspsp: b.aspsp,
        tested: b.tested,
    }
}

/// Static catalog merged with the live list. Live entries win on
/// (country, ASPSP) clashes because their spelling is authoritative.
fn merged_psd2(live: Vec<Psd2Bank>) -> Vec<Psd2Bank> {
    let key = |b: &Psd2Bank| (b.country.to_uppercase(), b.aspsp.to_lowercase());
    let mut out: Vec<Psd2Bank> = Vec::new();
    for mut b in psd2_catalog() {
        if let Some(l) = live.iter().find(|l| key(l) == key(&b)) {
            b.live = true;
            b.aspsp = l.aspsp.clone();
        }
        out.push(b);
    }
    for l in live {
        if !out.iter().any(|b| key(b) == key(&l)) {
            out.push(l);
        }
    }
    out
}

/// Searches every source. `query` is a bank name, an 8-digit BLZ or a German
/// IBAN; `live_psd2` is the list from `Vault::list_enable_banking_aspsps`
/// (empty without credentials). At most `per_group_limit` rows per access
/// mode; order: FinTS, own API, PSD2.
pub fn search(
    query: &str,
    live_psd2: Vec<Psd2Bank>,
    per_group_limit: usize,
) -> Vec<BankSearchResult> {
    let mut out = Vec::new();
    let q = fold(query);
    if q.is_empty() || per_group_limit == 0 {
        return out;
    }

    // BLZ / IBAN: route directly to the appropriate access mode.
    if let Some(blz) = fints_directory::blz_from_input(query) {
        if fints_directory::COMDIRECT_BLZ.contains(&blz.as_str()) {
            // two ways: FinTS (PIN/TAN only) and comdirect's own API
            out.extend(fints_directory::lookup(&blz).map(fints_result));
            out.push(own_result(ConnectorKind::Comdirect, "comdirect"));
        } else if blz == "10110600" {
            out.push(own_result(ConnectorKind::Quirion, "quirion"));
        } else if let Some(b) = fints_directory::lookup(&blz) {
            out.push(fints_result(b));
        } else if blz == "70013000" {
            out.push(own_result(ConnectorKind::Fnz, "FNZ Bank"));
        } else if blz == BHW_BLZ {
            out.push(own_result(ConnectorKind::Bhw, "BHW (Bausparen)"));
        } else if blz == OPEL_DIREKTBANK_BLZ {
            out.push(own_result(ConnectorKind::OpelDirektbank, "Opel Direktbank"));
        } else if blz == HANSEATIC_BLZ {
            out.push(own_result(ConnectorKind::HanseaticBank, "Hanseatic Bank"));
        } else if blz == FORD_BANK_BLZ {
            out.push(own_result(ConnectorKind::FordMoney, "Ford Money"));
        } else if blz == FFB_BLZ {
            out.push(own_result(ConnectorKind::Ffb, "FFB (FIL Fondsbank)"));
        } else if blz == ALTE_LEIPZIGER_BLZ {
            out.push(own_result(ConnectorKind::AlteLeipziger, "Alte Leipziger"));
        } else if blz == SCHWAEBISCH_HALL_BLZ {
            out.push(own_result(
                ConnectorKind::SchwaebischHall,
                "Schwäbisch Hall",
            ));
        }
        return out;
    }

    // BIC ("DEUTDEFFXXX"): the FinTS banks that use it
    let by_bic = fints_directory::search_by_bic(query, per_group_limit);
    if !by_bic.is_empty() {
        return by_bic.into_iter().map(fints_result).collect();
    }

    let words: Vec<&str> = q.split(' ').collect();
    out.extend(
        fints_directory::search(query, per_group_limit)
            .into_iter()
            .map(fints_result),
    );
    for (blz, extra) in FINTS_ALIASES {
        let hit = words.iter().all(|w| extra.split(' ').any(|x| x == *w));
        if hit
            && !out.iter().any(|r| r.blz == *blz)
            && let Some(b) = fints_directory::lookup(blz)
        {
            out.push(fints_result(b));
        }
    }
    // comdirect by name: FinTS next to its own API (the FinTS directory
    // search above leaves it out, see `fints_directory::COMDIRECT_BLZ`)
    if matches_all("comdirect", &words) {
        out.push(comdirect_fints_result());
    }
    for (kind, name, extra) in own_api() {
        if matches_all(&format!("{} {}", fold(name), extra), &words) {
            out.push(own_result(kind, name));
        }
    }
    let mut n = 0;
    for b in merged_psd2(live_psd2) {
        if n >= per_group_limit {
            break;
        }
        if matches_all(&format!("{} {}", fold(&b.name), fold(&b.aspsp)), &words) {
            out.push(psd2_result(b));
            n += 1;
        }
    }
    out
}

/// Searches the FinTS directory, the own-API banks and the PSD2 catalog
/// (plus `live_psd2`, the live ASPSP list, if the app has loaded it).
pub fn search_banks(
    query: String,
    live_psd2: Vec<Psd2Bank>,
    per_group_limit: u32,
) -> Vec<BankSearchResult> {
    search(&query, live_psd2, per_group_limit as usize)
}

/// Offline PSD2 search over the curated catalog.
#[cfg(test)]
fn search_psd2_catalog(query: String) -> Vec<Psd2Bank> {
    let q = fold(&query);
    let words: Vec<&str> = q.split(' ').filter(|w| !w.is_empty()).collect();
    psd2_catalog()
        .into_iter()
        .filter(|b| matches_all(&format!("{} {}", fold(&b.name), fold(&b.aspsp)), &words))
        .collect()
}

/// Converts a `GET /aspsps` answer into picker rows.
pub(crate) fn live_banks(v: &serde_json::Value) -> Vec<Psd2Bank> {
    parse_aspsps(v)
        .into_iter()
        .map(|(name, country)| Psd2Bank {
            name: name.clone(),
            country,
            aspsp: name,
            connector: None,
            live: true,
            tested: String::new(),
        })
        .collect()
}

#[uniffi::export(async_runtime = "tokio")]
impl Vault {
    /// Every bank Enable Banking offers (`GET /aspsps`), for one country
    /// (ISO alpha-2) or all of them (empty). Needs the Enable Banking
    /// application ID and private key from the settings.
    pub async fn list_enable_banking_aspsps(
        self: Arc<Self>,
        country: String,
    ) -> Result<Vec<Psd2Bank>, CoreError> {
        // the default application, else any login's own (#824)
        let (app_id, pem) = {
            let db = self.db()?;
            crate::vault::enable_banking_apps::any_app(&db)?.ok_or_else(|| {
                CoreError::MissingSetting {
                    key: crate::vault::enable_banking_apps::APP_ID_KEY.to_string(),
                }
            })?
        };
        let jwt = EnableBankingConnector::make_jwt_for(&app_id, &pem)?;
        let country = country.trim().to_uppercase();
        if !country.is_empty()
            && !(country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()))
        {
            return Err(CoreError::invalid("country must be a two-letter code"));
        }
        let mut req = super::http_client()?
            .get("https://api.enablebanking.com/aspsps")
            .bearer_auth(jwt)
            .header("Accept", "application/json");
        if !country.is_empty() {
            req = req.query(&[("country", country.as_str())]);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let v: serde_json::Value = super::read_json_or_null(resp).await;
        if !status.is_success() {
            let msg = v["message"]
                .as_str()
                .unwrap_or("Enable Banking error")
                .to_string();
            return Err(if matches!(status.as_u16(), 401 | 403) {
                CoreError::Authentication { message: msg }
            } else {
                CoreError::Bank {
                    code: status.as_u16().to_string(),
                    message: msg,
                }
            });
        }
        let mut banks = live_banks(&v);
        banks.sort_by_key(|b| (b.name.to_lowercase(), b.country.clone()));
        Ok(banks)
    }
}

/// Bank name for matching: lower case, letters/digits only, without legal
/// forms and the word "bank" ("C24 Bank AG" → "c24").
fn match_key(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter(|w| {
            !matches!(
                *w,
                "bank" | "ag" | "se" | "sa" | "gmbh" | "eg" | "kg" | "nv" | "plc" | "ltd"
            )
        })
        .collect()
}

/// The generic PSD2 bank (Enable Banking, no own connector) for a bank as
/// another program names it, e.g. MoneyMoney's "C24 Bank": the same name
/// or ASPSP (German entries first), else the single entry whose name
/// contains it or is contained in it. `None` when nothing or more than one
/// bank fits – then the user picks the bank by hand.
pub(crate) fn match_psd2_bank(bank_name: &str, candidates: &[Psd2Bank]) -> Option<Psd2Bank> {
    let want = match_key(bank_name);
    if want.len() < 2 {
        return None;
    }
    let generic: Vec<&Psd2Bank> = candidates
        .iter()
        .filter(|b| b.connector.is_none())
        .collect();
    let keys = |b: &Psd2Bank| [match_key(&b.name), match_key(&b.aspsp)];
    let pick = |hits: Vec<&Psd2Bank>| -> Option<Psd2Bank> {
        // one bank can be listed in several countries: prefer Germany
        let de: Vec<&&Psd2Bank> = hits.iter().filter(|b| b.country == "DE").collect();
        match (de.as_slice(), hits.as_slice()) {
            ([one], _) => Some((**one).clone()),
            ([], [one]) => Some((*one).clone()),
            _ => None,
        }
    };
    let exact: Vec<&Psd2Bank> = generic
        .iter()
        .copied()
        .filter(|b| keys(b).contains(&want))
        .collect();
    if !exact.is_empty() {
        return pick(exact);
    }
    let fuzzy: Vec<&Psd2Bank> = generic
        .iter()
        .copied()
        .filter(|b| {
            keys(b)
                .iter()
                .any(|k| k.len() >= 3 && (k.contains(&want) || want.contains(k.as_str())))
        })
        .collect();
    pick(fuzzy)
}

/// `match_psd2_bank` for the app: matches a MoneyMoney bank name against the
/// live Enable Banking list (`list_enable_banking_aspsps`).
pub fn match_psd2_bank_by_name(bank_name: String, candidates: Vec<Psd2Bank>) -> Option<Psd2Bank> {
    match_psd2_bank(&bank_name, &candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(r: &[BankSearchResult]) -> Vec<String> {
        r.iter().map(|b| b.id.clone()).collect()
    }

    #[test]
    fn advanzia_is_not_offered_through_enable_banking() {
        // Enable Banking answers 422 "Wrong ASPSP name provided" for
        // Advanzia (Mastercard Gold) – verified with a real account.
        assert!(psd2_catalog().iter().all(|b| !b.name.contains("Advanzia")));
        assert_eq!(match_psd2_bank("Advanzia Bank", &psd2_catalog()), None);
    }

    #[test]
    fn psd2_match_by_bank_name() {
        let cat = psd2_catalog();
        let m = |n: &str| match_psd2_bank(n, &cat).map(|b| b.aspsp);
        assert_eq!(m("C24 Bank").as_deref(), Some("C24"));
        assert_eq!(m("C24 Bank GmbH").as_deref(), Some("C24"));
        assert_eq!(m("n26").as_deref(), Some("N26"));
        // own connectors (Amex, Amazon Visa) are not generic PSD2 banks
        assert_eq!(m("American Express"), None);
        assert_eq!(m("Sparkasse Musterstadt"), None);
        assert_eq!(m(""), None);
        // ambiguous: several banks fit → no guess
        let two = vec![
            psd2("Muster Bank Nord", "DE", "Muster Nord", None),
            psd2("Muster Bank Süd", "DE", "Muster Süd", None),
        ];
        assert_eq!(match_psd2_bank("Muster", &two), None);
        // the same bank in several countries: Germany wins
        let multi = vec![
            psd2("Revolut", "LT", "Revolut", None),
            psd2("Revolut", "DE", "Revolut", None),
        ];
        assert_eq!(match_psd2_bank("Revolut", &multi).unwrap().country, "DE");
    }

    #[test]
    fn catalog_has_the_popular_psd2_banks() {
        let c = psd2_catalog();
        for n in [
            "N26",
            "Revolut",
            "C24",
            "Tomorrow",
            "Vivid",
            "Wise",
            "American Express",
            "Barclays",
            "Hanseatic",
            "Santander",
            "Commerzbank",
            "Raiffeisen",
            "Erste",
        ] {
            assert!(c.iter().any(|b| b.name.contains(n)), "{n}");
        }
        for b in &c {
            assert_eq!(b.country.len(), 2);
            assert!(!b.aspsp.is_empty() && !b.live);
        }
        assert_eq!(search_psd2_catalog("revolut".into()).len(), 2);
        assert!(
            search_psd2_catalog("österreich erste".into())
                .iter()
                .all(|b| b.country == "AT")
        );
        // Amex / Amazon Visa keep their dedicated connectors
        let amex = c.iter().find(|b| b.name == "American Express").unwrap();
        assert_eq!(amex.connector, Some(ConnectorKind::AmericanExpress));
    }

    #[test]
    fn name_search_groups_by_access_mode() {
        let r = search("n26", vec![], 10);
        assert!(r.iter().all(|b| b.mode == BankAccessMode::Psd2));
        assert!(ids(&r).contains(&"psd2:DE:N26".to_string()));
        let tr_res = search("trade republic", vec![], 10);
        assert!(ids(&tr_res).contains(&"psd2:DE:Trade Republic".to_string()));

        let r = search("bunq", vec![], 10);
        assert_eq!(ids(&r), vec!["api:bunq"]);
        assert_eq!(r[0].mode, BankAccessMode::OwnApi);
        assert_eq!(r[0].connector, Some(ConnectorKind::Bunq));

        for (q, kind) in [
            ("trade republic", ConnectorKind::TradeRepublic),
            ("PayPal", ConnectorKind::PayPal),
            ("comdirect", ConnectorKind::Comdirect),
            ("Scalable Capital", ConnectorKind::ScalableCapital),
            ("scalable", ConnectorKind::ScalableCapital),
            ("Interactive Brokers", ConnectorKind::Ibkr),
            ("ibkr", ConnectorKind::Ibkr),
        ] {
            let r = search(q, vec![], 10);
            assert!(
                r.iter()
                    .any(|b| b.mode == BankAccessMode::OwnApi && b.connector == Some(kind)),
                "{q}"
            );
        }

        let r = search("sparkasse", vec![], 5);
        assert_eq!(r.len(), 5);
        assert!(
            r.iter()
                .all(|b| b.mode == BankAccessMode::FinTs && b.url.starts_with("https://"))
        );

        let r = search("ING", vec![], 10);
        assert!(
            r.iter()
                .any(|b| b.mode == BankAccessMode::FinTs && b.blz == "50010517")
        );
        assert!(
            r.iter()
                .any(|b| b.mode == BankAccessMode::Psd2 && b.name == "ING Business")
        );
        assert!(search("DKB", vec![], 5).iter().any(|b| b.blz == "12030000"));
        assert!(
            search("volksbank", vec![], 5)
                .iter()
                .all(|b| b.mode == BankAccessMode::FinTs)
        );
        assert!(search("postbank", vec![], 3).len() == 3);
        assert!(search("", vec![], 5).is_empty());
        assert!(search("zzzzqqq", vec![], 5).is_empty());
    }

    #[test]
    fn blz_and_iban_queries_find_exactly_one_bank() {
        let r = search("50010517", vec![], 10);
        assert_eq!(ids(&r), vec!["fints:50010517"]);
        assert_eq!(r[0].name, "ING");
        let r = search("DE41 5001 0517 0123 4567 89", vec![], 10);
        assert_eq!(ids(&r), vec!["fints:50010517"]);
        // valid IBAN of a bank that is not in the directory: nothing to offer
        assert!(search("DE89 3704 0044 0532 0130 00", vec![], 10).is_empty());
    }

    #[test]
    fn deutsche_bank_is_found_by_name_bank_code_and_bic() {
        let blzs = ["10070000", "50070010", "50070024"];
        // name
        // the directory holds hundreds of branches named "Deutsche Bank …"
        let r = search("Deutsche Bank", vec![], 1000);
        for blz in blzs {
            let b = r.iter().find(|b| b.blz == blz);
            assert!(b.is_some(), "name: {blz}");
            let b = b.unwrap();
            assert_eq!(b.mode, BankAccessMode::FinTs);
            assert_eq!(b.url, "https://fints.deutsche-bank.de/");
        }
        // bank code
        for blz in blzs {
            let r = search(blz, vec![], 10);
            assert_eq!(ids(&r), vec![format!("fints:{blz}")], "blz {blz}");
            assert!(r[0].name.starts_with("Deutsche Bank"));
        }
        // BIC, with and without branch code, any case
        for q in ["DEUTDEFFXXX", "deutdeff", "DEUTDEFF"] {
            let r = search(q, vec![], 500);
            assert!(r.iter().any(|b| b.blz == "50070010"), "bic {q}");
            assert!(
                r.iter().all(|b| b.name.contains("Deutsche Bank")),
                "bic {q}"
            );
        }
        let r = search("DEUTDEDBXXX", vec![], 500);
        assert!(r.iter().any(|b| b.blz == "50070024"), "bic DEUTDEDBXXX");
        // a word of eight letters is no BIC
        assert!(!search("deutsche", vec![], 5).is_empty());
    }

    #[test]
    fn miles_and_more_card_is_found_by_name_and_bank_code() {
        for q in ["miles", "miles more", "lufthansa", "50070439"] {
            let r = search(q, vec![], 12);
            let c = r.iter().find(|b| b.blz == "50070439");
            assert!(c.is_some_and(|b| b.name.contains("Miles & More")), "{q}");
        }
    }

    #[test]
    fn consorsbank_is_found_by_former_names() {
        for q in ["consors", "dab", "bnp paribas", "76030080"] {
            let r = search(q, vec![], 12);
            let c = r.iter().find(|b| b.blz == "76030080");
            assert!(c.is_some(), "{q}");
        }
    }

    #[test]
    fn comdirect_blz_offers_fints_and_its_own_api() {
        for (q, blz) in [
            ("20041111", "20041111"),
            ("DE10 2004 1155 0123 4567 89", "20041155"),
        ] {
            let r = search(q, vec![], 10);
            assert_eq!(
                ids(&r),
                vec![format!("fints:{blz}"), "api:comdirect".to_string()],
                "{q}"
            );
            assert_eq!(r[0].mode, BankAccessMode::FinTs);
            assert_eq!((r[0].name.as_str(), r[0].blz.as_str()), ("comdirect", blz));
            assert_eq!(r[0].url, fints_directory::COMDIRECT_FINTS_URL);
            assert_eq!(r[1].mode, BankAccessMode::OwnApi);
        }
    }

    #[test]
    fn comdirect_by_name_lists_fints_once_without_a_bank_code() {
        let r = search("comdirect", vec![], 10);
        assert_eq!(ids(&r), vec!["fints:comdirect", "api:comdirect"]);
        // the bank code depends on the account: the form asks for BLZ or IBAN
        assert!(r[0].blz.is_empty());
        assert_eq!(r[0].url, fints_directory::COMDIRECT_FINTS_URL);
        assert_eq!(r[0].name, r[1].name);
        // unrelated searches are unaffected
        assert!(
            search("paypal", vec![], 10)
                .iter()
                .all(|b| b.id != "fints:comdirect")
        );
    }

    #[test]
    fn paypal_offers_direct_api_and_psd2_without_live_catalog() {
        let results = search("PayPal", vec![], 10);
        assert!(results.iter().any(|bank| {
            bank.mode == BankAccessMode::OwnApi && bank.connector == Some(ConnectorKind::PayPal)
        }));
        assert!(results.iter().any(|bank| {
            bank.mode == BankAccessMode::Psd2
                && bank.aspsp == "PayPal"
                && bank.country == "DE"
                && bank.connector.is_none()
        }));
    }

    #[test]
    fn equateplus_routes_employee_plan_searches_to_browser() {
        for query in [
            "EquatePlus",
            "equate plus",
            "equatex",
            "mitarbeiteraktien",
            "rsu",
        ] {
            let result = search(query, vec![], 20);
            assert!(
                result
                    .iter()
                    .any(|r| r.id == "api:equateplus"
                        && r.connector == Some(ConnectorKind::EquatePlus)),
                "{query}"
            );
        }
    }

    #[test]
    fn quirion_routes_names_blz_and_iban_to_browser() {
        for query in [
            "quirion",
            "Quirin Privatbank",
            "10110600",
            "DE98101106000000000000",
        ] {
            let r = search(query, vec![], 20);
            assert_eq!(ids(&r), vec!["api:quirion"], "{query}");
            assert_eq!(r[0].connector, Some(ConnectorKind::Quirion));
        }
    }

    #[test]
    fn fnz_routes_names_blz_and_iban_to_its_direct_portal() {
        for query in [
            "FNZ",
            "FNZ Bank",
            "ebase",
            "finvesto",
            "70013000",
            "700 130 00",
            "DE19 7001 3000 0123 4567 89",
        ] {
            let r = search(query, vec![], 10);
            assert_eq!(ids(&r), vec!["api:fnz"], "{query}");
            assert_eq!(r[0].mode, BankAccessMode::OwnApi);
            assert_eq!(r[0].connector, Some(ConnectorKind::Fnz));
        }
        assert!(search("70013000", vec![], 0).is_empty());
    }

    #[test]
    fn c24_is_one_entry_with_its_verification_note() {
        // the live list spells it "C24"; the catalog must match, or C24 shows twice
        let live = vec![Psd2Bank {
            name: "C24".into(),
            country: "DE".into(),
            aspsp: "C24".into(),
            connector: None,
            live: true,
            tested: String::new(),
        }];
        let r = search("c24", live, 10);
        let c24: Vec<_> = r
            .iter()
            .filter(|b| b.mode == BankAccessMode::Psd2)
            .collect();
        assert_eq!(c24.len(), 1, "{:?}", ids(&r));
        assert_eq!(c24[0].aspsp, "C24");
        assert!(c24[0].tested.contains("Pocket"));
        // untested banks carry no note
        let n26 = search("n26", vec![], 10);
        assert!(n26.iter().all(|b| b.tested.is_empty()));
    }

    #[test]
    fn live_list_adds_banks_and_fixes_spelling() {
        let live = vec![
            Psd2Bank {
                name: "Sparebank 1".into(),
                country: "NO".into(),
                aspsp: "Sparebank 1".into(),
                connector: None,
                live: true,
                tested: String::new(),
            },
            Psd2Bank {
                name: "n26".into(),
                country: "DE".into(),
                aspsp: "N26 Bank".into(),
                connector: None,
                live: true,
                tested: String::new(),
            },
        ];
        let r = search("sparebank", live.clone(), 10);
        assert_eq!(ids(&r), vec!["psd2:NO:Sparebank 1"]);
        // a live entry with a different spelling replaces nothing: matched by
        // (country, ASPSP) case-insensitively, so "n26" merges into the catalog row
        let live_same = vec![Psd2Bank {
            name: "N26".into(),
            country: "DE".into(),
            aspsp: "n26".into(),
            connector: None,
            live: true,
            tested: String::new(),
        }];
        let r = search("n26", live_same, 10);
        let de: Vec<_> = r.iter().filter(|b| b.country == "DE").collect();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0].aspsp, "n26"); // live spelling wins
        let _ = live;
    }

    #[test]
    fn live_response_parsing() {
        let v = json!({"aspsps": [
            {"name": "N26", "country": "de", "psu_types": ["personal", "business"]},
            {"name": "Only Business", "country": "DE", "psu_types": ["business"]},
            {"name": "No Types", "country": "AT"}
        ]});
        let b = live_banks(&v);
        // business-only banks (e.g. Finom) are listed too
        assert_eq!(b.len(), 3);
        assert!(b.iter().any(|x| x.name == "Only Business"));
        assert_eq!(b[0].country, "DE");
        assert!(b.iter().all(|x| x.live));
        assert!(live_banks(&json!({})).is_empty());
    }

    #[test]
    fn alte_leipziger_routes_names_blz_and_iban_to_fin4u() {
        for query in [
            "Alte Leipziger",
            "bausparvertrag",
            "fin4u",
            "50012800",
            "DE18 5001 2800 0123 4567 89",
        ] {
            let r = search(query, vec![], 10);
            assert!(
                r.iter().any(|b| b.mode == BankAccessMode::OwnApi
                    && b.connector == Some(ConnectorKind::AlteLeipziger)),
                "{query}"
            );
        }
        assert_eq!(ids(&search("50012800", vec![], 10)), vec!["api:fin4u"]);
    }

    #[test]
    fn schwaebisch_hall_routes_names_and_blz_to_browser() {
        for query in [
            "Schwäbisch Hall",
            "schwaebisch hall",
            "Bausparvertrag",
            "BSH",
            "62220000",
        ] {
            let r = search(query, vec![], 20);
            assert!(
                r.iter().any(|b| b.mode == BankAccessMode::OwnApi
                    && b.connector == Some(ConnectorKind::SchwaebischHall)),
                "{query}"
            );
        }
        assert_eq!(
            ids(&search("62220000", vec![], 10)),
            vec!["api:schwaebischhall"]
        );
    }
}
