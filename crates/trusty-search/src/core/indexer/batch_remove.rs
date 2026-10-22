//! Remove every file under a path prefix, or in a file list, in one call (#10110).
//!
//! Why: removing an excluded repository went one file at a time. Each call
//! rehydrated the whole evicted corpus to find one file's chunks, opened its
//! own redb transactions, and stamped its own graph-dirty mark; one 24-chunk
//! removal raised a production daemon by ~1.7 GiB that never came back.
//! What: [`CodeIndexer::remove_files_matching`] reads the selected rows by
//! redb key range (never `ensure_chunks_loaded`), stamps ONE durable dirty
//! mark, then per slice of at most [`REMOVE_SLICE_IDS`] ids deletes the chunk
//! and entity rows in ONE write transaction, drops them from memory, and
//! removes their vectors. The removal path itself does not rehydrate chunks.
//! Deleting vectors promotes a view-mode HNSW index to the heap
//! (`UsearchStore::remove` calls `ensure_mutable`), which costs O(index
//! size) until the write-cooldown demote (#10110). The ONE scheduled graph
//! rebuild rehydrates the corpus; it defers, keeping the dirty mark, while a
//! slow scan outlasts its bounded wait.
//! [`CodeIndexer::count_files_matching`] is the read-only dry run.
//! Test: `indexer::tests::batch_remove_10110`;
//! `service::server::remove_files_tests`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Context, Result};

use super::CodeIndexer;
use crate::core::corpus::SelectedRows;
use crate::core::path_selection::PathSelection;

/// Chunk ids one redb write transaction deletes, at most. A file's ids never
/// split across slices, so one oversized file is one slice of its own.
pub(crate) const REMOVE_SLICE_IDS: usize = 500;

/// #10110 test seam: for each `(index_id, n)` listed, slice `n` and every
/// later slice of a batch removal is refused before its redb write.
#[cfg(test)]
pub(crate) static TEST_FAIL_SLICE_FROM: std::sync::Mutex<Vec<(String, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// What one batch removal matched and removed (#10110).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BatchRemoveCounts {
    /// Files holding at least one selected chunk row.
    pub(crate) matched_files: usize,
    /// Selected chunk rows.
    pub(crate) matched_chunks: usize,
    /// Files whose rows left redb in a committed slice.
    pub(crate) removed_files: usize,
    /// Chunk rows that left redb in a committed slice.
    pub(crate) removed_chunks: usize,
}

/// Why a batch removal did not finish (#10110).
#[derive(Debug)]
pub(crate) enum BatchRemoveError {
    /// A corpus rehydrate, or another batch removal, holds this index's scan
    /// gate. Nothing was changed; a retry can succeed.
    Busy,
    /// A step failed. `counts` reports what committed before it; a retry
    /// finishes the job.
    Failed {
        counts: BatchRemoveCounts,
        source: anyhow::Error,
    },
}

impl std::fmt::Display for BatchRemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str(
                "a corpus rehydrate or another batch removal is running on this index; retry",
            ),
            Self::Failed { counts, source } => write!(
                f,
                "batch removal stopped after {} chunks of {}: {source:#}",
                counts.removed_chunks, counts.matched_chunks
            ),
        }
    }
}

impl std::error::Error for BatchRemoveError {}

/// The rows and vectors one call will remove.
struct Plan {
    rows: SelectedRows,
    /// Selected ids the vector store holds with no redb row left.
    orphan_vectors: Vec<String>,
    counts: BatchRemoveCounts,
}

/// One redb write transaction's worth of removal.
#[derive(Default)]
struct Slice {
    ids: Vec<String>,
    files: usize,
    entity_files: Vec<String>,
}

/// Group `rows` by file and cut them into slices of about `slice_ids` ids,
/// never splitting a file. Entity rows of files with no chunk row go last.
fn file_slices(rows: &SelectedRows, slice_ids: usize) -> Vec<Slice> {
    let mut by_file: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (id, file) in &rows.chunks {
        by_file.entry(file.as_str()).or_default().push(id.clone());
    }
    let entity_files: BTreeSet<&str> = rows.entity_files.iter().map(String::as_str).collect();
    let mut slices = Vec::new();
    let mut current = Slice::default();
    for (file, ids) in &by_file {
        current.ids.extend(ids.iter().cloned());
        current.files += 1;
        if entity_files.contains(file) {
            current.entity_files.push((*file).to_string());
        }
        if current.ids.len() >= slice_ids.max(1) {
            slices.push(std::mem::take(&mut current));
        }
    }
    current.entity_files.extend(
        entity_files
            .iter()
            .filter(|f| !by_file.contains_key(*f))
            .map(|f| (*f).to_string()),
    );
    if !current.ids.is_empty() || !current.entity_files.is_empty() {
        slices.push(current);
    }
    slices
}

impl CodeIndexer {
    /// Count what [`Self::remove_files_matching`] would remove, changing
    /// nothing (#10110 dry run).
    ///
    /// Why: a prefix removal is destructive; the caller sees the file and
    /// chunk counts first.
    /// What: the same range read the real run plans from. No gate, no mark,
    /// no rehydrate, no write. Refused on a corpus closed for shutdown.
    /// Test: `dry_run_mutates_nothing_and_matches_the_real_run`.
    pub(crate) async fn count_files_matching(
        &self,
        sel: &PathSelection,
    ) -> Result<BatchRemoveCounts> {
        self.refuse_if_closed_for_shutdown()?;
        Ok(self.plan_batch_remove(sel).await?.counts)
    }

    /// Remove every file `sel` names from redb, memory and the vector store
    /// in one call (#10110).
    ///
    /// Why: see the module doc — one rehydrate, one dirty mark and one graph
    /// rebuild per file did not scale to an 848-file purge.
    /// What: [`Self::remove_files_matching_sliced`] at [`REMOVE_SLICE_IDS`].
    /// Test: `prefix_removal_clears_every_store_and_spares_a_sibling`.
    pub(crate) async fn remove_files_matching(
        &self,
        sel: &PathSelection,
    ) -> Result<BatchRemoveCounts, BatchRemoveError> {
        self.remove_files_matching_sliced(sel, REMOVE_SLICE_IDS)
            .await
    }

    /// [`Self::remove_files_matching`] with the slice size chosen by the caller.
    ///
    /// Why: each failure branch must leave a state a retry can finish, and
    /// none may answer success for rows redb kept (#10110 F1-F9).
    /// What, in order:
    /// 1. Refuse a corpus closed for shutdown (F9).
    /// 2. Take the scan gate's write side with `try_write`; a held gate is
    ///    [`BatchRemoveError::Busy`]. A rehydrate that read redb before a
    ///    slice commit would publish the deleted ids back into memory (F8);
    ///    holding the gate stops a new scan for the whole call.
    /// 3. Plan by key range: the selected rows, plus selected ids the vector
    ///    store still holds with no row (left by an earlier failed call).
    ///    Nothing selected returns zeros with no mark written (F7).
    /// 4. Stamp ONE durable graph-dirty mark; a failed stamp is an error
    ///    before any mutation (F4). `skip_kg` indexes stamp nothing.
    /// 5. Per slice: delete chunk and entity rows in one transaction (F1,
    ///    F3), then drop them from memory, then delete their vectors. A
    ///    failed vector delete is an error after the memory drop; the retry
    ///    finds the vectors in step 3 (fix-bar 1).
    /// 6. Delete the orphan vectors step 3 found.
    ///
    /// Test: `batch_remove_fails_closed_on_a_refused_slice_and_a_retry_finishes`,
    /// `a_failed_vector_removal_is_an_error_and_a_retry_removes_the_orphans`,
    /// `batch_remove_refuses_when_the_dirty_mark_cannot_be_stamped`,
    /// `batch_remove_during_a_rehydrate_is_refused_and_leaves_no_orphan_ids`,
    /// `batch_remove_is_refused_on_a_corpus_closed_for_shutdown`.
    pub(crate) async fn remove_files_matching_sliced(
        &self,
        sel: &PathSelection,
        slice_ids: usize,
    ) -> Result<BatchRemoveCounts, BatchRemoveError> {
        let fail = |counts: BatchRemoveCounts, source: anyhow::Error| BatchRemoveError::Failed {
            counts,
            source,
        };
        self.refuse_if_closed_for_shutdown()
            .map_err(|e| fail(BatchRemoveCounts::default(), e))?;
        // #10110 Q4: the race check runs under the lock the batch then holds.
        let Ok(_scan_gate) = self.corpus_scan_gate.try_write() else {
            return Err(BatchRemoveError::Busy);
        };
        let plan = self
            .plan_batch_remove(sel)
            .await
            .map_err(|e| fail(BatchRemoveCounts::default(), e))?;
        let mut counts = plan.counts;
        if plan.rows.chunks.is_empty()
            && plan.rows.entity_files.is_empty()
            && plan.orphan_vectors.is_empty()
        {
            return Ok(counts);
        }
        // #10110: one durable mark for the whole call, so one rebuild.
        let _graph_write = if self.skip_kg {
            None
        } else {
            Some(
                self.begin_graph_write()
                    .await
                    .map_err(|e| fail(counts, e))?,
            )
        };
        for (n, slice) in file_slices(&plan.rows, slice_ids).iter().enumerate() {
            self.delete_slice_rows(n, slice)
                .await
                .map_err(|e| fail(counts, e))?;
            counts.removed_chunks += slice.ids.len();
            counts.removed_files += slice.files;
            self.drop_slice_from_memory(slice).await;
            self.try_remove_vectors(&slice.ids)
                .await
                .map_err(|e| fail(counts, e))?;
        }
        self.try_remove_vectors(&plan.orphan_vectors)
            .await
            .map_err(|e| fail(counts, e))?;
        Ok(counts)
    }

    /// The rows and orphan vectors `sel` names. Reads only.
    async fn plan_batch_remove(&self, sel: &PathSelection) -> Result<Plan> {
        let rows = match self.corpus.clone() {
            Some(corpus) => {
                let owned = sel.clone();
                tokio::task::spawn_blocking(move || corpus.rows_under(&owned))
                    .await
                    .context("batch-remove range read task failed")??
            }
            // No durable corpus (BM25-only / tests): memory is the only store.
            None => self.rows_in_memory(sel).await,
        };
        let orphan_vectors = match &self.store {
            Some(store) => {
                let owned = sel.clone();
                let shapes = self.chunk_id_shapes();
                let keep = move |id: &str| owned.selects_chunk_id(id, shapes);
                let in_rows: HashSet<&str> =
                    rows.chunks.iter().map(|(id, _)| id.as_str()).collect();
                store
                    .ids_where(&keep)
                    .await
                    .context("list the selected vector ids")?
                    .into_iter()
                    .filter(|id| !in_rows.contains(id.as_str()))
                    .collect()
            }
            None => Vec::new(),
        };
        let files: HashSet<&str> = rows.chunks.iter().map(|(_, f)| f.as_str()).collect();
        let counts = BatchRemoveCounts {
            matched_files: files.len(),
            matched_chunks: rows.chunks.len(),
            ..BatchRemoveCounts::default()
        };
        Ok(Plan {
            rows,
            orphan_vectors,
            counts,
        })
    }

    /// [`CorpusStore::rows_under`]'s answer from the in-memory maps, for an
    /// indexer with no durable corpus (which is never evicted).
    ///
    /// [`CorpusStore::rows_under`]: crate::core::corpus::CorpusStore::rows_under
    async fn rows_in_memory(&self, sel: &PathSelection) -> SelectedRows {
        let mut rows = SelectedRows::default();
        for chunk in self.chunks.read().await.values() {
            if sel.selects_file(&chunk.file) {
                rows.chunks.push((chunk.id.clone(), chunk.file.clone()));
            }
        }
        rows.entity_files = self
            .entities
            .read()
            .await
            .keys()
            .filter(|f| sel.selects_file(f))
            .cloned()
            .collect();
        rows.chunks.sort();
        rows.entity_files.sort();
        rows
    }

    /// Delete one slice's chunk and entity rows in one redb transaction.
    async fn delete_slice_rows(&self, n: usize, slice: &Slice) -> Result<()> {
        #[cfg(test)]
        if TEST_FAIL_SLICE_FROM.lock().is_ok_and(|f| {
            f.iter()
                .any(|(id, from)| *id == self.index_id && n >= *from)
        }) {
            anyhow::bail!("injected batch-remove slice {n} failure (#10110 test seam)");
        }
        #[cfg(not(test))]
        let _ = n;
        let Some(corpus) = self.corpus.clone() else {
            return Ok(());
        };
        let ids = slice.ids.clone();
        let entity_files = slice.entity_files.clone();
        tokio::task::spawn_blocking(move || {
            corpus.delete_chunk_and_entity_rows(&ids, &entity_files)
        })
        .await
        .context("batch-remove delete task failed")?
        .context("delete a batch-remove slice from redb")
    }

    /// Drop one slice from every in-memory structure except the vector store.
    ///
    /// What: chunk map first, so a deferred-embed commit racing this sees the
    /// id gone before its vector is deleted (#8761). An evicted map holds
    /// none of these ids, so the removes are no-ops; nothing rehydrates.
    async fn drop_slice_from_memory(&self, slice: &Slice) {
        {
            let mut chunks = self.chunks.write().await;
            for id in &slice.ids {
                chunks.remove(id);
            }
        }
        {
            let mut entities = self.entities.write().await;
            for file in &slice.entity_files {
                entities.remove(file);
            }
        }
        {
            let mut emb = self.chunk_embeddings.write().await;
            for id in &slice.ids {
                emb.pop(id);
            }
        }
        let mut bm25 = self.bm25.write().await;
        for id in &slice.ids {
            bm25.remove_document(id);
        }
    }

    /// Delete `ids` from the vector store; every id is tried, and the first
    /// failure is returned with the count that failed (#10110 fix-bar 1).
    async fn try_remove_vectors(&self, ids: &[String]) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut failed = 0usize;
        let mut first: Option<anyhow::Error> = None;
        for id in ids {
            if let Err(e) = store.remove(id).await {
                failed += 1;
                first.get_or_insert(e);
            }
        }
        match first {
            None => Ok(()),
            Some(e) => Err(e.context(format!(
                "{failed} of {} vector deletes failed; their redb rows are gone, \
                 and a retry removes the vectors",
                ids.len()
            ))),
        }
    }
}
