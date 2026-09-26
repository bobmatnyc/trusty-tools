//! Closing a deleted index's files before `DELETE /indexes/{id}` returns
//! (#8167, #8232).
//!
//! Why: dropping the registry's `Arc<IndexHandle>` closes nothing while
//! another clone lives — and one routinely does (a job parked in
//! `service::reindex::defer_embed_queue`, an in-flight request). The redb file
//! stayed open, so `umount` of a bind-mounted root answered `EBUSY` (#8167),
//! and the HNSW snapshot stayed mapped, so truncating or replacing
//! `hnsw.usearch` turned the surviving clone's next search into a SIGBUS that
//! killed the daemon (#8232). Closing through the indexer makes the release
//! independent of who else holds the handle. Split out of `search.rs`, which
//! sits near the 500-SLOC cap.
//! What: [`close_index_files`] and its budgets, plus
//! [`close_after_writer_drains`] for a delete whose writer outlived the
//! quiesce wait. The first half of the approach — taking the corpus out and
//! waiting out transient clones — follows the unmerged WIP on
//! `fix/trusty-search-8229-8147-8167-8148-r2` (1af43710c).
//! Test: `service::server::tests_8167`, and the SIGBUS reproduction in
//! `tests/delete_releases_mmap_8232.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::registry::{IndexHandle, IndexId};

/// Total time the corpus close may wait before the delete abandons itself.
///
/// Why: an in-flight read holds the indexer lock or a short-lived
/// `Arc<CorpusStore>` clone for milliseconds; none of it may wedge the HTTP
/// handler. What: one wall-clock deadline across the lock wait and the
/// corpus-clone wait.
/// Test: `a_corpus_still_referenced_elsewhere_abandons_the_delete`.
#[cfg(not(test))]
pub(super) const CLOSE_BUDGET: Duration = Duration::from_secs(5);
/// Test build: short, so the abandon path runs in milliseconds.
#[cfg(test)]
pub(super) const CLOSE_BUDGET: Duration = Duration::from_millis(300);

/// How long a delete waits for a detached corpus rehydrate to finish.
///
/// Why: the rehydrate (#3683) holds an `Arc<CorpusStore>` for its whole redb
/// scan, 27-40 s on a large corpus, and nothing can cut the scan short. The
/// wait takes no indexer lock, so searches keep running during it. What:
/// matches the 30 s quiesce wait, which covers most of that window.
/// Test: `a_delete_during_a_slow_rehydrate_waits_without_the_write_lock`.
#[cfg(not(test))]
pub(super) const REHYDRATE_WAIT_BUDGET: Duration = Duration::from_secs(30);
/// Test build: long enough to outlast the tests' artificial 800 ms scan.
#[cfg(test)]
pub(super) const REHYDRATE_WAIT_BUDGET: Duration = Duration::from_secs(3);

/// How often the close re-checks a rehydrate or corpus clone it is waiting out.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Close the redb corpus and the HNSW snapshot of an index being deleted.
///
/// Why: see the module docs.
/// What: runs BEFORE the deregistration, so a close that cannot finish
/// abandons a delete that has changed nothing — the #3049 shape.
/// 1. Waits, holding no indexer lock, for a detached corpus rehydrate to
///    drop its corpus reference (`rehydrate_budget`). One still running at
///    the deadline returns `Err` with nothing changed.
/// 2. Takes the indexer write lock and re-checks the rehydrate gate. Under
///    that lock no new rehydrate can start. Then calls `detach_for_delete`:
///    the indexer is marked deleted and its corpus taken out.
/// 3. Drops the last `Arc<CorpusStore>`, retrying while a transient clone
///    lives. On the deadline it re-attaches the corpus under the lock it never
///    released and returns `Err` — the index is exactly as it was.
/// 4. Closes the vector store in place, releasing the mapping for every
///    surviving clone. This step is irreversible, so the delete is committed
///    from here: a failure is `Ok(Some(reason))` for the caller to report.
///
/// `Ok(None)` means everything this daemon held open for the index is closed,
/// including when there was no hot handle to close.
///
/// Test: `delete_releases_the_corpus_and_vector_files_while_a_clone_survives`,
/// `a_corpus_still_referenced_elsewhere_abandons_the_delete`,
/// `a_delete_during_a_slow_rehydrate_waits_without_the_write_lock`,
/// `a_rehydrate_outlasting_its_budget_abandons_the_delete_unchanged`.
pub(super) async fn close_index_files(
    handle: Option<&Arc<IndexHandle>>,
    rehydrate_budget: Duration,
    budget: Duration,
) -> Result<Option<String>, String> {
    let Some(handle) = handle else {
        return Ok(None);
    };
    let lock_timeout = || {
        format!(
            "the indexer lock was still held after {budget:?}, so its files could not be closed"
        )
    };
    let Ok(reader) = tokio::time::timeout(budget, handle.indexer.read()).await else {
        return Err(lock_timeout());
    };
    let gate = reader.rehydrate_gate();
    drop(reader);
    let rehydrate_deadline = Instant::now() + rehydrate_budget;
    let (mut indexer, deadline) = loop {
        // #3683: wait out the rehydrate BEFORE taking the write lock, so a
        // long scan never stalls searches behind a delete.
        while gate.in_flight() {
            if Instant::now() >= rehydrate_deadline {
                return Err(format!(
                    "a corpus rehydrate scan (#3683) was still reading index.redb after \
                     {rehydrate_budget:?}; re-issue the delete once it finishes"
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let deadline = Instant::now() + budget;
        let Ok(guard) = tokio::time::timeout(budget, handle.indexer.write()).await else {
            return Err(lock_timeout());
        };
        // A rehydrate can start between the poll and the lock; none can
        // start while the write lock is held.
        if !gate.in_flight() {
            break (guard, deadline);
        }
    };
    let files = indexer.detach_for_delete();
    if let Some(mut corpus) = files.corpus {
        loop {
            match Arc::try_unwrap(corpus) {
                Ok(store) => {
                    drop(store);
                    break;
                }
                Err(shared) if Instant::now() >= deadline => {
                    let others = Arc::strong_count(&shared).saturating_sub(1);
                    indexer.reattach_after_abandoned_delete(Some(shared));
                    return Err(format!(
                        "{others} other reference(s) to the redb corpus were still live \
                         after {budget:?}, so the file stays open"
                    ));
                }
                Err(shared) => {
                    corpus = shared;
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }
    drop(indexer);
    let Some(store) = files.store else {
        return Ok(None);
    };
    Ok(store
        .close()
        .await
        .err()
        .map(|e| format!("vector store close failed: {e:#}")))
}

/// Close a deregistered index's files once the writer that outlived the
/// delete's quiesce wait has finished.
///
/// Why: a `delete_data=false` delete whose writer is still running after the
/// quiesce wait deregisters anyway and answers `quiesced: false`. It cannot
/// close the files under that writer, and a second delete of the id answers
/// 404, so without this the files stayed open until the last handle clone
/// dropped (#8167).
/// What: spawns a task that takes the EXCLUSIVE side of the teardown lock
/// with no timeout — it waits for the writer — then runs
/// [`close_index_files`] and logs the outcome. The response has already been
/// sent, so the log line is the only report: `deferred close done` at INFO,
/// or an ERROR naming what stayed open. A no-op when there was no hot handle.
/// Test: `a_writer_outliving_the_quiesce_wait_closes_the_files_when_it_finishes`.
pub(super) fn close_after_writer_drains(index_id: IndexId, handle: Option<Arc<IndexHandle>>) {
    let Some(handle) = handle else {
        return;
    };
    tokio::spawn(async move {
        let _quiesced = crate::service::reindex::acquire_index_teardown_write(&index_id).await;
        match close_index_files(Some(&handle), REHYDRATE_WAIT_BUDGET, CLOSE_BUDGET).await {
            Ok(None) => tracing::info!(
                "delete[{index_id}]: deferred close done — the writer finished, and \
                 index.redb and hnsw.usearch are now closed (issue #8167)"
            ),
            Ok(Some(reason)) => tracing::error!(
                "delete[{index_id}]: deferred close closed index.redb, but {reason} \
                 (issue #8232)"
            ),
            Err(reason) => tracing::error!(
                "delete[{index_id}]: deferred close FAILED — {reason}. The files stay \
                 open until the last handle to this index drops or the daemon restarts \
                 (issue #8167)"
            ),
        }
    });
}
