//! Derived data kept on the vault until its data changes: the booking list's finisher and
//! classes (`bookings::ListCache`), the history behind category suggestions and the known
//! recipients of the transfer form.
//!
//! An entry belongs to one [`DataStamp`]. Every write goes through the vault's one connection
//! and moves its `total_changes()`. A restore (`restore_backup`) replaces the pages with the
//! SQLite backup API, which moves no count, so it bumps the vault's data generation instead.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

use crate::vault::Vault;

/// One state of the vault's data: equal stamps, equal data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DataStamp {
    generation: u64,
    changes: u64,
}

impl Vault {
    /// The stamp of what `db` (this vault's connection, held by the caller) shows now.
    pub(crate) fn data_stamp(&self, db: &Connection) -> DataStamp {
        DataStamp {
            generation: self.data_generation.load(Ordering::SeqCst),
            changes: db.total_changes(),
        }
    }

    /// The data changed without moving the change count (restore): every cached entry is stale.
    /// Called with the connection held, so no reader takes a stamp in between.
    pub(crate) fn data_replaced(&self) {
        self.data_generation.fetch_add(1, Ordering::SeqCst);
    }
}

/// The last value built from one state of the data. Callers build it with the stamp they read
/// and skip [`put`](Self::put) inside an open transaction: its rollback would not lower the
/// count, so a value built from its data would outlive it.
pub(crate) struct ChangeCache<T>(Mutex<Option<(DataStamp, Arc<T>)>>);

impl<T> Default for ChangeCache<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> ChangeCache<T> {
    pub(crate) fn get(&self, stamp: DataStamp) -> Option<Arc<T>> {
        let slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref()
            .filter(|(s, _)| *s == stamp)
            .map(|(_, v)| v.clone())
    }

    pub(crate) fn put(&self, stamp: DataStamp, value: Arc<T>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some((stamp, value));
    }

    /// The stored value, whatever its stamp (tests: was it built again?).
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<Arc<T>> {
        let slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map(|(_, v)| v.clone())
    }
}
