//! The copy a graph compaction rebuilds from (issue #9450).
//!
//! Why: a rebuild of a 124K-vector graph takes ~9 s. Run under the store's
//! mutation gate, it made every index writer wait that long while holding
//! `indexer.write()`, and the tokio `RwLock` queues the searches that arrive
//! meanwhile behind that writer, so a `trusty-search query` (5 s client
//! timeout) failed. Reading from the live graph without the gate is no
//! better: `rebuild_at` holds the graph's read lock throughout, so a writer
//! waits on the graph write lock instead, with the same queueing.
//! What: [`UsearchStore::snapshot_vectors`] copies every mapped vector out as
//! `f32` while the caller holds the gate (milliseconds of parallel reads),
//! together with the graph epoch and churn count it was taken at;
//! [`build_from_snapshot`] builds the new graph from that copy with no store
//! lock held. The caller swaps only if [`UsearchStore::graph_epoch`] and the
//! churn count are unchanged, so a write during the build is never lost.
//! Test: `super::tests_9450::a_write_during_a_compaction_waits_only_for_the_copy`.

use anyhow::{anyhow, Result};
use usearch::{Index, ScalarKind};

use super::usearch_requant::{add_parallel, new_rebuild_index, rebuild_threads, RebuildFault};
use super::usearch_store::UsearchStore;

/// Every mapped vector of one store, copied at one mutation boundary.
///
/// Invariant: `data.len() == keys.len() * dim` and
/// `present.len() == keys.len()`; `present[i]` is false when the graph held
/// no vector for `keys[i]` (map drift, skipped as `rebuild_at` does).
pub(super) struct VectorSnapshot {
    keys: Vec<u64>,
    data: Vec<f32>,
    present: Vec<bool>,
    dim: usize,
    pub(super) kind: ScalarKind,
    /// Graph size when the copy was taken.
    pub(super) vectors_before: usize,
    /// [`UsearchStore::graph_epoch`] when the copy was taken.
    pub(super) epoch: u64,
    /// Churn count when the copy was taken.
    pub(super) churn: u64,
}

impl VectorSnapshot {
    /// Mapped keys the copy covers, found or not.
    pub(super) fn mapped(&self) -> usize {
        self.keys.len()
    }
}

impl UsearchStore {
    /// A counter that moves on every change to the graph's contents: each
    /// `mark_dirty` write and each whole-graph adoption (#9450).
    pub(super) fn graph_epoch(&self) -> u64 {
        self.write_clock
            .epoch()
            .wrapping_add(self.compact.replacements())
    }

    /// Copy every mapped vector out of the live graph.
    ///
    /// Precondition: the caller holds `save_lock`, so the key map, the graph,
    /// the epoch and the churn count describe one mutation boundary.
    /// What: reads under the graph READ lock (searches keep running) on the
    /// blocking pool, on [`rebuild_threads`] threads.
    pub(super) async fn snapshot_vectors(&self) -> Result<VectorSnapshot> {
        let keys: Vec<u64> = self.id_to_key.read().await.values().copied().collect();
        let source = self.index.clone().read_owned().await;
        let (kind, vectors_before) = (source.scalar_kind(), source.size());
        let (epoch, churn) = (self.graph_epoch(), self.compact.churn());
        let dim = self.dim;
        tokio::task::spawn_blocking(move || {
            let (data, present) = extract_vectors(&source, &keys, dim);
            VectorSnapshot {
                keys,
                data,
                present,
                dim,
                kind,
                vectors_before,
                epoch,
                churn,
            }
        })
        .await
        .map_err(|e| anyhow!("usearch compaction copy task panicked: {e}"))
    }
}

/// Read `keys` from `source` into one flat buffer; returns it and which keys
/// the graph held.
fn extract_vectors(source: &Index, keys: &[u64], dim: usize) -> (Vec<f32>, Vec<bool>) {
    let mut data = vec![0f32; keys.len().saturating_mul(dim)];
    let mut present = vec![false; keys.len()];
    if dim == 0 || keys.is_empty() {
        return (data, present);
    }
    let per_thread = keys.len().div_ceil(rebuild_threads(keys.len())).max(1);
    std::thread::scope(|scope| {
        let parts = keys
            .chunks(per_thread)
            .zip(data.chunks_mut(per_thread * dim))
            .zip(present.chunks_mut(per_thread));
        for ((chunk, out), found) in parts {
            scope.spawn(move || {
                for ((&key, slot), hit) in chunk.iter().zip(out.chunks_mut(dim)).zip(found) {
                    *hit = match source.get(key, slot) {
                        Ok(n) => n > 0,
                        Err(e) => {
                            tracing::warn!(
                                "usearch: compaction could not read key {key}: {e} — skipping"
                            );
                            false
                        }
                    };
                }
            });
        }
    });
    (data, present)
}

/// Build a fresh graph from `snapshot` at its precision, holding no store
/// lock; returns it and the count of keys the copy had no vector for.
/// `fault` runs before each add (#9450 crash-safety tests).
pub(super) async fn build_from_snapshot(
    snapshot: VectorSnapshot,
    label: String,
    fault: RebuildFault,
) -> Result<(Index, usize)> {
    tokio::task::spawn_blocking(move || -> Result<(Index, usize)> {
        let VectorSnapshot {
            keys,
            data,
            present,
            dim,
            kind,
            ..
        } = snapshot;
        let rebuilt = new_rebuild_index(dim, kind, keys.len(), &label)?;
        let missing = add_parallel(&rebuilt, keys.len(), dim, &label, &fault, &|i, buffer| {
            if !present[i] {
                return Ok(None);
            }
            buffer.copy_from_slice(&data[i * dim..(i + 1) * dim]);
            Ok(Some(keys[i]))
        })?;
        Ok((rebuilt, missing))
    })
    .await
    .map_err(|e| anyhow!("usearch compaction build task panicked: {e}"))?
}
