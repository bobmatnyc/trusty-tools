//! HNSW graph compaction after remove churn (issue #9450).
//!
//! Why: usearch 2.25.2 `Index::remove` unlinks a node without re-linking its
//! neighbours, and a later `add` reuses the freed slot, so in-links that
//! pointed at the removed vector now lead somewhere unrelated. Under heavy
//! churn a surviving vector is left with no useful in-links and an unfiltered
//! search cannot reach it, even with its own embedding as the query (#9414's
//! report: absent from the top 800 at ef 256, rank 1 under a path filter).
//! The Rust API exposes no compaction, so the repair is a fresh build, which
//! links every surviving node again.
//!
//! What: [`CompactState`] counts churn (removals plus replacements) on every
//! store mutation path and carries the graph-heal generation; both persist in
//! the key sidecar, so the count survives a restart where
//! `removed_since_save` does not. [`UsearchStore::compact_graph_now`]
//! rebuilds at the CURRENT precision through `rebuild_at` (the #6822
//! requantize machinery) and swaps under the HNSW write lock (#6826). It
//! writes nothing to disk; the caller's next save publishes the rebuilt graph
//! through the shrink-guarded `save`. Callers:
//! - the idle write-cooldown persist and the incremental persister, once
//!   churn crosses [`compact_threshold`];
//! - M005, unconditionally, after it drops orphaned vectors;
//! - [`UsearchStore::heal_on_load`], once per pre-#9450 snapshot.
//!
//! Test: `super::tests_9450`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio::sync::Semaphore;

use super::super::store_config::{MmapServeMode, VectorQuant};
use super::types::{CompactMode, CompactReport, ReindexProbe};
use super::usearch_requant::{no_fault, RebuildFault};
use super::usearch_store::UsearchStore;

/// The graph-heal generation a snapshot is stamped with once rebuilt.
///
/// A sidecar written before #9450 carries no `heal_epoch` and reads as 0, so
/// [`UsearchStore::heal_on_load`] compacts it once; a later release that
/// needs another one-time heal bumps this number.
pub(super) const GRAPH_HEAL_EPOCH: u32 = 1;

/// Churn, as a fraction of live keys, that makes a compaction due: 1/10.
///
/// Why 10%: `tests_9450::threshold_margin_sweep` (20K clustered keys, f16,
/// default graph, three seeds) counts survivors that miss rank-1 self-recall
/// as churn since the last rebuild grows. A fresh build missed 0/2/0; at 10%
/// churn 1/4/1, at 20% 3/5/2, at 30% 8/12/3, at 100% 38/46/39. Stranding
/// starts with the first churn and grows faster than linearly, so the
/// threshold trades rebuild cost against that tail: at 10% the damage stays
/// within ~2 survivors per 20K of a fresh build, while a 150K-key index
/// rebuilds at most once per 15K churn events (about 10 re-inserted vectors
/// per churn event, amortised).
const COMPACT_CHURN_DIVISOR: u64 = 10;

/// Floor on the threshold so a tiny index does not rebuild on every persist.
const MIN_COMPACT_CHURN: u64 = 32;

/// One compaction at a time across the daemon (#9450): each rebuild holds a
/// second copy of its graph on the heap until the swap, so a warm boot
/// healing every index at once would multiply the peak. The load heal keeps
/// the permit through its save and re-view, so that copy is released before
/// the next heal starts.
static HEAL_GATE: Semaphore = Semaphore::const_new(1);

/// Churn that makes a compaction due for an index holding `live` vectors.
/// Test: `super::tests_9450::the_threshold_scales_with_live_keys`.
pub(super) fn compact_threshold(live: usize) -> u64 {
    (live as u64 / COMPACT_CHURN_DIVISOR).max(MIN_COMPACT_CHURN)
}

/// Churn counter and graph-heal generation for one store (#9450).
///
/// Invariant: `churn` and `heal_epoch` change only under the store's
/// `save_lock`, so `save` captures them at the same mutation boundary as the
/// graph and the key map.
#[derive(Debug)]
pub(crate) struct CompactState {
    churn: AtomicU64,
    heal_epoch: AtomicU32,
    /// After a failed `IfDue` compaction, the churn count at which to try
    /// again; 0 = no back-off. In memory only.
    retry_at_churn: AtomicU64,
}

impl CompactState {
    /// A store built from scratch: every churn event is counted from birth,
    /// so it needs no one-time heal.
    pub(super) fn fresh() -> Self {
        Self {
            churn: AtomicU64::new(0),
            heal_epoch: AtomicU32::new(GRAPH_HEAL_EPOCH),
            retry_at_churn: AtomicU64::new(0),
        }
    }

    /// Take the values a sidecar (or an adopted snapshot) recorded.
    pub(super) fn restore(&self, churn: u64, heal_epoch: u32) {
        self.churn.store(churn, Ordering::Release);
        self.heal_epoch.store(heal_epoch, Ordering::Release);
        self.retry_at_churn.store(0, Ordering::Release);
    }

    /// Count `n` removals or replacements.
    pub(super) fn record(&self, n: u64) {
        self.churn.fetch_add(n, Ordering::AcqRel);
    }

    pub(super) fn churn(&self) -> u64 {
        self.churn.load(Ordering::Acquire)
    }

    pub(super) fn heal_epoch(&self) -> u32 {
        self.heal_epoch.load(Ordering::Acquire)
    }
}

impl UsearchStore {
    /// Removals plus replacements since the graph was last rebuilt (#9450).
    pub fn churn_since_compact(&self) -> u64 {
        self.compact.churn()
    }

    /// The graph-heal generation of the live graph (#9450); test accessor.
    #[doc(hidden)]
    pub fn graph_heal_epoch(&self) -> u32 {
        self.compact.heal_epoch()
    }

    /// `true` when churn has crossed [`compact_threshold`] and no back-off
    /// from a failed attempt is pending.
    async fn compaction_due(&self) -> bool {
        let live = self.index.read().await.size();
        let churn = self.compact.churn();
        churn >= compact_threshold(live)
            && churn >= self.compact.retry_at_churn.load(Ordering::Acquire)
    }

    /// Rebuild the graph now (`Always`) or only when churn is due (`IfDue`).
    ///
    /// Why: see the module docs.
    /// What: `IfDue` returns `Ok(None)` below the threshold. Otherwise waits
    /// for the daemon-wide heal gate, then [`Self::compact_with_fault`]. A
    /// failed `IfDue` attempt backs off until churn grows by another
    /// threshold, so a rebuild that cannot succeed is not retried on every
    /// persist. Writes nothing to disk.
    /// Test: `super::tests_9450::churn_past_the_threshold_compacts_at_the_next_persist`,
    /// `super::tests_9450::a_failed_compaction_keeps_the_churn_count_and_marker`.
    pub async fn compact_graph_now(&self, mode: CompactMode) -> Result<Option<CompactReport>> {
        if mode == CompactMode::IfDue && !self.compaction_due().await {
            return Ok(None);
        }
        let _permit = HEAL_GATE
            .acquire()
            .await
            .map_err(|e| anyhow!("compaction gate closed: {e}"))?;
        match self.compact_with_fault(no_fault()).await {
            Ok(report) => Ok(Some(report)),
            Err(e) => {
                if mode == CompactMode::IfDue {
                    let live = self.index.read().await.size();
                    let retry = self.compact.churn().saturating_add(compact_threshold(live));
                    self.compact.retry_at_churn.store(retry, Ordering::Release);
                }
                Err(e)
            }
        }
    }

    /// The compaction itself, with a fault hook for crash-safety tests.
    ///
    /// What: under `save_lock` (writers wait; searches keep their read lock),
    /// rebuilds every mapped vector at the live precision. Before the swap it
    /// checks that the new graph holds every key the old one returned; any
    /// rebuild error or a failed check returns `Err` with the old graph, the
    /// churn count and the heal marker untouched. After the swap: the store
    /// is mutable and dirty, churn is zero, and the heal marker is current.
    /// Test: `super::tests_9450::compaction_keeps_every_key`,
    /// `super::tests_9450::a_failed_rebuild_leaves_the_old_index_serving`.
    pub(super) async fn compact_with_fault(&self, fault: RebuildFault) -> Result<CompactReport> {
        let _mutation_guard = self.save_lock.lock().await;
        self.refuse_if_closed("compact")?;
        let started = Instant::now();
        let (kind, vectors_before) = {
            let index = self.index.read().await;
            (index.scalar_kind(), index.size())
        };
        let mapped = self.id_to_key.read().await.len();
        let churn = self.compact.churn();
        let label = VectorQuant::from_scalar_kind(kind).map_or("the live precision", |q| q.label());

        let (rebuilt, missing) = self.rebuild_at(kind, label, fault).await?;
        let vectors_after = rebuilt.size();
        // #9450 postcondition: every mapped key the old graph returned is in
        // the new one. `rebuild_at` aborts on a failed add, so a mismatch
        // means a key was silently lost — refuse the swap.
        if vectors_after + missing != mapped {
            return Err(anyhow!(
                "compaction rebuilt {vectors_after} vectors for {mapped} mapped keys \
                 ({missing} unreadable) — refusing to swap (#9450)"
            ));
        }
        {
            let mut index = self.index.write().await;
            *index = rebuilt;
            // #6826: the flags that describe the swap move under the same lock.
            self.is_view.store(false, Ordering::Release);
            self.mark_dirty();
        }
        // Graph entries no chunk id maps to are unreachable and are not
        // rebuilt; credit them so `save`'s #1717 shrink guard reads the drop
        // as explained.
        let unmapped = vectors_before.saturating_sub(vectors_after) as u64;
        self.removed_since_save
            .fetch_add(unmapped, Ordering::AcqRel);
        self.compact.churn.fetch_sub(churn, Ordering::AcqRel);
        self.compact
            .heal_epoch
            .store(GRAPH_HEAL_EPOCH, Ordering::Release);
        self.compact.retry_at_churn.store(0, Ordering::Release);

        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            "usearch: compacted HNSW graph ({vectors_before} → {vectors_after} vectors, \
             {churn} churn cleared, {missing} unreadable, {elapsed_ms} ms) (#9450)"
        );
        Ok(CompactReport {
            vectors_before,
            vectors_after,
            churn_cleared: churn,
            missing,
            elapsed_ms,
        })
    }

    /// One compaction for a snapshot that predates #9450, or whose recorded
    /// churn is already due; then save, and return to the mmap view.
    ///
    /// Why: a snapshot written before the churn counter existed may already
    /// hold stranded vectors, and its count of past churn is unknown.
    /// What: no-op (`Ok(None)`) when the heal marker is current and churn is
    /// below the threshold, when no snapshot path is recorded, or when
    /// `reindexing()` reports a staged reindex. Otherwise, one store at a
    /// time across the daemon: compact, `save` to the recorded path (which
    /// stamps the marker into the sidecar), and re-view the snapshot unless
    /// `TRUSTY_HNSW_MMAP_SERVE` asked for heap serving. A reindex that starts
    /// during the compaction skips the save: its own staged checkpoints carry
    /// the rebuilt graph and marker, and the live snapshot stays the
    /// pre-reindex one an abort restores (#3975). A failed compaction or
    /// save leaves the on-disk marker unset, so the next load heals again.
    /// Test: `super::tests_9450::the_load_heal_runs_once`,
    /// `super::tests_9450::the_load_heal_never_saves_during_a_reindex`.
    pub async fn heal_on_load(
        &self,
        reindexing: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<CompactReport>> {
        let needs_heal = |store: &Self| store.compact.heal_epoch() < GRAPH_HEAL_EPOCH;
        if !needs_heal(self) && !self.compaction_due().await {
            return Ok(None);
        }
        let _permit = HEAL_GATE
            .acquire()
            .await
            .map_err(|e| anyhow!("compaction gate closed: {e}"))?;
        // Re-check: another trigger may have compacted while this one waited.
        if (!needs_heal(self) && !self.compaction_due().await) || reindexing() {
            return Ok(None);
        }
        let Some(path) = self.hnsw_path.read().await.clone() else {
            return Ok(None);
        };
        let report = self.compact_with_fault(no_fault()).await?;
        if reindexing() {
            return Ok(Some(report));
        }
        self.save(&path).await?;
        if !MmapServeMode::from_env().promote_on_load() {
            self.try_demote_to_view().await?;
        }
        Ok(Some(report))
    }

    /// Run [`Self::heal_on_load`] in the background for a freshly loaded
    /// store, logging the outcome (#9450). Warm boot does not wait on it.
    pub fn spawn_heal_on_load(self: Arc<Self>, index_id: String, reindexing: ReindexProbe) {
        tokio::spawn(async move {
            match self.heal_on_load(&*reindexing).await {
                Ok(Some(r)) => tracing::info!(
                    "load heal: compacted '{index_id}' ({} vectors, {} ms) (#9450)",
                    r.vectors_after,
                    r.elapsed_ms
                ),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "load heal: could not compact '{index_id}' ({e:#}) — the old graph keeps \
                     serving and the next load retries (#9450)"
                ),
            }
        });
    }
}
