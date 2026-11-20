//! FinTS 3.0 bank presets for the "add account" sheet. Users can pick one or
//! enter any 8-digit BLZ + https FinTS URL by hand ("custom").

/// A bank offered in the add-account sheet. Empty `blz` means the user must
/// enter it (e.g. one of many Volksbanken); empty `url` means "enter manually".
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FinTsPreset {
    pub id: String,
    pub name: String,
    pub blz: String,
    pub url: String,
    pub note: String,
}

const ATRUVIA: &str = "https://fints1.atruvia.de/cgi-bin/hbciservlet";

fn p(id: &str, name: &str, blz: &str, url: &str, note: &str) -> FinTsPreset {
    FinTsPreset {
        id: id.into(),
        name: name.into(),
        blz: blz.into(),
        url: url.into(),
        note: note.into(),
    }
}

pub fn fints_presets() -> Vec<FinTsPreset> {
    vec![
        p(
            "postbank",
            "Postbank",
            "10010010",
            "https://hbci.postbank.de/banking/hbci.do",
            "BLZ vorbelegt (Berlin) – ändern Sie sie auf Ihre regionale Postbank-BLZ, z. B. 20010020, 37010050, 50010060 oder 70010080.",
        ),
        p(
            "sparda-berlin",
            "Sparda-Bank Berlin eG",
            "12096597",
            ATRUVIA,
            "",
        ),
        p(
            "sparda",
            "Sparda-Banken (Atruvia) – andere BLZ",
            "",
            ATRUVIA,
            "BLZ Ihrer Sparda-Bank eintragen – die Server-URL wird automatisch passend gesetzt.",
        ),
        p(
            "volksbank",
            "Volks- und Raiffeisenbanken (Atruvia)",
            "",
            ATRUVIA,
            "BLZ Ihrer Volksbank/Raiffeisenbank eintragen.",
        ),
        p(
            "vb-kassel",
            "Volksbank Kassel Göttingen",
            "52090000",
            ATRUVIA,
            "",
        ),
        p(
            "1822direkt",
            "1822direkt",
            "50050201",
            "https://banking-hs6.s-fints-pt-hs.de/fints30",
            "",
        ),
        p(
            "dkb",
            "DKB",
            "12030000",
            "https://fints.dkb.de/fints",
            "URL ggf. laut DK-Institutsliste prüfen.",
        ),
        p("ing", "ING", "50010517", "https://fints.ing.de/fints/", ""),
        p(
            "comdirect",
            "comdirect",
            "",
            super::fints_directory::COMDIRECT_FINTS_URL,
            "BLZ aus Ihrer IBAN eintragen (Stellen 5–12, z. B. 20041155) oder die IBAN einfügen. Anmeldename: Ihre Zugangsnummer.",
        ),
        p("gls", "GLS Gemeinschaftsbank", "43060967", ATRUVIA, ""),
        p(
            "consorsbank",
            "Consorsbank",
            "76030080",
            "https://brokerage-hbci.consorsbank.de/hbci",
            "Anmeldename: 10-stellige Kontonummer des Verrechnungskontos (mit führenden Nullen) + 001, z. B. 0001234567001. TAN aus der Consorsbank App oder dem SecurePlus-Generator.",
        ),
        p(
            "norisbank",
            "Norisbank",
            "10077777",
            "https://fints.norisbank.de/",
            "",
        ),
        p(
            "deutsche-bank",
            "Deutsche Bank",
            "",
            "https://fints.deutsche-bank.de/",
            "BLZ Ihrer Filiale eintragen (steht in Ihrer IBAN, Stellen 5–12).",
        ),
        p(
            "hvb",
            "HypoVereinsbank (UniCredit)",
            "70020270",
            "https://hbci.hypovereinsbank.de/hbci/hbci.do",
            "",
        ),
        p("bbbank", "BBBank", "66090800", ATRUVIA, ""),
        p(
            "apobank",
            "Deutsche Apotheker- und Ärztebank (apoBank)",
            "30060601",
            "https://fints.apobank.de/FinTs30PinTanHttpGate",
            "",
        ),
        p("ethikbank", "EthikBank", "83094495", ATRUVIA, ""),
        p(
            "psd",
            "PSD Banken (Atruvia)",
            "",
            ATRUVIA,
            "BLZ Ihrer PSD Bank eintragen.",
        ),
        p(
            "custom",
            "Andere FinTS-Bank (BLZ + URL manuell)",
            "",
            "",
            "FinTS-3.0-PIN/TAN-URL Ihrer Bank (DK-Institutsliste).",
        ),
    ]
}

/// FinTS endpoints must be https URLs without whitespace.
///
/// The PIN goes to this address, so a prefix test is not enough: it must parse, carry
/// no user info (`https://bank.de@evil.example/`), and name a public host, not a
/// literal local, private or link-local address.
pub fn is_valid_fints_url(url: &str) -> bool {
    if url.len() <= "https://x".len()
        || !url.starts_with("https://")
        || url.chars().any(|c| c.is_whitespace())
    {
        return false;
    }
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d.contains('.')
                && d != "localhost"
                && !d.ends_with(".localhost")
                && !d.ends_with(".local")
        }
        Some(url::Host::Ipv4(ip)) => {
            !(ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified())
        }
        Some(url::Host::Ipv6(ip)) => !(ip.is_loopback() || ip.is_unspecified()),
        None => false,
    }
}

/// Preset BLZs MoneyMoney imports map onto the generic connector.
pub fn known_fints_blz(blz: &str) -> bool {
    is_postbank_blz(blz)
        || fints_presets().iter().any(|p| p.blz == blz)
        || super::fints_directory::lookup(blz).is_some()
}

/// Postbank uses regional bank codes; all share the same FinTS server.
/// Explicit list on purpose: never route a non-Postbank BLZ to Postbank.
const POSTBANK_BLZ: &[&str] = &[
    "10010010", "20010020", "25010030", "36010043", "37010050", "44010046", "50010060", "59010066",
    "60010070", "66010075", "70010080", "76010085", "80010090", "86010090", "90010085",
];

pub fn is_postbank_blz(blz: &str) -> bool {
    POSTBANK_BLZ.contains(&blz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ConnectorKind;

    #[test]
    fn presets_are_valid() {
        for p in fints_presets() {
            if !p.blz.is_empty() {
                assert!(
                    ConnectorKind::fints_blz(&ConnectorKind::fints_key(&p.blz)).is_some(),
                    "{}",
                    p.id
                );
            }
            if !p.url.is_empty() {
                assert!(is_valid_fints_url(&p.url), "{}", p.id);
            }
        }
        assert!(fints_presets().iter().any(|p| p.blz == "12096597"));
        assert!(known_fints_blz("76030080"));
        assert_eq!(
            ConnectorKind::from_str("fints:76030080"),
            Some(ConnectorKind::FinTsGeneric)
        );
        assert!(!is_valid_fints_url("http://bank.de/fints"));
        assert!(!is_valid_fints_url("https://bank.de@evil.example/fints"));
        assert!(!is_valid_fints_url("https://user:pw@bank.de/fints"));
        assert!(!is_valid_fints_url("https://localhost/fints"));
        assert!(!is_valid_fints_url("https://intranet/fints"));
        assert!(!is_valid_fints_url("https://127.0.0.1/fints"));
        assert!(!is_valid_fints_url("https://192.168.1.5/fints"));
        assert!(!is_valid_fints_url("https:///fints"));
        assert!(is_valid_fints_url(
            "https://fints1.atruvia.de/cgi-bin/hbciservlet"
        ));
        assert!(known_fints_blz("37010050") && known_fints_blz("70010080"));
        for b in POSTBANK_BLZ {
            assert!(is_postbank_blz(b) && known_fints_blz(b), "{b}");
            assert_eq!(b.len(), 8);
        }
        assert_eq!(POSTBANK_BLZ.len(), 15);
        // looks like a Postbank code (..0100..) but is not on the list
        assert!(!is_postbank_blz("12010099") && !is_postbank_blz("10070000"));
    }

    #[test]
    fn new_presets_match_the_directory() {
        for id in [
            "ing",
            "gls",
            "consorsbank",
            "norisbank",
            "hvb",
            "bbbank",
            "apobank",
            "ethikbank",
            "dkb",
            "1822direkt",
        ] {
            let pr = fints_presets().into_iter().find(|p| p.id == id).unwrap();
            let d = crate::connectors::fints_directory::lookup(&pr.blz).unwrap();
            assert_eq!(d.url, pr.url, "{id}");
        }
        assert!(known_fints_blz("50010517") && known_fints_blz("10050000"));
        assert!(known_fints_blz("20041155")); // comdirect: FinTS next to its own API
        let comdirect = fints_presets()
            .into_iter()
            .find(|p| p.id == "comdirect")
            .unwrap();
        assert!(comdirect.blz.is_empty() && is_valid_fints_url(&comdirect.url));
        assert!(!known_fints_blz("12345678"));
    }

    #[test]
    fn connector_key_roundtrip() {
        assert_eq!(
            ConnectorKind::from_str("fints:12096597"),
            Some(ConnectorKind::FinTsGeneric)
        );
        assert_eq!(
            ConnectorKind::from_str("fints:52090000"),
            Some(ConnectorKind::VolksbankKasselGoettingen)
        );
        assert_eq!(ConnectorKind::from_str("fints:12x"), None);
        assert_eq!(ConnectorKind::fints_blz("fints:12096597"), Some("12096597"));
    }
}
