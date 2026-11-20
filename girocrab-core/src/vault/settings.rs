//! Key/value settings (secrets flagged) and the list of known settings.

use rusqlite::{Connection, OptionalExtension, params};
use zeroize::Zeroizing;

use super::Vault;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::*;

/// Known settings shown in the Settings UI. Nothing is pre-filled. The FinTS
/// product ID here is the user's *own* one; GiroCrab's registration is compiled
/// in separately (see `connectors::fints::product_id`). API applications are
/// always the user's own.
/// (key, label, help, group, is_secret, default)
pub const KNOWN_SETTINGS: &[(&str, &str, &str, &str, bool, &str)] = &[
    (
        "fints.product_id",
        "Eigene FinTS-Produktregistrierung",
        "Nur nötig, wenn Sie Ihre eigene Registrierung statt der von GiroCrab verwenden. Kostenlos bei der Deutschen Kreditwirtschaft zu beantragen.",
        "FinTS 3.0",
        true,
        "",
    ),
    (
        "fints.product_version",
        "FinTS-Produktversion",
        "Versionsangabe, die mit HKVVB gesendet wird.",
        "FinTS 3.0",
        false,
        "0.1",
    ),
    (
        "fints.url.52090000",
        "Server Volksbank Kassel Göttingen (BLZ 52090000)",
        "FinTS-PIN/TAN-Adresse laut öffentlicher DK-Institutsliste.",
        "FinTS 3.0",
        false,
        "https://fints1.atruvia.de/cgi-bin/hbciservlet",
    ),
    (
        "fints.url.50050201",
        "Server 1822direkt (BLZ 50050201)",
        "FinTS-PIN/TAN-Adresse laut öffentlicher DK-Institutsliste.",
        "FinTS 3.0",
        false,
        "https://banking-hs6.s-fints-pt-hs.de/fints30",
    ),
    (
        "comdirect.client_id",
        "comdirect Client-ID",
        "Aus dem comdirect-Entwicklerportal (Persönlicher Bereich › Verwaltung › Entwicklerzugang).",
        "comdirect REST-API",
        false,
        "",
    ),
    (
        "comdirect.client_secret",
        "comdirect Client-Secret",
        "Wird beim Freischalten des API-Zugangs einmalig angezeigt.",
        "comdirect REST-API",
        true,
        "",
    ),
    (
        "paypal.client_id",
        "PayPal Client-ID",
        "REST-App auf developer.paypal.com mit aktivierter „Transaction Search“ anlegen.",
        "PayPal",
        false,
        "",
    ),
    (
        "paypal.client_secret",
        "PayPal Secret",
        "Secret derselben REST-App.",
        "PayPal",
        true,
        "",
    ),
    (
        "paypal.api_username",
        "PayPal API-Benutzername",
        "Nur für den Abruf per API-Signatur (klassische NVP-API): PayPal › Kontoeinstellungen › API-Zugriff.",
        "PayPal",
        false,
        "",
    ),
    (
        "paypal.api_password",
        "PayPal API-Passwort",
        "Passwort derselben API-Zugangsdaten (nicht das Login-Passwort).",
        "PayPal",
        true,
        "",
    ),
    (
        "paypal.api_signature",
        "PayPal API-Signatur",
        "Ist sie gesetzt, ruft GiroCrab per API-Signatur statt per REST-App ab.",
        "PayPal",
        true,
        "",
    ),
    (
        "paypal.environment",
        "PayPal-Umgebung",
        "live (echte Daten) oder sandbox (nur Testdaten, z. B. „Initial Balance“)",
        "PayPal",
        false,
        "live",
    ),
    (
        "kontist.client_id",
        "Kontist Client-ID",
        "Auf kontist.dev unter Client Management einen Client anlegen (Redirect-URI z. B. https://localhost/girocrab/callback, Scopes Offline, Accounts, Transactions, Users) und dessen Client-ID hier eintragen.",
        "Kontist",
        false,
        "",
    ),
    (
        "kontist.client_secret",
        "Kontist Client-Secret",
        "Nur nötig, wenn kontist.dev zum Client ein Secret anzeigt; sonst leer lassen.",
        "Kontist",
        true,
        "",
    ),
    (
        "bunq.environment",
        "bunq-Umgebung",
        "production oder sandbox (Test-API von bunq).",
        "bunq",
        false,
        "production",
    ),
    (
        "enablebanking.application_id",
        "Enable Banking Application-ID",
        "Anwendung auf enablebanking.com registrieren (wird als JWT-„kid“ verwendet).",
        "Enable Banking (PSD2)",
        false,
        "",
    ),
    (
        "enablebanking.private_key_pem",
        "Enable Banking Private Key (PEM)",
        "RSA-Schlüssel der Anwendung; signiert das RS256-JWT.",
        "Enable Banking (PSD2)",
        true,
        "",
    ),
    (
        "enablebanking.redirect_url",
        "Redirect-URL",
        "Muss in Ihrer Enable-Banking-Anwendung freigegeben sein.",
        "Enable Banking (PSD2)",
        false,
        "https://localhost/girocrab/callback",
    ),
    (
        "enablebanking.aspsp.amex",
        "ASPSP-Name American Express",
        "Exakter Name aus GET /aspsps.",
        "Enable Banking (PSD2)",
        false,
        "American Express",
    ),
    (
        "enablebanking.country.amex",
        "Land American Express",
        "Zweistelliger Ländercode.",
        "Enable Banking (PSD2)",
        false,
        "DE",
    ),
    (
        "enablebanking.aspsp.amazonvisa",
        "ASPSP-Name Amazon Visa",
        "Herausgeber in Deutschland: Open Bank S.A. (Marke Zinia). Je nach Listung „Open Bank“ oder „Zinia“ eintragen.",
        "Enable Banking (PSD2)",
        false,
        "Open Bank",
    ),
    (
        "enablebanking.country.amazonvisa",
        "Land Amazon Visa",
        "Zweistelliger Ländercode.",
        "Enable Banking (PSD2)",
        false,
        "DE",
    ),
    // Opt-in per card: the Enable Banking credentials are shared with PSD2
    // banks (C24, N26 …), so having them must not switch the cards away from
    // the web login.
    (
        "enablebanking.use.amex",
        "American Express per Enable Banking abrufen",
        "1 = PSD2-API statt Web-Login (eigene Enable-Banking-Anwendung nötig). Leer oder 0 = Web-Login.",
        "Enable Banking (PSD2)",
        false,
        "",
    ),
    (
        "enablebanking.use.amazonvisa",
        "Amazon Visa per Enable Banking abrufen",
        "1 = PSD2-API statt Web-Login (eigene Enable-Banking-Anwendung nötig). Leer oder 0 = Web-Login.",
        "Enable Banking (PSD2)",
        false,
        "",
    ),
    (
        "merchants.online_logos",
        "Händler-Logos online laden",
        "Anbieter für Händler-Logos: off, duckduckgo oder google. Logos werden verschlüsselt zwischengespeichert; übertragen wird nur die Händler-Domain (z. B. lidl.de). Standard: off.",
        "Händler-Logos",
        false,
        "off",
    ),
    (
        "sync.psd2_auto_hours",
        "Automatischer PSD2-Abruf (Stunden)",
        "Solange GiroCrab läuft, werden PSD2-Bankzugänge (Enable Banking) alle N Stunden ohne TAN abgerufen, solange die Zustimmung gilt. Banken erlauben meist nur 4 Abrufe pro Tag. 0 = aus.",
        "Allgemein",
        false,
        "6",
    ),
    (
        "sync.days_back",
        "Tage beim ersten Abruf",
        "Ältere Umsätze werden nur einmal abgefragt.",
        "Allgemein",
        false,
        "90",
    ),
];

impl Vault {
    /// [`Vault::set_setting`] on a connection the caller already holds (a
    /// read-modify-write under one lock).
    pub(crate) fn set_setting_inner(conn: &Connection, key: &str, value: &str) -> CoreResult<()> {
        if value.trim().is_empty() {
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        } else {
            let secret = KNOWN_SETTINGS.iter().any(|s| s.0 == key && s.4)
                || key.contains("secret")
                || key.contains("private")
                || key.starts_with("pin.");
            conn.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, is_secret = excluded.is_secret",
                params![key, value.trim(), secret as i64],
            )?;
        }
        Ok(())
    }

    pub(crate) fn setting_inner(conn: &Connection, key: &str) -> CoreResult<Option<String>> {
        let stored: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        let stored = stored.filter(|v| !v.is_empty());
        if stored.is_some() {
            return Ok(stored);
        }
        Ok(KNOWN_SETTINGS
            .iter()
            .find(|s| s.0 == key)
            .map(|s| s.5.to_string())
            .filter(|v| !v.is_empty()))
    }
}

#[uniffi::export]
impl Vault {
    pub fn list_settings(&self) -> Result<Vec<SettingEntry>, CoreError> {
        let db = self.db()?;
        let mut out = vec![];
        for (key, label, help, group, secret, default) in KNOWN_SETTINGS {
            let stored: Option<String> = db
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()?;
            let value = stored.clone().unwrap_or_else(|| default.to_string());
            out.push(SettingEntry {
                key: key.to_string(),
                label: tr(label).to_string(),
                help: tr(help).to_string(),
                group: group.to_string(),
                group_title: tr(group).to_string(),
                value: if *secret {
                    String::new()
                } else {
                    value.clone()
                },
                is_secret: *secret,
                has_value: !value.is_empty(),
            });
        }
        Ok(out)
    }

    /// Stored choice of the FinTS product registration: `"builtin"`, `"own"`
    /// or empty (not decided yet).
    pub fn fints_product_id_mode(&self) -> Result<String, CoreError> {
        let db = self.db()?;
        let v = Self::setting_inner(&db, crate::connectors::fints::product_id::MODE_KEY)?
            .unwrap_or_default();
        Ok(crate::connectors::fints::product_id::normalize_mode(&v).to_string())
    }

    /// Stores the choice (`"builtin"` / `"own"`; anything else clears it). The
    /// user's own ID is never touched.
    pub fn set_fints_product_id_mode(&self, mode: String) -> Result<(), CoreError> {
        let mode = crate::connectors::fints::product_id::normalize_mode(&mode);
        self.set_setting(
            crate::connectors::fints::product_id::MODE_KEY.to_string(),
            mode.to_string(),
        )
    }

    /// Which registration would be sent right now: `"builtin"`, `"own"` or
    /// empty when none is available. Never returns the ID itself.
    pub fn fints_product_id_source(&self) -> Result<String, CoreError> {
        use crate::connectors::fints::product_id as pid;
        let db = self.db()?;
        let mode = Self::setting_inner(&db, pid::MODE_KEY)?.unwrap_or_default();
        let own = Self::setting_inner(&db, pid::OWN_KEY)?.unwrap_or_default();
        Ok(pid::resolve(&mode, &own)
            .map(|(s, _)| s.as_str().to_string())
            .unwrap_or_default())
    }

    pub fn get_setting(&self, key: String) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        Self::setting_inner(&db, &key)
    }

    /// Stores a setting inside the encrypted database. Empty value deletes it.
    pub fn set_setting(&self, key: String, value: String) -> Result<(), CoreError> {
        let value = Zeroizing::new(value);
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        // a new default Enable Banking application must not move existing logins (#824)
        super::enable_banking_apps::before_global_write(&tx, &key, &value)?;
        Self::set_setting_inner(&tx, &key, &value)?;
        tx.commit()?;
        Ok(())
    }
}
