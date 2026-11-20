//! One-off file and web-capture importers (MoneyMoney database / exports,
//! card-portal captures and statements, Amazon orders).
//!
//! Unlike the live bank connectors in `connectors/` these only parse data the
//! user hands over; results reach the vault through the same `store_fetch`
//! / merge pipeline.

pub mod amazon_orders;
pub mod moneymoney_db;
pub mod moneymoney_import;
pub mod portal_import;
pub mod portal_statements;
