//! `EngineError`, the typed error of the palace open paths (#9278 T3b-1).

use crate::engine::store::kg::Internal;
use crate::engine::store::palace_store::PalaceStoreError;

/// The error `PalaceHandle::open` and `PalaceHandle::open_with_intent` return.
///
/// Why (#9278, ADR-0066): the frozen engine API must not name `anyhow`, and a
/// caller must tell a palace-format refusal from an open fault without
/// parsing text.
/// What: `Palace` carries a palace-store error as-is: a format refusal, or
/// another store fault. `Other` boxes the whole internal error, so `{e:#}`
/// prints its whole chain; it is message-only and not a stable match target.
/// A later variant may take a case that lands in `Other` today.
/// Test: `engine_error_from_internal_maps_each_variant_and_keeps_text`,
/// `a_read_only_open_of_a_format_1_palace_is_refused_and_writes_nothing`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The palace format refused the open, or another palace-store fault.
    #[error(transparent)]
    Palace(PalaceStoreError),
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
            Err(e) => Self::Other(Box::new(Internal(e))),
        }
    }

    /// The internal error `Other` carries, for the crate's open classifiers.
    /// Test: `engine_error_from_internal_maps_each_variant_and_keeps_text`.
    pub(crate) fn internal(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Other(b) => b.downcast_ref::<Internal>().map(|i| &i.0),
            Self::Palace(_) => None,
        }
    }
}
