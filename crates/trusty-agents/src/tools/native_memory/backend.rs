//! Shared backend bundle for the native memory tools.
//!
//! Why: `store_memory` writes, `retrieve_memory` reads, and `list_memory_keys`
//! enumerate — all three want the same `Arc<dyn MemoryStore>` + `Arc<dyn
//! Embedder>`. Bundling them in one `Arc` keeps the ToolRegistry wiring flat.
//! What: `MemoryBackend` plus the kv-key helpers (`kv_key`, `load_keys`,
//! `save_keys`, `zero_vec`) shared across the three tools.
//! Test: Exercised via the store/retrieve/list tests in `super::tests`.

use std::sync::Arc;

use crate::memory::Embedder;
use crate::memory::store::{MemoryStore, Segment};

/// Prefix applied to user-visible keys so kv entries don't collide with the
/// session / edge rows written by `MemoryGraph`.
const KV_PREFIX: &str = "kv:";
/// Reserved key that stores the JSON-array manifest of live kv keys.
const KV_INDEX_KEY: &str = "kv-index";

/// Backend bundle shared across the three memory tools.
///
/// Why: `store_memory` writes, `retrieve_memory` reads, and `list_memory_keys`
/// enumerates — all three want the same `Arc<dyn MemoryStore>` + `Arc<dyn
/// Embedder>`. Bundling them in one `Arc` keeps the ToolRegistry wiring flat.
/// The optional `session_id` is stamped into every stored payload so memories
/// can later be scoped to the originating session (workflow run / CTRL turn /
/// docs seed). When absent, payloads carry no `session_id` field.
#[derive(Clone)]
pub struct MemoryBackend {
    pub store: Arc<dyn MemoryStore>,
    pub embedder: Arc<dyn Embedder>,
    pub session_id: Option<String>,
}

impl MemoryBackend {
    /// Public constructor so production callers (PM loop / subprocess runner)
    /// can assemble a `MemoryBackend` outside the tools module once the
    /// session/code stores are initialized. Currently only tests invoke it;
    /// `#[allow(dead_code)]` keeps the strict build clean until the wiring
    /// in `main.rs` lands.
    #[allow(dead_code)]
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            store,
            embedder,
            session_id: None,
        }
    }

    /// Builder: attach a session_id that will be stamped into every payload
    /// written through this backend. Used by `StoreMemoryTool` and inspected
    /// by `MemoryRecallTool` to resolve the `"current"` magic filter value.
    #[allow(dead_code)]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub(super) fn zero_vec(&self) -> Vec<f32> {
        vec![0.0; self.embedder.dimension()]
    }

    pub(super) fn kv_key(user_key: &str) -> String {
        format!("{KV_PREFIX}{user_key}")
    }

    /// Read the live kv-key manifest (JSON array of strings).
    pub(super) async fn load_keys(&self) -> anyhow::Result<Vec<String>> {
        let raw = self.store.get(Segment::AgentMemory, KV_INDEX_KEY).await?;
        let Some(v) = raw else {
            return Ok(Vec::new());
        };
        let keys: Vec<String> = serde_json::from_value(v).unwrap_or_default();
        Ok(keys)
    }

    /// Persist the kv-key manifest.
    pub(super) async fn save_keys(&self, keys: &[String]) -> anyhow::Result<()> {
        let vec = self.zero_vec();
        let payload = serde_json::to_value(keys)?;
        self.store
            .insert(Segment::AgentMemory, KV_INDEX_KEY, &vec, payload)
            .await
    }
}
