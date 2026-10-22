//! The redb half of [`HnswStore::upsert`]: one write transaction that stores
//! the vector row and its uuid mapping.
//!
//! Moved out of `hnsw_store.rs` to keep it under the 500-SLOC cap (#9487).

// #9278: `commit_vector_row` serves only the test-only `HnswStore::upsert`.
#[cfg(test)]
use redb::ReadableTable;

#[cfg(test)]
use super::{DELETED_VECTORS, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS, allocate_vector_id};
use super::{HnswStore, HnswStoreError, Result, quiet_insert, stranded};

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
    #[cfg(test)]
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

    /// Insert a committed row's vector into the in-memory graph.
    ///
    /// Why: `upsert` and `replace_group` (#9275) commit rows the same way and
    /// must index them the same way.
    /// What: marks a re-upserted id shadowed (#5171), inserts the point under
    /// the bounded insert gate and graph lock (#9487, #9187), and records it —
    /// and any point the insert evicted from a neighbour list, or left
    /// reachable only through one (#10186) — for an exact scan if a search for
    /// its own vector does not reach it (#9174).
    /// Test: `upsert_and_search_round_trips`,
    /// `a_drawer_reachable_only_through_a_stranded_one_is_still_found`,
    /// `upsert_marks_a_shadow_before_the_graph_can_serve_it`,
    /// `upsert_chunks_replaces_the_group_and_shrinks`.
    pub(super) fn index_point(
        &self,
        vector: &[f32],
        vector_id: u64,
        shadows_previous: bool,
    ) -> Result<()> {
        // #5171: mark BEFORE the graph can serve the shadow point. The two
        // steps cannot be atomic, and the asymmetry runs one way: a search
        // that sees the shadow without the flag scores the drawer by whichever
        // copy is nearer, which is the defect. A search that sees the flag
        // without the shadow re-reads the already-committed `VECTORS` row and
        // gets the right answer, so over-marking costs one point read.
        //
        // #5171: safe only because `exhaustive_nearest` (exhaustive.rs) never
        // truncates before `resolve_shadowed` corrects every candidate — a
        // pre-correction cap there would reopen this race.
        if shadows_previous {
            self.shadowed.write().insert(vector_id);
        }
        // #9187: the redb row is committed either way; on Err the next open
        // replays it.
        // #9487: both waits are bounded; a timeout trips the breaker.
        let _gate = self.bounded_gate("upsert")?;
        let index = self.bounded_graph("upsert")?;
        // #9174: the lists this insert can evict from, read before it runs.
        let before = stranded::neighbourhoods_before_insert(&index, vector);
        quiet_insert::insert_quietly(&index, vector, vector_id as usize)
            .map_err(HnswStoreError::StdoutGuard)?;
        // #9174: a point its own search misses is scanned exactly instead —
        // the new one, and every one this insert evicted from a list.
        // #10186: and every point behind an evicted one that its search misses.
        if !stranded::reachable(&index, vector, vector_id) {
            stranded::add_stranded(&mut self.stranded.write(), vector, vector_id);
        }
        stranded::restrand_evicted(&index, before, vector_id, &self.stranded);
        Ok(())
    }
}
