//! Booking export generators (GitHub #372, #401, #483): Excel CSV and ISO 20022
//! CAMT.053 account statements (`camt.053.001.02` and `.08`).
//!
//! CSV is a pure function: the app loads bookings, prepares rows (order,
//! purpose line, balance column) and the core writes the file. CAMT.053 reads
//! statement entries and booked balances straight from the vault, so pending
//! entries, an older bank balance, foreign currency and a computed balance do
//! not distort OPBD/CLBD. Tests validate the XML against the official XSDs in
//! `tests/fixtures/iso20022/` and read it back with the FinTS CAMT parser.

mod camt;
mod csv;

pub use camt::{Camt053Request, CamtVersion, esc};
pub use csv::{ExportCsvBalanceLine, ExportCsvColumns, ExportCsvRow, export_csv};
pub(crate) use csv::{csv_amount, csv_field, csv_text};
