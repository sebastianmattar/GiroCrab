//! Bank and user parameter data (BPD/UPD): what the bank offers (segment
//! versions, TAN methods, order parameters) and which accounts the user has,
//! learned from the replies; picks the segment and pain.001 versions to send.

use std::collections::{HashMap, HashSet};

use super::syntax::Segment;
use super::{BankReply, FinTsConnector, UpdAccount, debit, sepa, standing_order, transfer};

/// Versions of each business transaction this client implements.
const IMPLEMENTED: &[(&str, &[u32])] = &[
    ("HKSAL", &[5, 6, 7, 8]),
    ("HKCAZ", &[1]),
    ("HKCCS", &[1]),
    ("HKIPZ", &[1]),
    ("HKCSE", &[1]),
    ("HKCCM", &[1]),
    ("HKCDB", &[1, 2]),
    ("HKCDE", &[1]),
    ("HKCDN", &[1]),
    ("HKCDL", &[1, 2]),
    ("HKDSE", &[1]),
    ("HKDME", &[1]),
    ("HKDSC", &[1]),
    ("HKDMC", &[1]),
    ("HKBSE", &[1]),
    ("HKBME", &[1]),
    ("HKVPP", &[1]),
    ("HKKAZ", &[5, 6, 7]),
    ("HKTAN", &[6, 7]),
    ("HKSPA", &[1, 2, 3]),
    ("HKTAB", &[4, 5]),
    ("HKEKA", &[3, 4, 5]),
    ("HKEKP", &[1, 2]),
    ("HKQTG", &[1]),
    ("HKWPD", &[5, 6]),
    ("HKKKU", &[1]),
    // card bookings, predecessor of HKKKU (comdirect, #807)
    ("DKKKU", &[2]),
];

pub(super) fn is_segment_code(s: &str) -> bool {
    s.len() == 5
        && (s.starts_with("HK") || s.starts_with("DK"))
        && s[2..].chars().all(|c| c.is_ascii_uppercase())
}

/// Parses one HIUPD segment (v4–v6).
pub(super) fn parse_upd(upd: &Segment) -> UpdAccount {
    let mut a = UpdAccount {
        account_number: upd.get(1, 0).to_string(),
        sub_account: upd.get(1, 1).to_string(),
        blz: upd.get(1, 3).to_string(),
        iban: upd.get(2, 0).to_string(),
        kind_code: upd.get(4, 0).parse().unwrap_or(1),
        currency: upd.get(5, 0).to_string(),
        owner: format!("{} {}", upd.get(6, 0), upd.get(7, 0))
            .trim()
            .to_string(),
        product: upd.get(8, 0).to_string(),
        allowed: HashSet::new(),
    };
    // HIUPD v4/v5 have no IBAN element: shift if element 2 is the customer id
    if !a.iban.is_empty()
        && !(a.iban.len() >= 15 && a.iban.chars().take(2).all(|c| c.is_ascii_uppercase()))
    {
        a.iban.clear();
    }
    for el in upd.elements.iter().skip(9) {
        if let Some(first) = el.first()
            && is_segment_code(first.text())
        {
            a.allowed.insert(first.text().to_string());
        }
    }
    a
}

/// pain.001 descriptors from a BPD parameter segment (HICCSS / HIIPZS / HICSES).
pub(super) fn pain_formats(reply: &BankReply, seg_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in reply.all(seg_id) {
        for el in &seg.elements {
            for v in el {
                let t = v.text();
                if t.starts_with("urn:iso:std:iso:20022:tech:xsd:pain.00")
                    && !t.ends_with(".xsd")
                    && !out.iter().any(|f| f == t)
                {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// Picks the TAN function from the codes allowed by 3920: app/decoupled
/// methods first (SecureGo plus, pushTAN …), then a fixed preference list,
/// never 999 (one-step).
fn choose_tan_function(allowed: &[String], names: &HashMap<String, String>) -> String {
    let by_name = allowed.iter().find(|a| {
        let n = names.get(*a).map(|s| s.to_lowercase()).unwrap_or_default();
        [
            "decoupled",
            "securego",
            "pushtan",
            "push",
            "app",
            "banking-app",
        ]
        .iter()
        .any(|k| n.contains(k))
    });
    if let Some(a) = by_name {
        return a.clone();
    }
    for pref in [
        "946", "944", "942", "921", "962", "972", "982", "920", "923", "913", "912", "911", "910",
        "900",
    ] {
        if allowed.iter().any(|a| a == pref) {
            return pref.to_string();
        }
    }
    allowed.first().cloned().unwrap_or_else(|| "999".into())
}

impl FinTsConnector {
    /// Highest version of `tx` that both the bank (BPD) and this client support.
    fn negotiated(&self, tx: &str) -> Option<u32> {
        let ours = IMPLEMENTED
            .iter()
            .find(|(k, _)| *k == tx)
            .map(|(_, v)| *v)
            .unwrap_or(&[]);
        self.st
            .versions
            .get(tx)
            .and_then(|vs| vs.iter().copied().filter(|v| ours.contains(v)).max())
    }

    /// Like `negotiated`, but falls back to our highest version when the bank
    /// sent no parameters for it (e.g. before the first BPD).
    pub(super) fn version(&self, tx: &str) -> u32 {
        self.negotiated(tx).unwrap_or_else(|| {
            IMPLEMENTED
                .iter()
                .find(|(k, _)| *k == tx)
                .and_then(|(_, v)| v.iter().max().copied())
                .unwrap_or(1)
        })
    }

    /// Whether the transfer document should be `pain.001.001.09` (true) or `.03` (false).
    /// Instant payments (HKIPZ) are always `.09`. Otherwise the BPD descriptors decide:
    /// `.09` whenever advertised, else `.03`; nothing advertised (or only formats we
    /// cannot produce) falls back to `.03`.
    pub(super) fn pain_version(&self, code: &str) -> Option<sepa::PainVersion> {
        let formats = self.st.pain_formats.get(code);
        if code == "HKIPZ" {
            return sepa::PainVersion::pick(formats.map(Vec::as_slice).unwrap_or(&[]));
        }
        // .09 whenever offered: banks that still list .03 may refuse it
        // (comdirect: "Sepa-Format nicht mehr gültig").
        let has = |suffix: &str| formats.is_some_and(|l| l.iter().any(|f| f.ends_with(suffix)));
        Some(if has(".001.09") {
            sepa::PainVersion::V09
        } else {
            sepa::PainVersion::V03
        })
    }

    pub(super) fn bank_supports(&self, tx: &str) -> bool {
        self.negotiated(tx).is_some()
    }

    pub(super) fn learn_bpd(&mut self, reply: &BankReply) {
        if crate::depot_debug::enabled() {
            // Which business transactions the bank offers at all (segment names and
            // versions only), incl. bank-specific ones such as DIKKUS (card statements).
            let mut offered: Vec<String> = reply
                .segments
                .iter()
                .filter(|s| {
                    let id = s.id();
                    id.len() == 6
                        && id.ends_with('S')
                        && (id.starts_with("HI") || id.starts_with("DI"))
                })
                .map(|s| format!("{}:{}", s.id(), s.version()))
                .collect();
            if offered.len() > 5 {
                offered.sort();
                offered.dedup();
                crate::depot_debug::log(&format!("fints BPD offers: {}", offered.join(" ")));
            }
        }
        for s in &reply.segments {
            let id = s.id();
            // Bank-specific parameter segments (DI…S) announce DK… business
            // transactions; only those GiroCrab sends (`IMPLEMENTED`) are ever used.
            if id.starts_with("DI") && id.ends_with('S') && id.len() == 6 {
                let tx = format!("DK{}", &id[2..5]);
                let v = self.st.versions.entry(tx).or_default();
                if !v.contains(&s.version()) {
                    v.push(s.version());
                }
                if id == "DIKKUS" || id == "DIKKSS" {
                    // bank parameters only (same for every customer)
                    let params: Vec<String> = s
                        .elements
                        .iter()
                        .skip(4)
                        .map(|el| el.iter().map(|v| v.text()).collect::<Vec<_>>().join(":"))
                        .collect();
                    crate::depot_debug::log(&format!(
                        "fints BPD {id} v{} params={}",
                        s.version(),
                        params.join("+")
                    ));
                }
            }
            if id.starts_with("HI") && id.ends_with('S') && id.len() == 6 {
                let tx = format!("HK{}", &id[2..5]);
                let v = self.st.versions.entry(tx).or_default();
                if !v.contains(&s.version()) {
                    v.push(s.version());
                }
                if id == "HIKAZS" || id == "HIKKUS" || id == "HIKKSS" {
                    // bank parameters only: storage period (days), flags
                    let params: Vec<String> = s
                        .elements
                        .iter()
                        .skip(4)
                        .map(|el| el.iter().map(|v| v.text()).collect::<Vec<_>>().join(":"))
                        .collect();
                    crate::depot_debug::log(&format!(
                        "fints BPD {id} v{} params={}",
                        s.version(),
                        params.join("+")
                    ));
                }
                if id == "HIEKAS" {
                    // bank parameters only (formats, flags) – no customer data
                    let params: Vec<String> = s
                        .elements
                        .iter()
                        .skip(4)
                        .map(|el| el.iter().map(|v| v.text()).collect::<Vec<_>>().join(":"))
                        .collect();
                    crate::depot_debug::log(&format!(
                        "statements: BPD HIEKAS v{} params={}",
                        s.version(),
                        params.join("+")
                    ));
                }
            }
            if id == "HIPINS" {
                // last element: list of (segment, J/N) pairs after 5 PIN fields
                if let Some(el) = s.elements.last() {
                    let texts: Vec<&str> = el.iter().map(|v| v.text()).collect();
                    for pair in texts.windows(2) {
                        if (pair[0].starts_with("HK") || pair[0].starts_with("DK"))
                            && pair[0].len() == 5
                            && (pair[1] == "J" || pair[1] == "N")
                        {
                            self.st
                                .tan_required
                                .insert(pair[0].to_string(), pair[1] == "J");
                        }
                    }
                }
            }
        }
        // HIKKUS: storage days : max entries allowed : date range allowed : account required
        // DIKKUS (DKKKU, the predecessor; comdirect) is read the same way: a value that
        // does not fit leaves the default (unknown period, date range, account).
        if let Some(p) = reply
            .all("HIKKUS")
            .last()
            .or_else(|| reply.all("DIKKUS").last())
        {
            self.st.kku_storage_days = p.get(4, 0).parse().unwrap_or(0);
            self.st.kku_no_date_range = p.get(4, 2) == "N";
            self.st.kku_no_account = p.get(4, 3) == "N";
        }
        if crate::depot_debug::enabled() && reply.all("HIPINS").next().is_some() {
            let card: Vec<String> = ["HKKKU", "DKKKU"]
                .iter()
                .filter_map(|t| self.st.tan_required.get(*t).map(|r| format!("{t}={r}")))
                .collect();
            crate::depot_debug::log(&format!("fints HIPINS cards: [{}]", card.join(",")));
        }
        for cazs in reply.all("HICAZS") {
            for el in &cazs.elements {
                for v in el {
                    let t = v.text();
                    if t.contains("camt.052") && !self.st.camt_formats.iter().any(|f| f == t) {
                        self.st.camt_formats.push(t.to_string());
                    }
                }
            }
            // best (newest) format first: …camt.052.001.08 > …001.02
            self.st.camt_formats.sort_by(|a, b| b.cmp(a));
        }
        for (seg, tx) in [
            ("HICCSS", "HKCCS"),
            ("HIIPZS", "HKIPZ"),
            ("HICSES", "HKCSE"),
            ("HICCMS", "HKCCM"),
            ("HICDBS", "HKCDB"),
            ("HICDES", "HKCDE"),
            ("HICDNS", "HKCDN"),
            ("HICDLS", "HKCDL"),
            ("HIDSES", "HKDSE"),
            ("HIDMES", "HKDME"),
            ("HIDSCS", "HKDSC"),
            ("HIDMCS", "HKDMC"),
            ("HIBSES", "HKBSE"),
            ("HIBMES", "HKBME"),
        ] {
            // Merge across BPD messages (anonymous and authenticated dialog).
            for f in pain_formats(reply, seg) {
                let list = self.st.pain_formats.entry(tx.to_string()).or_default();
                if !list.contains(&f) {
                    list.push(f);
                }
            }
        }
        // FinTS 3.0 banks (e.g. comdirect, #1419) advertise supported SEPA schemas
        // centrally in HISPAS while HICCSS carries no format list of its own.
        for spas in reply.all("HISPAS") {
            for el in &spas.elements {
                for v in el {
                    let t = v.text();
                    let targets: &[&str] = if t.contains("pain.001.") {
                        &["HKCCS", "HKCSE", "HKCCM", "HKCDE", "HKCDN", "HKCDL"]
                    } else if t.contains("pain.008.") {
                        &["HKDSE", "HKDME", "HKDSC", "HKDMC", "HKBSE", "HKBME"]
                    } else {
                        continue;
                    };
                    for tx in targets {
                        let list = self.st.pain_formats.entry((*tx).to_string()).or_default();
                        let norm = t.strip_suffix(".xsd").unwrap_or(t);
                        if !list.iter().any(|f| f == norm) {
                            list.push(norm.to_string());
                        }
                    }
                }
            }
        }
        if let Some(seg) = reply.first("HICCMS") {
            self.st.batch_params = Some(transfer::parse_batch_params(seg));
        }
        if let Some(seg) = reply.first("HICDES") {
            self.st.standing_params.create = Some(standing_order::parse_create_rules(seg));
        }
        if let Some(seg) = reply.first("HICDNS") {
            self.st.standing_params.change = Some(standing_order::parse_change_rules(seg));
        }
        for (seg, tx) in [
            ("HIDMES", "HKDME"),
            ("HIDMCS", "HKDMC"),
            ("HIBMES", "HKBME"),
        ] {
            if let Some(s) = reply.first(seg) {
                self.st
                    .debit_params
                    .insert(tx.to_string(), debit::parse_debit_params(s));
            }
        }
        if let Some(vpps) = reply.first("HIVPPS") {
            let (format, orders) = transfer::vop_parameters(vpps);
            self.st.vop_format = format;
            self.st.vop_orders = orders;
        }
        let upds: Vec<UpdAccount> = reply.all("HIUPD").map(parse_upd).collect();
        for a in upds {
            if let Some(existing) = self
                .upd
                .iter_mut()
                .find(|e| e.account_number == a.account_number && e.sub_account == a.sub_account)
            {
                *existing = a;
            } else {
                self.upd.push(a);
            }
        }
        if let Some(sync) = reply.first("HISYN") {
            let id = sync.get(1, 0);
            if !id.is_empty() {
                self.st.system_id = id.to_string();
            }
        }
        // Allowed TAN methods for this user.
        if let Some(c) = reply.code("3920") {
            let allowed: Vec<String> =
                c.3.iter()
                    .filter(|p| !p.is_empty() && p.as_str() != "999")
                    .cloned()
                    .collect();
            let names = self.tan_method_names(reply);
            if !allowed.is_empty()
                && (self.st.sec_func.is_empty() || !allowed.contains(&self.st.sec_func))
            {
                // Prefer app based / decoupled methods.
                let pick = choose_tan_function(&allowed, &names);
                self.st.tan_method_name = names.get(&pick).cloned().unwrap_or_default();
                self.st.sec_func = pick;
            }
        }
        if let (Some(hitab), true) = (reply.first("HITAB"), self.tan_medium_override.is_empty()) {
            for el in hitab.elements.iter().skip(2) {
                let texts: Vec<&str> = el.iter().map(|v| v.text()).collect();
                if texts.get(1) == Some(&"1")
                    && let Some(name) = texts
                        .iter()
                        .skip(2)
                        .find(|t| t.chars().any(|c| c.is_alphabetic()) && t.len() > 2)
                {
                    self.st.tan_medium = name.to_string();
                    break;
                }
            }
        }
    }

    /// code -> human name, from HITANS (v6/v7: code, process, tech id, ZKA name, ZKA version, name …).
    fn tan_method_names(&self, reply: &BankReply) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for s in reply.all("HITANS") {
            if let Some(el) = s.elements.get(4) {
                let t: Vec<&str> = el.iter().map(|v| v.text()).collect();
                for i in 0..t.len().saturating_sub(5) {
                    if t[i].len() == 3
                        && t[i].starts_with('9')
                        && (t[i + 1] == "1" || t[i + 1] == "2")
                    {
                        out.insert(t[i].to_string(), t[i + 5].to_string());
                    }
                }
            }
        }
        out
    }
}
