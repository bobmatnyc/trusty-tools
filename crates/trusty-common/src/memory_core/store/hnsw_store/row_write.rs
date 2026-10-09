//! The redb half of [`HnswStore::upsert`]: one write transaction that stores
//! the vector row and its uuid mapping.
//!
//! Moved out of `hnsw_store.rs` to keep it under the 500-SLOC cap (#9487).

use redb::ReadableTable;

use super::{
    DELETED_VECTORS, HnswStore, Result, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS, allocate_vector_id,
};

impl HnswStore {
    /// Commit `vector` under `uuid` in one redb write transaction.
    ///
    /// Why: the graph insert that follows must only ever see an id whose row
    /// is durable, so a failed insert is replayed on the next open (#9187).
    /// What: reuses the uuid's existing id or allocates one from
    /// `VECTOR_ID_SEQ` (#5005), writes the postcard-encoded vector, clears any
    /// tombstone, commits, and invalidates the key cache (#9141). Returns the
    /// id and whether it replaced an earlier vector for the same uuid (#5171).
    /// Test: `upsert_and_search_round_trips`,
    /// `two_live_stores_over_one_file_never_alias_ids`.
    pub(super) fn commit_vector_row(&self, uuid: &str, vector: &[f32]) -> Result<(u64, bool)> {
        let encoded: Vec<u8> = postcard::to_allocvec(&vector.to_vec())?;
        let wtx = self.db.begin_write()?;
        let vector_id;
        // #5171: a re-upsert leaves the old embedding in the graph under this
        // same id, so `search` must re-read the authoritative vector for it.
        let shadows_previous;
        {
            let mut vectors = wtx.open_table(VECTORS)?;
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            let mut seq = wtx.open_table(VECTOR_ID_SEQ)?;

            // Resolve the existing id in a scoped block so the AccessGuard
            // (immutable borrow of `keys`) is dropped before we re-borrow
            // `keys` mutably for `insert`.
            let existing: Option<u64> = keys.get(uuid)?.map(|g| g.value());
            shadows_previous = existing.is_some();
            vector_id = match existing {
                Some(id) => id,
                None => {
                    // #5005: allocate from redb, inside this txn.
                    let id = allocate_vector_id(&mut seq, &vectors, &keys)?;
                    keys.insert(uuid, id)?;
                    id
                }
            };
            vectors.insert(vector_id, encoded.as_slice())?;
            // Clear any prior tombstone so a re-upsert revives the row.
            let _ = tombstones.remove(vector_id)?;
        }
        wtx.commit()?;
        self.keys.invalidate(); // #9141
        Ok((vector_id, shadows_previous))
    }
}
