//! Local test of the PayPal connector via API signature (classic NVP API, #156),
//! without the app and without a vault. Prints balances and bookings to stdout
//! (your own data, nothing is stored or sent anywhere but to PayPal).
//!
//! ```sh
//! read -s PAYPAL_API_PASSWORD; read -s PAYPAL_API_SIGNATURE; export PAYPAL_API_PASSWORD PAYPAL_API_SIGNATURE
//! PAYPAL_API_USERNAME=… [PAYPAL_SANDBOX=1] [SINCE=2026-01-01] \
//!   cargo run -q -p girocrab-core --example paypal_nvp
//! ```
//!
//! `PAYPAL_NVP_RAW=1` instead prints the unfiltered rows of one TransactionSearch
//! since `SINCE` (type, status, amounts; name and e-mail only as present/absent),
//! to see what PayPal sends for a booking that is shown oddly.
//! `PAYPAL_NVP_CLASS=BalanceAffecting` (or another TRANSACTIONCLASS) filters it.
use std::collections::HashMap;

use girocrab_core::connectors::paypal::PayPalConnector;
use girocrab_core::connectors::paypal_nvp::{NvpClient, NvpCredentials};
use girocrab_core::connectors::{BankConnector, ConnectContext, Phase};
use girocrab_core::money::format_cents;
use girocrab_core::secret::SecretString;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        eprintln!("{key} fehlt");
        std::process::exit(2)
    })
}

#[tokio::main]
async fn main() {
    let mut settings = HashMap::new();
    for (k, e) in [
        ("paypal.api_username", "PAYPAL_API_USERNAME"),
        ("paypal.api_password", "PAYPAL_API_PASSWORD"),
        ("paypal.api_signature", "PAYPAL_API_SIGNATURE"),
    ] {
        settings.insert(k.to_string(), SecretString::new(env(e)));
    }
    if std::env::var("PAYPAL_SANDBOX").is_ok_and(|v| v == "1") {
        settings.insert(
            "paypal.environment".into(),
            SecretString::new("sandbox".into()),
        );
    }
    let since = std::env::var("SINCE").unwrap_or_else(|_| {
        let today = jiff::Timestamp::now()
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .date();
        (today - jiff::Span::new().days(90)).to_string()
    });
    let ctx = ConnectContext {
        connection_id: 0,
        login_name: "PayPal".into(),
        secret: SecretString::default(),
        settings,
        since,
        ..Default::default()
    };
    if std::env::var("PAYPAL_NVP_RAW").is_ok_and(|v| v == "1") {
        raw(&ctx).await;
        return;
    }
    let mut c = PayPalConnector::new();
    match c.begin(&ctx).await {
        Ok(Phase::Ready) => eprintln!("Anmeldung ok (seit {})", ctx.since),
        Ok(_) => unreachable!("NVP has no challenge"),
        Err(e) => {
            eprintln!("Anmeldung fehlgeschlagen: {e}");
            std::process::exit(1)
        }
    }
    let r = match c.fetch(&ctx).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Abruf fehlgeschlagen: {e}");
            std::process::exit(1)
        }
    };
    for a in &r.accounts {
        println!(
            "{}  {}  {}",
            a.external_ref,
            a.name,
            format_cents(a.balance_cents, a.currency.clone())
        );
    }
    println!("--- {} Umsätze", r.transactions.len());
    for t in &r.transactions {
        println!(
            "{}  {:>12}  {:<20}  {}  {}  [{}]",
            t.booking_date,
            format_cents(t.amount_cents, t.currency.clone()),
            t.booking_text,
            t.counterparty,
            t.purpose,
            t.external_id
        );
    }
}

async fn raw(ctx: &ConnectContext) {
    let get = |k: &str| ctx.settings[k].expose().trim().to_string();
    let sandbox = ctx
        .settings
        .get("paypal.environment")
        .is_some_and(|v| v.expose() == "sandbox");
    let nvp = NvpClient::new(
        sandbox,
        NvpCredentials {
            user: get("paypal.api_username"),
            password: SecretString::new(ctx.settings["paypal.api_password"].expose().to_string()),
            signature: SecretString::new(get("paypal.api_signature")),
        },
    )
    .expect("http client");
    let since = jiff::civil::Date::strptime("%Y-%m-%d", &ctx.since).expect("SINCE");
    // e.g. PAYPAL_NVP_CLASS=BalanceAffecting (TRANSACTIONCLASS of TransactionSearch)
    let class = std::env::var("PAYPAL_NVP_CLASS").ok();
    if let Some(c) = &class {
        println!("TRANSACTIONCLASS={c}");
    }
    let rows = match nvp.raw_rows(since, class.as_deref()).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Abruf fehlgeschlagen: {e}");
            std::process::exit(1)
        }
    };
    let f = |r: &girocrab_core::connectors::paypal_nvp::Nvp, k: &str| {
        r.get(k).cloned().unwrap_or_else(|| "-".into())
    };
    println!(
        "{} Zeilen (höchstens 100, sonst SINCE näher legen)",
        rows.len()
    );
    for r in &rows {
        let id = f(r, "L_TRANSACTIONID");
        let id_tail: String = id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        println!(
            "{}  type={:?}  status={:?}  amt={} fee={} net={} {}  name={} email={}  id=…{}",
            f(r, "L_TIMESTAMP"),
            f(r, "L_TYPE"),
            f(r, "L_STATUS"),
            f(r, "L_AMT"),
            f(r, "L_FEEAMT"),
            f(r, "L_NETAMT"),
            f(r, "L_CURRENCYCODE"),
            if r.contains_key("L_NAME") {
                "ja"
            } else {
                "nein"
            },
            if r.contains_key("L_EMAIL") {
                "ja"
            } else {
                "nein"
            },
            id_tail
        );
    }
}
