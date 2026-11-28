//! The three `items` calls the update and attachment ports make on a store (#12029 P2h).
//!
//! Why: `ingest` and the attachment `record` read and write `items` only
//! through `list_docs`, `get` and `update(if_version=...)`; a seam over
//! those three lets the error-arm tests stage a conflict or a failed write
//! between the read and the write, which no single process can record.
//! What: [`ItemStore`], implemented for [`PyStore`] (every write a one-op
//! batch under the shared `flock`, so versioning, the change log and the
//! writer-class claim hold).
//! Test: `a_conflict_is_reported_and_writes_nothing`,
//! `record_retries_five_conflicts_then_gives_up`.

use crate::dashboard::store::py::{PyDocOut, PyStore, PyStoreError};
use crate::pyjson_load::{PyInt, PyValue};

/// `store.list_docs("items")`, `store.get("items", id)` and `store.update("items", ...)`.
pub(crate) trait ItemStore {
    /// `list_docs("items")`.
    ///
    /// # Errors
    /// The store's exception.
    fn list_items(&self) -> Result<Vec<PyDocOut>, PyStoreError>;

    /// `get("items", id)`.
    ///
    /// # Errors
    /// [`PyStoreError::NotFound`] when absent, or the store's exception.
    fn get_item(&self, id: &str) -> Result<PyDocOut, PyStoreError>;

    /// `update("items", id, data, if_version=version, actor=actor)`: the new version.
    ///
    /// # Errors
    /// [`PyStoreError::Conflict`] on a stale version, or the store's exception.
    fn update_item(
        &self,
        id: &str,
        data: &PyValue,
        version: &PyInt,
        actor: Option<&str>,
    ) -> Result<PyInt, PyStoreError>;

    /// The `now_iso()` the store's clock gives.
    fn now_iso(&self) -> String;
}

fn items() -> PyValue {
    PyValue::Str("items".into())
}

impl ItemStore for PyStore {
    fn list_items(&self) -> Result<Vec<PyDocOut>, PyStoreError> {
        self.list_docs(&items(), None, None, false, None)
    }

    fn get_item(&self, id: &str) -> Result<PyDocOut, PyStoreError> {
        self.get(&items(), &PyValue::Str(id.to_owned()))
    }

    fn update_item(
        &self,
        id: &str,
        data: &PyValue,
        version: &PyInt,
        actor: Option<&str>,
    ) -> Result<PyInt, PyStoreError> {
        let (id, v) = (PyValue::Str(id.to_owned()), PyValue::Int(version.clone()));
        let out = self.update(&items(), &id, Some(data), Some(&v), actor)?;
        out.version
            .ok_or_else(|| PyStoreError::Store("update returned no version".into()))
    }

    fn now_iso(&self) -> String {
        PyStore::now_iso(self)
    }
}
