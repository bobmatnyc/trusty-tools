//! Closing a `UsearchStore` when its index is deleted (#8167, #8232).
//!
//! Why: the store is reached through `Arc<dyn VectorStore>` clones that can
//! outlive `DELETE /indexes/{id}`. While any clone lives, the snapshot's
//! `Index::view` mapping lives too: the root cannot be unmounted, and a
//! truncated or replaced `hnsw.usearch` raises SIGBUS on the next search.
//! Closing in place — through the shared `Arc<RwLock<Index>>` — releases the
//! mapping no matter how many clones survive.
//! What: [`UsearchStore::close_and_unmap`] and the refusal every later use of a
//! closed store answers with.
//! Test: `close_unmaps_the_view_and_refuses_every_later_use` in
//! `super::tests_close_8232`.

use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};

use super::usearch_store::UsearchStore;

impl UsearchStore {
    /// Unmap the snapshot, close its descriptor, and refuse all later use.
    ///
    /// Why: see the module docs.
    /// What: raises `closed` first, so an operation that checks it after
    /// taking its own lock sees it. Then takes the mutation gate and the graph
    /// write lock — waiting out any in-flight write or search on the mapping —
    /// and calls `Index::reset`, which usearch documents as releasing the
    /// mapping and its descriptor. The key maps and `hnsw_path` are cleared, so
    /// neither the idle demote sweep nor a view promotion can map the file
    /// again. Idempotent.
    /// Test: `close_unmaps_the_view_and_refuses_every_later_use`.
    pub(super) async fn close_and_unmap(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let _mutation_guard = self.save_lock.lock().await;
        {
            let index = self.index.write().await;
            index
                .reset()
                .map_err(|e| anyhow!("usearch reset (close) failed: {e}"))?;
        }
        self.is_view.store(false, Ordering::Release);
        self.dirty.store(false, Ordering::Release);
        *self.hnsw_path.write().await = None;
        self.id_to_key.write().await.clear();
        self.key_to_id.write().await.clear();
        Ok(())
    }

    /// `Err` once [`Self::close_and_unmap`] has run; `Ok` otherwise.
    ///
    /// Why: a closed store holds an empty graph, so answering from it would
    /// report "no matches" for an index that no longer exists (#8232).
    /// What: callers invoke it after taking the lock that orders them against
    /// the close — the graph read lock or the mutation gate.
    /// Test: `close_unmaps_the_view_and_refuses_every_later_use`.
    pub(super) fn refuse_if_closed(&self, op: &str) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!(
                "vector store {op} refused: this index was deleted and its HNSW \
                 snapshot is closed (#8232)"
            ));
        }
        Ok(())
    }
}
