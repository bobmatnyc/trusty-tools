use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{ListMemoryKeysTool, MemoryBackend, RetrieveMemoryTool, StoreMemoryTool};
use crate::memory::Embedder;
use crate::memory::store::{MemoryResult, MemoryStore, Segment};
use crate::tools::traits::ToolExecutor;

// Mock store that behaves as a simple key-value map (vector ignored).
struct KvMockStore {
    inner: Mutex<HashMap<String, Value>>,
}

impl KvMockStore {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl MemoryStore for KvMockStore {
    async fn insert(
        &self,
        _segment: Segment,
        id: &str,
        _vector: &[f32],
        payload: Value,
    ) -> anyhow::Result<()> {
        self.inner.lock().unwrap().insert(id.to_string(), payload);
        Ok(())
    }

    async fn search(
        &self,
        _segment: Segment,
        _query_vec: &[f32],
        _top_k: usize,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        Ok(Vec::new())
    }

    async fn get(&self, _segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
        Ok(self.inner.lock().unwrap().get(id).cloned())
    }

    async fn delete(&self, _segment: Segment, id: &str) -> anyhow::Result<()> {
        self.inner.lock().unwrap().remove(id);
        Ok(())
    }
}

struct FixedEmbedder;
impl Embedder for FixedEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
    }
    fn embed_single(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.1_f32; 4])
    }
    fn dimension(&self) -> usize {
        4
    }
}

fn make_backend() -> MemoryBackend {
    let store: Arc<dyn MemoryStore> = Arc::new(KvMockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    MemoryBackend::new(store, embedder)
}

// ------- store_memory -------

#[tokio::test]
async fn store_memory_missing_key_errors() {
    let t = StoreMemoryTool::new();
    assert!(t.execute(json!({"content": "x"})).await.is_error());
}

#[tokio::test]
async fn store_memory_missing_content_errors() {
    let t = StoreMemoryTool::new();
    assert!(t.execute(json!({"key": "k"})).await.is_error());
}

#[tokio::test]
async fn store_memory_degrades_gracefully() {
    let t = StoreMemoryTool::new();
    let out = t.execute(json!({"key": "k", "content": "v"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["stored"], false);
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn store_memory_writes_to_backend() {
    let backend = make_backend();
    let t = StoreMemoryTool::with_backend(backend.clone());
    let out = t
        .execute(json!({"key": "hello", "content": "world", "tags": ["greeting"]}))
        .await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["stored"], true);
    assert_eq!(v["key"], "hello");

    // Directly verify the payload landed under the kv: prefix.
    let got = backend
        .store
        .get(Segment::AgentMemory, "kv:hello")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["content"], "world");
    assert_eq!(got["tags"][0], "greeting");
}

#[tokio::test]
async fn store_memory_stamps_session_id_when_set() {
    // When the backend has session_id configured, every stored payload
    // must include that session_id so future recall can filter by it.
    let backend = make_backend().with_session_id("session-xyz");
    let t = StoreMemoryTool::with_backend(backend.clone());
    t.execute(json!({"key": "k1", "content": "v1"})).await;

    let payload = backend
        .store
        .get(Segment::AgentMemory, "kv:k1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some("session-xyz")
    );
    assert_eq!(
        payload.get("tag").and_then(|v| v.as_str()),
        Some("memories/session")
    );
    assert!(payload.get("created_at").and_then(|v| v.as_str()).is_some());
}

#[tokio::test]
async fn store_memory_accepts_custom_taxonomy_tag() {
    // Agents can override the default `memories/session` with a sub-tag.
    let backend = make_backend();
    let t = StoreMemoryTool::with_backend(backend.clone());
    t.execute(json!({
        "key": "decision-1",
        "content": "use redb for storage",
        "tag": "memories/decision",
    }))
    .await;

    let payload = backend
        .store
        .get(Segment::AgentMemory, "kv:decision-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        payload.get("tag").and_then(|v| v.as_str()),
        Some("memories/decision")
    );
}

#[tokio::test]
async fn store_memory_auto_tags_from_identity() {
    // #193: When the caller is an Agent, `store_memory` must merge the
    // identity's auto_tags (session/, project/, agent/) into the stored
    // payload. Tags supplied by the caller are preserved.
    let backend = make_backend();
    let identity = crate::identity::CallerIdentity::Agent {
        session_id: "sess-7".into(),
        project_id: "myproj".into(),
        agent_id: "research-agent".into(),
    };
    let t = StoreMemoryTool::with_backend(backend.clone()).with_identity(Some(identity));
    t.execute(json!({"key": "k1", "content": "c1", "tags": ["mine"]}))
        .await;

    let payload = backend
        .store
        .get(Segment::AgentMemory, "kv:k1")
        .await
        .unwrap()
        .unwrap();
    let tags: Vec<String> = serde_json::from_value(payload["tags"].clone()).unwrap();
    assert!(tags.contains(&"mine".to_string()));
    assert!(tags.contains(&"session/sess-7".to_string()));
    assert!(tags.contains(&"project/myproj".to_string()));
    assert!(tags.contains(&"agent/research-agent".to_string()));
}

#[tokio::test]
async fn store_memory_auto_tags_for_ctrl_identity() {
    // CTRL identity contributes a single `scope/user` tag.
    let backend = make_backend();
    let t = StoreMemoryTool::with_backend(backend.clone())
        .with_identity(Some(crate::identity::CallerIdentity::Ctrl));
    t.execute(json!({"key": "k1", "content": "c1"})).await;

    let payload = backend
        .store
        .get(Segment::AgentMemory, "kv:k1")
        .await
        .unwrap()
        .unwrap();
    let tags: Vec<String> = serde_json::from_value(payload["tags"].clone()).unwrap();
    assert!(tags.contains(&"scope/user".to_string()));
}

#[tokio::test]
async fn store_memory_omits_session_id_when_unset() {
    // Without a session_id on the backend, payloads should not carry one
    // (avoids accidentally tagging memories with empty strings).
    let backend = make_backend();
    let t = StoreMemoryTool::with_backend(backend.clone());
    t.execute(json!({"key": "k1", "content": "v1"})).await;

    let payload = backend
        .store
        .get(Segment::AgentMemory, "kv:k1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        payload.get("session_id").is_none(),
        "no session_id should be stamped when backend has none"
    );
}

// ------- retrieve_memory -------

#[tokio::test]
async fn retrieve_memory_missing_key_errors() {
    let t = RetrieveMemoryTool::new();
    assert!(t.execute(json!({})).await.is_error());
}

#[tokio::test]
async fn retrieve_memory_degrades_gracefully() {
    let t = RetrieveMemoryTool::new();
    let out = t.execute(json!({"key": "k"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["key"], "k");
    assert!(v["content"].is_null());
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn retrieve_memory_returns_stored_value() {
    let backend = make_backend();
    let store_tool = StoreMemoryTool::with_backend(backend.clone());
    store_tool
        .execute(json!({"key": "color", "content": "blue"}))
        .await;

    let retrieve = RetrieveMemoryTool::with_backend(backend);
    let out = retrieve.execute(json!({"key": "color"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["key"], "color");
    assert_eq!(v["content"], "blue");
}

#[tokio::test]
async fn retrieve_memory_returns_null_for_unknown_key() {
    let backend = make_backend();
    let retrieve = RetrieveMemoryTool::with_backend(backend);
    let out = retrieve.execute(json!({"key": "missing"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["key"], "missing");
    assert!(v["content"].is_null());
}

// ------- list_memory_keys -------

#[tokio::test]
async fn list_memory_keys_degrades_gracefully() {
    let t = ListMemoryKeysTool::new();
    let out = t.execute(json!({})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert!(v["keys"].is_array());
    assert_eq!(v["keys"].as_array().unwrap().len(), 0);
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn list_memory_keys_enumerates_stored_keys() {
    let backend = make_backend();
    let store_tool = StoreMemoryTool::with_backend(backend.clone());
    store_tool
        .execute(json!({"key": "alpha", "content": "a"}))
        .await;
    store_tool
        .execute(json!({"key": "beta", "content": "b"}))
        .await;
    store_tool
        .execute(json!({"key": "alchemy", "content": "c"}))
        .await;

    let list = ListMemoryKeysTool::with_backend(backend);
    let out = list.execute(json!({})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    let keys: Vec<String> = serde_json::from_value(v["keys"].clone()).unwrap();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"alpha".to_string()));
    assert!(keys.contains(&"beta".to_string()));
    assert!(keys.contains(&"alchemy".to_string()));
}

#[tokio::test]
async fn list_memory_keys_filters_by_prefix() {
    let backend = make_backend();
    let store_tool = StoreMemoryTool::with_backend(backend.clone());
    store_tool
        .execute(json!({"key": "alpha", "content": "a"}))
        .await;
    store_tool
        .execute(json!({"key": "beta", "content": "b"}))
        .await;
    store_tool
        .execute(json!({"key": "alchemy", "content": "c"}))
        .await;

    let list = ListMemoryKeysTool::with_backend(backend);
    let out = list.execute(json!({"prefix": "al"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    let keys: Vec<String> = serde_json::from_value(v["keys"].clone()).unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|k| k.starts_with("al")));
    assert_eq!(v["prefix"], "al");
}

// ------- name/schema smoke tests -------

/// Segment-tracking mock store (#277). Unlike `KvMockStore`, this records
/// the `Segment` each insert lands in so tests can verify routing.
struct SegmentTrackingStore {
    // (segment, id) -> payload
    inner: Mutex<HashMap<(Segment, String), Value>>,
}

impl SegmentTrackingStore {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl MemoryStore for SegmentTrackingStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        _vector: &[f32],
        payload: Value,
    ) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert((segment, id.to_string()), payload);
        Ok(())
    }
    async fn search(
        &self,
        _segment: Segment,
        _q: &[f32],
        _k: usize,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        Ok(Vec::new())
    }
    async fn get(&self, segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(segment, id.to_string()))
            .cloned())
    }
    async fn delete(&self, segment: Segment, id: &str) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .remove(&(segment, id.to_string()));
        Ok(())
    }
}

fn make_segment_backend() -> (MemoryBackend, Arc<SegmentTrackingStore>) {
    let store = Arc::new(SegmentTrackingStore::new());
    let store_dyn: Arc<dyn MemoryStore> = store.clone();
    let embedder: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    (MemoryBackend::new(store_dyn, embedder), store)
}

/// #277: No `segment` arg → write lands in `AgentMemory` (legacy default).
#[tokio::test]
async fn store_memory_defaults_to_agent_memory_segment() {
    let (backend, raw) = make_segment_backend();
    let t = StoreMemoryTool::with_backend(backend);
    t.execute(json!({"key": "k1", "content": "v1"})).await;

    let map = raw.inner.lock().unwrap();
    assert!(
        map.contains_key(&(Segment::AgentMemory, "kv:k1".to_string())),
        "default segment must be AgentMemory; got keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
}

/// #277: `segment: "context"` routes the write to `Segment::Context`.
#[tokio::test]
async fn store_memory_routes_to_context_segment() {
    let (backend, raw) = make_segment_backend();
    let t = StoreMemoryTool::with_backend(backend);
    t.execute(json!({
        "key": "arch-1",
        "content": "redb is the storage engine",
        "segment": "context"
    }))
    .await;

    let map = raw.inner.lock().unwrap();
    assert!(
        map.contains_key(&(Segment::Context, "kv:arch-1".to_string())),
        "segment=context should route to Context; got keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
}

/// #277: `segment: "brief"` routes to `Segment::Brief`.
#[tokio::test]
async fn store_memory_routes_to_brief_segment() {
    let (backend, raw) = make_segment_backend();
    let t = StoreMemoryTool::with_backend(backend);
    t.execute(json!({
        "key": "goal-1",
        "content": "ship #277",
        "segment": "brief"
    }))
    .await;

    let map = raw.inner.lock().unwrap();
    assert!(
        map.contains_key(&(Segment::Brief, "kv:goal-1".to_string())),
        "segment=brief should route to Brief; got keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
}

/// #277: Unknown segment value falls back to `AgentMemory` (graceful).
#[tokio::test]
async fn store_memory_unknown_segment_falls_back_to_agent_memory() {
    let (backend, raw) = make_segment_backend();
    let t = StoreMemoryTool::with_backend(backend);
    let out = t
        .execute(json!({
            "key": "k1",
            "content": "v1",
            "segment": "no_such_tier"
        }))
        .await;
    assert!(!out.is_error());

    let map = raw.inner.lock().unwrap();
    assert!(
        map.contains_key(&(Segment::AgentMemory, "kv:k1".to_string())),
        "unknown segment should fall back to AgentMemory; got keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
}

#[test]
fn store_memory_schema_names_tool() {
    let t = StoreMemoryTool::new();
    assert_eq!(t.name(), "store_memory");
    assert_eq!(t.schema()["function"]["name"], "store_memory");
}

#[test]
fn retrieve_memory_schema_names_tool() {
    let t = RetrieveMemoryTool::new();
    assert_eq!(t.name(), "retrieve_memory");
    assert_eq!(t.schema()["function"]["name"], "retrieve_memory");
}

#[test]
fn list_memory_keys_schema_names_tool() {
    let t = ListMemoryKeysTool::new();
    assert_eq!(t.name(), "list_memory_keys");
    assert_eq!(t.schema()["function"]["name"], "list_memory_keys");
}
