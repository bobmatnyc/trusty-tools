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
//! Test: `super::relink_9450_tests`.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::{anyhow, Result};
use usearch::Index;

use super::usearch_requant::ReadItem;

/// A key is reachable when its own vector returns it among this many hits.
/// Wider than the rank-1 test criterion so an exact duplicate vector, which
/// can tie for rank 1, does not read as a stranding.
const SWEEP_K: usize = 10;

/// Most sweep-and-repair rounds per build; a clean sweep ends the loop early.
const MAX_PASSES: usize = 4;

/// Search every item `0..n` of `read` with its own vector on `threads`
/// threads; returns the `(item, key)` pairs the index did not return.
pub(super) fn find_stranded(
    index: &Index,
    n: usize,
    dim: usize,
    threads: usize,
    read: &ReadItem<'_>,
) -> Result<Vec<(usize, u64)>> {
    let stranded: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let per_thread = n.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for start in (0..n).step_by(per_thread) {
            let end = start.saturating_add(per_thread).min(n);
            let (stranded, first_error) = (&stranded, &first_error);
            scope.spawn(move || {
                let mut buffer = vec![0f32; dim];
                let mut mine = Vec::new();
                for i in start..end {
                    let step = read(i, &mut buffer).and_then(|found| {
                        let Some(key) = found else { return Ok(()) };
                        let hits = index
                            .search(&buffer, SWEEP_K)
                            .map_err(|e| anyhow!("usearch self-search of key {key} failed: {e}"))?;
                        if !hits.keys.contains(&key) {
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
    Ok(found)
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
    read: &ReadItem<'_>,
) -> Result<usize> {
    let started = Instant::now();
    let mut relinked = 0usize;
    let mut buffer = vec![0f32; dim];
    for _ in 0..MAX_PASSES {
        let stranded = find_stranded(index, n, dim, threads, read)?;
        if stranded.is_empty() {
            return Ok(finish(relinked, 0, label, started));
        }
        for (i, key) in stranded {
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
            relinked += 1;
        }
    }
    let left = find_stranded(index, n, dim, threads, read)?.len();
    Ok(finish(relinked, left, label, started))
}

/// Log the sweep's outcome (silent when nothing was stranded) and pass the
/// count through.
fn finish(relinked: usize, left: usize, label: &str, started: Instant) -> usize {
    if left > 0 {
        tracing::warn!(
            "usearch: {left} vectors still stranded after {MAX_PASSES} re-link passes at {label} (#9450)"
        );
    }
    if relinked > 0 {
        tracing::warn!(
            "usearch: re-linked {relinked} stranded vectors after rebuild at {label} \
             in {} ms (#9450)",
            started.elapsed().as_millis()
        );
    }
    relinked
}
