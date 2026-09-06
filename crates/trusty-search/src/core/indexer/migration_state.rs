//! The migration-in-progress window: the flag M005 raises while an index's
//! corpus is mid-rebuild, and the error a query lands on there (#6581).
//!
//! Why: M005 clears the corpus and re-chunks it from source in batches, so for
//! the length of that pass — potentially many batches of file I/O and redb
//! commits on a large index — the corpus is empty or partial. It is dispatched
//! at boot under `acquire_index_teardown_read`, the SHARED side of the teardown
//! lock, which blocks a concurrent DELETE and nothing else; every search call
//! site takes its own independent read path with no knowledge of the migration.
//! A query landing inside the window therefore saw a corpus that read cleanly
//! and simply held nothing, and answered `results: []` at HTTP 200 — the same
//! "total outage rendered as nothing matched" failure #5917 fixed for the
//! unreadable-corpus case, arriving by a different route.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
