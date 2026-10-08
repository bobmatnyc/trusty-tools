//! Close every redb corpus before the daemon exits (#9459, #9477).
//!
//! Why: `handle_start` leaves through `process::exit(0)` once `run_daemon_with`
//! returns, which runs no `Drop`. redb writes its allocator state and clears
//! its recovery flag only in `Database::drop`, so every corpus still open at
//! that point is left needing repair. Dropping the state at the end of
//! `run_daemon_with` would not help either: the corpus reopen sweep and other
//! background tasks hold strong `SearchAppState` clones for the life of the
//! process. The next start then repaired every corpus (#9459), and a read-only
//! open — the one `project.resolve` uses for the `reindexed_unix` stamp —
//! failed with `RepairAborted` (#9477).
//! What: [`close_corpora_on_shutdown`] takes the corpus out of every
//! registered indexer no reindex holds and drops the last `Arc` itself, inside
//! a deadline.
//! Test: `a_normal_stop_leaves_every_corpus_openable_read_only` in
//! `service::daemon_tests`; this module's own tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::OwnedSemaphorePermit;

use crate::core::corpus::CorpusStore;
use crate::core::registry::IndexHandle;
use crate::service::SearchAppState;

/// How long a normal stop may spend closing corpora.
///
/// Why: the close runs after the shutdown flush, inside the 5 s
/// `trusty_common::shutdown::CLEANUP_RESERVE` that the flush budget leaves for
/// post-flush cleanup. 3 s leaves 2 s of that reserve for removing the
/// discovery files and exiting. A clean `Database::drop` is one small commit
/// and an fsync, normally milliseconds; the wait exists for a background task
/// that still holds a corpus clone for a moment. A stop is never slower than
/// before by more than this value.
/// What: one wall-clock deadline across the indexer locks, the clone waits and
/// the drops.
/// Test: `a_held_corpus_or_lock_cannot_stall_the_close`.
pub const SHUTDOWN_CORPUS_CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// How often the close re-checks a corpus another task still references.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Page cache for the post-close read-only probe; it reads the header only.
const PROBE_CACHE_BYTES: usize = 1 << 20;

/// What [`close_corpora_on_shutdown`] did.
#[derive(Debug, Default)]
pub struct CorpusCloseReport {
    /// Corpora dropped and confirmed clean by a read-only open.
    pub closed: usize,
    /// One line per corpus left open or not confirmed clean, naming the index
    /// and why. Each one needs repair on the next open, as before #9459.
    pub unclean: Vec<String>,
    /// The per-index permits the close took. Holding the report keeps every
    /// reindex, relocate and embed pass off those indexes.
    permits: Vec<OwnedSemaphorePermit>,
}

/// Close every registered index's redb corpus, bounded by `budget`.
///
/// Why: see the module docs. The close must never turn a stop into a hang, so
/// every wait shares one deadline, and anything still held at it is logged
/// and left for redb's repair on the next open — the pre-#9459 behaviour.
/// What: runs [`close_one`] for every registered handle concurrently, so one
/// held index cannot spend another's time, then logs the outcome: `info` when
/// every corpus closed clean, `warn` naming each one that did not.
///
/// An index whose reindex, relocate or embed pass is running keeps its corpus
/// and is reported unclean. Each indexer the close takes a corpus from is left
/// without one and marked closed for shutdown, so a later `index_file` or
/// `purge_file_committed` — the detached reconcile delta's writes — returns
/// `Err`, and the delta does not stamp the HEAD SHA. The returned
/// report holds the permits it took, so no reindex, relocate or embed pass
/// starts on those indexes while the caller keeps it. Call it only when the process is about to exit, and
/// hold the report until then.
///
/// Test: `a_normal_stop_leaves_every_corpus_openable_read_only`,
/// `a_held_corpus_or_lock_cannot_stall_the_close`,
/// `a_clone_released_inside_the_budget_is_closed_cleanly`,
/// `a_corpus_whose_reindex_is_running_stays_attached`,
/// `a_reconcile_delta_after_the_close_fails_and_does_not_stamp`.
pub async fn close_corpora_on_shutdown(
    state: &SearchAppState,
    budget: Duration,
) -> CorpusCloseReport {
    let deadline = Instant::now() + budget;
    let closes = state
        .registry
        .list_handles()
        .into_iter()
        .map(|handle| close_one(handle, deadline));
    let mut report = CorpusCloseReport::default();
    for (outcome, permit) in futures::future::join_all(closes).await {
        report.permits.extend(permit);
        match outcome {
            Ok(true) => report.closed += 1,
            Ok(false) => {}
            Err(line) => report.unclean.push(line),
        }
    }
    if report.unclean.is_empty() {
        if report.closed > 0 {
            tracing::info!("shutdown: closed {} corpus file(s) cleanly", report.closed);
        }
    } else {
        // #9459 Fail-Open Check: never silent. The next open repairs each one.
        tracing::warn!(
            "shutdown: closed {} corpus file(s) cleanly; {} were not closed within {budget:?} \
             and need repair on the next open (#9459): {}",
            report.closed,
            report.unclean.len(),
            report.unclean.join("; "),
        );
    }
    report
}

/// Close one index's corpus by `deadline`, unless a reindex holds the index.
///
/// Why: a reindex that loses its corpus fails open — its last batches and
/// prunes are dropped, yet `finish_reindex` still writes the HEAD-SHA marker,
/// so the next boot does no catch-up. The shutdown flush skips such an index
/// too (#1717).
/// What: takes the index permit with `try_acquire_owned`, as `corpus_reopen`
/// does, and returns it so the caller holds it past the close; a held permit
/// leaves the corpus attached and returns `Err`. Then [`close_attached`] runs.
async fn close_one(
    handle: Arc<IndexHandle>,
    deadline: Instant,
) -> (Result<bool, String>, Option<OwnedSemaphorePermit>) {
    // #9459: never take the corpus from under a reindex, relocate or embed
    // pass; its permit is the one `run_reindex` holds through the finish.
    let Ok(permit) = crate::service::reindex::index_semaphore(&handle.id).try_acquire_owned()
    else {
        let id = &handle.id;
        let line = format!(
            "{id}: a reindex, relocate or embed pass holds the index; its corpus stays attached"
        );
        return (Err(line), None);
    };
    (close_attached(handle, deadline).await, Some(permit))
}

/// Close one index's corpus by `deadline`; the caller holds its permit.
///
/// What:
/// 1. Takes the indexer write lock, then marks the indexer closed for
///    shutdown and takes the corpus out with `take_corpus_for_shutdown`.
/// 2. Retries `Arc::try_unwrap` until this is the last reference.
/// 3. Drops it on a blocking thread — the drop writes redb's allocator state
///    and fsyncs — then reopens the file read-only to confirm it closed clean.
///
/// `Ok(true)` closed clean, `Ok(false)` had no corpus, `Err` names the index
/// and the step still pending at the deadline, or the probe's failure.
async fn close_attached(handle: Arc<IndexHandle>, deadline: Instant) -> Result<bool, String> {
    let id = &handle.id;
    let wait = deadline.saturating_duration_since(Instant::now());
    let Ok(mut indexer) = tokio::time::timeout(wait, handle.indexer.write()).await else {
        return Err(format!("{id}: the indexer lock was still held"));
    };
    // #9459: the flag and the take share this write lock, so the detached
    // reconcile delta's next write is refused, not kept in memory as `Ok`.
    let Some(mut corpus) = indexer.take_corpus_for_shutdown() else {
        return Ok(false);
    };
    drop(indexer);
    let store = loop {
        match Arc::try_unwrap(corpus) {
            Ok(store) => break store,
            Err(shared) if Instant::now() >= deadline => {
                return Err(format!(
                    "{id}: {} other reference(s) to {} were still live",
                    Arc::strong_count(&shared).saturating_sub(1),
                    shared.path().display()
                ));
            }
            Err(shared) => {
                corpus = shared;
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    };
    let wait = deadline.saturating_duration_since(Instant::now());
    let close = tokio::task::spawn_blocking(move || drop_and_probe(store));
    match tokio::time::timeout(wait, close).await {
        Ok(Ok(Ok(()))) => Ok(true),
        Ok(Ok(Err(e))) => Err(format!("{id}: {e}")),
        Ok(Err(join)) => Err(format!("{id}: the close panicked: {join}")),
        Err(_) => Err(format!("{id}: the close was still running")),
    }
}

/// Drop the last reference to `store`, then confirm the file closed clean.
///
/// Why: redb's `Drop` logs a failed close only through its optional `logging`
/// feature, which this workspace does not enable. A read-only open succeeds
/// only on a file with no pending repair, so it is the check.
/// What: drops `store`, then opens its path read-only and drops that too.
/// `Err` names the read-only open's failure.
fn drop_and_probe(store: CorpusStore) -> Result<(), String> {
    let path: PathBuf = store.path().to_path_buf();
    drop(store);
    redb::Builder::new()
        .set_cache_size(PROBE_CACHE_BYTES)
        .open_read_only(&path)
        .map(drop)
        .map_err(|e| format!("{} did not close clean: {e}", path.display()))
}

#[cfg(test)]
#[path = "shutdown_close_tests.rs"]
mod tests;
