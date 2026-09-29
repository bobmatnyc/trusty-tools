//! [`CorpusStore`] access to the vector-refusal record in `_meta` (#8884).
//!
//! Why: the record of chunks whose embedding the store refused must survive a
//! restart, and it describes this corpus's content, so it lives in the corpus.
//! What: raw-bytes read and write of `META_KEY_VECTOR_REFUSALS`. Parsing is
//! the caller's job (`core::indexer::ingest::refusals`).
//! Test: `a_restore_after_a_refused_embedding_does_not_demote_the_stage`,
//! `an_unreadable_refusal_record_is_treated_as_a_real_gap`.

use anyhow::{Context, Result};
use redb::ReadableDatabase;

use super::store_impl::CorpusStore;

impl CorpusStore {
    /// Read the vector-refusal record bytes, if any (#8884).
    ///
    /// What: `Ok(None)` when the `_meta` table or the key is absent, which is
    /// every corpus written before the record existed. Any other read fault
    /// is an `Err`; the caller must not read it as "nothing refused".
    pub(crate) fn read_vector_refusals_sync(&self) -> Result<Option<Vec<u8>>> {
        use crate::core::migration::{META_KEY_VECTOR_REFUSALS, META_TABLE};
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        let value = table
            .get(META_KEY_VECTOR_REFUSALS)
            .context("read vector_refusals")?;
        Ok(value.map(|v| v.value().to_vec()))
    }

    /// Replace the vector-refusal record, or remove it with `None` (#8884).
    ///
    /// What: one atomic redb write transaction, so a crash leaves the old
    /// record or the new one, never a torn one.
    pub(crate) fn write_vector_refusals_sync(&self, bytes: Option<&[u8]>) -> Result<()> {
        use crate::core::migration::{META_KEY_VECTOR_REFUSALS, META_TABLE};
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            match bytes {
                Some(bytes) => table.insert(META_KEY_VECTOR_REFUSALS, bytes).map(|_| ()),
                None => table.remove(META_KEY_VECTOR_REFUSALS).map(|_| ()),
            }
            .context("write vector_refusals")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }
}
