//! Modular two-phase bank connectors.
//!
//! Phase 1 `begin()` logs in. It either finishes (`Phase::Ready`) or returns a
//! `Challenge` the user must answer: a decoupled push-TAN (approve in the
//! banking app, then we poll), a code (SMS/app TAN, 2FA PIN) or a redirect
//! (PSD2 consent in the browser).
//!
//! Phase 2 `respond()` answers the challenge (or polls the decoupled one) and
//! again returns `Ready` or another `Challenge`.
//!
//! Once ready, `fetch()` downloads accounts and transactions, which the
//! `SyncSession` merges into the vault and runs the categorisation rules over.

use std::time::Duration;

use crate::error::{CoreError, CoreResult};
use crate::i18n::trf;

pub mod advanzia;
pub mod amazon_documents;
pub mod bank_directory;
pub mod bhw;
pub mod broker_documents;
pub mod bunq;
pub mod comdirect;
pub(crate) mod comdirect_postbox;
pub(crate) mod consorsbank_archive;
pub mod demo;
pub mod dpop;
pub mod easybank;
pub mod enable_banking;
pub mod equateplus;
pub mod ffb;
pub mod fin4u;
pub mod finpension;
pub mod fints;
pub mod fints_directory;
pub mod fints_presets;
pub mod fnz;
pub mod fordmoney;
pub mod hanseatic;
pub mod ibkr;
pub mod kontist;
pub mod opel;
pub mod paypal;
pub mod paypal_nvp;
pub mod quirion;
pub mod scalable;
pub mod schwaebisch_hall;
pub mod template;
pub mod trade_republic;
pub mod uberspace;
pub mod volkswagen_bank;

pub(crate) mod account_selection;
pub use account_selection::ConnectionAccountChoice;
mod catalog;
mod html;
mod paging;
mod persist;
mod sync_session;
mod transfer_session;
mod types;

pub use catalog::*;
pub(crate) use paging::{Paged, paginate};
pub use persist::*;
pub use sync_session::*;
pub use transfer_session::*;
pub use types::*;

/// The `User-Agent` GiroCrab sends, except to portals that only serve
/// browsers: those get [`BROWSER_UA`].
pub(crate) const USER_AGENT: &str = concat!(
    "GiroCrab/",
    env!("CARGO_PKG_VERSION"),
    " (open source; +https://github.com/)"
);

pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

/// Client builder with [`USER_AGENT`], a 15 s connection timeout and a separate
/// timeout for the whole request.
pub(crate) fn client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
}

pub(crate) fn http_client() -> CoreResult<reqwest::Client> {
    Ok(client_builder(Duration::from_secs(60))
        .cookie_store(true)
        .build()?)
}

/// Redirect policy for clients that send credentials (login forms, tokens, API
/// keys): follows a redirect only inside the same host, so a bank or portal
/// answer (or a man in the middle) cannot make the client replay a login body,
/// session token or refresh token to a different host. Scheme and port must stay
/// the same too (no https -> http downgrade, no other service on the host). A
/// redirect to another origin ends the chain; the 3xx answer is then returned as it is.
pub(crate) fn same_host_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let next = attempt.url();
        let same = attempt.previous().last().is_some_and(|prev| {
            prev.scheme() == next.scheme()
                && prev.host_str() == next.host_str()
                && prev.port_or_known_default() == next.port_or_known_default()
        });
        if same && attempt.previous().len() < 10 {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// [`http_client`] with [`same_host_redirects`], for connectors that post credentials.
pub(crate) fn http_client_same_host() -> CoreResult<reqwest::Client> {
    Ok(client_builder(Duration::from_secs(60))
        .cookie_store(true)
        .redirect(same_host_redirects())
        .build()?)
}

/// HTTP client for FinTS: never follows redirects, so POST bodies carrying
/// PIN/TAN cannot be replayed to a different location.
pub(crate) fn http_client_no_redirect() -> CoreResult<reqwest::Client> {
    Ok(client_builder(Duration::from_secs(60))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// FinTS URLs must be https. Plain http to localhost is accepted in tests only.
pub(crate) fn ensure_secure_fints_url(url: &str) -> CoreResult<()> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") {
        if fints_presets::is_valid_fints_url(url.trim()) {
            return Ok(());
        }
        return Err(CoreError::protocol(
            "FinTS URL is not a public https address",
        ));
    }
    #[cfg(test)]
    if lower.starts_with("http://127.0.0.1") || lower.starts_with("http://localhost") {
        return Ok(());
    }
    Err(CoreError::protocol("FinTS URL must use https://"))
}

/// Upper bound for JSON / text / FinTS responses.
pub(crate) const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;
/// Upper bound for downloaded documents (PDF statements, receipts, images).
pub(crate) const MAX_DOC_BYTES: usize = 25 * 1024 * 1024;

fn too_large(limit: usize) -> CoreError {
    CoreError::Network {
        message: trf(
            "Antwort der Bank zu groß (über {0} MB) – abgebrochen.",
            &[&(limit / (1024 * 1024))],
        ),
    }
}

/// Reads a response body, but never more than `limit` bytes (no OOM on a
/// huge or endless body): larger bodies are an error.
#[allow(clippy::disallowed_methods)]
pub(crate) async fn read_body_limited(
    mut resp: reqwest::Response,
    limit: usize,
) -> CoreResult<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_large(limit));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > limit {
            return Err(too_large(limit));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// [`read_body_limited`] with [`MAX_TEXT_BYTES`], decoded as UTF-8 (lossy).
pub(crate) async fn read_text_limited(resp: reqwest::Response) -> CoreResult<String> {
    let b = read_body_limited(resp, MAX_TEXT_BYTES).await?;
    Ok(crate::text::decode_bank_bytes(&b))
}

/// [`read_body_limited`] with [`MAX_TEXT_BYTES`], parsed as JSON; an
/// unreadable or oversized body yields `Value::Null`.
pub(crate) async fn read_json_or_null(resp: reqwest::Response) -> serde_json::Value {
    match read_body_limited(resp, MAX_TEXT_BYTES).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

pub(crate) use crate::dates::today_iso as today;

#[cfg(test)]
mod tests;
