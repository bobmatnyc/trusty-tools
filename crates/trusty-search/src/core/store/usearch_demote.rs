//! HNSW view↔heap demotion state machine (issues #2164 and #6826).
//!
//! Why: `load_from` opens a snapshot as a read-only mmap `Index::view` (#709),
//! and the first write promotes it to a heap copy that nothing ever released.
//! #2164 added a demote for the case where the graph already matches disk;
//! #6826 adds the case that actually dominates a developer machine — a store
//! that HAS been written, has gone quiet, and needs saving before it can be
//! re-viewed. Both live here so the whole transition, and the locking argument
//! that makes it safe, is one file rather than a rule split across `save`,
//! `promote_view_to_mutable`, and the idle ticker.
//! What: [`WriteClock`] (when the last graph mutation happened, and a
//! monotonic epoch that says whether one happened during a save),
//! [`UsearchStore::mark_dirty`] (the single write-record point),
//! [`UsearchStore::try_demote_to_view`] (#2164, clean stores) and
//! [`UsearchStore::try_demote_after_write_cooldown`] (#6826, written stores).
//! Test: `super::tests::test_demote_to_view_*` and
//! `super::tests::test_demote_after_write_cooldown_*`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::types::DemoteStats;
use super::usearch_store::UsearchStore;

/// When this store's HNSW graph was last mutated, plus a monotonic counter of
/// how many times (issue #6826).
///
/// Why: the demote-after-write path has to answer two questions the `dirty`
/// flag alone cannot. "Has this store been quiet long enough to be worth
/// saving?" needs a TIMESTAMP, and a bool has none. "Did a write land while
/// `save()` was running?" needs an EPOCH, because `save()` releases the HNSW
/// write lock after the FFI serialize and before it clears `dirty` — a writer
/// slipping into that window would have its mutation un-flagged, and the
/// demote would then re-view a snapshot that does not contain it. Comparing
/// the epoch across the whole save turns that into a refusal to clear `dirty`.
/// What: `last_write_millis` is milliseconds since `origin` PLUS ONE, so `0`
/// stays an unambiguous "never written" sentinel; `epoch` counts recorded
/// mutations and is only ever incremented. Both are written by
/// [`UsearchStore::mark_dirty`] while its caller still holds the HNSW write
/// lock, so a demote re-checking under that same lock cannot miss one.
/// Test: `super::tests::test_demote_after_write_cooldown_waits_for_cooldown`
/// covers the timestamp; `super::tests::test_demote_after_write_cooldown_never_loses_a_racing_write`
/// covers the epoch.
pub(super) struct WriteClock {
    origin: Instant,
    epoch: AtomicU64,
    last_write_millis: AtomicU64,
}

impl WriteClock {
    /// A clock for a store that has not been written yet.
    pub(super) fn new() -> Self {
        Self {
            origin: Instant::now(),
            epoch: AtomicU64::new(0),
            last_write_millis: AtomicU64::new(0),
        }
    }

    /// Record one graph mutation: bump the epoch and stamp the time.
    ///
    /// Callers must already hold the HNSW write lock — see the struct doc.
    pub(super) fn record_write(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let millis = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_write_millis
            .store(millis.saturating_add(1), Ordering::Release);
    }

    /// Current mutation epoch. Compare two reads to learn whether a write
    /// happened between them.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// How long since the last recorded write, or `None` when this store has
    /// never been written.
    pub(super) fn since_last_write(&self) -> Option<Duration> {
        let stamped = self.last_write_millis.load(Ordering::Acquire);
        if stamped == 0 {
            return None;
        }
        Some(
            self.origin
                .elapsed()
                .saturating_sub(Duration::from_millis(stamped - 1)),
        )
    }
}

impl Default for WriteClock {
    fn default() -> Self {
        Self::new()
    }
}

impl UsearchStore {
    /// Record that the HNSW graph was just mutated (issues #2164, #6826).
    ///
    /// Why: `dirty` and the write clock have to move together or the demote
    /// paths disagree about the same mutation — one refusing to demote while
    /// the other believes the store has been quiet for hours. Making this the
    /// only place either is set keeps them in step.
    /// What: sets `dirty` and stamps [`WriteClock::record_write`]. Every caller
    /// (`upsert`, `remove`, `upsert_batch`, the rebuild-from-view fallback, the
    /// requantize rewrite, the #6299 staged-swap abort) calls this while
    /// holding the HNSW write lock, so a demote's under-lock re-check always
    /// observes it.
    ///
    /// Key rewrites also call this (#6961): an unsaved sidecar change must
    /// prevent demotion, or a later view-mode save would skip those new IDs.
    /// Test: `super::tests::test_demote_to_view_skips_when_dirty`.
    pub(super) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        self.write_clock.record_write();
    }

    /// Demote a promoted, currently-idle, ALREADY-CLEAN store back to
    /// mmap-view mode, releasing the heap-resident HNSW copy (issue #2164).
    ///
    /// Why: the #709 mmap-view optimization keeps a freshly warm-booted HNSW
    /// index pageable (`load_from` calls `Index::view`), but ANY write —
    /// `index_file`/`reindex`/file-watcher commit — promotes it to a full heap
    /// copy via [`UsearchStore::promote_view_to_mutable`], and until this
    /// method there was no path back. Live measurement on a 77-index
    /// production daemon found mmap-resident was only ~80 KB total.
    /// What: a no-op (`Ok(false)`) unless the store is currently mutable
    /// (`is_view == false`), has no unpersisted writes (`dirty == false`), and
    /// has a known source path to re-open (`hnsw_path.is_some()`). Under those
    /// conditions, takes the HNSW write lock — excluding concurrent searches
    /// (read lock) and concurrent writers/promoters (same write lock) —
    /// re-checks the same conditions, then calls `Index::view` on the *same*
    /// `Index` handle, mirroring how `promote_view_to_mutable` calls
    /// `Index::load` in place for the reverse transition. `id_to_key` /
    /// `key_to_id` are never touched. Returns `Ok(true)` on an actual demotion.
    ///
    /// CRITICAL correctness (never lose vectors): this only re-views from a
    /// snapshot that is byte-for-byte the current in-memory graph — the
    /// `dirty` gate refuses whenever there is a mutation `save()` has not
    /// flushed. Demotion is an optimization, not a correctness requirement, so
    /// any doubt (unreadable path, non-UTF8 path, a racing writer, a `view()`
    /// failure) means "skip this cycle", never "demote anyway".
    /// Test: `super::tests::test_demote_to_view_full_cycle`,
    /// `super::tests::test_demote_to_view_skips_when_dirty`,
    /// `super::tests::test_demote_to_view_skips_without_path`.
    pub(super) async fn try_demote_to_view(&self) -> Result<bool> {
        let _mutation_guard = self.save_lock.lock().await;
        // Fast pre-check without the write lock: skip the common case
        // (already a view, dirty, or never associated with a source path)
        // without contending for the lock searches and writers also need.
        if self.is_view.load(Ordering::Acquire) {
            return Ok(false);
        }
        if self.dirty.load(Ordering::Acquire) {
            return Ok(false);
        }
        let path = {
            let guard = self.hnsw_path.read().await;
            guard.clone()
        };
        let Some(path) = path else {
            return Ok(false);
        };
        let Some(path_str) = path.to_str() else {
            tracing::warn!(
                "usearch: cannot demote {} to view — non-utf8 path",
                path.display()
            );
            return Ok(false);
        };
        let path_str = path_str.to_string();

        let index = self.index.write().await;
        // Re-check under the write lock: `is_view` and `dirty` are only ever
        // flipped by a caller holding this same write lock, so the values
        // observed here are authoritative — a racing promote/mutate/demote
        // between the fast pre-check and this point is caught here.
        if self.is_view.load(Ordering::Acquire) || self.dirty.load(Ordering::Acquire) {
            return Ok(false);
        }

        if let Err(e) = index.view(&path_str) {
            tracing::warn!(
                "usearch: failed to demote {} to view ({e}) — leaving index heap-resident",
                path.display()
            );
            return Ok(false);
        }
        let size = index.size();
        self.is_view.store(true, Ordering::Release);
        tracing::info!(
            "usearch: demoted mutable → view for {} ({} vectors, heap reclaimed)",
            path.display(),
            size
        );
        Ok(true)
    }

    /// Persist a WRITTEN store that has been quiet for `cooldown`, then demote
    /// it back to an mmap view (issue #6826).
    ///
    /// Why: [`Self::try_demote_to_view`] refuses whenever `dirty` is set, and a
    /// store that has taken even one write is dirty until something saves it.
    /// Nothing did, so on the 128 GB reference host every actively-edited index
    /// stayed heap-resident for the daemon's whole life — 76 MB mmap-resident
    /// against 9 GB of heap across 56 indexes. Saving the store ourselves after
    /// a write-idle cooldown is what turns that one-way promotion into a cycle.
    /// What: skips unless the store is mutable, dirty, has a recorded snapshot
    /// path, and its last write is at least `cooldown` old. Then `save()`s to
    /// that path and calls [`Self::try_demote_to_view`], which re-checks every
    /// gate under the HNSW write lock. Returns `Ok(Some(stats))` only when the
    /// store actually became a view.
    ///
    /// # Concurrency argument
    ///
    /// **The outer gate excludes mutation** (#6961): `save_lock` covers
    /// capture and publication in `save`, and the complete mutation in each
    /// writer. It also covers demotion's path lookup and view swap. Internal
    /// promotion/adoption helpers do not reacquire it. This cooldown wrapper
    /// holds no gate across its calls to `save` and `try_demote_to_view`.
    ///
    /// **No torn read.** Searches use the graph read lock, and serialization
    /// and view replacement use its write lock. Snapshot files are staged
    /// and renamed, so readers never map a partially written file.
    ///
    /// **No lost write.** A mutation before save acquires the gate is included
    /// in the snapshot. A mutation arriving during save waits for publication
    /// and then marks the graph dirty. A mutation between save and demotion
    /// also sets dirty; demotion rechecks it under both the gate and graph
    /// write lock. A writer after demotion promotes before touching the graph.
    /// The epoch comparison in save remains an additional safeguard.
    ///
    /// **The cooldown is advisory, not a gate.** It only decides WHEN to try;
    /// correctness rests entirely on the `dirty` re-check under the write lock,
    /// exactly as #2164's demote does.
    /// Test: `super::tests::test_demote_after_write_cooldown_persists_and_views`,
    /// `super::tests::test_demote_after_write_cooldown_waits_for_cooldown`,
    /// `super::tests::test_demote_after_write_cooldown_disabled_by_zero`,
    /// `super::tests::test_demote_after_write_cooldown_never_loses_a_racing_write`.
    pub(super) async fn try_demote_after_write_cooldown(
        &self,
        cooldown: Duration,
    ) -> Result<Option<DemoteStats>> {
        // A zero cooldown is the operator's "off" switch, not "demote now" —
        // see `store_config::hnsw_demote_cooldown`.
        if cooldown.is_zero() {
            return Ok(None);
        }
        // Already a view: nothing on the heap to reclaim.
        if self.is_view.load(Ordering::Acquire) {
            return Ok(None);
        }
        // Clean already: #2164's demote owns that case and needs no save.
        if !self.dirty.load(Ordering::Acquire) {
            return Ok(None);
        }
        match self.write_clock.since_last_write() {
            // Dirty with no recorded write means the flag came from a path
            // that does not go through `mark_dirty` — refuse rather than guess
            // at how stale the graph is.
            None => return Ok(None),
            Some(since) if since < cooldown => return Ok(None),
            Some(_) => {}
        }
        let path = {
            let guard = self.hnsw_path.read().await;
            guard.clone()
        };
        // No recorded snapshot path: a store built by `new()` that has never
        // been saved. `save()` records the path, so it becomes eligible after
        // its first persist; nothing here has to invent a destination.
        let Some(path) = path else {
            return Ok(None);
        };

        let started = Instant::now();
        self.save(&path).await?;
        if !self.try_demote_to_view().await? {
            // A write landed during or after the save. The save itself is
            // still useful work — the snapshot is fresher — so this is a
            // skipped demote, not an error.
            return Ok(None);
        }

        let vectors = self.index.read().await.size();
        // Heap released is not directly measurable through usearch's API. The
        // on-disk snapshot is the serialized form of exactly the arena that
        // was just dropped, so its size is the honest proxy, reported as one.
        let snapshot_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let elapsed = started.elapsed();
        tracing::info!(
            "usearch: demoted written store {} back to mmap-view after {}s write-idle \
             ({} vectors, ~{} bytes of heap released per the on-disk snapshot, took {} ms)",
            path.display(),
            cooldown.as_secs(),
            vectors,
            snapshot_bytes,
            elapsed.as_millis(),
        );
        Ok(Some(DemoteStats {
            vectors,
            snapshot_bytes,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        }))
    }
}
