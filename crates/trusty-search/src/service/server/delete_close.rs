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
//! What: [`close_index_files`] and its budget. The first half of the
//! approach — taking the corpus out and waiting out transient clones — follows
//! the unmerged WIP on `fix/trusty-search-8229-8147-8167-8148-r2` (1af43710c).
//! Test: `service::server::tests_8167`, and the SIGBUS reproduction in
//! `tests/delete_releases_mmap_8232.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::registry::IndexHandle;

/// Total time the corpus close may wait before the delete abandons itself.
///
/// Why: an in-flight read holds the indexer lock or a short-lived
/// `Arc<CorpusStore>` clone for milliseconds; none of it may wedge the HTTP
/// handler. What: one wall-clock deadline across both waits.
/// Test: `a_corpus_still_referenced_elsewhere_abandons_the_delete`.
#[cfg(not(test))]
pub(super) const CLOSE_BUDGET: Duration = Duration::from_secs(5);
/// Test build: short, so the abandon path runs in milliseconds.
#[cfg(test)]
pub(super) const CLOSE_BUDGET: Duration = Duration::from_millis(300);

/// How often the close re-checks a corpus clone it is waiting out.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Close the redb corpus and the HNSW snapshot of an index being deleted.
///
/// Why: see the module docs.
/// What: runs BEFORE the deregistration, so a close that cannot finish
/// abandons a delete that has changed nothing — the #3049 shape.
/// 1. Takes the indexer write lock and calls `detach_for_delete`: the indexer
///    is marked deleted and its corpus taken out.
/// 2. Drops the last `Arc<CorpusStore>`, retrying while a transient clone
///    lives. On the deadline it re-attaches the corpus under the lock it never
///    released and returns `Err` — the index is exactly as it was.
/// 3. Closes the vector store in place, releasing the mapping for every
///    surviving clone. This step is irreversible, so the delete is committed
///    from here: a failure is `Ok(Some(reason))` for the caller to report.
///
/// `Ok(None)` means everything this daemon held open for the index is closed,
/// including when there was no hot handle to close.
///
/// Test: `delete_releases_the_corpus_and_vector_files_while_a_clone_survives`,
/// `a_corpus_still_referenced_elsewhere_abandons_the_delete`.
pub(super) async fn close_index_files(
    handle: Option<&Arc<IndexHandle>>,
    budget: Duration,
) -> Result<Option<String>, String> {
    let Some(handle) = handle else {
        return Ok(None);
    };
    let deadline = Instant::now() + budget;
    let Ok(mut indexer) = tokio::time::timeout(budget, handle.indexer.write()).await else {
        return Err(format!(
            "the indexer lock was still held after {budget:?}, so its files could not \
             be closed"
        ));
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
