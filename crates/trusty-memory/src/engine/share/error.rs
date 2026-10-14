//! `ShareError`, the typed error of the share export/import paths (#9278 T3a).

use crate::engine::store::hnsw_store::HnswStoreError;
use crate::engine::store::kg::Internal;
use crate::engine::store::kg_redb::READ_ONLY_ERROR_MSG;
use crate::engine::store::palace_store::PalaceStoreError;

/// The error every public export, import and supersede function returns.
///
/// Why (#9278, ADR-0066): the frozen engine API must not name `anyhow`, and a
/// caller must tell a read-only palace from a store fault without parsing
/// text. The share path reaches the vector store and, through it, the palace
/// store, so those errors keep their own variants.
/// What: `ReadOnly` refuses a write into a snapshot palace before anything is
/// touched. `Hnsw` and `Palace` carry the store errors as-is. `Other` boxes
/// the whole internal error, so `{e:#}` prints its whole chain; it is
/// message-only and not a stable match target.
/// Test: `share_writes_refuse_a_read_only_palace_with_share_error`,
/// `share_error_keeps_typed_sources_visible_to_the_classifiers`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShareError {
    /// The palace is a read-only snapshot; writes go through the daemon.
    #[error("{}", READ_ONLY_ERROR_MSG)]
    ReadOnly,
    /// The vector store refused or failed a write.
    #[error(transparent)]
    Hnsw(HnswStoreError),
    /// The palace format refused, or another palace-store fault.
    #[error(transparent)]
    Palace(PalaceStoreError),
    /// Any other failure.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl ShareError {
    /// Map an internal `anyhow` error to its variant; the first match wins.
    ///
    /// What: a `ShareError` or a `HnswStoreError` in the error (under any
    /// `.context()`) comes back as its variant, then a root `PalaceStoreError`
    /// is `Palace`; the rest is `Other`, which keeps the whole chain's text.
    /// Test: `share_error_keeps_typed_sources_visible_to_the_classifiers`.
    pub(crate) fn from_internal(e: anyhow::Error) -> Self {
        let e = match e.downcast::<ShareError>() {
            Ok(s) => return s,
            Err(e) => e,
        };
        let e = match e.downcast::<HnswStoreError>() {
            Ok(h) => return Self::Hnsw(h),
            Err(e) => e,
        };
        match e.downcast::<PalaceStoreError>() {
            Ok(p) => Self::Palace(p),
            Err(e) => Self::Other(Box::new(Internal(e))),
        }
    }

    /// The internal error `Other` carries, for the crate's open classifiers.
    /// Test: `share_error_keeps_typed_sources_visible_to_the_classifiers`.
    pub(crate) fn internal(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Other(b) => b.downcast_ref::<Internal>().map(|i| &i.0),
            _ => None,
        }
    }
}
