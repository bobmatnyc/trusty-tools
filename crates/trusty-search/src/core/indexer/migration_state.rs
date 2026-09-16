//! The migration-in-progress window: the flag M005 raises while an index's
//! corpus is mid-rebuild, and the error a query lands on there (#6581).
//!
//! Why: M005 clears the corpus and re-chunks it from source in batches, so for
//! the length of that pass — potentially many batches of file I/O and redb
//! commits on a large index — the corpus is empty or partial. `run_migrations_
//! exclusive` holds the per-index permit and the teardown lock's shared side,
//! which between them exclude a reindex and a DELETE — but not a READER. Every
//! search call site takes its own independent read path with no knowledge of the
//! migration, so a query landing inside the window saw a corpus that read
//! cleanly and simply held nothing, and answered `results: []` at HTTP 200 — the
//! same "total outage rendered as nothing matched" failure #5917 fixed for the
//! unreadable-corpus case, arriving by a different route. This flag is the
//! reader-facing half; the permit is the writer-facing half.
//!
//! What: [`MigrationWindow`], an RAII guard that raises an index's
//! `migration_in_progress` flag on open and lowers it on drop (so a migration
//! that fails partway cannot leave every later query refusing), and
//! [`IndexMigrationInProgress`], the typed error `search_with_drops` raises
//! while the flag is up. `service::server::degraded` renders it as the same 503
//! shape `CorpusReadUnavailable` uses, with a distinct `failure_kind`.
//!
//! This is deliberately NOT `CorpusReadFault`: that record is about a corpus
//! that FAILED a read and is cleared by any successful one, whereas here every
//! read succeeds and the corpus is merely, temporarily, incomplete.
//!
//! Test: `core::migration::m005::tests::a_search_during_the_migration_window_is_refused_not_empty`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::CodeIndexer;

/// A query arrived while this index was being migrated (#6581).
///
/// Why: the search path must raise something the HTTP layer can recognise, so
/// the caller is told "not yet" rather than shown an empty result set it cannot
/// distinguish from a genuine miss. Mirrors `CorpusReadUnavailable`, which
/// solves the same reporting problem for a corpus that cannot be read at all.
/// What: carries the index it is about. The `Display` body is what the caller
/// reads, so it says what is happening and that it is transient.
/// Test: `a_search_during_the_migration_window_is_refused_not_empty`.
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}': a schema migration is rebuilding this index's corpus — \
     answering now would report a mid-rebuild corpus as an empty one (#6581). \
     This is transient; retry once the migration completes."
)]
pub struct IndexMigrationInProgress {
    /// The index being migrated.
    pub index_id: String,
}

/// Raises an index's migration-in-progress flag for the lifetime of the guard.
///
/// Why: M005's Steps 4 and 5 propagate with `?`, so a manually paired
/// set/clear would skip the lower on any failure and leave the index refusing
/// every query for the rest of the process's life. Drop runs on all of them.
/// What: stores `true` on [`Self::open`] and `false` on drop. The flag is an
/// `Arc<AtomicBool>` because the migration runs in a detached task that holds
/// only clones of the index's shared state.
/// Test: `a_search_during_the_migration_window_is_refused_not_empty`, whose
/// tail asserts the flag is lowered once the guard drops.
pub struct MigrationWindow {
    flag: Arc<AtomicBool>,
}

impl MigrationWindow {
    /// Raise `flag` until the returned guard drops.
    pub fn open(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self { flag }
    }
}

impl Drop for MigrationWindow {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

// ── Failed-migration record (#7979) ──────────────────────────────────────────

/// Stage label for a failed legacy `chunks.json` → `index.redb` migration.
pub const MIGRATION_STAGE_JSON_TO_REDB: &str = "json_to_redb";

/// Stage label for a failed schema-version migration chain (M001–M005).
pub const MIGRATION_STAGE_SCHEMA_CHAIN: &str = "schema_chain";

/// The most recent migration failure for one index, as status reports it.
///
/// Why (#7979): a migration that fails does not advance the schema stamp and
/// leaves the index serving whatever it already held — frequently nothing. The
/// only signal was one `warn!` line, so `GET /indexes/:id/status` and
/// `search_health` both answered as if the index were merely empty, and an
/// operator diagnosing a zero-chunk index was steered toward a reindex that
/// cannot repair a corrupt snapshot.
/// What: the failing stage ([`MIGRATION_STAGE_JSON_TO_REDB`] or
/// [`MIGRATION_STAGE_SCHEMA_CHAIN`]), the error chain rendered as text, and the
/// RFC 3339 instant it was recorded.
/// Test: `failed_schema_chain_is_reported_as_migration_error_in_status`.
#[derive(Debug, Clone)]
pub struct MigrationFault {
    /// Which migration stage failed.
    pub stage: &'static str,
    /// The failure, rendered as `{err:#}`.
    pub detail: String,
    /// When the failure was recorded, RFC 3339.
    pub at: String,
}

/// Outstanding migration faults for one index, keyed by stage.
///
/// Why: both runners record through `&CodeIndexer` — the schema chain holds
/// only a read lock on the indexer, and taking a write lock to record would
/// invert the lock order that chain already established. Interior mutability is
/// the shape `CorpusReadFault` uses for the same reason.
///
/// Keyed by stage rather than a single cell because the two runners are
/// sequential at boot and do NOT describe each other: `restore_indexes` records
/// a `json_to_redb` fault, and `spawn_index_migrations` then runs the schema
/// chain for the same index. A chain with nothing to do returns `Ok` at its
/// `current >= target` no-op, and an un-keyed cell let that success erase the
/// JSON fault that was still true — the exact wipe #7979 exists to prevent.
/// What: a `Mutex<BTreeMap<&'static str, MigrationFault>>`. `BTreeMap` so the
/// reported order is stable across calls rather than hash-random.
/// Test: `a_no_op_schema_chain_does_not_clear_a_json_to_redb_fault`,
/// `failed_schema_chain_is_reported_as_migration_error_in_status`.
#[derive(Debug, Default)]
pub(crate) struct MigrationFaultRecord {
    by_stage: Mutex<BTreeMap<&'static str, MigrationFault>>,
}

impl MigrationFaultRecord {
    /// Poison-tolerant lock: the critical sections are single map operations,
    /// so a poisoned mutex must not turn a migration fault into a daemon-wide
    /// one.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<&'static str, MigrationFault>> {
        self.by_stage.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl CodeIndexer {
    /// Record a failed migration under its own stage (#7979).
    pub(crate) fn record_migration_failure(&self, stage: &'static str, detail: impl Into<String>) {
        self.migration_fault.lock().insert(
            stage,
            MigrationFault {
                stage,
                detail: detail.into(),
                at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }

    /// Clear ONLY `stage` after that stage's run succeeded (#7979).
    ///
    /// Why: without any clear, a single failure would mark the index broken in
    /// `status` for the rest of the process's life, outliving the condition
    /// that caused it. Without the stage key, one runner's success would erase
    /// the other runner's still-true fault — see [`MigrationFaultRecord`].
    pub(crate) fn clear_migration_failure(&self, stage: &'static str) {
        self.migration_fault.lock().remove(stage);
    }

    /// Every outstanding migration fault, stage-ordered; empty when none.
    pub fn migration_faults(&self) -> Vec<MigrationFault> {
        self.migration_fault.lock().values().cloned().collect()
    }
}
