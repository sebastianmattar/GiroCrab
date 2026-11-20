//! Strict parser; errors contain no bank-provided values.
use super::super::RemoteTransaction;
use super::*;
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Entry {
    date: String,
    cents: i64,
    name: String,
    purpose: String,
    pending: bool,
    pub(super) id: String,
    pub(super) legacy_id: String,
}
impl Entry {
    pub(super) fn remote(self, account_ref: &str) -> RemoteTransaction {
        RemoteTransaction {
            account_ref: account_ref.into(),
            external_id: self.id,
            booking_date: self.date.clone(),
            value_date: self.date,
            amount_cents: self.cents,
            currency: "EUR".into(),
            counterparty: self.name,
            purpose: self.purpose,
            booking_text: if self.pending {
                "Kartenumsatz (vorgemerkt)"
            } else {
                "Kartenumsatz"
            }
            .into(),
            ..Default::default()
        }
    }
}
fn cents(v: &Value) -> CoreResult<i64> {
    if !v.is_number() {
        return Err(invalid("Ungültiger Betrag."));
    }
    crate::money::json_number_to_cents(v).map_err(|_| invalid("Ungültiger Betrag."))
}
pub(super) fn parse_balance(raw: &str) -> CoreResult<i64> {
    if raw.len() > 48 {
        return Err(invalid("Ungültiger Bankkontostand."));
    }
    let n: Value = serde_json::from_str(raw).map_err(|_| invalid("Ungültiger Bankkontostand."))?;
    let c = cents(&n)?;
    if !(-10_000_000_000..=10_000_000_000).contains(&c) {
        return Err(invalid("Ungültiger Bankkontostand."));
    }
    Ok(c)
}
fn parse(raw: &str) -> CoreResult<Vec<Entry>> {
    if raw.len() > 3_000_000 {
        return Err(invalid("Antwort zu groß."));
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| invalid("Ungültige Umsatzantwort."))?;
    let items = root["transactions"]
        .as_array()
        .ok_or_else(|| invalid("Umsatzliste fehlt."))?;
    if items.len() > 1000 {
        return Err(invalid("Antwort zu groß."));
    }
    let mut entries = Vec::new();
    for v in items {
        let pending = match v["status"]
            .as_str()
            .unwrap_or("")
            .to_ascii_uppercase()
            .as_str()
        {
            "PENDING" => true,
            "APPROVED" => false,
            "DECLINED" | "CANCELLED" | "CANCELED" | "REJECTED" => continue,
            _ => return Err(invalid("Unbekannter Buchungsstatus.")),
        };
        if v["currency"].as_str() != Some("EUR") {
            return Err(invalid("Unerwartete Buchungswährung."));
        }
        let date = v["transactionDate"]
            .as_str()
            .and_then(|s| s.get(..10))
            .ok_or_else(|| invalid("Ungültiges Buchungsdatum."))?;
        jiff::civil::Date::strptime("%Y-%m-%d", date)
            .map_err(|_| invalid("Ungültiges Buchungsdatum."))?;
        let text = |key: &str| v[key].as_str().map(str::trim).filter(|s| !s.is_empty());
        let name = text("merchantName")
            .or_else(|| text("text"))
            .ok_or_else(|| invalid("Händlername fehlt."))?;
        let reference = if pending {
            text("uniqueReference")
        } else {
            text("reference").or_else(|| text("uniqueReference"))
        }
        .ok_or_else(|| invalid("Buchungsreferenz fehlt."))?;
        // SHA-256 is already a core dependency. IDs expose no bank reference.
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(reference.as_bytes()));
        entries.push(Entry {
            date: date.into(),
            cents: cents(&v["amount"])?,
            name: name.into(),
            purpose: text("text").unwrap_or("").into(),
            pending,
            legacy_id: compatibility::legacy_id(pending, reference),
            id: format!(
                "{}{}",
                if pending {
                    HOLD_PREFIX
                } else {
                    "advanzia-post-"
                },
                digest
            ),
        });
    }
    Ok(entries)
}
pub(super) fn parse_captures(bodies: &[String]) -> CoreResult<Vec<Entry>> {
    if bodies.is_empty()
        || bodies.len() > 37
        || bodies.iter().map(String::len).sum::<usize>() > 12_000_000
    {
        return Err(invalid("Unvollständige oder zu große Umsatzantwort."));
    }
    let mut entries: HashMap<String, Entry> = HashMap::new();
    for (i, body) in bodies.iter().enumerate() {
        for e in parse(body)? {
            if e.pending && i + 1 != bodies.len() {
                continue;
            }
            if let Some(old) = entries.get(&e.id) {
                if old != &e {
                    return Err(invalid("Widersprüchliche Buchungsreferenz."));
                }
            } else {
                entries.insert(e.id.clone(), e);
            }
        }
    }
    let mut result: Vec<_> = entries.into_values().collect();
    result.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    Ok(result)
}
