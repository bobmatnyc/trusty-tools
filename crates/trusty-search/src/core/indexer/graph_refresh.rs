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
//! The stale mark is also durable (#8959): a write stamps a generation into
//! the corpus `_meta` table before it mutates anything, a rebuild removes the
//! stamp once its graph is saved, and a boot that finds the stamp schedules a
//! rebuild. A restart, park or crash inside the debounce window therefore
//! never serves the old persisted graph as current.
//! A write counts as active from its start, embed included, and the quiet
//! window never elapses while one is active (#10159); only the max-wait bound
//! can fire mid-write.
//! Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`,
//! `a_deferred_rebuild_survives_a_reopen`,
//! `rebuild_due_waits_for_quiet_or_max_wait`,
//! `continuous_writes_still_rebuild_at_the_max_wait_cap`,
//! `a_quiet_rebuild_waits_while_a_write_is_embedding` and
//! `a_serial_burst_of_slow_writes_rebuilds_at_most_once_per_max_wait` in
//! `indexer::tests::file_lifecycle_8959`; the ticker by
//! `graph_refresh_tick_rebuilds_a_stale_index_once`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// #9179: tokio's clock, so a paused-time test drives the debounce windows.
use tokio::time::Instant;

use crate::core::symbol_graph::SymbolGraph;

/// #10110 test seam: the durable dirty-mark stamp fails for each index id
/// listed here, so a caller's fail-before-mutation path can be proven.
#[cfg(test)]
pub(crate) static TEST_FAIL_DIRTY_MARK: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

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
    /// #8959: generation of the latest single-file write; seeded from the
    /// durable mark at boot so a rebuild can clear a mark written before it.
    write_gen: AtomicU64,
    /// #8959: single-file writes between their durable mark and their
    /// in-memory mark. A rebuild that starts while one is in flight may miss
    /// it, so it leaves the durable mark in place.
    inflight: AtomicU64,
    /// #10159: single-file writes running now, embed phase included. Holds
    /// back only the quiet rebuild; separate from `inflight`, so the #8959
    /// clear rules do not change.
    active_writes: AtomicU64,
}

/// What a full rebuild may clear once its graph is persisted (#8959).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RebuildTicket {
    /// The write generation the rebuild's snapshot covers.
    through: u64,
    /// `false` when a write was in flight as the rebuild began.
    clear_ok: bool,
}

/// An in-flight single-file write (#8959). Dropping it marks the serving
/// graph stale in memory, on success and on every error path alike, because
/// a failed write may still have mutated part of the corpus.
pub(crate) struct GraphWriteGuard<'a> {
    refresh: &'a GraphRefresh,
}

impl Drop for GraphWriteGuard<'_> {
    fn drop(&mut self) {
        self.refresh.mark_stale();
        self.refresh.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A single-file write in progress, embed phase included (#10159).
///
/// Why: `index_file` embeds before it takes [`GraphWriteGuard`], so an embed
/// longer than [`GRAPH_REFRESH_QUIET`] looked like a quiet index, and a serial
/// burst of slow writes paid one full rebuild per write.
/// What: the write counts as active until the guard drops. The drop, on
/// success, error, panic or cancellation alike, restamps the last write and
/// then uncounts the write, so the quiet window runs from the write's end.
/// Test: `a_quiet_rebuild_waits_while_a_write_is_embedding`,
/// `a_failed_or_panicked_write_does_not_hold_back_the_quiet_rebuild`.
pub(crate) struct ActiveWriteGuard<'a> {
    refresh: &'a GraphRefresh,
}

impl Drop for ActiveWriteGuard<'_> {
    fn drop(&mut self) {
        // #10159: stamp before the decrement, so a ticker that reads zero
        // active writes also reads this write's stamp.
        let now = self.refresh.now_ms();
        self.refresh.last_write_ms.store(now, Ordering::Release);
        self.refresh.active_writes.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for GraphRefresh {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            stale_since_ms: AtomicU64::new(0),
            last_write_ms: AtomicU64::new(0),
            flush_lock: tokio::sync::Mutex::new(()),
            full_rebuilds: AtomicU64::new(0),
            write_gen: AtomicU64::new(0),
            inflight: AtomicU64::new(0),
            active_writes: AtomicU64::new(0),
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
    ///
    /// #8959: the generation is read BEFORE the in-flight count. A write that
    /// enters after the count was read takes a generation after `through`, so
    /// its durable mark outlives this rebuild's clear.
    pub(crate) fn begin_full_rebuild(&self) -> RebuildTicket {
        let through = self.write_gen.load(Ordering::SeqCst);
        let clear_ok = self.inflight.load(Ordering::SeqCst) == 0;
        self.stale_since_ms.store(0, Ordering::Release);
        self.full_rebuilds.fetch_add(1, Ordering::Relaxed);
        RebuildTicket { through, clear_ok }
    }

    fn is_stale(&self) -> bool {
        self.stale_since_ms.load(Ordering::Acquire) != 0
    }

    fn is_due(&self, quiet: Duration, max_wait: Duration) -> bool {
        // #10159: an active write means the index is not quiet, however long
        // its embed runs; the max-wait bound still applies.
        let quiet_ms = if self.active_writes.load(Ordering::SeqCst) > 0 {
            u64::MAX
        } else {
            duration_ms(quiet)
        };
        rebuild_due(
            self.stale_since_ms.load(Ordering::Acquire),
            self.last_write_ms.load(Ordering::Acquire),
            self.now_ms(),
            quiet_ms,
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

    /// Count a single-file write as active for its whole run (#10159).
    ///
    /// What: `None` on a `skip_kg` index, whose graph a write never marks;
    /// otherwise an [`ActiveWriteGuard`] the caller holds until the write ends.
    /// Test: `a_skip_kg_write_is_not_counted_active`.
    pub(crate) fn begin_active_write(&self) -> Option<ActiveWriteGuard<'_>> {
        if self.skip_kg {
            return None;
        }
        let refresh = &self.graph_refresh;
        refresh.active_writes.fetch_add(1, Ordering::SeqCst);
        Some(ActiveWriteGuard { refresh })
    }

    /// Start a single-file write that leaves the symbol graph stale (#8959).
    ///
    /// Why: the in-memory stale mark dies with the process, and the persisted
    /// graph then boots as current while missing every deferred write.
    /// What: counts the write in flight, takes a write generation, and stamps
    /// it into the corpus as the durable dirty mark BEFORE the caller mutates
    /// anything, so a crash at any later point still finds the mark. The
    /// returned guard sets the in-memory mark when dropped. A failed stamp is
    /// an `Err` and the caller must not mutate; no corpus wired is a no-op
    /// stamp.
    /// Test: `a_deferred_rebuild_survives_a_reopen`.
    pub(crate) async fn begin_graph_write(&self) -> anyhow::Result<GraphWriteGuard<'_>> {
        let refresh = &self.graph_refresh;
        refresh.inflight.fetch_add(1, Ordering::SeqCst);
        let guard = GraphWriteGuard { refresh };
        let generation = refresh.write_gen.fetch_add(1, Ordering::SeqCst) + 1;
        #[cfg(test)]
        if TEST_FAIL_DIRTY_MARK
            .lock()
            .is_ok_and(|f| f.contains(&self.index_id))
        {
            anyhow::bail!("injected symbol-graph dirty mark failure (#10110 test seam)");
        }
        if let Some(corpus) = self.corpus.clone() {
            tokio::task::spawn_blocking(move || corpus.mark_kg_graph_dirty(generation))
                .await
                .map_err(|e| anyhow::anyhow!("symbol-graph dirty mark task failed: {e}"))?
                .map_err(|e| e.context("persist the symbol-graph dirty mark (#8959)"))?;
        }
        Ok(guard)
    }

    /// [`Self::begin_graph_write`] for removing `file_path`, or `None` when
    /// the removal cannot change the graph: a `skip_kg` index, or a path
    /// holding no chunks and no entity list (#8959).
    /// Test: `a_purge_without_its_rebuild_is_rebuilt_after_a_reopen`.
    pub(crate) async fn begin_graph_write_for_removal(
        &self,
        file_path: &str,
    ) -> anyhow::Result<Option<GraphWriteGuard<'_>>> {
        if self.skip_kg
            || (self.file_chunk_ids(file_path).await?.is_empty()
                && !self.entities.read().await.contains_key(file_path))
        {
            return Ok(None);
        }
        self.begin_graph_write().await.map(Some)
    }

    /// Remove the durable dirty mark after a rebuild persisted its graph,
    /// unless a write the rebuild may have missed stamped it (#8959).
    /// A failure is logged: the mark stays, which costs one extra rebuild.
    pub(super) async fn clear_graph_dirty_mark(&self, ticket: RebuildTicket) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        if !ticket.clear_ok {
            return;
        }
        let through = ticket.through;
        match tokio::task::spawn_blocking(move || corpus.clear_kg_graph_dirty_through(through))
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(
                index_id = %self.index_id,
                "kg: could not clear the dirty mark ({e:#}); the next boot rebuilds (#8959)"
            ),
            Err(e) => tracing::warn!(
                index_id = %self.index_id,
                "kg: dirty-mark clear task failed ({e}); the next boot rebuilds (#8959)"
            ),
        }
    }

    /// Boot half of the durable mark (#8959): seed the write generation from
    /// the stored mark so the scheduled rebuild can clear it, and report
    /// whether the persisted graph must be treated as stale. An unreadable
    /// mark counts as stale.
    pub(super) fn adopt_graph_dirty_mark(&self, stored: &anyhow::Result<Option<u64>>) -> bool {
        match stored {
            Ok(None) => false,
            Ok(Some(generation)) => {
                self.graph_refresh
                    .write_gen
                    .fetch_max(*generation, Ordering::SeqCst);
                true
            }
            Err(e) => {
                tracing::warn!(
                    index_id = %self.index_id,
                    "kg: dirty mark unreadable ({e:#}); scheduling a rebuild (#8959)"
                );
                true
            }
        }
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
    /// whether it rebuilt; `false` when the rebuild was deferred or installed
    /// nothing, with the stale mark kept (#10110).
    /// Test: `index_file_and_remove_file_defer_the_symbol_graph_rebuild`,
    /// `a_rebuild_that_outlasts_the_rehydrate_wait_persists_nothing_and_retries_10110`.
    pub async fn flush_symbol_graph(&self) -> bool {
        if !self.graph_refresh.is_stale() || self.refuse_if_deleted().is_err() {
            return false;
        }
        let _flush = self.graph_refresh.flush_lock.lock().await;
        if !self.graph_refresh.is_stale() {
            return false; // a concurrent flusher already rebuilt
        }
        // #10110: a rebuild deferred behind a still-running rehydrate leaves
        // the mark set, so it reports `false` and the ticker retries.
        self.rebuild_symbol_graph().await.merge_error.is_none()
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
