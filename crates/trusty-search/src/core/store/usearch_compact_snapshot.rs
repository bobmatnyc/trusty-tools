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
//! The copy lives in fixed-size pieces ([`PIECE_BYTES`]), not one buffer.
//! Test: `super::compact_9450_tests::a_write_during_a_compaction_waits_only_for_the_copy`,
//! `compaction_rss_stays_flat_across_rounds` (`tests/hnsw_compact_9450.rs`).

use anyhow::{anyhow, Result};
use usearch::{Index, ScalarKind};

use super::usearch_requant::{add_parallel, new_rebuild_index, rebuild_threads, RebuildFault};
use super::usearch_store::UsearchStore;

/// Size of one piece of the vector copy: 16 MiB, rounded down to whole rows.
///
/// Why (#9450): one `keys x dim` buffer is ~230 MB at 150K x 384, and its
/// size changes whenever the key count does. The macOS allocator kept each
/// freed buffer resident when the next copy asked for a different size, so
/// RSS rose by a copy per compaction (455 -> 1144 MB over six rounds). Pieces
/// of one fixed size are reused by the next copy instead, and the size never
/// depends on the key count. Chosen over an anonymous mmap because that needs
/// `unsafe` and a new dependency for the same effect.
const PIECE_BYTES: usize = 16 << 20;

/// Rows of `dim` f32 values that fit in one piece; at least one.
fn rows_per_piece(dim: usize) -> usize {
    (PIECE_BYTES / dim.max(1).saturating_mul(size_of::<f32>())).max(1)
}

/// Every mapped vector of one store, copied at one mutation boundary.
///
/// Invariant: `present.len() == keys.len()`; row `i` lives in
/// `pieces[i / rows]` at `(i % rows) * dim`, and every piece holds
/// `rows * dim` values. `present[i]` is false when the graph held no vector
/// for `keys[i]` (map drift, skipped as `rebuild_at` does).
pub(super) struct VectorSnapshot {
    keys: Vec<u64>,
    pieces: Vec<Vec<f32>>,
    rows: usize,
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

    /// The copied vector of `keys[i]`.
    fn row(&self, i: usize) -> &[f32] {
        let at = (i % self.rows) * self.dim;
        &self.pieces[i / self.rows][at..at + self.dim]
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
            let rows = rows_per_piece(dim);
            let (pieces, present) = extract_vectors(&source, &keys, dim, rows);
            VectorSnapshot {
                keys,
                pieces,
                rows,
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

/// Read `keys` from `source` into pieces of `rows` rows each; returns them
/// and which keys the graph held. Each thread fills whole pieces.
fn extract_vectors(
    source: &Index,
    keys: &[u64],
    dim: usize,
    rows: usize,
) -> (Vec<Vec<f32>>, Vec<bool>) {
    let mut present = vec![false; keys.len()];
    if dim == 0 || keys.is_empty() {
        return (Vec::new(), present);
    }
    // #9450: every piece has the same size, the last one included.
    let mut pieces: Vec<Vec<f32>> = (0..keys.len().div_ceil(rows))
        .map(|_| vec![0f32; rows * dim])
        .collect();
    let per_thread = pieces.len().div_ceil(rebuild_threads(keys.len())).max(1);
    std::thread::scope(|scope| {
        let parts = keys
            .chunks(per_thread * rows)
            .zip(pieces.chunks_mut(per_thread))
            .zip(present.chunks_mut(per_thread * rows));
        for ((chunk, out), found) in parts {
            scope.spawn(move || {
                let slots = out.iter_mut().flat_map(|piece| piece.chunks_mut(dim));
                for ((&key, slot), hit) in chunk.iter().zip(slots).zip(found) {
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
    (pieces, present)
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
        let (n, dim) = (snapshot.keys.len(), snapshot.dim);
        let rebuilt = new_rebuild_index(dim, snapshot.kind, n, &label)?;
        let missing = add_parallel(&rebuilt, n, dim, &label, &fault, &|i, buffer| {
            if !snapshot.present[i] {
                return Ok(None);
            }
            buffer.copy_from_slice(snapshot.row(i));
            Ok(Some(snapshot.keys[i]))
        })?;
        Ok((rebuilt, missing))
    })
    .await
    .map_err(|e| anyhow!("usearch compaction build task panicked: {e}"))?
}
