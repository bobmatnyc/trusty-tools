//! Warm one index's corpus caches to completion (#9027).
//!
//! Why: a query only ever waits a bounded budget for an idle-evicted corpus to
//! come back (`idle_evict::ensure_corpus_rehydrated`), so the first all-index
//! search after an idle spell pays for every evicted index. Warm-all rehydrates
//! ahead of the search, as a background job that may wait as long as the scan
//! takes.
//! What: [`warm_corpus`].
//! Test: `warm_corpus_rehydrates_an_evicted_index`,
//! `warm_corpus_reports_an_unreadable_corpus_as_an_error`.

use std::time::Duration;

use tokio::sync::RwLock;

use super::CodeIndexer;

/// How often a warm re-checks a rehydrate whose wakeup it may have missed.
///
/// `begin_corpus_rehydrate` documents the narrow window in which a waiter
/// builds its `notified()` future after the task already notified; polling the
/// in-flight gate bounds that miss to one interval instead of forever.
const WARM_POLL: Duration = Duration::from_millis(250);

/// Rehydrate attempts before a warm gives up.
///
/// A concurrent eviction makes the detached task skip its commit and leave the
/// corpus evicted (`rehydrate_generation`); each attempt after that is a fresh
/// scan, so the count is bounded rather than retried forever.
const WARM_MAX_ATTEMPTS: u32 = 3;

/// Rehydrate one index's chunk map, BM25 corpus and entity map, waiting for the
/// scan to finish.
///
/// Why: see the module doc. Takes the lock rather than `&CodeIndexer` so the
/// read guard is held only for the instant checks, never across the wait: tokio's
/// `RwLock` is write-preferring, so a guard held for a 30 s scan would queue a
/// reindex's write behind it and every search on that index behind the writer.
/// What: `Ok(())` once nothing is evicted — immediately for a resident index or
/// one with no durable corpus. Otherwise joins or starts the one detached
/// rehydrate and waits for it. A scan that failed leaves the corpus evicted with
/// the read fault recorded, and that fault is the `Err`. A commit skipped by a
/// racing eviction is retried up to [`WARM_MAX_ATTEMPTS`] times. The caller owns
/// any overall deadline; dropping this future never cancels the detached scan.
/// Test: `warm_corpus_rehydrates_an_evicted_index`,
/// `warm_corpus_reports_an_unreadable_corpus_as_an_error`.
pub(crate) async fn warm_corpus(indexer: &RwLock<CodeIndexer>) -> Result<(), String> {
    for _ in 0..WARM_MAX_ATTEMPTS {
        let (notify, gate) = {
            let guard = indexer.read().await;
            match guard.begin_corpus_rehydrate() {
                Some(notify) => (notify, std::sync::Arc::clone(&guard.rehydrate_inflight)),
                None => return Ok(()),
            }
        };
        while gate.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            let _ = tokio::time::timeout(WARM_POLL, notify.notified()).await;
        }
        let guard = indexer.read().await;
        if !guard.corpus_evicted() {
            return Ok(());
        }
        if let Some(detail) = guard.corpus_read_fault.detail() {
            return Err(format!("corpus rehydrate failed: {detail}"));
        }
    }
    Err(format!(
        "corpus still evicted after {WARM_MAX_ATTEMPTS} rehydrate attempts \
         (concurrent evictions kept invalidating the scan)"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::RwLock;

    use super::warm_corpus;
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;

    /// A corpus-backed indexer holding one file, plus its corpus handle.
    async fn indexer_with_corpus(dir: &std::path::Path) -> (RwLock<CodeIndexer>, Arc<CorpusStore>) {
        let corpus = Arc::new(CorpusStore::open(&dir.join("index.redb")).expect("open corpus"));
        let mut indexer = CodeIndexer::new("warm-corpus", dir);
        indexer.set_corpus_store(Arc::clone(&corpus));
        indexer
            .index_files_batch(&[("src/a.rs".into(), "fn warm_me() {}\n".into())])
            .await
            .expect("index batch");
        (RwLock::new(indexer), corpus)
    }

    /// Why: warm-all reports an index warm only after this returns `Ok`, so it
    /// must not return while the corpus is still evicted.
    /// What: evicts, warms, and asserts the corpus is resident again.
    /// Test: this test.
    #[tokio::test]
    async fn warm_corpus_rehydrates_an_evicted_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (indexer, _corpus) = indexer_with_corpus(tmp.path()).await;
        assert!(indexer.read().await.reclaim_memory_now().await > 0);
        assert!(indexer.read().await.corpus_evicted());

        warm_corpus(&indexer).await.expect("warm succeeds");
        assert!(
            !indexer.read().await.corpus_evicted(),
            "warm left it resident"
        );
    }

    /// Why: Fail-Open Check — an unreadable corpus must surface as a failed
    /// warm, never as a warm index that answers every query empty.
    /// What: drops the chunks table so the rehydrate scan fails, then asserts
    /// `Err` and that the corpus is still evicted.
    /// Test: this test.
    #[tokio::test]
    async fn warm_corpus_reports_an_unreadable_corpus_as_an_error() {
        const CHUNKS_TABLE: redb::TableDefinition<'static, &str, &[u8]> =
            redb::TableDefinition::new("chunks");
        let tmp = tempfile::tempdir().expect("tempdir");
        let (indexer, corpus) = indexer_with_corpus(tmp.path()).await;
        let txn = corpus.db().begin_write().expect("begin write");
        assert!(txn.delete_table(CHUNKS_TABLE).expect("drop chunks table"));
        txn.commit().expect("commit");
        assert!(indexer.read().await.reclaim_memory_now().await > 0);

        let err = warm_corpus(&indexer).await.expect_err("warm must fail");
        assert!(err.contains("rehydrate failed"), "{err}");
        assert!(indexer.read().await.corpus_evicted(), "still evicted");
    }
}
