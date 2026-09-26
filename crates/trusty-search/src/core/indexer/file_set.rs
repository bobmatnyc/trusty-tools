//! The distinct set of files an index holds chunks for (#8266).
//!
//! Why: grep needs only file paths — it re-reads every file from disk — but it
//! took them from `raw_chunks_snapshot`, which rehydrates an evicted index's
//! whole chunk map, BM25 corpus and entity map first. A one-file grep on a
//! 117,773-chunk index took 9.07 s cold against 0.14 s warm, and went cold
//! again at the next idle sweep.
//! What: [`CodeIndexer::indexed_file_set`].
//! Test: `a_cold_narrow_grep_does_not_materialize_the_corpus`.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use super::corpus_fault::CorpusReadUnavailable;
use super::CodeIndexer;

impl CodeIndexer {
    /// The sorted, distinct file paths this index holds chunks for.
    ///
    /// Why: see the module doc. An evicted index can answer this from the
    /// durable corpus without refilling any in-memory cache.
    /// What: evicted with a durable corpus → `CorpusStore::list_indexed_files`
    /// on a blocking thread, which decodes only each row's `file` field and
    /// leaves the index evicted. A failed read records the #5917 corpus fault
    /// and returns [`CorpusReadUnavailable`], so grep refuses with `503` rather
    /// than answering "no matches". Resident, or no durable corpus → the
    /// in-memory map behind the same `ensure_corpus_view_is_current` gate
    /// `raw_chunks_snapshot` uses, collecting paths without cloning a chunk.
    /// Test: `a_cold_narrow_grep_does_not_materialize_the_corpus`,
    /// `grep_over_an_unreadable_corpus_returns_503_naming_the_index`.
    pub(crate) async fn indexed_file_set(&self) -> Result<Vec<String>> {
        if self.chunks_evicted.load(Ordering::Relaxed) {
            if let Some(corpus) = self.corpus.clone() {
                let listed = tokio::task::spawn_blocking(move || corpus.list_indexed_files())
                    .await
                    .context("indexed_file_set: corpus file listing task panicked")?;
                return match listed {
                    Ok(mut files) => {
                        files.sort();
                        Ok(files)
                    }
                    Err(e) => {
                        let detail = format!("{e:#}");
                        self.corpus_read_fault.record(detail.clone());
                        Err(CorpusReadUnavailable {
                            index_id: self.index_id.clone(),
                            detail,
                        }
                        .into())
                    }
                };
            }
        }
        self.ensure_corpus_view_is_current().await?;
        let chunks = self.chunks.read().await;
        let files: BTreeSet<&str> = chunks.values().map(|c| c.file.as_str()).collect();
        Ok(files.into_iter().map(str::to_owned).collect())
    }
}
