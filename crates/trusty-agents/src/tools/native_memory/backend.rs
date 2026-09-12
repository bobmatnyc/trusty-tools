//! Shared backend bundle for the native memory tools.
//!
//! Why: `store_memory` writes, `retrieve_memory` reads, and `list_memory_keys`
//! enumerate — all three want the same `Arc<dyn MemoryStore>` + `Arc<dyn
//! Embedder>`. Bundling them in one `Arc` keeps the ToolRegistry wiring flat.
//! What: `MemoryBackend` plus the kv-key helpers (`kv_key`, `load_keys`,
//! `save_keys`, `zero_vec`) shared across the three tools.
//! Test: Exercised via the store/retrieve/list tests in `super::tests`.

use std::path::Path;
use std::sync::Arc;

use crate::memory::scope::MemoryScope;
use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{Embedder, open_memory_store_for_assistant};
use crate::stores::AgentStoreBinding;

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
    /// The assistant whose palace `store` addresses, when one was resolved
    /// (#7443). `None` is the pre-#7443 single-tenant backend that tests and
    /// the seeders build directly; production wiring goes through
    /// [`open_assistant_memory_backend`], which never produces `None`.
    pub scope: Option<MemoryScope>,
}

/// Build the native memory tools' backend for ONE assistant (#7443).
///
/// Why: `memory_recall`, `store_memory`, `retrieve_memory` and
/// `list_memory_keys` all address `Segment::AgentMemory`, which an unscoped
/// store maps onto one process-global palace. This is the only constructor
/// production code may use, and it FAILS when #7428's resolution produces no
/// palace for `agent` — a missing scope must never degrade into the shared
/// palace, because that degradation is silent and writes another assistant's
/// drawer. The caller's choice is then to run without the memory tools, not to
/// run them against someone else's memory.
/// What: resolves [`MemoryScope::for_assistant`] from the agent name and its
/// `[[stores]]` binding, opens the scoped store under `data_dir`, and bundles
/// it with `embedder`.
/// Test: `two_assistants_never_cross_read`,
/// `an_unresolvable_assistant_gets_no_backend`.
pub fn open_assistant_memory_backend(
    data_dir: &Path,
    agent: &str,
    binding: Option<&AgentStoreBinding>,
    embedder: Arc<dyn Embedder>,
) -> anyhow::Result<MemoryBackend> {
    // #7443: resolution failure is terminal — never a fall-through to the
    // process-global palace.
    let scope = MemoryScope::for_assistant(agent, binding)?;
    let store = open_memory_store_for_assistant(data_dir, &scope)?;
    Ok(MemoryBackend {
        store,
        embedder,
        session_id: None,
        scope: Some(scope),
    })
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
            scope: None,
        }
    }

    /// [`Self::new`] with an explicit assistant scope (#7443).
    ///
    /// Why: a caller that already holds a scoped store — the tests that drive
    /// two assistants through one process — still has to record WHICH assistant
    /// the backend speaks for, so the tools can name it in an error.
    /// Test: `two_assistants_never_cross_read`.
    pub fn for_scope(
        store: Arc<dyn MemoryStore>,
        embedder: Arc<dyn Embedder>,
        scope: MemoryScope,
    ) -> Self {
        Self {
            store,
            embedder,
            session_id: None,
            scope: Some(scope),
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
