//! Deferred, coalesced symbol-graph rebuilds for single-file writes
//! (#8959, #9179).
//!
//! Why: `index_file` and `remove_file` each ran `rebuild_symbol_graph`, a
//! whole-corpus snapshot, sort, build and persist. On a 315K-chunk index one
//! `remove_file` took over 60 s and allocated about 1.2 GB, and a client
//! pushing N files paid N full rebuilds.
//! What: a single-file write now only marks the serving graph stale, in O(1).
//! The daemon's graph-refresh ticker rebuilds once the index has been quiet
//! for [`GRAPH_REFRESH_QUIET`], or once it has been stale for
//! [`GRAPH_REFRESH_MAX_WAIT`] under a continuous write stream. A burst of
//! writes therefore costs one rebuild. Readers that must see their own
//! writes (graph export, call chain) call [`CodeIndexer::fresh_symbol_graph`];
//! the search KG lane and the status endpoints read the serving graph and
//! never wait on a rebuild.
//! Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`,
//! `rebuild_due_waits_for_quiet_or_max_wait` and
//! `continuous_writes_still_rebuild_at_the_max_wait_cap` in
//! `indexer::tests::file_lifecycle_8959`; the ticker by
//! `graph_refresh_tick_rebuilds_a_stale_index_once`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// #9179: tokio's clock, so a paused-time test drives the debounce windows.
use tokio::time::Instant;

use crate::core::symbol_graph::SymbolGraph;

use super::CodeIndexer;

/// Quiet window after the last write before the ticker rebuilds (#8959).
pub(crate) const GRAPH_REFRESH_QUIET: Duration = Duration::from_secs(2);

/// Longest a write stream may keep the graph stale before the ticker rebuilds
/// anyway (#8959). Bounds staleness, and bounds full rebuilds to one per window.
pub(crate) const GRAPH_REFRESH_MAX_WAIT: Duration = Duration::from_secs(60);

/// Per-index staleness record for the serving symbol graph.
///
/// Why: the write paths and the ticker share one fact — "the serving graph
/// misses writes since T" — and it must be cheap to set from a write.
/// What: two millisecond stamps on tokio's monotonic clock (`0` means fresh), a
/// mutex that makes concurrent flushers share one rebuild, and a count of full
/// rebuilds, which tests and diagnostics read.
/// Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`.
#[derive(Debug)]
pub(crate) struct GraphRefresh {
    origin: Instant,
    /// Stamp of the first write the serving graph does not reflect; `0` = fresh.
    stale_since_ms: AtomicU64,
    /// Stamp of the latest such write.
    last_write_ms: AtomicU64,
    /// Held across a flush so concurrent flushers coalesce onto one rebuild.
    flush_lock: tokio::sync::Mutex<()>,
    /// Full `rebuild_symbol_graph` passes run on this index.
    full_rebuilds: AtomicU64,
}

impl Default for GraphRefresh {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            stale_since_ms: AtomicU64::new(0),
            last_write_ms: AtomicU64::new(0),
            flush_lock: tokio::sync::Mutex::new(()),
            full_rebuilds: AtomicU64::new(0),
        }
    }
}

impl GraphRefresh {
    /// Milliseconds since `origin`, offset by one so a stamp is never `0`.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    /// Record a write the serving graph does not reflect yet.
    fn mark_stale(&self) {
        let now = self.now_ms();
        // Keep the FIRST stale stamp: it is what the max-wait bound measures.
        let _ = self
            .stale_since_ms
            .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
        self.last_write_ms.store(now, Ordering::Release);
    }

    /// Called at the start of every full rebuild: the rebuild's snapshot is
    /// taken after this, so it covers every write marked before it.
    pub(crate) fn begin_full_rebuild(&self) {
        self.stale_since_ms.store(0, Ordering::Release);
        self.full_rebuilds.fetch_add(1, Ordering::Relaxed);
    }

    fn is_stale(&self) -> bool {
        self.stale_since_ms.load(Ordering::Acquire) != 0
    }

    fn is_due(&self, quiet: Duration, max_wait: Duration) -> bool {
        rebuild_due(
            self.stale_since_ms.load(Ordering::Acquire),
            self.last_write_ms.load(Ordering::Acquire),
            self.now_ms(),
            duration_ms(quiet),
            duration_ms(max_wait),
        )
    }
}

fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Whether a stale graph is due for its deferred rebuild (#8959).
///
/// Why: a pure predicate so the debounce rule is tested without a clock.
/// What: `false` when fresh (`stale_since_ms == 0`); otherwise `true` once the
/// index has been quiet for `quiet_ms`, or stale for `max_wait_ms`.
/// Test: `rebuild_due_waits_for_quiet_or_max_wait`.
pub(crate) fn rebuild_due(
    stale_since_ms: u64,
    last_write_ms: u64,
    now_ms: u64,
    quiet_ms: u64,
    max_wait_ms: u64,
) -> bool {
    if stale_since_ms == 0 {
        return false;
    }
    now_ms.saturating_sub(last_write_ms) >= quiet_ms
        || now_ms.saturating_sub(stale_since_ms) >= max_wait_ms
}

impl CodeIndexer {
    /// Mark the serving symbol graph stale instead of rebuilding it (#8959).
    ///
    /// Why/What: see the module docs. O(1); never touches the corpus.
    /// Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`.
    pub(crate) fn mark_symbol_graph_stale(&self) {
        self.graph_refresh.mark_stale();
    }

    /// Whether writes have landed since the serving graph was built.
    pub fn symbol_graph_is_stale(&self) -> bool {
        self.graph_refresh.is_stale()
    }

    /// Full symbol-graph rebuilds this index has run (#9179 diagnostics).
    pub fn symbol_graph_full_rebuilds(&self) -> u64 {
        self.graph_refresh.full_rebuilds.load(Ordering::Relaxed)
    }

    /// Rebuild the symbol graph now if writes are pending (#8959).
    ///
    /// Why: a reader that must see its own writes, and the ticker, both need
    /// "rebuild once if stale", and concurrent callers must share that one
    /// rebuild rather than each run their own.
    /// What: returns `false` with no work when the graph is fresh or the index
    /// is deleted. Otherwise takes the flush lock, re-checks, and runs one
    /// full `rebuild_symbol_graph`, which clears the stale mark. Returns
    /// whether it rebuilt.
    /// Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`.
    pub async fn flush_symbol_graph(&self) -> bool {
        if !self.graph_refresh.is_stale() || self.refuse_if_deleted().is_err() {
            return false;
        }
        let _flush = self.graph_refresh.flush_lock.lock().await;
        if !self.graph_refresh.is_stale() {
            return false; // a concurrent flusher already rebuilt
        }
        self.rebuild_symbol_graph().await;
        true
    }

    /// The ticker's entry point: flush when the debounce window says so.
    ///
    /// What: [`Self::flush_symbol_graph`] gated on [`rebuild_due`].
    /// Test: `graph_refresh_tick_rebuilds_a_stale_index_once`,
    /// `continuous_writes_still_rebuild_at_the_max_wait_cap`.
    pub(crate) async fn refresh_symbol_graph_if_due(
        &self,
        quiet: Duration,
        max_wait: Duration,
    ) -> bool {
        if !self.graph_refresh.is_due(quiet, max_wait) {
            return false;
        }
        self.flush_symbol_graph().await
    }

    /// Flush pending writes, then snapshot the symbol graph (#8959).
    ///
    /// Why: graph export and call-chain answer questions about the graph
    /// itself, so they read their own writes. Status and the search KG lane
    /// use [`Self::snapshot_symbol_graph`] / [`Self::symbol_graph`], which
    /// never wait on a rebuild.
    /// Test: `graph_handler_exports_nodes_and_edges` (export after
    /// `index_file` with no ticker running).
    pub async fn fresh_symbol_graph(&self) -> Arc<SymbolGraph> {
        self.flush_symbol_graph().await;
        self.snapshot_symbol_graph().await
    }
}
