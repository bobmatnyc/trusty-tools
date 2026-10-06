//! `From` conversions from redb's error types into [`HnswStoreError`].
//!
//! Moved out of `hnsw_store.rs` to keep it under the 500-SLOC cap (#9174).

use super::HnswStoreError;

// Why: redb's `?` operator needs a `From<redb::StorageError>` (etc.) impl
// to convert. The `#[from] Box<redb::StorageError>` derive only generates
// `From<Box<redb::StorageError>>`, so we add an explicit hop that boxes
// the inner error on the fly. This keeps call sites using `?` clean
// without forcing every caller to `.map_err(Box::new)`.
impl From<redb::Error> for HnswStoreError {
    fn from(e: redb::Error) -> Self {
        Self::Redb(Box::new(e))
    }
}
impl From<redb::StorageError> for HnswStoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::RedbStorage(Box::new(e))
    }
}
impl From<redb::TransactionError> for HnswStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::RedbTransaction(Box::new(e))
    }
}
impl From<redb::TableError> for HnswStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::RedbTable(Box::new(e))
    }
}
impl From<redb::CommitError> for HnswStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::RedbCommit(Box::new(e))
    }
}
