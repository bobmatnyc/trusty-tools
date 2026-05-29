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
use redb::{ReadableTable, ReadableTableMetadata};

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
        // 1. Persist payload + label mapping inside one redb txn.
        let payload_json =
            serde_json::to_string(&payload).context("serializing memory payload to JSON")?;
        let payload_key = format!("{}:{}", segment.prefix(), id);
        let (label_to_id_def, id_to_label_def) = Self::label_tables(segment);

        let label = {
            let write_txn = self.db.begin_write()?;
            let label = {
                // Reuse existing label if this id has been written before,
                // otherwise allocate a new one.
                let existing = {
                    let id_to_label = write_txn.open_table(id_to_label_def)?;
                    id_to_label.get(id)?.map(|v| v.value())
                };
                let label = match existing {
                    Some(l) => l,
                    None => next_label(&write_txn, segment)?,
                };

                {
                    let mut payloads = write_txn.open_table(PAYLOAD_TABLE)?;
                    payloads.insert(payload_key.as_str(), payload_json.as_str())?;
                }
                {
                    let mut label_to_id = write_txn.open_table(label_to_id_def)?;
                    label_to_id.insert(label, id)?;
                }
                {
                    let mut id_to_label = write_txn.open_table(id_to_label_def)?;
                    id_to_label.insert(id, label)?;
                }
                label
            };
            write_txn.commit()?;
            label
        };

        // 2. Add vector to usearch and flush to disk. Mutations happen under
        //    the mutex so concurrent inserts can't race on capacity growth.
        //    `ensure_loaded` transparently re-hydrates an evicted index so the
        //    file watcher path keeps writing through cool-down windows.
        let (_, index_path) = self.index_for(segment);
        let guard = self.ensure_loaded(segment).await?;
        let index = guard.as_ref().expect("ensure_loaded guarantees Some");
        ensure_capacity(index)?;
        // If the label is already present (re-insert of same id), remove
        // first so usearch doesn't end up with stale duplicates.
        if index.contains(label) {
            let _ = index
                .remove(label)
                .map_err(|e| anyhow!("removing stale usearch entry: {e}"))?;
        }
        index
            .add(label, vector)
            .map_err(|e| anyhow!("adding vector to usearch: {e}"))?;
        save_index(index, &index_path)?;

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
            let key = format!("{}:{}", segment.prefix(), &id);
            let Some(payload_raw) = payloads.get(key.as_str())? else {
                continue;
            };
            let payload: serde_json::Value = serde_json::from_str(payload_raw.value())
                .context("deserializing stored payload JSON")?;
            results.push(MemoryResult {
                id,
                score: 1.0 - *distance,
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
        if from == to {
            return Ok(());
        }

        // 1. Fetch payload from source.
        let payload = match self.get(from, id).await? {
            Some(p) => p,
            None => {
                return Err(anyhow!(
                    "move_segment: id {id:?} not found in source segment {:?}",
                    from
                ));
            }
        };

        // 2. Fetch vector from source's usearch index.
        let vector: Vec<f32> = {
            let guard = self.ensure_loaded(from).await?;
            let index = guard.as_ref().expect("ensure_loaded guarantees Some");
            let dim = index.dimensions();
            let (_, id_to_label_def) = Self::label_tables(from);
            let read_txn = self.db.begin_read()?;
            let id_to_label = read_txn.open_table(id_to_label_def)?;
            let label = id_to_label
                .get(id)?
                .map(|v| v.value())
                .ok_or_else(|| anyhow!("move_segment: missing label for id {id:?}"))?;
            let mut buf = vec![0.0f32; dim];
            let n = index
                .get(label, &mut buf)
                .map_err(|e| anyhow!("usearch get during move: {e}"))?;
            if n == 0 {
                return Err(anyhow!(
                    "move_segment: vector for id {id:?} missing in source segment"
                ));
            }
            buf
        };

        // 3. Insert into destination, then delete from source. Best-effort
        //    atomicity — if delete fails after insert, the record is briefly
        //    duplicated rather than lost.
        self.insert(to, id, &vector, payload).await?;
        self.delete(from, id).await?;
        Ok(())
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let (label_to_id_def, id_to_label_def) = Self::label_tables(segment);
        let key = format!("{}:{}", segment.prefix(), id);

        // 1. Look up label and drop all redb rows in one transaction.
        let label_opt = {
            let write_txn = self.db.begin_write()?;
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
                let mut payloads = write_txn.open_table(PAYLOAD_TABLE)?;
                payloads.remove(key.as_str())?;
            }
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
