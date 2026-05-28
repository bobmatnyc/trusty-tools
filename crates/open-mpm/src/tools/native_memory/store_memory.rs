//! `store_memory` — persist a memory entry keyed by a caller-chosen string.
//!
//! Why: Agents need durable key/value memory that survives process restarts
//! and is attributable to its origin (session/agent) for later scoped recall.
//! What: `StoreMemoryTool` embeds the content, writes it under a `kv:` prefix
//! in the chosen segment, merges identity auto-tags, and keeps the kv-index
//! manifest current. Degrades gracefully without a backend.
//! Test: See `super::tests` — happy-path, missing-input, identity-tag,
//! segment-routing, and graceful-degradation cases.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::identity::CallerIdentity;
use crate::memory::store::Segment;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::backend::MemoryBackend;

/// `store_memory` — persist a memory entry keyed by a caller-chosen string.
pub struct StoreMemoryTool {
    backend: Option<MemoryBackend>,
    /// Test-only identity override. When `None`, the tool reads
    /// `CallerIdentity::from_env()` at execute time. (#193)
    identity_override: Option<CallerIdentity>,
}

impl StoreMemoryTool {
    pub fn new() -> Self {
        Self {
            backend: None,
            identity_override: None,
        }
    }

    pub fn with_backend(backend: MemoryBackend) -> Self {
        Self {
            backend: Some(backend),
            identity_override: None,
        }
    }

    /// Pin an explicit identity (test-only).
    #[allow(dead_code)]
    pub fn with_identity(mut self, identity: Option<CallerIdentity>) -> Self {
        self.identity_override = identity;
        self
    }
}

impl Default for StoreMemoryTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for StoreMemoryTool {
    fn name(&self) -> &str {
        "store_memory"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "store_memory",
                "description": "Persist a memory entry keyed by a caller-chosen string. Idempotent (re-store overwrites). Optional `tag` selects a node in the memory taxonomy tree (default `memories/session`); use sub-tags like `memories/decision` or `memories/observation` to classify.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "key": {"type": "string"},
                        "content": {"type": "string"},
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "tag": {
                            "type": "string",
                            "description": "Hierarchical taxonomy tag (default `memories/session`). Examples: `memories/session`, `memories/observation`, `memories/decision`."
                        },
                        "segment": {
                            "type": "string",
                            "description": "Memory tier to write into: context (stable architecture facts), brief (active goals), history (past decisions), or agent_memory (default catch-all).",
                            "enum": ["context", "brief", "history", "agent_memory"]
                        }
                    },
                    "required": ["key", "content"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(key) = args.get("key").and_then(Value::as_str) else {
            return ToolResult::err("store_memory: missing 'key'");
        };
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::err("store_memory: missing 'content'");
        };
        let mut tags: Vec<String> = args
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // #193: Auto-merge caller-identity tags so future scope filters can
        // reliably attribute the memory to its origin. Agents writing without
        // these tags would otherwise be invisible to their own scope-Agent
        // recall queries. Identity is read from the env-var bridge populated
        // by the harness at spawn time; callers that don't set OPEN_MPM_CALLER
        // (legacy/tests) get no auto-tags so behavior is unchanged.
        let identity = self
            .identity_override
            .clone()
            .or_else(CallerIdentity::from_env);
        if let Some(id) = identity.as_ref() {
            for t in id.auto_tags() {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
        // Hierarchical taxonomy tag (default `memories/session`). Agents can
        // pass subtags like `memories/decision` to classify within the tree.
        let tag = args
            .get("tag")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("memories/session")
            .to_string();

        // #277: optional `segment` arg routes the write to a specific memory
        // tier (Context / Brief / History / AgentMemory). Unknown values fall
        // back to `AgentMemory` so a typo doesn't fail the tool call. The
        // kv-index manifest stays on `AgentMemory` regardless so
        // `list_memory_keys` / `retrieve_memory` continue to work.
        let target_segment = args
            .get("segment")
            .and_then(Value::as_str)
            .and_then(Segment::from_name)
            .unwrap_or(Segment::AgentMemory);

        let Some(backend) = self.backend.as_ref() else {
            let out = json!({
                "stored": false,
                "key": key,
                "error": "memory store not available — StoreMemoryTool was constructed without a MemoryBackend."
            });
            return ToolResult::ok(out.to_string());
        };

        // Embed the content so later semantic searches can surface it.
        let vec = match backend.embedder.embed_single(content) {
            Ok(v) => v,
            Err(e) => {
                let out = json!({
                    "stored": false,
                    "key": key,
                    "error": format!("embed failed: {e}")
                });
                return ToolResult::ok(out.to_string());
            }
        };
        let storage_key = MemoryBackend::kv_key(key);
        let mut payload = json!({
            "key": key,
            "content": content,
            "tags": tags,
            "tag": tag,
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        if let Some(sid) = backend.session_id.as_deref() {
            payload["session_id"] = json!(sid);
        }
        if let Err(e) = backend
            .store
            .insert(target_segment, &storage_key, &vec, payload)
            .await
        {
            let out = json!({
                "stored": false,
                "key": key,
                "error": format!("insert failed: {e}")
            });
            return ToolResult::ok(out.to_string());
        }

        // Update the manifest so list_memory_keys can enumerate this key.
        let mut keys = backend.load_keys().await.unwrap_or_default();
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
            if let Err(e) = backend.save_keys(&keys).await {
                // Don't fail the write — surface the degradation to the caller
                // so they know list_memory_keys may be stale.
                let out = json!({
                    "stored": true,
                    "key": key,
                    "warning": format!("key saved but manifest update failed: {e}")
                });
                return ToolResult::ok(out.to_string());
            }
        }

        let out = json!({
            "stored": true,
            "key": key,
        });
        ToolResult::ok(out.to_string())
    }
}
