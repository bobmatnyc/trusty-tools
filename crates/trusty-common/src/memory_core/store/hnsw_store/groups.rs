//! Drawer-level operations over chunk rows (#9275).
//!
//! Why: a drawer is stored as one row per chunk — `{drawer}` for chunk 0 and
//! `{drawer}#n` for the rest (grammar in `store::vector_key`). Search must
//! count `k` in drawers, a drawer write must replace every row at once, and a
//! drawer delete must leave no row behind.
//! What: [`HnswStore::replace_group`] and [`HnswStore::delete_group`], each
//! one redb write transaction; [`HnswStore::search_grouped`], which folds the
//! ranked rows into one hit per drawer and widens the graph arm until `k`
//! drawers or the index is exhausted; [`remove_drawer_rows`], shared with
//! `unalias`.
//! Test: `chunk_tests.rs` (this module's siblings) and `vector/chunk_tests.rs`.

use std::collections::HashSet;

use redb::{ReadableTable, Table};

use super::exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS;
use super::key_cache::KeyMap;
use super::{
    DELETED_VECTORS, HnswStore, HnswStoreError, Result, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS,
    allocate_vector_id, op_watch,
};
use crate::memory_core::store::vector_key::{self, MAX_CHUNKS_PER_DRAWER};

#[cfg(test)]
thread_local! {
    /// Test-only fault: makes [`remove_drawer_rows`] fail on this thread.
    static FAIL_GROUP_REMOVAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Remove `key` and every other row of its drawer, tombstoning each row's
/// vector id and recording each removed key in `freed`.
///
/// Why: `delete_group` and `unalias` both free whole drawers (Architect ruling
/// Q5). Running inside the caller's write transaction makes the drawer's rows
/// go together or not at all.
/// What: removes `key`, the drawer's bare key and every key in its
/// `vector_key::group_range`. A key with a malformed suffix is its own group.
/// Test: `remove_deletes_every_chunk_row_of_the_drawer`,
/// `unalias_frees_the_whole_drawer_when_a_chunk_row_collides`,
/// `unalias_reports_error_when_a_group_delete_fails`.
pub(super) fn remove_drawer_rows(
    keys: &mut Table<'_, &'static str, u64>,
    tombstones: &mut Table<'_, u64, &'static [u8]>,
    key: &str,
    freed: &mut Vec<String>,
) -> Result<()> {
    #[cfg(test)]
    if FAIL_GROUP_REMOVAL.with(std::cell::Cell::get) {
        return Err(HnswStoreError::ReadOnly);
    }
    let drawer = vector_key::group_key(key);
    let (lo, hi) = vector_key::group_range(drawer);
    let mut doomed = vec![key.to_string(), drawer.to_string()];
    for entry in keys.range(lo.as_str()..hi.as_str())? {
        doomed.push(entry?.0.value().to_string());
    }
    for k in doomed {
        let removed = keys.remove(k.as_str())?.map(|g| g.value());
        if let Some(id) = removed {
            tombstones.insert(id, [].as_slice())?;
            freed.push(k);
        }
    }
    Ok(())
}

/// One hit per drawer from `raw`, best row first, at most `k` of them.
///
/// What: skips tombstoned and repeated vector ids and ids with no key, then
/// keeps the first (nearest) row of each drawer and emits the drawer key.
/// Returns whether `k` drawers were found.
fn group_rows(raw: Vec<(u64, f32)>, keys: &KeyMap, k: usize) -> (Vec<(String, f32)>, bool) {
    let mut out: Vec<(String, f32)> = Vec::with_capacity(k);
    let mut ids: HashSet<u64> = HashSet::new();
    let mut drawers: HashSet<&str> = HashSet::new();
    for (id, distance) in raw {
        if keys.tombstones.contains(&id) || !ids.insert(id) {
            continue;
        }
        let Some(key) = keys.reverse.get(&id) else {
            continue;
        };
        let drawer = vector_key::group_key(key);
        if drawers.insert(drawer) {
            out.push((drawer.to_string(), distance));
            if out.len() >= k {
                return (out, true);
            }
        }
    }
    (out, false)
}

impl HnswStore {
    /// Write `vectors` as the whole of drawer `drawer`: chunk `n` under
    /// `vector_key::format(drawer, n)`, and no other row.
    ///
    /// Why (#9275): an edited, shorter drawer must not keep matching its old
    /// tail, and a failed write must not leave a half-written drawer.
    /// What: validates the chunk count (1..=[`MAX_CHUNKS_PER_DRAWER`], a hard
    /// error above it) and every dimension BEFORE opening the write
    /// transaction. Then, in one transaction, writes each chunk row (reusing
    /// the row's vector id or allocating one, #5005) and removes the drawer's
    /// surplus `#n` rows. Then inserts each chunk into the graph as
    /// [`HnswStore::upsert`] does. Returns the vector ids in chunk order.
    /// Test: `upsert_chunks_replaces_the_group_and_shrinks`,
    /// `upsert_chunks_with_one_bad_dimension_changes_nothing`,
    /// `upsert_chunks_rejects_empty_and_over_cap`,
    /// `single_upsert_drops_stale_chunk_rows`.
    pub fn replace_group(&self, drawer: &str, vectors: &[Vec<f32>]) -> Result<Vec<u64>> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        if vectors.is_empty() || vectors.len() > MAX_CHUNKS_PER_DRAWER {
            return Err(HnswStoreError::ChunkCount {
                got: vectors.len(),
                max: MAX_CHUNKS_PER_DRAWER,
            });
        }
        if let Some(bad) = vectors.iter().find(|v| v.len() != self.dim) {
            return Err(HnswStoreError::DimensionMismatch {
                expected: self.dim,
                got: bad.len(),
            });
        }
        let _op = self.ops.begin(op_watch::HnswOpKind::Upsert);
        let written = self.commit_group(drawer, vectors)?;
        for ((id, shadows), vector) in written.iter().zip(vectors) {
            self.index_point(vector, *id, *shadows)?;
        }
        Ok(written.into_iter().map(|(id, _)| id).collect())
    }

    /// The redb half of [`Self::replace_group`]: one write transaction.
    fn commit_group(&self, drawer: &str, vectors: &[Vec<f32>]) -> Result<Vec<(u64, bool)>> {
        let encoded = vectors
            .iter()
            .map(postcard::to_allocvec)
            .collect::<std::result::Result<Vec<Vec<u8>>, _>>()?;
        let wtx = self.db.begin_write()?;
        let mut written = Vec::with_capacity(vectors.len());
        {
            let mut rows = wtx.open_table(VECTORS)?;
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            let mut seq = wtx.open_table(VECTOR_ID_SEQ)?;
            for (n, bytes) in encoded.iter().enumerate() {
                // n < MAX_CHUNKS_PER_DRAWER, so it fits u32.
                let key = vector_key::format(drawer, n as u32);
                let existing = keys.get(key.as_str())?.map(|g| g.value());
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let id = allocate_vector_id(&mut seq, &rows, &keys)?;
                        keys.insert(key.as_str(), id)?;
                        id
                    }
                };
                rows.insert(id, bytes.as_slice())?;
                let _ = tombstones.remove(id)?;
                written.push((id, existing.is_some()));
            }
            // Surplus: every `#`-suffixed row this write did not produce,
            // malformed suffixes included.
            let (lo, hi) = vector_key::group_range(drawer);
            let mut stale = Vec::new();
            for entry in keys.range(lo.as_str()..hi.as_str())? {
                let key = entry?.0.value().to_string();
                let kept =
                    vector_key::split(&key).is_some_and(|(_, n)| (n as usize) < vectors.len());
                if !kept {
                    stale.push(key);
                }
            }
            for key in stale {
                if let Some(id) = keys.remove(key.as_str())?.map(|g| g.value()) {
                    tombstones.insert(id, [].as_slice())?;
                }
            }
        }
        wtx.commit()?;
        self.keys.invalidate(); // #9141
        Ok(written)
    }

    /// Delete every row of drawer `drawer`, bare and `#n`, in one write
    /// transaction. Returns the number of rows removed.
    ///
    /// Why (#9275): `forget` must leave no chunk row of the drawer behind.
    /// What: [`remove_drawer_rows`] under one write transaction; tombstones
    /// each row's vector id as [`HnswStore::delete`] does.
    /// Test: `remove_deletes_every_chunk_row_of_the_drawer`,
    /// `forget_removes_all_vector_rows_for_a_drawer`.
    pub fn delete_group(&self, drawer: &str) -> Result<usize> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        let wtx = self.db.begin_write()?;
        let mut freed = Vec::new();
        {
            let mut keys = wtx.open_table(VECTOR_KEYS)?;
            let mut tombstones = wtx.open_table(DELETED_VECTORS)?;
            remove_drawer_rows(&mut keys, &mut tombstones, drawer, &mut freed)?;
        }
        wtx.commit()?;
        self.keys.invalidate(); // #9141
        Ok(freed.len())
    }

    /// Up to `k` drawers nearest `query`, each scored by its nearest row.
    ///
    /// Why (#9275): `k` counts drawers. `candidates.rs` reads a short answer
    /// as "index exhausted", so fewer than `k` may come back only when the
    /// index holds fewer than `k` drawers.
    /// What: ranks rows ([`HnswStore::ranked`]) and folds them per drawer. The
    /// exhaustive arm ranks every row, so one pass is exact. The graph arm
    /// ranks a candidate pool sized in rows; while it yields fewer than `k`
    /// drawers it doubles the pool, up to the live row count.
    /// Test: `top_k_counts_drawers_not_chunks`,
    /// `search_widens_until_k_drawers_in_the_graph_arm`,
    /// `search_returns_short_only_when_the_index_is_exhausted`.
    pub(super) fn search_grouped(
        &self,
        query: &[f32],
        k: usize,
        keys: &KeyMap,
    ) -> Result<Vec<(String, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let live = keys.reverse.len();
        let exhaustive = live <= EXHAUSTIVE_SCAN_MAX_POINTS;
        let mut want = k;
        loop {
            let raw = self.ranked(query, want, keys)?;
            let (out, filled) = group_rows(raw, keys, k);
            if filled || exhaustive || want >= live {
                return Ok(out);
            }
            want = want.saturating_mul(2).min(live);
        }
    }

    /// #9275 test seam: make every [`remove_drawer_rows`] on this thread fail.
    #[cfg(test)]
    pub(crate) fn fail_group_removal_for_test(on: bool) {
        FAIL_GROUP_REMOVAL.with(|f| f.set(on));
    }
}
