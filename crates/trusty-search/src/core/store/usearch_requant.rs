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

use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use usearch::{Index, IndexOptions, MetricKind};

use super::super::store_config::VectorQuant;
use super::types::RequantizeReport;
use super::usearch_store::{hnsw_max_elements, UsearchStore};

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
        let (rebuilt, missing) = self.rebuild_at(target).await?;
        report.missing = missing;

        {
            let mut index = self.index.write().await;
            *index = rebuilt;
        }
        // The replacement is a heap-resident mutable index, and nothing on disk
        // matches it until the save below.
        self.is_view.store(false, Ordering::Release);
        self.dirty.store(true, Ordering::Release);

        // #6822: the durable write keeps `save`'s own guards — a conversion
        // that lost most of the arena is refused there rather than published.
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
    /// `target` precision.
    ///
    /// Why split out: keeps [`Self::requantize`]'s control flow readable and
    /// confines the usearch handling to one place. Held separately so the
    /// expensive re-encode runs under a READ lock — the write lock is taken
    /// only for the pointer swap.
    /// What: reserves for the full vector count, then for each `(id, key)` in
    /// `id_to_key` reads the stored vector through `Index::get::<f32>` (usearch
    /// casts from whatever the source precision is) and `add`s it under the
    /// SAME key, so the sidecar mapping stays valid verbatim. Returns the new
    /// index plus the count of keys the source index could not return.
    /// Test: `tests/vector_quant_default_6822.rs::backfill_converts_an_f32_index_to_f16_and_keeps_recall`.
    async fn rebuild_at(&self, target: VectorQuant) -> Result<(Index, usize)> {
        let keys: Vec<u64> = {
            let id_map = self.id_to_key.read().await;
            id_map.values().copied().collect()
        };
        let source = self.index.read().await;
        // Mirror `with_capacity_hint`'s graph tuning so a converted index keeps
        // the recall characteristics it was built with.
        let (connectivity, expansion_add, expansion_search) = if keys.len() > 50_000 {
            (32, 128, 64)
        } else {
            (0, 0, 0)
        };
        let rebuilt = Index::new(&IndexOptions {
            dimensions: self.dim,
            metric: MetricKind::Cos,
            quantization: target.scalar_kind(),
            connectivity,
            expansion_add,
            expansion_search,
            multi: false,
        })
        .map_err(|e| {
            anyhow!(
                "usearch Index::new for requantize to {} failed: {e}",
                target.label()
            )
        })?;
        rebuilt
            .reserve(keys.len().max(1).min(hnsw_max_elements()))
            .map_err(|e| anyhow!("usearch reserve for requantize failed: {e}"))?;

        let mut buffer = vec![0f32; self.dim];
        let mut missing = 0usize;
        for key in keys {
            match source.get(key, &mut buffer) {
                Ok(n) if n > 0 => rebuilt.add(key, &buffer).map_err(|e| {
                    anyhow!("usearch add during requantize (key {key}) failed: {e}")
                })?,
                // The key map named a vector the graph does not hold. Pre-existing
                // drift, not something this conversion caused — count and skip.
                Ok(_) => missing += 1,
                Err(e) => {
                    tracing::warn!("usearch: requantize could not read key {key}: {e} — skipping");
                    missing += 1;
                }
            }
        }
        Ok((rebuilt, missing))
    }
}
