//! `MemoryStore` adapter backed by `trusty-memory-core`'s `PalaceHandle`.
//!
//! Why: Issue #379 — bring trusty-memory-core in as a path dependency so we can
//! incrementally migrate trusty-agents's flat `MemoryStore` interface onto trusty's
//! Palace/Wing/Room/Drawer hierarchy without touching the nine consumer files.
//! Existing call sites continue to use the `MemoryStore` trait; this adapter
//! routes those calls through a per-`Segment` `PalaceHandle` (one Palace per
//! segment, one Room of the corresponding `RoomType`).
//! What: `TrustyBackedMemoryStore` wraps a `PalaceRegistry` and a payload
//! sidecar map (string-id ↔ `Uuid` ↔ JSON payload) so the trusty-agents trait —
//! which works with arbitrary string ids and pre-computed vectors — can ride
//! on top of trusty's UUID/embedding-managed Drawer model.
//! Test: See `tests` module below — round-trips insert/search/get/delete and
//! confirms segment isolation. (Tests are gated behind `#[cfg(test)]` and use
//! a tempdir-backed registry so they don't touch the real `.trusty-agents/` state.)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::registry::PalaceRegistry;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::payload_store::PayloadStore;
use trusty_common::memory_core::store::vector::VectorStore;

use crate::memory::store::{MemoryResult, MemoryStore, Segment};

/// Deterministic UUIDv5 namespace used to map trusty-agents string ids onto the
/// `Uuid` keyspace that trusty's `UsearchStore` requires.
///
/// Why: `MemoryStore::insert` accepts a free-form `&str` id but trusty's
/// `VectorStore::upsert(uuid, vec)` only accepts `Uuid`. A v5 hash gives us a
/// stable bidirectional mapping (per (segment, id) pair) that survives process
/// restarts without an external mapping table for the vector layer itself —
/// the payload sidecar still needs persistence work, but the index keys stay
/// reproducible.
/// What: Random fixed UUID generated once for this adapter; do not change.
const TRUSTY_BACKED_NAMESPACE: Uuid = Uuid::from_bytes([
    0x4a, 0x2c, 0x9b, 0x1f, 0x8e, 0x6d, 0x4f, 0x3a, 0xa1, 0x55, 0x7b, 0x9c, 0x2d, 0x88, 0xe3, 0x10,
]);

/// Vector dimension expected by trusty's `UsearchStore` when opened via
/// `PalaceHandle::open` (currently hard-coded to 384 in trusty).
const TRUSTY_VECTOR_DIM: usize = 384;

/// Per-segment payload sidecar entry.
#[derive(Debug, Clone)]
struct PayloadEntry {
    /// Original trusty-agents string id (kept so `MemoryResult::id` round-trips).
    id: String,
    /// Application payload supplied by the caller.
    payload: Value,
}

/// `MemoryStore` adapter that persists each `Segment` as its own
/// `PalaceHandle` inside a shared `PalaceRegistry`.
///
/// Why: Lets us bring `trusty-memory-core` online without rewriting the nine
/// existing `MemoryStore` consumers. The trait surface stays identical; only
/// the storage backend changes.
/// What: Holds the registry, a data root for palace files, an in-memory
/// per-segment `id ↔ uuid ↔ payload` map (the hot path), and a SQLite-backed
/// `PayloadStore` that durably mirrors that map so payloads survive a
/// process restart (issue #52).
/// Test: `tests::insert_search_get_delete_roundtrip`,
/// `tests::payloads_persist_across_reopen`, and friends.
pub struct TrustyBackedMemoryStore {
    registry: PalaceRegistry,
    data_root: PathBuf,
    /// `Segment → (string id → (uuid, payload))` plus reverse `uuid → string id`
    /// so search hits can be translated back to the caller's id. Hydrated from
    /// `payloads` on construction; written through on every mutating call.
    sidecar: Arc<RwLock<HashMap<Segment, SegmentSidecar>>>,
    /// Durable SQLite-backed payload sidecar (issue #52). Lives at
    /// `<data_root>/payloads.db`. All `sidecar` mutations are mirrored here so
    /// the next `TrustyBackedMemoryStore::new` call hydrates the same state.
    payloads: PayloadStore,
}

#[derive(Default)]
struct SegmentSidecar {
    by_id: HashMap<String, (Uuid, Value)>,
    by_uuid: HashMap<Uuid, String>,
}

impl TrustyBackedMemoryStore {
    /// Build a new adapter rooted at `data_root`.
    ///
    /// Why: Each segment becomes a sibling palace under `data_root`; keeping a
    /// single root simplifies migration and cleanup. Issue #52: hydrate the
    /// in-memory sidecar from the durable SQLite payload store so prior-run
    /// payloads are immediately visible to `get` and `search`.
    /// What: Creates `data_root` if missing, opens `<data_root>/payloads.db`,
    /// reloads every persisted row into the per-segment sidecar map, and
    /// returns a ready-to-use adapter. Palaces themselves are still lazily
    /// opened on first use of each segment.
    /// Test: `tests::insert_search_get_delete_roundtrip` (constructs a store
    /// in a tempdir); `tests::payloads_persist_across_reopen` exercises the
    /// hydrate-on-construct path.
    pub fn new(data_root: impl Into<PathBuf>) -> Result<Self> {
        let data_root = data_root.into();
        std::fs::create_dir_all(&data_root)
            .with_context(|| format!("create trusty-backed data root {}", data_root.display()))?;

        let payload_db = data_root.join("payloads.db");
        let payloads = PayloadStore::open(&payload_db)
            .with_context(|| format!("open trusty payload store at {}", payload_db.display()))?;

        // Hydrate the in-memory sidecar from persisted rows.
        let mut sidecar: HashMap<Segment, SegmentSidecar> = HashMap::new();
        let rows = payloads
            .load_all(None)
            .with_context(|| format!("load payloads from {}", payload_db.display()))?;
        for row in rows {
            let Some(segment) = Segment::from_name(&row.segment) else {
                tracing::warn!(
                    segment = %row.segment,
                    "skipping persisted payload row with unknown segment prefix"
                );
                continue;
            };
            let entry = sidecar.entry(segment).or_default();
            entry.by_id.insert(row.id.clone(), (row.uuid, row.payload));
            entry.by_uuid.insert(row.uuid, row.id);
        }

        Ok(Self {
            registry: PalaceRegistry::new(),
            data_root,
            sidecar: Arc::new(RwLock::new(sidecar)),
            payloads,
        })
    }

    /// Stable string identifier for the palace backing `segment`.
    fn palace_id_for(segment: Segment) -> PalaceId {
        PalaceId::new(format!("trusty-agents-{}", segment.prefix()))
    }

    /// Map an trusty-agents `Segment` onto the trusty `RoomType` taxonomy.
    ///
    /// Why: trusty rooms are topical buckets; we pick the closest semantic
    /// match for each segment so the data is at least browsable from
    /// trusty-native tools.
    /// What: AgentMemory→General, CodeIndex→Backend, Context→Documentation,
    /// Brief→Planning, History→Research.
    fn room_type_for(segment: Segment) -> RoomType {
        match segment {
            Segment::AgentMemory => RoomType::General,
            Segment::CodeIndex => RoomType::Backend,
            Segment::Context => RoomType::Documentation,
            Segment::Brief => RoomType::Planning,
            Segment::History => RoomType::Research,
        }
    }

    /// Convert a (segment, string-id) pair into a deterministic `Uuid`.
    fn uuid_for(segment: Segment, id: &str) -> Uuid {
        let combined = format!("{}/{}", segment.prefix(), id);
        Uuid::new_v5(&TRUSTY_BACKED_NAMESPACE, combined.as_bytes())
    }

    /// Open or fetch the `PalaceHandle` for `segment`.
    fn handle_for(&self, segment: Segment) -> Result<Arc<PalaceHandle>> {
        let palace_id = Self::palace_id_for(segment);
        if let Some(h) = self.registry.get(&palace_id) {
            return Ok(h);
        }

        let palace_dir = self.data_root.join(palace_id.as_str());
        // Try open-from-disk first; if no metadata exists, create a fresh one.
        if palace_dir.join("palace.json").exists() {
            return self
                .registry
                .open_palace(&self.data_root, &palace_id)
                .with_context(|| format!("open trusty palace {palace_id}"));
        }

        let palace = Palace {
            id: palace_id.clone(),
            name: format!("trusty-agents {} segment", segment.prefix()),
            description: Some(format!(
                "Auto-created by TrustyBackedMemoryStore for segment {:?}",
                segment
            )),
            created_at: chrono::Utc::now(),
            data_dir: palace_dir.clone(),
        };
        self.registry
            .create_palace(&self.data_root, palace)
            .with_context(|| format!("create trusty palace {palace_id}"))
    }
}

#[async_trait]
impl MemoryStore for TrustyBackedMemoryStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> Result<()> {
        if vector.len() != TRUSTY_VECTOR_DIM {
            anyhow::bail!(
                "TrustyBackedMemoryStore expects {}-d vectors (got {})",
                TRUSTY_VECTOR_DIM,
                vector.len()
            );
        }
        let handle = self.handle_for(segment)?;
        let uuid = Self::uuid_for(segment, id);
        handle
            .vector_store
            .upsert(uuid, vector.to_vec())
            .await
            .with_context(|| {
                format!("upsert vector into trusty palace for segment {:?}", segment)
            })?;

        // Persist payload first (durable), then update the in-memory map. If
        // the SQLite write fails we surface the error without polluting the
        // sidecar with a row that isn't on disk.
        self.payloads
            .upsert(segment.prefix(), id, uuid, &payload)
            .with_context(|| {
                format!(
                    "persist payload for segment {:?} id {} in trusty payload store",
                    segment, id
                )
            })?;

        let mut guard = self.sidecar.write().expect("sidecar lock poisoned");
        let entry = guard.entry(segment).or_default();
        entry.by_id.insert(id.to_string(), (uuid, payload));
        entry.by_uuid.insert(uuid, id.to_string());
        Ok(())
    }

    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        let handle = self.handle_for(segment)?;
        let hits = handle
            .vector_store
            .search(query_vec, top_k)
            .await
            .with_context(|| format!("search trusty palace for segment {:?}", segment))?;

        let guard = self.sidecar.read().expect("sidecar lock poisoned");
        let sidecar = guard.get(&segment);
        let segment_label = format!("{:?}", segment);
        let results = hits
            .into_iter()
            .filter_map(|hit| {
                let id_str = sidecar.and_then(|s| s.by_uuid.get(&hit.drawer_id))?;
                let payload = sidecar
                    .and_then(|s| s.by_id.get(id_str))
                    .map(|(_, p)| p.clone())
                    .unwrap_or(Value::Null);
                Some(MemoryResult {
                    id: id_str.clone(),
                    score: hit.score,
                    payload,
                    segment: segment_label.clone(),
                })
            })
            .collect();
        Ok(results)
    }

    async fn get(&self, segment: Segment, id: &str) -> Result<Option<Value>> {
        let guard = self.sidecar.read().expect("sidecar lock poisoned");
        Ok(guard
            .get(&segment)
            .and_then(|s| s.by_id.get(id))
            .map(|(_, payload)| payload.clone()))
    }

    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let handle = self.handle_for(segment)?;
        let uuid = Self::uuid_for(segment, id);
        if let Err(e) = handle.vector_store.remove(uuid).await {
            tracing::warn!(?segment, %id, "trusty vector remove failed: {e:#}");
        }
        // Drop the durable row first so a crash mid-delete doesn't resurrect
        // the payload on the next open.
        if let Err(e) = self.payloads.delete(segment.prefix(), id) {
            tracing::warn!(?segment, %id, "trusty payload delete failed: {e:#}");
        }
        let mut guard = self.sidecar.write().expect("sidecar lock poisoned");
        if let Some(entry) = guard.get_mut(&segment) {
            entry.by_id.remove(id);
            entry.by_uuid.remove(&uuid);
        }
        Ok(())
    }

    async fn list_segments(&self) -> Result<Vec<Segment>> {
        let guard = self.sidecar.read().expect("sidecar lock poisoned");
        Ok(guard
            .iter()
            .filter_map(|(seg, side)| {
                if side.by_id.is_empty() {
                    None
                } else {
                    Some(*seg)
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    /// Build a 384-d vector dominated by `seed` so cosine search has a clear
    /// nearest match.
    fn vec_with_seed(seed: f32) -> Vec<f32> {
        let mut v = vec![0.0_f32; TRUSTY_VECTOR_DIM];
        v[0] = seed;
        v
    }

    #[tokio::test]
    async fn insert_search_get_delete_roundtrip() {
        let dir = tempdir().unwrap();
        let store = TrustyBackedMemoryStore::new(dir.path()).unwrap();

        let payload = json!({"hello": "world"});
        store
            .insert(
                Segment::AgentMemory,
                "rec-1",
                &vec_with_seed(1.0),
                payload.clone(),
            )
            .await
            .unwrap();

        // get round-trips the payload
        let got = store.get(Segment::AgentMemory, "rec-1").await.unwrap();
        assert_eq!(got, Some(payload.clone()));

        // search returns the inserted record
        let hits = store
            .search(Segment::AgentMemory, &vec_with_seed(1.0), 5)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "expected at least one search hit");
        assert_eq!(hits[0].id, "rec-1");
        assert_eq!(hits[0].payload, payload);

        // delete drops the payload
        store.delete(Segment::AgentMemory, "rec-1").await.unwrap();
        let after = store.get(Segment::AgentMemory, "rec-1").await.unwrap();
        assert!(after.is_none());
    }

    /// Why: Issue #52 — payloads must survive a process restart so
    /// `TrustyBackedMemoryStore` can replace `RedbUsearchStore` as the
    /// production default without losing the application-supplied payload
    /// data on each daemon restart.
    /// What: Insert a payload, drop the store, re-open the same data root, and
    /// assert both `get` and `search` still return the original payload.
    /// Test: This test itself.
    #[tokio::test]
    async fn payloads_persist_across_reopen() {
        let dir = tempdir().unwrap();
        let payload = json!({"content": "the quokka is a marsupial"});

        // Phase 1: write through the first store instance and drop it.
        {
            let store = TrustyBackedMemoryStore::new(dir.path()).unwrap();
            store
                .insert(
                    Segment::AgentMemory,
                    "rec-1",
                    &vec_with_seed(1.0),
                    payload.clone(),
                )
                .await
                .unwrap();
        }

        // Phase 2: a fresh store rooted at the same path must see the payload.
        let store2 = TrustyBackedMemoryStore::new(dir.path()).unwrap();
        let got = store2.get(Segment::AgentMemory, "rec-1").await.unwrap();
        assert_eq!(
            got,
            Some(payload.clone()),
            "payload must survive process restart"
        );

        // Search must also surface the persisted payload.
        let hits = store2
            .search(Segment::AgentMemory, &vec_with_seed(1.0), 5)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.id == "rec-1" && h.payload == payload),
            "search after reopen should return the persisted payload; got {hits:?}"
        );
    }

    #[tokio::test]
    async fn segments_are_isolated() {
        let dir = tempdir().unwrap();
        let store = TrustyBackedMemoryStore::new(dir.path()).unwrap();

        store
            .insert(
                Segment::AgentMemory,
                "a",
                &vec_with_seed(1.0),
                json!({"seg": "mem"}),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::Context,
                "a",
                &vec_with_seed(1.0),
                json!({"seg": "ctx"}),
            )
            .await
            .unwrap();

        let mem = store.get(Segment::AgentMemory, "a").await.unwrap();
        let ctx = store.get(Segment::Context, "a").await.unwrap();
        assert_eq!(mem, Some(json!({"seg": "mem"})));
        assert_eq!(ctx, Some(json!({"seg": "ctx"})));
    }
}
