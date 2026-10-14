//! `EngineError`, the typed error of the palace open and write paths (#9278
//! T3b-1, T3c-1).

use crate::engine::retrieval::VerifyNotStored;
use crate::engine::store::hnsw_store::HnswStoreError;
use crate::engine::store::kg::{Internal, KgError};
use crate::engine::store::palace_store::PalaceStoreError;

/// The error `PalaceHandle::{open, open_with_intent, flush, remember,
/// remember_with_options, forget}` return.
///
/// Why (#9278, ADR-0066): the frozen engine API must not name `anyhow`, and a
/// caller must tell a palace-format refusal, a KG or vector-store fault and a
/// committed slot without its verify command apart without parsing text.
/// What: `Palace`, `Kg`, `Hnsw` and `VerifyNotStored` carry the typed error
/// as-is. `Other` boxes the whole internal error, so `{e:#}` prints its whole
/// chain; it is message-only and not a stable match target. A later variant
/// may take a case that lands in `Other` today.
/// Test: `engine_error_from_internal_maps_each_variant_and_keeps_text`,
/// `a_read_only_open_of_a_format_1_palace_is_refused_and_writes_nothing`,
/// `the_write_paths_return_engine_error_with_a_typed_verify_miss`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The palace format refused the open, or another palace-store fault.
    #[error(transparent)]
    Palace(PalaceStoreError),
    /// The knowledge-graph store failed (#9278 T3c-1).
    #[error(transparent)]
    Kg(KgError),
    /// The vector store failed (#9278 T3c-1).
    #[error(transparent)]
    Hnsw(HnswStoreError),
    /// The slot drawer committed, but its verify command was not stored.
    #[error(transparent)]
    VerifyNotStored(VerifyNotStored),
    /// Any other failure.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl EngineError {
    /// Map an internal `anyhow` error to its variant; the first match wins.
    ///
    /// What: an `EngineError` or a root `PalaceStoreError` (under any
    /// `.context()`) comes back as its variant, without the context prefix
    /// (#9278 Q7, provisional); the rest is `Other`, which keeps the whole
    /// chain's text.
    /// Test: `engine_error_from_internal_maps_each_variant_and_keeps_text`,
    /// `engine_error_of_a_contexted_palace_error_is_palace_without_its_prefix`.
    pub(crate) fn from_internal(e: anyhow::Error) -> Self {
        let e = match e.downcast::<EngineError>() {
            Ok(x) => return x,
            Err(e) => e,
        };
        match e.downcast::<PalaceStoreError>() {
            Ok(p) => Self::Palace(p),
            // #9278 T3c-1: a typed wrapper root first; see `from_wrapper`.
            Err(e) => Self::from_wrapper(e),
        }
    }

    /// A root `KgError`, `HnswStoreError` or `VerifyNotStored` (under any
    /// `.context()`) as its variant, without the context prefix (#9278 Q7b,
    /// provisional); the rest is `Other`, which keeps the whole chain's text.
    /// Test: `a_contexted_typed_root_is_its_variant_without_its_prefix`.
    fn from_wrapper(e: anyhow::Error) -> Self {
        let e = match e.downcast::<KgError>() {
            Ok(k) => return Self::Kg(k),
            Err(e) => e,
        };
        let e = match e.downcast::<HnswStoreError>() {
            Ok(h) => return Self::Hnsw(h),
            Err(e) => e,
        };
        match e.downcast::<VerifyNotStored>() {
            Ok(v) => Self::VerifyNotStored(v),
            Err(e) => Self::Other(Box::new(Internal(e))),
        }
    }

    /// The internal error `Other`, or a boxed `Kg`/`Hnsw` variant, carries,
    /// for the crate's open classifiers.
    /// Test: `classifiers_see_a_refusal_through_kg_and_hnsw_variants`.
    pub(crate) fn internal(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Other(b) => b.downcast_ref::<Internal>().map(|i| &i.0),
            Self::Kg(k) => k.internal(),
            Self::Hnsw(h) => h.internal(),
            Self::Palace(_) | Self::VerifyNotStored(_) => None,
        }
    }
}
