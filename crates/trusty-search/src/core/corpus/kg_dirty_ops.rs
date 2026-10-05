//! Durable "persisted symbol graph is stale" flag (#8959).
//!
//! Why: single-file writes defer the symbol-graph rebuild to a ticker, so the
//! persisted `kg_*` rows can lag the chunk corpus for up to a minute. Without
//! a durable record of that lag, a restart, park or crash inside the window
//! boots the old graph as if it were current and serves removed symbols until
//! some unrelated rebuild happens.
//! What: one `_meta` row holding the highest write generation that may be
//! missing from the persisted graph. A writer stamps it before it mutates the
//! corpus; a full rebuild removes it after its graph is saved, but only when
//! no later generation has been stamped. Boot reads it to schedule a rebuild.
//! Test: `a_deferred_rebuild_survives_a_reopen` in
//! `core::indexer::tests::file_lifecycle_8959`, and `kg_dirty_flag_*` in
//! this module.

use anyhow::{Context, Result};
use redb::{ReadableDatabase as _, ReadableTable as _};

use super::store_impl::CorpusStore;
use super::tables::META_KEY_KG_GRAPH_DIRTY;
use crate::core::migration::META_TABLE;

/// Decode a stored generation. A value that is not 8 bytes still means
/// "dirty"; it reads as generation `0`, which the next rebuild clears.
fn decode(bytes: &[u8]) -> u64 {
    <[u8; 8]>::try_from(bytes).map_or(0, u64::from_le_bytes)
}

impl CorpusStore {
    /// Record that the persisted graph may miss writes up to `generation`.
    ///
    /// What: stores `max(stored, generation)` in one write transaction, so a
    /// slower writer with an older generation never lowers the mark.
    pub fn mark_kg_graph_dirty(&self, generation: u64) -> Result<()> {
        let txn = self.db().begin_write().context("begin kg dirty mark txn")?;
        {
            let mut meta = txn.open_table(META_TABLE).context("open _meta table")?;
            let stored = meta
                .get(META_KEY_KG_GRAPH_DIRTY)
                .context("read kg_graph_dirty")?
                .map(|v| decode(v.value()));
            let next = stored.map_or(generation, |s| s.max(generation));
            meta.insert(META_KEY_KG_GRAPH_DIRTY, next.to_le_bytes().as_slice())
                .context("insert kg_graph_dirty")?;
        }
        txn.commit().context("commit kg dirty mark txn")?;
        Ok(())
    }

    /// Remove the dirty mark when it covers no generation after `through`.
    ///
    /// Why: a rebuild's snapshot covers every write stamped at or before the
    /// generation it read when it began; a later stamp belongs to a write the
    /// snapshot may have missed, and must survive.
    /// What: reads and conditionally removes the row in one write transaction.
    /// Returns whether the row is now absent.
    pub fn clear_kg_graph_dirty_through(&self, through: u64) -> Result<bool> {
        let txn = self
            .db()
            .begin_write()
            .context("begin kg dirty clear txn")?;
        let cleared = {
            let mut meta = txn.open_table(META_TABLE).context("open _meta table")?;
            let stored = meta
                .get(META_KEY_KG_GRAPH_DIRTY)
                .context("read kg_graph_dirty")?
                .map(|v| decode(v.value()));
            match stored {
                None => true,
                Some(s) if s <= through => {
                    meta.remove(META_KEY_KG_GRAPH_DIRTY)
                        .context("remove kg_graph_dirty")?;
                    true
                }
                Some(_) => false,
            }
        };
        txn.commit().context("commit kg dirty clear txn")?;
        Ok(cleared)
    }

    /// The stored dirty generation, or `None` when the persisted graph is
    /// current. A missing `_meta` table reads as `None` (a fresh corpus).
    pub fn kg_graph_dirty(&self) -> Result<Option<u64>> {
        let txn = self.db().begin_read().context("begin kg dirty read txn")?;
        let meta = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        Ok(meta
            .get(META_KEY_KG_GRAPH_DIRTY)
            .context("read kg_graph_dirty")?
            .map(|v| decode(v.value())))
    }
}

#[cfg(test)]
mod tests {
    use super::CorpusStore;

    fn store() -> (tempfile::TempDir, CorpusStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CorpusStore::open(&dir.path().join("corpus.redb")).expect("open");
        (dir, store)
    }

    /// A mark only rises; a clear removes it only when no later write stamped.
    #[test]
    fn kg_dirty_flag_keeps_a_later_generation_through_a_clear() {
        let (_dir, store) = store();
        assert_eq!(store.kg_graph_dirty().unwrap(), None);
        store.mark_kg_graph_dirty(5).unwrap();
        store.mark_kg_graph_dirty(3).unwrap();
        assert_eq!(store.kg_graph_dirty().unwrap(), Some(5), "never lowered");

        assert!(!store.clear_kg_graph_dirty_through(4).unwrap());
        assert_eq!(
            store.kg_graph_dirty().unwrap(),
            Some(5),
            "a later stamp survives"
        );
        assert!(store.clear_kg_graph_dirty_through(5).unwrap());
        assert_eq!(store.kg_graph_dirty().unwrap(), None);
    }
}
