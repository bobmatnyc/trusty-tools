//! The indexer half of closing a deleted index's files (#8167, #8232).
//!
//! Why: `DELETE /indexes/{id}` drops the registry's `Arc<IndexHandle>`, but
//! other clones survive it — a queued deferred-embed job, an in-flight
//! request. Each one keeps the indexer, and through it the redb corpus and the
//! HNSW mapping, alive. The delete therefore takes the corpus out of the
//! indexer and closes the vector store in place, and every surviving clone
//! must then get an error rather than an answer built from what is left.
//! What: [`IndexDeleted`], the refusal, and the three `CodeIndexer` methods the
//! delete path in `service::server::delete_close` drives.
//! Test: `delete_releases_the_corpus_and_vector_files_while_a_clone_survives`,
//! `a_surviving_clone_gets_an_error_not_an_empty_result` in
//! `service::server::tests_8167`.

use std::sync::Arc;

use super::CodeIndexer;
use crate::core::corpus::CorpusStore;
use crate::core::store::VectorStore;

/// A read or write reached an index after `DELETE` closed its files.
///
/// Why: the files are closed, so the only answers left are an empty result or
/// one built from stale in-memory state. Both would read as a real answer
/// (#8232's Fail-Open Check).
/// What: carries the index id; the `Display` body is what the caller reads.
/// Test: `a_surviving_clone_gets_an_error_not_an_empty_result`.
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}' was deleted: its corpus and vector files are closed, so \
     this handle can no longer serve it (#8167, #8232)"
)]
pub struct IndexDeleted {
    /// The deleted index.
    pub index_id: String,
}

/// What [`CodeIndexer::detach_for_delete`] took out, for the caller to close.
pub(crate) struct DetachedFiles {
    /// The durable corpus, now out of the indexer. Dropping the last `Arc`
    /// closes `index.redb`.
    pub(crate) corpus: Option<Arc<CorpusStore>>,
    /// The vector store. It stays wired, so a surviving clone reaches it and
    /// gets its closed-store error; the caller closes it in place.
    pub(crate) store: Option<Arc<dyn VectorStore>>,
}

impl CodeIndexer {
    /// Mark this indexer deleted and take its corpus out.
    ///
    /// Why: no surviving clone of the handle may obtain the corpus again once
    /// the delete has started closing it.
    /// What: sets `deleted`, takes `corpus`, and clones the vector store for
    /// the caller to close. Reversible until the store is closed — see
    /// [`Self::reattach_after_abandoned_delete`].
    /// Test: `delete_releases_the_corpus_and_vector_files_while_a_clone_survives`.
    pub(crate) fn detach_for_delete(&mut self) -> DetachedFiles {
        self.deleted = true;
        DetachedFiles {
            corpus: self.corpus.take(),
            store: self.store.clone(),
        }
    }

    /// Undo [`Self::detach_for_delete`] for a delete that is being abandoned.
    ///
    /// Why: a delete that cannot close the corpus abandons itself with nothing
    /// changed (the #3049 shape), so the index keeps serving.
    /// What: restores `corpus` exactly as it was taken and clears `deleted`.
    /// Assigns the field directly rather than through `set_corpus_store`, which
    /// would also clear a #4122 quarantine the detach never touched.
    /// Test: `a_corpus_still_referenced_elsewhere_abandons_the_delete`.
    pub(crate) fn reattach_after_abandoned_delete(&mut self, corpus: Option<Arc<CorpusStore>>) {
        self.corpus = corpus;
        self.deleted = false;
    }

    /// `Err(IndexDeleted)` once a delete has closed this indexer's files.
    pub(crate) fn refuse_if_deleted(&self) -> anyhow::Result<()> {
        if self.deleted {
            return Err(anyhow::Error::new(IndexDeleted {
                index_id: self.index_id.clone(),
            }));
        }
        Ok(())
    }
}
