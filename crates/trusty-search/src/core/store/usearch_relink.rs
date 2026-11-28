//! Reachability sweep after an HNSW rebuild (issue #9450).
//!
//! Why: an HNSW graph depends on insert order, and now and then a node ends up
//! with no in-link a search can follow: the key is in the graph but not even
//! its own vector finds it. Concurrent `add`s make this differ from run to run
//! (1 survivor in 6000, about once in 165 runs of
//! `survivors_self_recall_after_heavy_remove_churn`, more often under CPU
//! load). A single-threaded build is no safer in production: the rebuild
//! reads keys from the store's `HashMap`, whose per-process `RandomState`
//! makes the insert order differ per process, and a sequential 100K build in
//! the tests stranded a node too. `rebuild_threads` is 1 below 10K keys and
//! on small machines, so those builds are swept as well.
//! What: [`relink_stranded`] searches every key with its own vector and
//! re-inserts (remove, then add, one thread) each key that is not among the
//! top [`SWEEP_K`] hits. It repeats up to [`MAX_PASSES`] times, because a
//! `remove` can strand a neighbour. `add_parallel` runs it after every build.
//! A key missing from its hits while one hit holds the identical stored
//! vector is a duplicate tie, not a strand (#11891).
//! Test: `super::relink_9450_tests`, `super::relink_dup_11891_tests`.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::{anyhow, Result};
use usearch::Index;

use super::usearch_requant::ReadItem;

/// A key is reachable when its own vector returns it among this many hits,
/// or returns a key holding the identical stored vector (#11891).
const SWEEP_K: usize = 10;

/// Most sweep-and-repair rounds per build; a clean sweep ends the loop early.
const MAX_PASSES: usize = 4;

/// One sweep's verdict: the graph-stranded `(item, key)` pairs, sorted, and
/// how many keys were reachable only through an identical-vector tie.
struct Sweep {
    stranded: Vec<(usize, u64)>,
    duplicate_ties: usize,
}

/// Search every item `0..n` of `read` with its own vector on `threads`
/// threads; returns the `(item, key)` pairs the index did not return and
/// that are not duplicate ties (see [`ties_with_duplicate`]).
/// Test: `super::relink_dup_11891_tests::duplicate_ties_are_reachable_and_real_strands_still_relink`.
// #11891: production calls `sweep` directly; this view serves the tests.
#[cfg(test)]
pub(super) fn find_stranded(
    index: &Index,
    n: usize,
    dim: usize,
    threads: usize,
    read: &ReadItem<'_>,
) -> Result<Vec<(usize, u64)>> {
    Ok(sweep(index, n, dim, threads, read)?.stranded)
}

/// True when one of `hits` holds the same stored vector as `key`, so the
/// self-search reached `key`'s vector and only the tie order left `key` out.
/// Why: more than [`SWEEP_K`] identical vectors cannot all appear in one
/// top-[`SWEEP_K`] search, and re-inserting them never changes that (#11891).
/// What: compares the decoded stored vectors (same quantization, so an exact
/// duplicate decodes to identical floats) of `key` and each hit.
/// Test: `super::relink_dup_11891_tests::duplicate_ties_are_reachable_and_real_strands_still_relink`.
fn ties_with_duplicate(
    index: &Index,
    key: u64,
    hits: &[u64],
    own: &mut [f32],
    other: &mut [f32],
) -> Result<bool> {
    let got = index
        .get(key, own)
        .map_err(|e| anyhow!("usearch read of key {key} failed: {e}"))?;
    if got == 0 {
        return Ok(false);
    }
    for &hit in hits {
        let got = index
            .get(hit, other)
            .map_err(|e| anyhow!("usearch read of key {hit} failed: {e}"))?;
        if got > 0 && own == other {
            return Ok(true);
        }
    }
    Ok(false)
}

/// [`find_stranded`] plus the duplicate-tie count.
fn sweep(
    index: &Index,
    n: usize,
    dim: usize,
    threads: usize,
    read: &ReadItem<'_>,
) -> Result<Sweep> {
    let stranded: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
    let ties = std::sync::atomic::AtomicUsize::new(0);
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let per_thread = n.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for start in (0..n).step_by(per_thread) {
            let end = start.saturating_add(per_thread).min(n);
            let (stranded, first_error, ties) = (&stranded, &first_error, &ties);
            scope.spawn(move || {
                let mut buffer = vec![0f32; dim];
                let (mut own, mut other) = (vec![0f32; dim], vec![0f32; dim]);
                let mut mine = Vec::new();
                for i in start..end {
                    let step = read(i, &mut buffer).and_then(|found| {
                        let Some(key) = found else { return Ok(()) };
                        let hits = index
                            .search(&buffer, SWEEP_K)
                            .map_err(|e| anyhow!("usearch self-search of key {key} failed: {e}"))?;
                        if hits.keys.contains(&key) {
                            return Ok(());
                        }
                        // #11891: an identical-vector hit is a tie, not a strand.
                        if ties_with_duplicate(index, key, &hits.keys, &mut own, &mut other)? {
                            ties.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        } else {
                            mine.push((i, key));
                        }
                        Ok(())
                    });
                    if let Err(e) = step {
                        let mut slot = first_error.lock().unwrap_or_else(|p| p.into_inner());
                        slot.get_or_insert(e);
                        return;
                    }
                }
                stranded
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend(mine);
            });
        }
    });
    if let Some(e) = first_error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    let mut found = stranded.into_inner().unwrap_or_else(|p| p.into_inner());
    found.sort_unstable();
    Ok(Sweep {
        stranded: found,
        duplicate_ties: ties.into_inner(),
    })
}

/// What one [`relink_stranded`] call saw, for its log line.
struct Outcome {
    /// Graph-stranded keys found by each sweep, in order; the last entry is
    /// the final sweep (0 when the loop converged).
    passes: Vec<usize>,
    /// Duplicate ties seen by the first sweep.
    duplicate_ties: usize,
    relinked: usize,
}

/// Make every key of `read` reachable by its own vector; returns how many
/// re-insertions that took, counted per pass.
///
/// Why: see the module doc. Postcondition: on `Ok`, the last sweep's strand
/// count is logged; a key still stranded after [`MAX_PASSES`] is reported, not
/// an error (the graph is then no worse than before the sweep).
/// What: per pass, [`find_stranded`] on `threads` threads, then `remove` plus
/// `add` of each stranded key on this thread. The index must be quiescent.
/// Test: `super::relink_9450_tests::the_sweep_relinks_forced_strandings`.
pub(super) fn relink_stranded(
    index: &Index,
    n: usize,
    dim: usize,
    threads: usize,
    label: &str,
    path: &str,
    read: &ReadItem<'_>,
) -> Result<usize> {
    let started = Instant::now();
    let mut out = Outcome {
        passes: Vec::with_capacity(MAX_PASSES + 1),
        duplicate_ties: 0,
        relinked: 0,
    };
    let mut buffer = vec![0f32; dim];
    for pass in 0..MAX_PASSES {
        let found = sweep(index, n, dim, threads, read)?;
        if pass == 0 {
            out.duplicate_ties = found.duplicate_ties;
        }
        out.passes.push(found.stranded.len());
        if found.stranded.is_empty() {
            return Ok(finish(&out, label, path, started));
        }
        for (i, key) in found.stranded {
            let Some(read_key) = read(i, &mut buffer)? else {
                continue;
            };
            debug_assert_eq!(read_key, key, "item {i} changed key between passes");
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch re-link remove of key {key} failed: {e}"))?;
            index
                .add(key, &buffer)
                .map_err(|e| anyhow!("usearch re-link add of key {key} failed: {e}"))?;
            out.relinked += 1;
        }
    }
    out.passes
        .push(sweep(index, n, dim, threads, read)?.stranded.len());
    Ok(finish(&out, label, path, started))
}

/// Log the sweep's outcome (silent when nothing was stranded or tied) and
/// pass the re-link count through. `path` is the index's snapshot file, so a
/// line names the index it concerns.
/// Test: `super::relink_warn_11891_tests::the_relink_warnings_name_the_index_path`.
fn finish(out: &Outcome, label: &str, path: &str, started: Instant) -> usize {
    let (relinked, left) = (out.relinked, out.passes.last().copied().unwrap_or(0));
    let graph_stranded = out.passes.first().copied().unwrap_or(0);
    // #11891: per-pass, duplicate-tie and graph-strand counts, the live data
    // that tells a removal cascade (H1) or f16 tie collapse (H3) apart.
    if graph_stranded > 0 || out.duplicate_ties > 0 {
        tracing::info!(
            path = %path,
            passes = ?out.passes,
            duplicate_ties = out.duplicate_ties,
            graph_stranded,
            left,
            "usearch: re-link sweep at {label} (#11891)"
        );
    }
    // #11891: `path` names the index; `label` alone (`f16`) named none.
    if left > 0 {
        tracing::warn!(
            path = %path,
            "usearch: {left} vectors still stranded after {MAX_PASSES} re-link passes at {label} (#9450)"
        );
    }
    if relinked > 0 {
        tracing::warn!(
            path = %path,
            "usearch: re-linked {relinked} stranded vectors after rebuild at {label} \
             in {} ms (#9450)",
            started.elapsed().as_millis()
        );
    }
    relinked
}
