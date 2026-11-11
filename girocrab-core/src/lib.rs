//! GiroCrab core (früher CashCrab).
//!
//! A clean-room personal-finance engine: an encrypted SQLCipher ledger,
//! hierarchical categories, keyword/regex auto-categorisation rules and a
//! modular two-phase bank connector layer (FinTS 3.0, comdirect REST,
//! PayPal Transaction Search, Trade Republic WebSocket, Enable Banking PSD2).
//!
//! Money is *always* an `i64` amount of cents (minor units). No floating point
//! value ever touches a balance or a transaction amount.
//!
//! Everything public is exported to Swift (and Kotlin, if you want) through
//! Mozilla UniFFI proc-macros.

// Stylistic lints: amounts are written as `euros_cents` (e.g. 27_65), bank
// rows are plain tuples, and `from_str` constructors return Option for UniFFI.
#![allow(
    clippy::inconsistent_digit_grouping,
    clippy::type_complexity,
    clippy::too_many_arguments,
    clippy::should_implement_trait,
    clippy::if_same_then_else
)]

uniffi::setup_scaffolding!();

pub mod connections;
pub mod connectors;
pub mod demo;
pub mod depot_debug;
pub mod documents;
pub mod error;
pub mod i18n;
pub mod insights;
pub mod merchants;
pub mod models;
pub mod money;
pub mod planning;
pub mod recurring;
pub mod rules;
pub mod salary;
pub mod search_expr;
pub mod secret;
pub mod text;
pub mod user_groups;
pub mod vault;

pub use connectors::{
    bank_catalog, BankDescriptor, Challenge, ChallengeKind, SyncSession, SyncStep, TransferSession,
    TransferStep,
};
pub use error::CoreError;
pub use merchants::{resolve_merchant, FlowKind, MerchantInfo, MerchantKind, MerchantResolver};
pub use models::*;
pub use money::{format_cents, parse_amount_to_cents};
pub use vault::Vault;

/// Version string of the core library (shown in the About/Settings screen).
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Generates 32 random bytes for a new SQLCipher master key.
/// The Swift side stores it in the Keychain behind Face ID / Touch ID.
#[uniffi::export]
pub fn generate_master_key() -> Vec<u8> {
    use rand::RngCore;
    let mut key = vec![0u8; 32];
    rand::rng().fill_bytes(&mut key);
    key
}
