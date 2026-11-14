use std::fmt;

use thiserror::Error;

use crate::i18n::{tr, trf};

/// Every error that can cross the FFI boundary.
#[derive(Debug, Error, uniffi::Error)]
pub enum CoreError {
    Database {
        message: String,
    },
    WrongKey,
    NotFound {
        what: String,
    },
    InvalidInput {
        message: String,
    },
    MissingSetting {
        key: String,
    },
    Network {
        message: String,
    },
    Bank {
        code: String,
        message: String,
    },
    Authentication {
        message: String,
    },
    Protocol {
        message: String,
    },
    /// Trade Republic: AWS WAF blocks direct logins – the app must open the
    /// in-app browser login (WKWebView) and hand over the session cookies.
    BrowserLoginRequired {
        message: String,
    },
    SessionClosed,
    /// This app version may no longer contact the bank (signed banking
    /// policy, see `banking_gate`). `message` is already localised by the app.
    BankingBlocked {
        message: String,
    },
    /// The vault was written by a newer GiroCrab (schema `vault_version`,
    /// this build knows up to `app_version`). It is left untouched.
    SchemaTooNew {
        vault_version: i64,
        app_version: i64,
    },
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(err, _) = &e {
            if err.code == rusqlite::ErrorCode::NotADatabase {
                return CoreError::WrongKey;
            }
        }
        CoreError::Database {
            message: e.to_string(),
        }
    }
}

impl From<reqwest::Error> for CoreError {
    fn from(e: reqwest::Error) -> Self {
        CoreError::Network {
            message: e.to_string(),
        }
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Protocol {
            message: format!("JSON: {e}"),
        }
    }
}

impl From<regex::Error> for CoreError {
    fn from(e: regex::Error) -> Self {
        CoreError::InvalidInput {
            message: format!("Regular expression: {e}"),
        }
    }
}

/// Localised (German/English) message, see `crate::i18n`.
impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            CoreError::Database { message } => trf("Datenbankfehler: {0}", &[message]),
            CoreError::WrongKey => tr(
                "Der Tresor konnte nicht entsperrt werden (falscher Schlüssel oder beschädigte Datei)",
            )
            .to_string(),
            CoreError::NotFound { what } => trf("Nicht gefunden: {0}", &[what]),
            CoreError::InvalidInput { message } => trf("Ungültige Eingabe: {0}", &[message]),
            CoreError::MissingSetting { key } => trf(
                "Einstellung fehlt: {0}. Bitte in den Einstellungen eintragen.",
                &[key],
            ),
            CoreError::Network { message } => trf("Netzwerkfehler: {0}", &[message]),
            CoreError::Bank { code, message } => trf(
                "Die Bank hat die Anfrage abgelehnt: {0} {1}",
                &[code, message],
            ),
            CoreError::Authentication { message } => {
                trf("Anmeldung fehlgeschlagen: {0}", &[message])
            }
            CoreError::Protocol { message } => trf("Protokollfehler: {0}", &[message]),
            CoreError::BrowserLoginRequired { message } => {
                trf("Browser-Anmeldung nötig: {0}", &[message])
            }
            CoreError::SessionClosed => tr("Dieser Abruf ist nicht mehr aktiv").to_string(),
            CoreError::BankingBlocked { message } => message.clone(),
            CoreError::SchemaTooNew { .. } => tr(
                "Dieser Tresor wurde mit einer neueren GiroCrab-Version gespeichert. Bitte die App aktualisieren.",
            )
            .to_string(),
        };
        f.write_str(&text)
    }
}

pub type CoreResult<T> = Result<T, CoreError>;
