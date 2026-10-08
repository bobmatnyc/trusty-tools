//! In-place scalar-precision backfill for an already-built HNSW index
//! (issue #6822).
//!
//! Why: #6822 flips the `TRUSTY_VECTOR_QUANT` default to `f16`, which only ever
//! reaches an index at CREATION time — usearch writes the scalar kind into the
//! snapshot header and rebuilds the metric from it on every `load`/`view`, so
//! every index already on disk keeps its `f32` vectors forever. A forced
//! reindex does not help either: the store object is built once at warm-boot
//! (`service::persistence_loader::build_store_for_entry`) and a reindex upserts
//! into that same handle, so `reindex --force` re-embeds at the OLD precision.
//! Without this module the default flip saves nothing on any existing fleet —
//! the "silent no-op" failure #6822 names.
//!
//! What: [`UsearchStore::requantize`] reads every vector out of the live index
//! as `f32`, rebuilds a fresh `usearch::Index` at the target [`VectorQuant`],
//! and swaps it in under the store's own HNSW write lock. The `chunk_id → u64`
//! key map is carried over unchanged, so no chunk id, no corpus row and no
//! sidecar mapping moves.
//!
//! Why this is not a reindex (issue #402): a reindex resolves a root, walks a
//! tree, and can re-point an index at a directory another index owns — the
//! hijack #402 records. This path takes no root, performs no walk, and never
//! re-registers a handle; it is addressed by index id alone and touches only
//! that index's vector arena. The corpus is not read, so it cannot be pruned.
//! The one durable write goes through [`UsearchStore::save`], which keeps its
//! #1711 empty-over-populated and #1717 unexplained-shrink guards — a
//! requantization that lost vectors is refused there, not silently published.
//!
//! Test: `tests/vector_quant_default_6822.rs` —
//! `backfill_converts_an_f32_index_to_f16_and_keeps_recall`,
//! `backfill_halves_the_vector_bytes_within_five_percent`,
//! `backfill_dry_run_reports_without_writing`,
//! `backfill_to_the_current_precision_is_a_no_op`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::super::store_config::VectorQuant;
use super::types::RequantizeReport;
use super::usearch_store::{hnsw_max_elements, UsearchStore};

/// A hook `rebuild_at` calls with the count of vectors added so far, before
/// each add; an `Err` aborts the rebuild (#9450 crash-safety tests).
pub(super) type RebuildFault = std::sync::Arc<dyn Fn(usize) -> Result<()> + Send + Sync>;

/// The production hook: never fails.
pub(super) fn no_fault() -> RebuildFault {
    std::sync::Arc::new(|_| Ok(()))
}

impl UsearchStore {
    /// The scalar precision the LIVE index actually holds, if this knob can
    /// express it.
    ///
    /// Why (#6822): `VectorQuant::from_env()` answers "what will the next index
    /// be built with", which is the wrong question for status reporting — a
    /// warm-booted snapshot carries its own precision. This reads the index.
    /// What: `Index::scalar_kind()` mapped back through
    /// [`VectorQuant::from_scalar_kind`]; `None` for a kind this crate never
    /// builds, so a caller reports "unknown" rather than guessing.
    /// Test: `tests/vector_quant_default_6822.rs::default_quant_for_a_new_index_is_f16`.
    pub async fn live_quant(&self) -> Option<VectorQuant> {
        VectorQuant::from_scalar_kind(self.index.read().await.scalar_kind())
    }

    /// Rebuild this index's vector arena at `target` precision, in place.
    ///
    /// Why: see the module docs — the #6822 default flip is inert on every
    /// index that already exists, and this is the operator's one-shot
    /// conversion. It exists as a store method (not a reindex option) because
    /// the conversion is purely a re-encode of vectors already held: no
    /// embedding, no tree walk, no root resolution.
    ///
    /// What: reports first, converts second.
    /// - Reads the live scalar kind. When it already equals `target`, returns a
    ///   report with `applied: false` and writes nothing — the operation is
    ///   idempotent, so re-running it across a fleet is safe.
    /// - `dry_run` returns the same report (index precision, vector count,
    ///   current on-disk bytes) without touching the index or the disk.
    /// - Otherwise: extracts every vector the key map names as `f32`, builds a
    ///   fresh `Index` at `target`, and installs it under the HNSW write lock.
    ///   The store is left mutable (`is_view = false`) and `dirty`, then
    ///   [`UsearchStore::save`] publishes it to the recorded snapshot path.
    ///
    /// A vector the key map names but the index cannot return is counted in
    /// `missing` and skipped rather than aborting — `id_to_key` can outlive a
    /// graph entry (see the removal-site comment in `usearch_impl::remove`),
    /// and refusing the whole conversion over pre-existing map drift would
    /// leave the index unconvertible forever. `save`'s #1717 shrink guard is
    /// what stops a LARGE loss from being published.
    ///
    /// Errors when the store has no recorded snapshot path (nothing to publish
    /// to), or when the rebuild itself fails — in which case the live index is
    /// left exactly as it was, because the swap is the last step.
    /// Test: see the module docs.
    pub async fn requantize(&self, target: VectorQuant, dry_run: bool) -> Result<RequantizeReport> {
        let mutation_guard = self.save_lock.lock().await;
        self.refuse_if_closed("requantize")?;
        let snapshot_path = self.hnsw_path.read().await.clone();
        let bytes_before = snapshot_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());

        let (current, vectors) = {
            let index = self.index.read().await;
            (
                VectorQuant::from_scalar_kind(index.scalar_kind()),
                index.size(),
            )
        };
        let mut report = RequantizeReport {
            current: current.map(|q| q.label()),
            target: target.label(),
            vectors,
            missing: 0,
            bytes_before,
            bytes_after: bytes_before,
            applied: false,
            dry_run,
        };

        if current == Some(target) {
            tracing::info!(
                "usearch: requantize to {} is a no-op — index already holds {} vector(s) at \
                 that precision (#6822)",
                target.label(),
                vectors,
            );
            return Ok(report);
        }
        if dry_run {
            return Ok(report);
        }

        let path = snapshot_path.ok_or_else(|| {
            anyhow!(
                "cannot requantize to {}: this index has no recorded HNSW snapshot path to \
                 publish to — reindex it once so a snapshot exists, then retry (#6822)",
                target.label()
            )
        })?;

        // Build the replacement OUTSIDE the write lock's mutation window: the
        // extraction below only needs a read lock, so concurrent searches keep
        // running for the (potentially long) re-encode of a large arena.
        let (rebuilt, missing) = self
            .rebuild_at(target.scalar_kind(), target.label(), no_fault())
            .await?;
        report.missing = missing;

        {
            let mut index = self.index.write().await;
            *index = rebuilt;
            // #6826: publish the swap and the flags that DESCRIBE it under the
            // same write lock. These two stores used to sit after the block, so
            // the handle was a fresh heap index for a window in which `is_view`
            // and `dirty` still described the old one — long enough on a
            // multi-thread runtime for a demote already queued on this lock to
            // wake and `Index::view` the stale on-disk file over the rebuilt
            // arena, or for `ensure_mutable` to `Index::load` it away. Either
            // one silently republishes the OLD precision while this function
            // reports `applied: true`.
            self.is_view.store(false, Ordering::Release);
            self.mark_dirty();
        }

        // #6822: the durable write keeps `save`'s own guards — a conversion
        // that lost most of the arena is refused there rather than published.
        // The replacement is complete and dirty. Release before public save
        // reacquires the same gate; any intervening writes are included there.
        drop(mutation_guard);
        self.save(&path).await?;
        report.applied = true;
        report.bytes_after = std::fs::metadata(&path).ok().map(|m| m.len());
        tracing::info!(
            "usearch: requantized {} from {} to {} ({} vector(s), {} skipped; {:?} → {:?} bytes) \
             (#6822)",
            path.display(),
            report.current.unwrap_or("unknown"),
            report.target,
            report.vectors,
            report.missing,
            report.bytes_before,
            report.bytes_after,
        );
        Ok(report)
    }

    /// Extract every mapped vector as `f32` and re-add it to a fresh index at
    /// `quantization` precision.
    ///
    /// Why split out: keeps [`Self::requantize`]'s control flow readable and
    /// confines the usearch handling to one place. Held separately so the
    /// expensive re-encode runs under a READ lock — the write lock is taken
    /// only for the pointer swap. #9450's compaction shares its index setup
    /// and add loop, but builds from a copy so it holds no lock meanwhile.
    /// What: reserves for the full vector count, then for each `(id, key)` in
    /// `id_to_key` reads the stored vector through `Index::get::<f32>` (usearch
    /// casts from whatever the source precision is) and `add`s it under the
    /// SAME key, so the sidecar mapping stays valid verbatim. Returns the new
    /// index plus the count of keys the source index could not return.
    /// `fault` is called with the count of vectors added so far before each
    /// add; an `Err` aborts the rebuild. Production passes [`no_fault`];
    /// The adds run on [`rebuild_threads`] threads (#9450).
    /// Test: `tests/vector_quant_default_6822.rs::backfill_converts_an_f32_index_to_f16_and_keeps_recall`.
    pub(super) async fn rebuild_at(
        &self,
        quantization: ScalarKind,
        label: &str,
        fault: RebuildFault,
    ) -> Result<(Index, usize)> {
        let keys: Vec<u64> = {
            let id_map = self.id_to_key.read().await;
            id_map.values().copied().collect()
        };
        let source = self.index.clone().read_owned().await;
        let dim = self.dim;
        let label = label.to_string();
        // #9450: the add loop is seconds of blocking FFI at 150K vectors; run
        // it on the blocking pool so it never pins a runtime worker (#1746).
        tokio::task::spawn_blocking(move || -> Result<(Index, usize)> {
            let rebuilt = new_rebuild_index(dim, quantization, keys.len(), &label)?;
            let missing = copy_vectors(&source, &rebuilt, &keys, dim, &label, &fault)?;
            Ok((rebuilt, missing))
        })
        .await
        .map_err(|e| anyhow!("usearch rebuild task panicked: {e}"))?
    }
}

/// Threads for one rebuild's add loop (#9450): half the cores, at most 8, and
/// one below 10K keys. A 150K x 384 f16 rebuild took 92 s on one thread and
/// 11 s on eight (release, 16 cores); usearch's index takes concurrent
/// `add`s, and half the cores leaves the rest for queries
/// (`tests/hnsw_compact_9450.rs`).
pub(super) fn rebuild_threads(keys: usize) -> usize {
    if keys < 10_000 {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cores / 2).clamp(1, 8)
}

/// An empty index sized for `n` vectors at `quantization`, tuned like
/// `with_capacity_hint` so a rebuilt index keeps its recall characteristics.
/// #9414: the same size-scaled table, so the two paths cannot drift.
pub(super) fn new_rebuild_index(
    dim: usize,
    quantization: ScalarKind,
    n: usize,
    label: &str,
) -> Result<Index> {
    let super::hnsw_tuning::HnswTuning {
        connectivity,
        expansion_add,
        expansion_search,
    } = super::hnsw_tuning::hnsw_tuning(n);
    let rebuilt = Index::new(&IndexOptions {
        dimensions: dim,
        metric: MetricKind::Cos,
        quantization,
        connectivity,
        expansion_add,
        expansion_search,
        multi: false,
    })
    .map_err(|e| anyhow!("usearch Index::new for rebuild at {label} failed: {e}"))?;
    rebuilt
        .reserve(n.max(1).min(hnsw_max_elements()))
        .map_err(|e| anyhow!("usearch reserve for rebuild at {label} failed: {e}"))?;
    Ok(rebuilt)
}

/// Copy every vector `keys` names from `source` into `rebuilt` under the same
/// key; returns the count `source` could not return.
/// Test: `tests/vector_quant_default_6822.rs::backfill_converts_an_f32_index_to_f16_and_keeps_recall`.
fn copy_vectors(
    source: &Index,
    rebuilt: &Index,
    keys: &[u64],
    dim: usize,
    label: &str,
    fault: &RebuildFault,
) -> Result<usize> {
    add_parallel(rebuilt, keys.len(), dim, label, fault, &|i, buffer| {
        let key = keys[i];
        match source.get(key, buffer) {
            Ok(n) if n > 0 => Ok(Some(key)),
            // The key map named a vector the graph does not hold.
            // Pre-existing drift, not caused here — count and skip.
            Ok(_) => Ok(None),
            Err(e) => {
                tracing::warn!("usearch: rebuild could not read key {key}: {e} — skipping");
                Ok(None)
            }
        }
    })
}

/// Reads item `i` of a rebuild source into the buffer and returns its key,
/// or `None` when the source has no vector for it.
pub(super) type ReadItem<'a> = dyn Fn(usize, &mut [f32]) -> Result<Option<u64>> + Sync + 'a;

/// Add items `0..n` of `read` to `rebuilt` on [`rebuild_threads`] threads;
/// returns the count `read` had no vector for. `fault` runs before each add.
/// The first read, add or fault error stops every thread and is returned.
/// Test: `super::tests_9450::a_failed_rebuild_leaves_the_old_index_serving`.
pub(super) fn add_parallel(
    rebuilt: &Index,
    n: usize,
    dim: usize,
    label: &str,
    fault: &RebuildFault,
    read: &ReadItem<'_>,
) -> Result<usize> {
    let missing = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let per_thread = n.div_ceil(rebuild_threads(n)).max(1);
    std::thread::scope(|scope| {
        for start in (0..n).step_by(per_thread) {
            let end = start.saturating_add(per_thread).min(n);
            let (missing, abort, first_error) = (&missing, &abort, &first_error);
            scope.spawn(move || {
                let mut buffer = vec![0f32; dim];
                for i in start..end {
                    if abort.load(Ordering::Acquire) {
                        return;
                    }
                    let step = fault(rebuilt.size()).and_then(|()| match read(i, &mut buffer)? {
                        Some(key) => rebuilt.add(key, &buffer).map_err(|e| {
                            anyhow!("usearch add during rebuild at {label} (key {key}) failed: {e}")
                        }),
                        None => {
                            missing.fetch_add(1, Ordering::AcqRel);
                            Ok(())
                        }
                    });
                    if let Err(e) = step {
                        abort.store(true, Ordering::Release);
                        let mut slot = first_error.lock().unwrap_or_else(|p| p.into_inner());
                        slot.get_or_insert(e);
                        return;
                    }
                }
            });
        }
    });
    match first_error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        Some(e) => Err(e),
        None => Ok(missing.into_inner()),
    }
}
