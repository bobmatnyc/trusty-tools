//! Persistence and concurrency-guard layer for trusty-review (issue #582).
//!
//! Why: live posting needs two coordination mechanisms beyond the review
//! pipeline itself — a durable cross-process dedup claim store (so retries and
//! restarts do not re-review the same head SHA) and an in-process in-flight
//! guard (so concurrent webhook deliveries for the same PR do not race).
//! Grouping them under one module keeps the storage concerns out of the
//! pipeline modules.
//!
//! What: re-exports the `dedup` SHA-keyed claim store and the `in_flight`
//! RAII guard registry.
//!
//! Test: each submodule carries its own unit tests.

pub mod dedup;
pub mod in_flight;

pub use dedup::{ClaimOutcome, DedupError, DedupStore};
pub use in_flight::{InFlightGuard, InFlightRegistry};

/// Classify a `redb::DatabaseError` as an incompatible / unreadable file format
/// (issue #702).
///
/// Why: redb 4.x cannot open a redb-2.x file (and rejects foreign/garbage files
/// outright). The store layer recovers by rebuilding empty, but must do so only
/// for genuine format problems — never for transient I/O or lock contention.
/// What: returns `true` for `UpgradeRequired` / `RepairAborted` /
/// `Storage(Corrupted)` / `Storage(Io(InvalidData))`; `false` otherwise.
/// Test: `dedup::tests::incompatible_dedup_db_is_recreated` exercises the
/// `InvalidData` path end-to-end.
pub(crate) fn redb_error_is_incompatible_format(err: &redb::DatabaseError) -> bool {
    use redb::{DatabaseError, StorageError};
    match err {
        DatabaseError::UpgradeRequired(_) | DatabaseError::RepairAborted => true,
        DatabaseError::Storage(StorageError::Corrupted(_)) => true,
        DatabaseError::Storage(StorageError::Io(io)) => {
            io.kind() == std::io::ErrorKind::InvalidData
        }
        _ => false,
    }
}
