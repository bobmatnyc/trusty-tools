//! The `impl MemoryStore for RedbUsearchStore` block.
//!
//! Why: The trait implementation (insert/search/get/list/move/delete plus the
//! evict/warm lifecycle) is the bulk of the store; isolating it from the
//! struct definition + helpers keeps both files under the 500-line cap.
//! What: Every async `MemoryStore` method, dispatching through the inherent
//! helpers (`label_tables`, `index_for`, `ensure_loaded`) and free functions
//! (`ensure_capacity`, `save_index`, `next_label`) defined in `mod.rs`.
//! Test: Exercised by `redb_usearch::tests`.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata};
use usearch::Index;

use super::{PAYLOAD_TABLE, RedbUsearchStore, ensure_capacity, next_label, save_index};
use crate::memory::store::{MemoryResult, MemoryStore, Segment};

#[async_trait]
impl MemoryStore for RedbUsearchStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()> {
        let payload_json =
            serde_json::to_string(&payload).context("serializing memory payload to JSON")?;

        // audit 2026-08-19: the segment mutex is taken BEFORE `begin_write`, so
        // the redb rows and the usearch vector are written in one critical
        // section, and redb commits LAST.
        //
        // Ordering rationale. The two stores can disagree in two directions and
        // they are not equally bad. A record redb knows about but usearch
        // cannot find is returned by `get` and missed by `search` — a
        // half-visible memory, and the one a reader notices. A vector whose
        // label has no redb row is inert: `search` already skips labels that
        // `label_to_id` cannot resolve, and the next write to that label
        // overwrites it. Committing redb only after the vector is on disk makes
        // the first direction unreachable, because every failure below drops
        // `write_txn` uncommitted and leaves redb exactly as it was. That is
        // the empty window; the residue is always the inert direction.
        //
        // `ensure_loaded` transparently re-hydrates an evicted index, so the
        // file-watcher path keeps writing through cool-down windows.
        let (_, index_path) = self.index_for(segment);
        let guard = self.ensure_loaded(segment).await?;
        let index = guard.as_ref().expect("ensure_loaded guarantees Some");

        let write_txn = self.db.begin_write()?;
        let label = write_rows(&write_txn, segment, id, &payload_json)?;

        ensure_capacity(index)?;
        let displaced = take_displaced(index, label)?;
        let vector_stored = index
            .add(label, vector)
            .map_err(|e| anyhow!("adding vector to usearch: {e}"))
            .and_then(|_| save_index(index, &index_path));
        if let Err(e) = vector_stored {
            // `write_txn` is dropped uncommitted below, so the id's previous
            // redb row survives and must keep pointing at a real vector.
            restore_displaced(index, label, displaced);
            return Err(e);
        }

        write_txn.commit()?;
        Ok(())
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        let matches = {
            let guard = self.ensure_loaded(segment).await?;
            let index = guard.as_ref().expect("ensure_loaded guarantees Some");
            if index.size() == 0 {
                return Ok(Vec::new());
            }
            index
                .search(query_vec, top_k)
                .map_err(|e| anyhow!("usearch search: {e}"))?
        };

        let (label_to_id_def, _) = Self::label_tables(segment);
        let read_txn = self.db.begin_read()?;
        let label_to_id = read_txn.open_table(label_to_id_def)?;
        let payloads = read_txn.open_table(PAYLOAD_TABLE)?;

        let mut results = Vec::with_capacity(matches.keys.len());
        for (label, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            let Some(id_val) = label_to_id.get(*label)? else {
                // Tombstoned or otherwise orphaned label — skip.
                continue;
            };
            let id = id_val.value().to_string();
            let key = format!("{}:{}", segment.prefix(), id);
            let Some(payload_raw) = payloads.get(key.as_str())? else {
                continue;
            };
            let payload: serde_json::Value = serde_json::from_str(payload_raw.value())
                .context("deserializing stored payload JSON")?;
            results.push(MemoryResult {
                id,
                // audit 2026-08-19: cosine distance reaches 2.0 for an
                // antipodal vector, so the raw `1.0 - distance` went to -1.0.
                // Clamp so callers can treat the score as a similarity in
                // [0.0, 1.0] without re-validating it.
                score: (1.0 - *distance).clamp(0.0, 1.0),
                payload,
                segment: segment.prefix().to_string(),
            });
        }
        Ok(results)
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>> {
        let key = format!("{}:{}", segment.prefix(), id);
        let read_txn = self.db.begin_read()?;
        let payloads = read_txn.open_table(PAYLOAD_TABLE)?;
        let Some(raw) = payloads.get(key.as_str())? else {
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(raw.value()).context("deserializing stored payload JSON")?;
        Ok(Some(value))
    }

    async fn list_segments(&self) -> Result<Vec<Segment>> {
        // Why: Iterate each segment's `id_to_label` redb table and report only
        // those with at least one row. We exclude `CodeIndex` from this scan
        // because callers (e.g., agent-memory tooling) treat the code-vector
        // namespace as a separate concern; querying it directly via search
        // remains supported.
        let candidates = [
            Segment::AgentMemory,
            Segment::Context,
            Segment::Brief,
            Segment::History,
        ];
        let read_txn = self.db.begin_read()?;
        let mut populated = Vec::new();
        for seg in candidates {
            let (_, id_to_label_def) = Self::label_tables(seg);
            let table = read_txn.open_table(id_to_label_def)?;
            if table.len()? > 0 {
                populated.push(seg);
            }
        }
        Ok(populated)
    }

    async fn move_segment(&self, id: &str, from: Segment, to: Segment) -> Result<()> {
        // Why: Reclassify a record (e.g., brief -> history) without losing
        // the original embedding. Reading payload + vector first means we
        // can avoid recomputing the embedding in the destination segment.
        //
        // audit 2026-08-19: the redb half of the move is now ONE transaction —
        // the destination rows are written and the source rows removed
        // together, so no failure can leave the record in both segments or in
        // neither. This replaces an insert-then-delete pair that ran two
        // independent transactions and documented a duplicate-over-loss bias.
        //
        // The two vector indexes cannot join that transaction, so they bracket
        // it, each on the side that keeps the residue inert: the destination
        // vector is flushed BEFORE the commit, because an addition must be
        // durable before metadata points at it; the source vector is dropped
        // AFTER, because a removal must not outrun its metadata or the
        // surviving row would point at nothing. Either bracket failing leaves
        // at worst an orphan vector with no redb row, which `search` skips.
        //
        // Achieved: atomic metadata move, no duplicate and no loss. Not
        // achieved: full cross-store atomicity. A crash between the commit and
        // the source flush leaves a stale vector in the source index —
        // unreachable from every read path, reclaimed by the next write to
        // that label.
        if from == to {
            return Ok(());
        }

        let payload = match self.get(from, id).await? {
            Some(p) => p,
            None => {
                return Err(anyhow!(
                    "move_segment: id {id:?} not found in source segment {:?}",
                    from
                ));
            }
        };
        let payload_json =
            serde_json::to_string(&payload).context("serializing memory payload to JSON")?;

        // Lock both segment indexes lowest-rank-first so two moves running in
        // opposite directions cannot deadlock on each other's mutex.
        let (lo, hi) = if lock_rank(from) < lock_rank(to) {
            (from, to)
        } else {
            (to, from)
        };
        let lo_guard = self.ensure_loaded(lo).await?;
        let hi_guard = self.ensure_loaded(hi).await?;
        let (src_guard, dst_guard) = if lo == from {
            (&lo_guard, &hi_guard)
        } else {
            (&hi_guard, &lo_guard)
        };
        let src_index = src_guard.as_ref().expect("ensure_loaded guarantees Some");
        let dst_index = dst_guard.as_ref().expect("ensure_loaded guarantees Some");

        // Source vector, read before anything mutates.
        let vector: Vec<f32> = {
            let (_, id_to_label_def) = Self::label_tables(from);
            let read_txn = self.db.begin_read()?;
            let id_to_label = read_txn.open_table(id_to_label_def)?;
            let label = id_to_label
                .get(id)?
                .map(|v| v.value())
                .ok_or_else(|| anyhow!("move_segment: missing label for id {id:?}"))?;
            let mut buf = vec![0.0f32; src_index.dimensions()];
            let n = src_index
                .get(label, &mut buf)
                .map_err(|e| anyhow!("usearch get during move: {e}"))?;
            if n == 0 {
                return Err(anyhow!(
                    "move_segment: vector for id {id:?} missing in source segment"
                ));
            }
            buf
        };

        let write_txn = self.db.begin_write()?;
        let src_label = remove_rows(&write_txn, from, id)?
            .ok_or_else(|| anyhow!("move_segment: missing label for id {id:?}"))?;
        let dst_label = write_rows(&write_txn, to, id, &payload_json)?;

        let (_, dst_path) = self.index_for(to);
        ensure_capacity(dst_index)?;
        let displaced = take_displaced(dst_index, dst_label)?;
        let dst_stored = dst_index
            .add(dst_label, &vector)
            .map_err(|e| anyhow!("adding vector to usearch: {e}"))
            .and_then(|_| save_index(dst_index, &dst_path));
        if let Err(e) = dst_stored {
            // `write_txn` drops uncommitted, so the record stays whole in the
            // source segment and the destination never sees it.
            restore_displaced(dst_index, dst_label, displaced);
            return Err(e);
        }

        write_txn.commit()?;

        // Past the commit the move has happened. Dropping the source vector is
        // cleanup: a failure here leaves an orphan no read path can reach, so
        // it is logged rather than reported as a failed move.
        let (_, src_path) = self.index_for(from);
        if src_index.contains(src_label)
            && let Err(e) = src_index.remove(src_label)
        {
            tracing::warn!(
                id, label = src_label, error = %e,
                "move committed but the source vector could not be removed"
            );
        }
        if let Err(e) = save_index(src_index, &src_path) {
            tracing::warn!(
                id, error = %e,
                "move committed but the source index could not be flushed"
            );
        }
        Ok(())
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        // 1. Look up label and drop all redb rows in one transaction. The
        //    metadata goes first on purpose: a vector left behind by a failure
        //    in step 2 has no `label_to_id` row, so `search` skips it. Removing
        //    the vector first would instead strand a live row pointing at
        //    nothing.
        let label_opt = {
            let write_txn = self.db.begin_write()?;
            let label_opt = remove_rows(&write_txn, segment, id)?;
            write_txn.commit()?;
            label_opt
        };

        // 2. Remove the vector from usearch (tombstone) and persist.
        if let Some(label) = label_opt {
            let (_, index_path) = self.index_for(segment);
            let guard = self.ensure_loaded(segment).await?;
            let index = guard.as_ref().expect("ensure_loaded guarantees Some");
            if index.contains(label) {
                let _ = index
                    .remove(label)
                    .map_err(|e| anyhow!("removing usearch entry: {e}"))?;
            }
            save_index(index, &index_path)?;
        }

        Ok(())
    }

    async fn evict_segment(&self, segment: Segment) -> Result<()> {
        // Why: Drop the in-memory HNSW for `segment` to free RAM after a
        // search-inactivity window. Persistence is unaffected — the on-disk
        // `.usearch` file plus redb metadata stay intact and the next access
        // path (`ensure_loaded`) rehydrates from disk transparently.
        // What: Locks the segment's mutex and replaces `Some(Index)` with
        // `None`. Idempotent: a second call while already evicted is a no-op.
        // Test: `evict_then_warm_returns_same_results`.
        let mutex_ref = match segment {
            Segment::AgentMemory => &self.mem_index,
            Segment::CodeIndex => &self.code_index,
            Segment::Context => &self.ctx_index,
            Segment::Brief => &self.brief_index,
            Segment::History => &self.hist_index,
        };
        let mut guard = mutex_ref.lock().await;
        if guard.is_some() {
            *guard = None;
            tracing::info!(segment = ?segment, "search index evicted from memory");
        }
        Ok(())
    }

    async fn warm_segment(&self, segment: Segment) -> Result<()> {
        // Why: Public counterpart to `evict_segment`. Pre-loads an evicted
        // index so the next search query sees zero warm-up latency. Callers
        // that just want lazy warm-up can omit this — `search` calls
        // `ensure_loaded` itself — but explicit pre-warm is useful at PM
        // startup ("never cold-start under user load").
        // What: Calls `ensure_loaded` and drops the guard.
        // Test: `evict_then_warm_returns_same_results`.
        let _ = self.ensure_loaded(segment).await?;
        Ok(())
    }

    async fn is_segment_warm(&self, segment: Segment) -> Result<bool> {
        // Why: Tests assert eviction actually happened; production callers
        // can use this to skip redundant warm-up work.
        let mutex_ref = match segment {
            Segment::AgentMemory => &self.mem_index,
            Segment::CodeIndex => &self.code_index,
            Segment::Context => &self.ctx_index,
            Segment::Brief => &self.brief_index,
            Segment::History => &self.hist_index,
        };
        Ok(mutex_ref.lock().await.is_some())
    }
}

// --- caller-owned-transaction helpers -----------------------------------
//
// audit 2026-08-19: `insert`, `delete` and `move_segment` all have to place
// their redb rows in a transaction the CALLER commits, so the vector writes
// can be sequenced around the commit. These helpers hold the row layout once
// so the three paths cannot drift apart.

/// Write the payload row and both label rows for `id`, returning the label the
/// record now occupies.
///
/// Why: `insert` and the destination half of `move_segment` need the same
/// three rows, and both must be able to abandon them by dropping the
/// transaction unommitted.
/// What: reuses the id's existing label when it has one, otherwise allocates
/// the next label for `segment`, then writes `payloads`, `label_to_id` and
/// `id_to_label`.
/// Test: `roundtrip_insert_and_search`, `move_segment_transfers_and_deletes`.
fn write_rows(
    write_txn: &redb::WriteTransaction,
    segment: Segment,
    id: &str,
    payload_json: &str,
) -> Result<u64> {
    let (label_to_id_def, id_to_label_def) = RedbUsearchStore::label_tables(segment);

    // Reuse the existing label if this id has been written before, otherwise
    // allocate a new one.
    let existing = {
        let id_to_label = write_txn.open_table(id_to_label_def)?;
        id_to_label.get(id)?.map(|v| v.value())
    };
    let label = match existing {
        Some(l) => l,
        None => next_label(write_txn, segment)?,
    };

    {
        let key = format!("{}:{}", segment.prefix(), id);
        let mut payloads = write_txn.open_table(PAYLOAD_TABLE)?;
        payloads.insert(key.as_str(), payload_json)?;
    }
    {
        let mut label_to_id = write_txn.open_table(label_to_id_def)?;
        label_to_id.insert(label, id)?;
    }
    {
        let mut id_to_label = write_txn.open_table(id_to_label_def)?;
        id_to_label.insert(id, label)?;
    }
    Ok(label)
}

/// Drop the payload row and both label rows for `id`, returning the label the
/// record occupied, or `None` when the id was not present.
///
/// Why: `delete` and the source half of `move_segment` need the same removal,
/// and `move_segment` needs it in the same transaction as the destination
/// write.
/// What: removes `id_to_label`, `label_to_id` and the `payloads` row.
/// Test: `delete_removes_from_both_stores`, `move_segment_transfers_and_deletes`.
fn remove_rows(
    write_txn: &redb::WriteTransaction,
    segment: Segment,
    id: &str,
) -> Result<Option<u64>> {
    let (label_to_id_def, id_to_label_def) = RedbUsearchStore::label_tables(segment);

    let label_opt = {
        let mut id_to_label = write_txn.open_table(id_to_label_def)?;
        let label = id_to_label.get(id)?.map(|v| v.value());
        if label.is_some() {
            id_to_label.remove(id)?;
        }
        label
    };
    if let Some(label) = label_opt {
        let mut label_to_id = write_txn.open_table(label_to_id_def)?;
        label_to_id.remove(label)?;
    }
    {
        let key = format!("{}:{}", segment.prefix(), id);
        let mut payloads = write_txn.open_table(PAYLOAD_TABLE)?;
        payloads.remove(key.as_str())?;
    }
    Ok(label_opt)
}

/// Clear whatever vector sits under `label`, handing it back to the caller.
///
/// Why: usearch `add` appends rather than replaces, so re-inserting an id has
/// to remove the old entry first. If the write that follows fails, the redb
/// transaction aborts and the old row survives — it must keep pointing at a
/// real vector, so the caller needs the removed one back.
/// What: reads the current vector for `label`, removes it, and returns the
/// snapshot. `Ok(None)` means there was nothing stored under `label`.
/// Test: `insert_index_write_failure_leaves_no_phantom_redb_row`.
fn take_displaced(index: &Index, label: u64) -> Result<Option<Vec<f32>>> {
    if !index.contains(label) {
        return Ok(None);
    }
    let mut buf = vec![0.0f32; index.dimensions()];
    let snapshot = match index.get(label, &mut buf) {
        Ok(n) if n > 0 => Some(buf),
        _ => None,
    };
    index
        .remove(label)
        .map_err(|e| anyhow!("removing stale usearch entry: {e}"))?;
    Ok(snapshot)
}

/// Put a displaced vector back after the write that replaced it failed.
///
/// Why: restores the pre-write state so an aborted transaction leaves both
/// stores exactly as they were.
/// What: re-adds `displaced` under `label`. A failure here cannot be
/// propagated — the caller is already returning the original error — so it is
/// logged at `error` level instead of being swallowed.
/// Test: `insert_index_write_failure_leaves_no_phantom_redb_row`.
fn restore_displaced(index: &Index, label: u64, displaced: Option<Vec<f32>>) {
    if let Some(old) = displaced
        && let Err(e) = index.add(label, &old)
    {
        tracing::error!(
            label, error = %e,
            "could not restore the usearch vector displaced by a failed write"
        );
    }
}

/// Stable total order over segments, used when one operation holds two
/// segment mutexes.
///
/// Why: `move_segment` locks both the source and the destination index. Two
/// concurrent moves in opposite directions would deadlock if each locked its
/// own source first; locking in rank order makes a cycle impossible.
/// What: assigns each variant a fixed rank.
/// Test: `move_segment_transfers_and_deletes` exercises the locking path.
fn lock_rank(segment: Segment) -> u8 {
    match segment {
        Segment::AgentMemory => 0,
        Segment::CodeIndex => 1,
        Segment::Context => 2,
        Segment::Brief => 3,
        Segment::History => 4,
    }
}
