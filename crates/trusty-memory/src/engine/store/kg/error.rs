//! `KgError`, the typed error of the knowledge-graph store paths (#9278 T1a).

use crate::engine::store::kg::AdjacencyDesync;
use crate::engine::store::kg_redb::READ_ONLY_ERROR_MSG;
use crate::engine::store::palace_store::PalaceStoreError;
use crate::engine::store::write_deadline::WriteTxnError;

/// The error every public `KgStoreRedb` and `KnowledgeGraph` path,
/// `resolve_or_create_room_sync` and a `CompactFaultHook` return.
///
/// Why (#9278, ADR-0066): the frozen engine API must not name `anyhow`, and a
/// caller must tell a read-only store, a rolled-back deadline write, a desynced
/// adjacency and a palace-format refusal apart without parsing text. A root
/// `downcast_ref::<WriteTxnError>()` on an `anyhow::Error` built from a
/// `KgError` returns `None`, so the typed payloads have accessors.
/// What: the typed variants carry engine-owned errors and display them as-is.
/// `Storage` (redb, io), `Codec` (postcard, serde_json) and `Other` box the
/// whole internal error, so `{e}` and `{e:#}` print the same text as before.
/// They are opaque: no source type is promised, so match the variant and read
/// the text (#9278 Q3). `Other` is message-only and not a stable match target.
/// Test: `from_internal_maps_each_variant_and_keeps_display`,
/// `kg_store_open_format_refusal_is_recoverable_as_palace_store`,
/// `kg_store_redb_paths_return_kg_error`, `knowledge_graph_paths_return_kg_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KgError {
    /// The store is a read-only snapshot; writes go through the daemon.
    #[error("{}", READ_ONLY_ERROR_MSG)]
    ReadOnly,
    /// A write transaction outran its deadline and rolled back (#8749).
    #[error(transparent)]
    WriteDeadline(WriteTxnError),
    /// Storage committed a write the in-memory adjacency could not record.
    #[error(transparent)]
    AdjacencyDesync(AdjacencyDesync),
    /// The palace format refused the open, or another palace-store fault.
    #[error(transparent)]
    Palace(PalaceStoreError),
    /// The storage engine or the filesystem failed.
    #[error(transparent)]
    Storage(Box<dyn std::error::Error + Send + Sync>),
    /// A stored row did not encode or decode.
    #[error(transparent)]
    Codec(Box<dyn std::error::Error + Send + Sync>),
    /// Any other failure.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl KgError {
    /// The deadline error, when a write rolled back at its deadline.
    pub fn as_write_deadline(&self) -> Option<&WriteTxnError> {
        match self {
            Self::WriteDeadline(e) => Some(e),
            _ => None,
        }
    }

    /// The adjacency desync, when a committed write went unindexed.
    pub fn as_adjacency_desync(&self) -> Option<&AdjacencyDesync> {
        match self {
            Self::AdjacencyDesync(e) => Some(e),
            _ => None,
        }
    }

    /// The palace-store error, such as a format refusal at open.
    pub fn as_palace_store(&self) -> Option<&PalaceStoreError> {
        match self {
            Self::Palace(e) => Some(e),
            _ => None,
        }
    }

    /// The internal error a boxed variant carries, for the crate's open
    /// classifiers (#9278 T1b-2).
    /// Test: `knowledge_graph_open_refusals_stay_classified`.
    pub(crate) fn internal(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Storage(b) | Self::Codec(b) | Self::Other(b) => {
                b.downcast_ref::<Internal>().map(|i| &i.0)
            }
            _ => None,
        }
    }

    /// The poisoned-adjacency-lock error of a graph traversal (#9278 Q6).
    /// Test: `from_internal_maps_each_variant_and_keeps_display`.
    pub(crate) fn poisoned<T>(_: T) -> Self {
        Self::Other("kg adjacency lock poisoned".into())
    }

    /// Map an internal `anyhow` error to its variant; the first match wins.
    ///
    /// What: a `KgError` already in the error (under any `.context()`) comes
    /// back as-is; then a `WriteTxnError` or `AdjacencyDesync` anywhere in the
    /// chain; then a root `PalaceStoreError`; then redb or io in the chain is
    /// `Storage`, postcard or serde_json is `Codec`, and the rest is `Other`.
    /// `ReadOnly` comes only from the type, never from message text.
    pub(crate) fn from_internal(e: anyhow::Error) -> Self {
        let e = match e.downcast::<KgError>() {
            Ok(kg) => return kg,
            Err(e) => e,
        };
        if let Some(d) = find::<WriteTxnError>(&e) {
            return Self::WriteDeadline(d.clone());
        }
        if let Some(d) = find::<AdjacencyDesync>(&e) {
            return Self::AdjacencyDesync(d.clone());
        }
        let e = match e.downcast::<PalaceStoreError>() {
            Ok(p) => return Self::Palace(p),
            Err(e) => e,
        };
        let any = |is: fn(&(dyn std::error::Error + 'static)) -> bool| e.chain().any(is);
        if any(|c| {
            c.is::<std::io::Error>()
                || c.is::<redb::Error>()
                || c.is::<redb::StorageError>()
                || c.is::<redb::DatabaseError>()
                || c.is::<redb::TransactionError>()
                || c.is::<redb::TableError>()
                || c.is::<redb::CommitError>()
        }) {
            Self::Storage(Box::new(Internal(e)))
        } else if any(|c| c.is::<postcard::Error>() || c.is::<serde_json::Error>()) {
            Self::Codec(Box::new(Internal(e)))
        } else {
            Self::Other(Box::new(Internal(e)))
        }
    }
}

/// The internal error a boxed variant carries. anyhow's own boxed form prints
/// only the outer message under `{:#}`; this keeps the whole chain (#9278).
#[derive(Debug)]
struct Internal(anyhow::Error);

impl std::fmt::Display for Internal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Internal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// `T` at the root, under a `.context()`, or anywhere in the source chain.
fn find<T: std::error::Error + Send + Sync + 'static>(e: &anyhow::Error) -> Option<&T> {
    e.downcast_ref::<T>()
        .or_else(|| e.chain().find_map(|c| c.downcast_ref::<T>()))
}
