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
//! `unalias`; [`HnswStore::entry_count`] for the size heuristics.
//! Test: `chunk_tests.rs` (this module's siblings) and `vector/chunk_tests.rs`.

use std::collections::HashSet;

use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, Table};

use super::exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS;
use super::key_cache::KeyMap;
use super::op_budget::OpProgress;
use super::{
    DELETED_VECTORS, HnswStore, HnswStoreError, Result, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS,
    allocate_vector_id, op_watch,
};
use crate::engine::store::vector_key::{self, MAX_CHUNKS_PER_DRAWER};

#[cfg(test)]
#[path = "index_pace.rs"]
pub(crate) mod index_pace;

#[cfg(test)]
thread_local! {
    /// Test-only fault: makes [`remove_drawer_rows`] fail on this thread.
    static FAIL_GROUP_REMOVAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// #9735 test-only fault: drawer keys whose next group write fails before its
/// redb commit. Process-wide, because the write runs on a blocking thread;
/// keyed by drawer, so a test arms only its own drawer.
#[cfg(test)]
static FAIL_GROUP_COMMIT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Consume an armed [`FAIL_GROUP_COMMIT`] fault for `drawer`, if any.
#[cfg(test)]
fn take_group_commit_fault(drawer: &str) -> Result<()> {
    let mut armed = FAIL_GROUP_COMMIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match armed.iter().position(|d| d == drawer) {
        Some(i) => {
            armed.swap_remove(i);
            let io = std::io::Error::other("#9735 injected group-commit fault");
            Err(Box::new(redb::StorageError::Io(io)).into())
        }
        None => Ok(()),
    }
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

/// Reject a drawer key that is empty or holds `#`.
///
/// Why (#9275): `split` cuts at the first `#`, so a drawer key `x#2` makes
/// its own chunk rows read as malformed and `delete_group("{d}#2")` reads as
/// drawer `d`. Either silently deletes rows behind an `Ok`.
/// Test: `replace_group_rejects_an_empty_or_hash_drawer_key`,
/// `delete_group_rejects_an_empty_or_hash_drawer_key`.
fn check_drawer_key(drawer: &str) -> Result<()> {
    if drawer.is_empty() || drawer.contains('#') {
        return Err(HnswStoreError::DrawerKey(drawer.to_string()));
    }
    Ok(())
}

/// One hit per drawer from `raw`, best row first, at most `k` of them.
///
/// What: skips tombstoned and repeated vector ids, ids with no key, and keys
/// with a malformed suffix (#9275: such a row names no drawer, so it must not
/// take one of the `k` slots). Keeps the first (nearest) row of each drawer
/// and emits the drawer key. Returns whether `k` drawers were found.
/// Test: `malformed_key_takes_no_top_k_slot`.
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
        let Some((drawer, _)) = vector_key::split(key) else {
            continue;
        };
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
    /// `single_upsert_drops_stale_chunk_rows`,
    /// `replace_group_rejects_an_empty_or_hash_drawer_key`.
    pub(crate) fn replace_group(&self, drawer: &str, vectors: &[Vec<f32>]) -> Result<Vec<u64>> {
        let progress = OpProgress::default();
        self.replace_group_capped(drawer, vectors, MAX_CHUNKS_PER_DRAWER, &progress)
    }

    /// [`Self::replace_group`] with the chunk-count cap passed in.
    ///
    /// Why: a test at the production cap inserts 4096 points in one call and
    /// outran the 60 s HNSW op budget on CI; a small cap tests the same
    /// boundary in milliseconds.
    /// What: `cap` must be at most [`MAX_CHUNKS_PER_DRAWER`], so a chunk
    /// index fits the `u32` key suffix. A count outside `1..=cap` is
    /// [`HnswStoreError::ChunkCount`] naming `cap`, before any write.
    /// #9728: `progress` ticks once after the redb commit and once after each
    /// indexed point, so a `run_bounded_progress` watcher sees a slow legal
    /// group advancing and abandons only a stalled one. Each tick also
    /// restarts the op's [`HnswStore::oldest_op`] age, which health reads.
    /// Test: `upsert_chunks_rejects_empty_and_over_cap`,
    /// `a_legal_group_slower_than_the_budget_completes_without_tripping`,
    /// `a_progressing_group_keeps_its_op_age_under_one_budget`.
    // #9275: cap seam so the boundary test fits the op budget.
    pub(crate) fn replace_group_capped(
        &self,
        drawer: &str,
        vectors: &[Vec<f32>],
        cap: usize,
        progress: &OpProgress,
    ) -> Result<Vec<u64>> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        check_drawer_key(drawer)?;
        if vectors.is_empty() || vectors.len() > cap {
            return Err(HnswStoreError::ChunkCount {
                got: vectors.len(),
                max: cap,
            });
        }
        if let Some(bad) = vectors.iter().find(|v| v.len() != self.dim) {
            return Err(HnswStoreError::DimensionMismatch {
                expected: self.dim,
                got: bad.len(),
            });
        }
        let mut op = self.ops.begin(op_watch::HnswOpKind::Upsert);
        let written = self.commit_group(drawer, vectors)?;
        // #9728: each tick restarts the watcher's budget and the op's health
        // age; a stall gets neither. Touch first, so the age never trails
        // the watcher's clock.
        op.touch();
        progress.advance();
        for ((id, shadows), vector) in written.iter().zip(vectors) {
            #[cfg(test)]
            index_pace::pace(drawer);
            self.index_point(vector, *id, *shadows)?;
            op.touch();
            progress.advance();
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
                // n < cap <= MAX_CHUNKS_PER_DRAWER, so it fits u32.
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
        // #9735: the rows above are written; dropping `wtx` rolls them back.
        #[cfg(test)]
        take_group_commit_fault(drawer)?;
        wtx.commit()?;
        self.keys.invalidate(); // #9141
        Ok(written)
    }

    /// Delete every row of drawer `drawer`, bare and `#n`, in one write
    /// transaction. Returns the number of rows removed.
    ///
    /// Why (#9275): `forget` must leave no chunk row of the drawer behind.
    /// What: [`remove_drawer_rows`] under one write transaction; tombstones
    /// each row's vector id as [`HnswStore::delete`] does. An empty key or one
    /// holding `#` is [`HnswStoreError::DrawerKey`] and deletes nothing.
    /// Test: `remove_deletes_every_chunk_row_of_the_drawer`,
    /// `forget_removes_all_vector_rows_for_a_drawer`,
    /// `delete_group_rejects_an_empty_or_hash_drawer_key`.
    pub(crate) fn delete_group(&self, drawer: &str) -> Result<usize> {
        if self.read_only {
            return Err(HnswStoreError::ReadOnly);
        }
        check_drawer_key(drawer)?;
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

    /// Live vector rows, with each drawer's chunk rows counted once.
    ///
    /// Why (#9275): the dream rebuild trigger and the open-time orphan warning
    /// compare the index with the drawer table. Counting rows makes a healthy
    /// chunked palace look orphan-ridden; counting parseable drawers hides
    /// the rows those checks exist for — keyless rows and keys naming no
    /// drawer. This is [`Self::len`] minus the surplus chunk rows, so a
    /// palace of bare keys reads exactly as `len` did on main.
    /// What: one read transaction; `len` minus (key rows minus distinct
    /// `vector_key::group_key` groups). A malformed key is a group of its own.
    /// Test: `unparseable_and_keyless_rows_still_trip_the_orphan_warning_and_rebuild`,
    /// `drawer_count_ignores_chunk_rows_and_dream_does_not_rebuild_a_chunked_palace`.
    pub(crate) fn entry_count(&self) -> Result<usize> {
        let rtx = self.db.begin_read()?;
        let live = (rtx.open_table(VECTORS)?.len()? as usize)
            .saturating_sub(rtx.open_table(DELETED_VECTORS)?.len()? as usize);
        let keys = rtx.open_table(VECTOR_KEYS)?;
        let mut rows = 0usize;
        let mut groups: HashSet<String> = HashSet::new();
        for entry in keys.iter()? {
            let key = entry?.0.value().to_string();
            rows += 1;
            groups.insert(vector_key::group_key(&key).to_string());
        }
        Ok(live.saturating_sub(rows - groups.len()))
    }

    /// #9275 test seam: make every [`remove_drawer_rows`] on this thread fail.
    #[cfg(test)]
    pub(crate) fn fail_group_removal_for_test(on: bool) {
        FAIL_GROUP_REMOVAL.with(|f| f.set(on));
    }

    /// #9728 test seam: commit `vectors` as drawer `drawer`'s rows without
    /// indexing them, so a reopen's parallel replay builds a large graph fast.
    #[cfg(test)]
    pub(crate) fn commit_group_unindexed_for_test(
        &self,
        drawer: &str,
        vectors: &[Vec<f32>],
    ) -> Result<()> {
        self.commit_group(drawer, vectors).map(drop)
    }

    /// #9735 test seam: the next group write for `drawer` fails after its
    /// rows are written and before the redb commit.
    #[cfg(test)]
    pub(crate) fn fail_next_group_commit_for_test(drawer: &str) {
        FAIL_GROUP_COMMIT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(drawer.to_string());
    }
}
