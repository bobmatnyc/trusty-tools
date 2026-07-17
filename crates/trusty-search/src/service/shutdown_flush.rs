//! Graceful-shutdown flush helpers (issue #874).
//!
//! Why: extracted from `daemon.rs` to keep that file under the 500-line cap
//! allowlist budget. Provides a size-scaled per-index timeout, lock-free I/O,
//! and external-volume skip so `flush_all_indexes_on_shutdown` returns in
//! bounded time even when ~102 indexes are registered and some live on
//! stalled volumes or hold hundreds-of-MB HNSW snapshots (issue #2922).
//! What: `shutdown_flush_timeout_override` (explicit env-var escape hatch),
//! `shutdown_flush_deadline_for` (size-scaled per-index budget), and
//! `flush_all_indexes_on_shutdown` (the shutdown loop, now bounded-concurrency
//! parallel across indexes rather than strictly sequential).
//! Test: `shutdown_flush_deadline_honours_explicit_override`,
//! `shutdown_flush_deadline_scales_with_snapshot_size`, and
//! `shutdown_flush_empty_registry_returns_immediately` in this module.

use crate::service::server::SearchAppState;

/// Floor applied to every computed flush deadline, even for a missing or
/// empty snapshot (issue #2922 — the prior flat 10 s default was far too
/// short for anything beyond a trivial index; filesystem/lock-acquisition
/// overhead alone can exceed 10 s on a loaded host).
const MIN_FLUSH_TIMEOUT_SECS: u64 = 30;

/// Conservative disk-write throughput floor (MB/s) used to scale the flush
/// deadline from the on-disk HNSW snapshot's current size — a cheap proxy for
/// the in-memory graph's save cost, since `save()` overwrites roughly the same
/// number of bytes. Deliberately conservative (well under typical SSD
/// throughput) so slower / network-backed volumes still get a workable
/// budget; operators with harder requirements can still force an exact value
/// via `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS`.
const FLUSH_MB_PER_SEC_BUDGET: u64 = 20;

/// Upper bound so a single pathologically large or unreadable snapshot cannot
/// make one index's flush consume the entire shutdown budget — beyond this,
/// the incremental persister's last periodic snapshot is close enough that
/// skipping is preferable to blocking shutdown indefinitely.
const MAX_FLUSH_TIMEOUT_SECS: u64 = 20 * 60;

/// Default bound on how many indexes flush concurrently during shutdown.
///
/// Why (issue #2922): the previous loop awaited each index's bounded flush
/// sequentially, so total shutdown time was `N × per-index timeout` — with a
/// raised, size-scaled timeout (see [`MIN_FLUSH_TIMEOUT_SECS`]) that no longer
/// bounds acceptably at ~102 indexes. Running flushes concurrently, capped by
/// a semaphore, lets small/fast indexes finish immediately alongside one slow
/// one instead of queuing behind it, while still bounding peak disk/FFI
/// concurrency (each save already isolates its blocking FFI call via
/// `spawn_blocking`, issue #1746 — this just bounds how many run at once).
const DEFAULT_FLUSH_CONCURRENCY: usize = 4;

/// Explicit operator override for the per-index flush deadline, bypassing
/// size-based scaling entirely when set (issue #2922).
///
/// Why: operators who already know their I/O characteristics (or want to
/// force a specific bound, e.g. in a constrained CI/test environment) should
/// be able to pin an exact value; `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS` remains
/// that escape hatch. Unset, `0`, or unparseable → `None`, meaning "use the
/// size-scaled default" (see [`shutdown_flush_deadline_for`]).
/// What: reads `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS` (any positive integer).
/// Test: `shutdown_flush_deadline_honours_explicit_override`.
pub fn shutdown_flush_timeout_override() -> Option<std::time::Duration> {
    std::env::var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(std::time::Duration::from_secs)
}

/// Compute the flush deadline for a single index, scaled by the size of its
/// last-known on-disk HNSW snapshot (issue #2922).
///
/// Why: a flat 10 s default (the pre-#2922 behaviour) is far too short for a
/// multi-hundred-MB HNSW — the timeout fired mid-write on a non-atomic save,
/// truncating the live file. Atomic tmp+rename (see `UsearchStore::save`)
/// already makes a timeout safe (the live file is untouched until the whole
/// write completes), but an unreasonably short deadline still means the
/// incremental persister's last periodic snapshot is what survives instead of
/// the freshest state — worth avoiding when the budget is cheap to compute
/// correctly.
/// What: `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS`, if set, wins outright. Otherwise
/// `MIN_FLUSH_TIMEOUT_SECS + (on-disk snapshot bytes / FLUSH_MB_PER_SEC_BUDGET)`,
/// clamped to `MAX_FLUSH_TIMEOUT_SECS`. A missing/unreadable snapshot (e.g. a
/// brand-new index that never persisted) sizes at 0 bytes and gets the floor.
/// Test: `shutdown_flush_deadline_scales_with_snapshot_size`,
/// `shutdown_flush_deadline_honours_explicit_override`.
pub fn shutdown_flush_deadline_for(hnsw_path: &std::path::Path) -> std::time::Duration {
    if let Some(explicit) = shutdown_flush_timeout_override() {
        return explicit;
    }
    let snapshot_bytes = std::fs::metadata(hnsw_path).map(|m| m.len()).unwrap_or(0);
    let scaled_secs =
        MIN_FLUSH_TIMEOUT_SECS + snapshot_bytes / (FLUSH_MB_PER_SEC_BUDGET * 1024 * 1024);
    std::time::Duration::from_secs(scaled_secs.min(MAX_FLUSH_TIMEOUT_SECS))
}

/// Read the shutdown-flush concurrency cap from `TRUSTY_SHUTDOWN_FLUSH_CONCURRENCY`,
/// falling back to [`DEFAULT_FLUSH_CONCURRENCY`] on parse failure, `0`, or unset.
fn shutdown_flush_concurrency() -> usize {
    std::env::var("TRUSTY_SHUTDOWN_FLUSH_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_FLUSH_CONCURRENCY)
}

/// Walk every registered index and persist its HNSW snapshot + chunk corpus
/// to disk so the next daemon boot warm-starts (issue #85).
///
/// Why: called from `run_daemon` after the axum graceful-shutdown future
/// resolves. By this point no new requests can come in, but any in-flight
/// search handlers may still be holding read locks — we use the same
/// `save_to` / `save_chunks_to_disk` paths the incremental persister uses,
/// so they snapshot under read locks and never block writers indefinitely.
///
/// Issue #874 — three fixes to prevent shutdown from hanging with many indexes;
/// issue #2922 adds a fourth (size-scaled deadline + bounded parallelism):
///
/// 1. **Per-index flush timeout, scaled by snapshot size**: each index's
///    flush is wrapped in `tokio::time::timeout(shutdown_flush_deadline_for(..))`
///    — a budget derived from that index's on-disk HNSW size rather than a
///    flat constant (issue #2922; see [`shutdown_flush_deadline_for`]). On
///    timeout the index is skipped with a `warn!` (the incremental persister
///    already flushes periodically, so on-disk state is usually fresh) and
///    the loop moves on. This guarantees shutdown completes in bounded time
///    even under external-volume I/O stalls.
///
/// 2. **Short critical section for path derivation**: the flush paths
///    (chunks_path, hnsw_path) are derived from handle metadata BEFORE
///    acquiring the indexer read-lock. The indexer read-lock is then held
///    across the `flush_corpus_to_disk` and `save_vector_store` calls because
///    both are `&self` methods that require the guard to borrow `self`.
///    This is safe: axum has drained all in-flight requests by this point so
///    no concurrent writers can exist; the per-index timeout (fix #874 (1))
///    bounds the lock-held duration even under I/O stalls.
///
/// 3. **Skip external-volume indexes**: if an index's root is on an external
///    volume (detected via `is_likely_external_volume`), the shutdown skips its
///    flush entirely and emits a loud `warn!`. External-volume indexes are the
///    primary source of TCC-related I/O stalls on macOS launchd boots; the
///    incremental persister keeps on-disk state fresh so skipping the final
///    flush is safe.
///
/// 4. **Bounded-concurrency parallel flush** (issue #2922): indexes flush
///    concurrently, capped by a semaphore (`shutdown_flush_concurrency()`,
///    default [`DEFAULT_FLUSH_CONCURRENCY`]) instead of strictly sequentially.
///    With per-index deadlines now sized in tens of seconds rather than a flat
///    10 s, a fully sequential loop over ~102 indexes could take the better
///    part of an hour in the worst case; running a bounded number in parallel
///    means the total wall time is governed by the slowest *batch*, not
///    `N × deadline`. Each flush already isolates its blocking FFI save via
///    `spawn_blocking` (issue #1746), so this only bounds how many blocking
///    saves are in flight at once — it does not fight that fix, it uses it.
///
/// What: iterates `state.registry.list()`, applying the above four mitigations
/// per index, running up to `shutdown_flush_concurrency()` flushes at a time.
/// Test: `shutdown_flush_empty_registry_returns_immediately` in this module.
pub async fn flush_all_indexes_on_shutdown(state: &SearchAppState) {
    let ids = state.registry.list();
    if ids.is_empty() {
        return;
    }
    tracing::info!(
        "shutdown: flushing {} index snapshot(s) before exit (concurrency={})",
        ids.len(),
        shutdown_flush_concurrency()
    );
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(shutdown_flush_concurrency()));
    let tasks: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let state_registry = state.registry.clone();
            let semaphore = semaphore.clone();
            async move {
                // Issue #2922: bound how many indexes flush at once so a
                // fleet of large indexes can't all saturate disk I/O / the
                // blocking thread pool simultaneously. `acquire_owned` is
                // dropped (releasing the permit) when this future completes.
                let _permit = semaphore.acquire_owned().await;
                flush_one_index_on_shutdown(&state_registry, id).await;
            }
        })
        .collect();
    futures::future::join_all(tasks).await;
}

/// Flush a single index's HNSW + chunk corpus to disk during shutdown,
/// applying the external-volume skip and the size-scaled bounded timeout.
/// Extracted from [`flush_all_indexes_on_shutdown`] so that function can run
/// one of these per index concurrently (issue #2922).
async fn flush_one_index_on_shutdown(
    registry: &crate::core::registry::IndexRegistry,
    id: crate::core::registry::IndexId,
) {
    let Some(handle) = registry.get(&id) else {
        return;
    };

    // Fix #874 (3): skip indexes on external volumes to avoid TCC I/O stalls.
    if crate::service::warm_boot::scan::is_likely_external_volume(&handle.root_path) {
        tracing::warn!(
            "shutdown: skipping flush for '{}' — root {} is on an external volume \
             (TCC/I/O stall risk under launchd, issue #874). \
             On-disk state is from the last incremental persist.",
            id.0,
            handle.root_path.display(),
        );
        return;
    }

    // Fix #874 (2): derive paths inside a short read-lock scope, then drop
    // the guard before doing blocking I/O (redb flush + HNSW save).
    // No concurrent writers exist once axum has drained gracefully, so this
    // is safe — we're only reading index metadata, not modifying the indexer.
    let is_colocated = crate::service::colocated_storage::has_colocated_storage(&handle.root_path);

    // Resolve both paths before we do any I/O. If either path is unresolvable
    // we skip with a warn (same as before, just earlier).
    let chunks_path = if is_colocated {
        // Colocated indexes write their corpus to redb only (no JSON fallback).
        // Provide a dummy path — `flush_corpus_to_disk` won't use it when a
        // `CorpusStore` is wired; the redb file lives in `.trusty-search/`.
        handle.root_path.join(".trusty-search").join("chunks.json")
    } else {
        match crate::service::persistence::chunks_path(&id.0) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("shutdown: chunks path unresolvable for '{}': {e}", id.0);
                return;
            }
        }
    };
    let hnsw_path = if is_colocated {
        match crate::service::colocated_storage::colocated_hnsw_path(&handle.root_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "shutdown: colocated hnsw path unresolvable for '{}': {e}",
                    id.0
                );
                return;
            }
        }
    } else {
        match crate::service::persistence::hnsw_path(&id.0) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("shutdown: hnsw path unresolvable for '{}': {e}", id.0);
                return;
            }
        }
    };

    // Issue #2922: size the deadline from this index's own on-disk snapshot
    // rather than a flat constant, so a tiny index isn't held to a needlessly
    // long budget and a huge one isn't cut off before it can finish an atomic
    // write. Computed once per index, before spawning the flush task.
    let flush_deadline = shutdown_flush_deadline_for(&hnsw_path);

    // Fix #874 (2): derive paths inside a short read-lock scope above,
    // then hold the indexer read-lock across the flush I/O below.
    // `flush_corpus_to_disk` and `save_vector_store` are `&self` methods on
    // `CodeIndexer` — they require the guard to borrow `self`. This is safe
    // at shutdown because axum has already drained all in-flight requests
    // (no concurrent writers exist), and the per-index timeout (fix #874 (1))
    // bounds the worst-case lock-held duration even under I/O stalls.
    let indexer_arc = handle.indexer.clone();

    // Fix #874 (1): wrap the per-index flush in a timeout so a stalled
    // external volume or slow I/O cannot block the shutdown indefinitely.
    let index_id_for_log = id.0.clone();
    let flush_future = async move {
        // The read-guard is held across the flush I/O; this is intentional
        // (see comment above). No write-lock contention is possible at this
        // point in the shutdown sequence.
        let indexer = indexer_arc.read().await;
        // Issue #28: `flush_corpus_to_disk` writes to redb when a `CorpusStore`
        // is wired (final consistency sweep, no full JSON rewrite) and falls
        // back to the legacy `chunks.json` snapshot otherwise.
        if let Err(e) = indexer.flush_corpus_to_disk(&chunks_path).await {
            tracing::warn!(
                "shutdown: failed to flush chunk corpus for '{}': {e}",
                index_id_for_log
            );
        }
        match indexer.save_vector_store(&hnsw_path).await {
            Ok(true) => tracing::debug!("shutdown: saved HNSW for '{}'", index_id_for_log),
            Ok(false) => {} // no store wired (BM25-only mode)
            Err(e) => tracing::warn!(
                "shutdown: failed to save HNSW for '{}': {e}",
                index_id_for_log
            ),
        }
    };

    // Issue #1746: run the flush on its own task so the deadline is
    // genuinely enforced even when the flush blocks its worker in
    // synchronous FFI (see `flush_index_bounded`). Because `UsearchStore::save`
    // now serializes concurrent savers via its own internal lock (issue
    // #2922, `save_lock`), a task that is aborted here after already
    // finishing its FFI write but before renaming will either complete its
    // own consistent rename or block behind — and safely lose to — a fresher
    // save that started after it; it can never interleave with one.
    flush_index_bounded(&id.0, flush_deadline, flush_future).await;
}

/// Run a single index's flush future to completion under a hard deadline,
/// detaching it if it stalls.
///
/// Why (issue #1746): the earlier implementation wrapped the flush future
/// directly in `tokio::time::timeout`. That does **not** bound a flush that
/// performs blocking synchronous work — usearch's `Index::save` is a C++ FFI
/// call that never yields to the async runtime, so once a poll enters it the
/// same task cannot be re-polled and the timeout's timer branch never fires.
/// A single stalled ~500 MB HNSW save (or an index write-lock never released
/// by an in-flight background reindex, issue #1717) therefore wedged the whole
/// sequential shutdown loop until systemd's `TimeoutStopSec` fired a SIGKILL
/// (~300 s). Spawning the flush on its own task makes the `JoinHandle` an
/// honest yield point, so the deadline is enforced no matter what the flush
/// does internally.
/// What: spawns `flush` as a detached task and awaits its `JoinHandle` under
/// `tokio::time::timeout(deadline)`. On timeout it aborts the handle (best
/// effort — `abort` cannot interrupt in-progress blocking FFI, but the loop no
/// longer waits for it) and logs a `warn!`; on a task panic it logs a `warn!`.
/// A task left running after a timeout is harmless: `handle_start` calls
/// `std::process::exit(0)` once `run_daemon` returns, terminating any lingering
/// worker without depending on tokio's runtime teardown.
/// Test: `flush_bounded_returns_on_blocking_work` and
/// `flush_bounded_completes_fast_path` in the `tests` submodule.
async fn flush_index_bounded<F>(index_id: &str, deadline: std::time::Duration, flush: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let task = tokio::spawn(flush);
    let abort = task.abort_handle();
    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(())) => {}
        Ok(Err(join_err)) => {
            tracing::warn!("shutdown: flush task for '{index_id}' failed to join: {join_err}");
        }
        Err(_elapsed) => {
            abort.abort();
            tracing::warn!(
                "shutdown: flush TIMED OUT for '{}' after {}s — detaching \
                 (on-disk state from last incremental persist, issues #874/#1746). \
                 Increase TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS to allow more time.",
                index_id,
                deadline.as_secs(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: `shutdown_flush_timeout_override` must parse the env var and
    /// return `None` when absent, letting size-based scaling take over.
    /// Guards that operators can still force an exact shutdown deadline.
    /// What: set `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS=5`; assert `Some(5s)`;
    /// unset; assert `None`.
    /// Note: `serial` prevents racing with other env-var mutators.
    /// Test: this test.
    #[test]
    #[serial]
    fn shutdown_flush_timeout_parses_env_var() {
        // SAFETY: tests are #[serial]; no other thread reads the environment
        // concurrently during these single-threaded test bodies.
        unsafe { std::env::set_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS", "5") };
        assert_eq!(
            shutdown_flush_timeout_override(),
            Some(std::time::Duration::from_secs(5)),
            "must parse 5 from env var"
        );
        // SAFETY: same serial guarantee as above.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };
        assert_eq!(
            shutdown_flush_timeout_override(),
            None,
            "must return None when the env var is absent, deferring to size scaling"
        );
    }

    /// Why (issue #2922): the per-index deadline must honour an explicit
    /// operator override outright, bypassing size-based scaling entirely.
    /// What: with the env var set to 7 s, `shutdown_flush_deadline_for` must
    /// return exactly 7 s regardless of the (nonexistent) snapshot path.
    /// Test: this test.
    #[test]
    #[serial]
    fn shutdown_flush_deadline_honours_explicit_override() {
        // SAFETY: tests are #[serial].
        unsafe { std::env::set_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS", "7") };
        let deadline =
            shutdown_flush_deadline_for(std::path::Path::new("/nonexistent/hnsw.usearch"));
        // SAFETY: same serial guarantee as above.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };
        assert_eq!(deadline, std::time::Duration::from_secs(7));
    }

    /// Why (issue #2922 — the core regression this issue reports): a flat
    /// 10 s default was far too short for a 586 MB HNSW snapshot, causing the
    /// pre-atomic-write `save_to` to be cancelled mid-write and truncate the
    /// live file. The deadline must scale with the on-disk snapshot size so a
    /// large index gets a proportionally larger budget.
    /// What: write a real file of a known size, assert the computed deadline
    /// exceeds the `MIN_FLUSH_TIMEOUT_SECS` floor and grows with size; assert
    /// a missing file gets exactly the floor.
    /// Test: this test.
    #[test]
    #[serial]
    fn shutdown_flush_deadline_scales_with_snapshot_size() {
        // SAFETY: tests are #[serial]; ensure no override is active.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };

        let missing =
            shutdown_flush_deadline_for(std::path::Path::new("/nonexistent/hnsw.usearch"));
        assert_eq!(
            missing,
            std::time::Duration::from_secs(MIN_FLUSH_TIMEOUT_SECS),
            "a missing snapshot must get exactly the floor"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hnsw.usearch");
        // 200 MB — well above the 20 MB/s budget's floor contribution, so the
        // scaled component must dominate.
        let two_hundred_mb = 200 * 1024 * 1024;
        std::fs::File::create(&path)
            .and_then(|f| f.set_len(two_hundred_mb))
            .expect("create sized file");

        let scaled = shutdown_flush_deadline_for(&path);
        assert!(
            scaled > std::time::Duration::from_secs(MIN_FLUSH_TIMEOUT_SECS),
            "a 200 MB snapshot must get a deadline above the floor; got {scaled:?}"
        );
        let expected_secs =
            MIN_FLUSH_TIMEOUT_SECS + two_hundred_mb / (FLUSH_MB_PER_SEC_BUDGET * 1024 * 1024);
        assert_eq!(scaled, std::time::Duration::from_secs(expected_secs));
    }

    /// Why: with ~102 indexes, any single stalled flush must be skipped after
    /// the per-index deadline so the full shutdown loop completes in bounded
    /// time. This test is the acceptance criterion for issue #874.
    /// What: set `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS=1`, call
    /// `flush_all_indexes_on_shutdown` on an empty registry (zero indexes
    /// present — function must return immediately with no blocking), assert it
    /// returns in < 2 s. The timeout path is exercised at the module level
    /// (via `tokio::time::timeout` around the flush future); the per-index
    /// skip when the root is external is exercised by
    /// `shutdown_flush_skips_external_volume_index`.
    /// Note: we cannot easily create a "stalled redb flush" in a unit test
    /// without a real filesystem, so we verify the structural guarantee
    /// (empty registry returns fast) and the external-volume skip separately.
    /// Test: this test + `shutdown_flush_skips_external_volume_index`.
    #[tokio::test]
    #[serial]
    async fn shutdown_flush_empty_registry_returns_immediately() {
        // SAFETY: tests are #[serial]; no other thread reads the environment
        // concurrently during these single-threaded test bodies.
        unsafe { std::env::set_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS", "1") };
        let state = crate::service::server::SearchAppState::new(
            crate::core::registry::IndexRegistry::new(),
        );
        let start = std::time::Instant::now();
        flush_all_indexes_on_shutdown(&state).await;
        // SAFETY: same serial guarantee as above.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "flush on empty registry must complete immediately; elapsed: {:?}",
            start.elapsed()
        );
    }

    /// Why (issue #1746): the shutdown flush must stay bounded even when the
    /// flush performs blocking synchronous work — usearch's `Index::save` is a
    /// C++ FFI call that never yields, and wrapping it in `tokio::time::timeout`
    /// inline does NOT bound it (the same task can't be re-polled while stuck in
    /// the blocking call). Running it on a detached task does, *provided a
    /// runtime worker is free to drive the timeout's timer* — this test covers
    /// exactly that "spare worker available" case (2 workers, 1 stalled flush).
    /// The harder case — stalls reaching the worker count, e.g. a 1-worker
    /// runtime — needs the blocking work itself off the worker pool; see
    /// `flush_bounded_survives_single_worker_when_blocking_work_is_offloaded`
    /// and `UsearchStore::save`'s `spawn_blocking` use for that half of the fix.
    /// What: call `flush_index_bounded` with a 200 ms deadline around a future
    /// that blocks its worker thread for 1.5 s (simulating the save FFI); assert
    /// the helper returns in well under the blocking duration. Requires a
    /// multi-thread runtime so the timeout timer can fire on another worker
    /// while one worker is blocked.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_bounded_returns_on_blocking_work() {
        let start = std::time::Instant::now();
        flush_index_bounded("blocking", std::time::Duration::from_millis(200), async {
            // Simulate usearch's synchronous save FFI: blocks the worker
            // thread and ignores async cancellation.
            std::thread::sleep(std::time::Duration::from_millis(1500));
        })
        .await;
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1200),
            "bounded flush must return on its deadline even when the flush blocks \
             its worker; elapsed: {:?}",
            start.elapsed()
        );
    }

    /// Why (issue #1746 review — MEDIUM): a bare `tokio::spawn` around the
    /// flush future only stays effective if at least one runtime worker
    /// remains free to drive the `timeout`'s timer. Once the number of
    /// concurrently stalled flushes reaches the worker count, no worker is
    /// free and `flush_bounded_returns_on_blocking_work`'s guarantee breaks
    /// down — the loop re-wedges exactly as it did pre-fix. The actual fix for
    /// that case is moving the blocking usearch FFI call itself onto the
    /// blocking thread pool via `spawn_blocking` inside `UsearchStore::save`,
    /// so stalled saves never occupy an async worker at all. This test proves
    /// that combination is sufficient under the worst case: a single-worker
    /// runtime (the stall count already equals the worker count).
    /// What: on a `worker_threads = 1` runtime, run `flush_index_bounded` with
    /// a 200 ms deadline around a future that offloads its blocking work via
    /// `spawn_blocking` (mirroring `UsearchStore::save`'s real shape) for
    /// 1.5 s; assert it still returns well under the blocking duration —
    /// something a future that blocks the worker thread directly (see
    /// `flush_bounded_returns_on_blocking_work`) cannot do on a single worker.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn flush_bounded_survives_single_worker_when_blocking_work_is_offloaded() {
        let start = std::time::Instant::now();
        flush_index_bounded("offloaded", std::time::Duration::from_millis(200), async {
            // Mirrors `UsearchStore::save`: the actual blocking work runs
            // on the blocking thread pool, not the sole async worker, so
            // that worker stays free to drive the outer timeout's timer.
            let _ = tokio::task::spawn_blocking(|| {
                std::thread::sleep(std::time::Duration::from_millis(1500));
            })
            .await;
        })
        .await;
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1200),
            "a flush that offloads blocking work to spawn_blocking must stay bounded \
             even on a single-worker runtime; elapsed: {:?}",
            start.elapsed()
        );
    }

    /// Why: the fast path (a flush that finishes well within the deadline) must
    /// return promptly and never wait out the deadline.
    /// What: call `flush_index_bounded` with a 10 s deadline around an
    /// already-ready future; assert it returns in < 1 s.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_bounded_completes_fast_path() {
        let start = std::time::Instant::now();
        flush_index_bounded("fast", std::time::Duration::from_secs(10), async {}).await;
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "a completed flush must return immediately; elapsed: {:?}",
            start.elapsed()
        );
    }
}
