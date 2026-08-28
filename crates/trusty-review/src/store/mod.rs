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
pub mod dedup_open;
pub mod in_flight;

pub use dedup::{ClaimOutcome, DedupError, DedupStore};
pub use dedup_open::{DedupNeed, open_for as open_dedup_for};
pub use in_flight::{InFlightCountGuard, InFlightGuard, InFlightRegistry};

/// Classify a `redb::DatabaseError` as an incompatible / unreadable file format
/// (issue #702).
///
/// Why: redb 4.x cannot open a redb-2.x file (and rejects foreign/garbage files
/// outright). The store layer recovers by rebuilding empty, but must do so only
/// for genuine format problems — never for transient I/O or lock contention.
/// What: returns `true` for `UpgradeRequired` / `RepairAborted` /
/// `Storage(Corrupted)` / `Storage(Io(InvalidData))`; `false` otherwise.
/// Delegates to `trusty_common::redb_open::is_incompatible_format`, the
/// workspace's single copy of this decision (#5063). The recovery POLICY stays
/// in `dedup_open` — it serialises the rename-aside behind a sidecar lock so
/// two processes cannot rename away each other's fresh database (see #5064).
/// Test: `dedup::tests::incompatible_dedup_db_is_recreated` exercises the
/// `InvalidData` path end-to-end.
pub(crate) fn redb_error_is_incompatible_format(err: &redb::DatabaseError) -> bool {
    // #5063: one classifier for the whole workspace — this crate used to carry
    // a byte-identical copy of the four-arm match.
    trusty_common::redb_open::is_incompatible_format(err)
}
