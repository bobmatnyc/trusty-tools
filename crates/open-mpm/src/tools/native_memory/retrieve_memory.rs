//! `retrieve_memory` — fetch a memory entry by key.
//!
//! Why: Pairs with `store_memory` so agents can read back durable kv entries
//! by exact key without a semantic search.
//! What: `RetrieveMemoryTool` looks up the `kv:`-prefixed row in
//! `Segment::AgentMemory` and returns its content + tags. Degrades gracefully
//! without a backend and returns null content for unknown keys.
//! Test: See `super::tests` — stored-value, unknown-key, missing-input, and
//! graceful-degradation cases.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::memory::store::Segment;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::backend::MemoryBackend;

/// `retrieve_memory` — fetch a memory entry by key.
pub struct RetrieveMemoryTool {
    backend: Option<MemoryBackend>,
}

impl RetrieveMemoryTool {
    pub fn new() -> Self {
        Self { backend: None }
    }

    pub fn with_backend(backend: MemoryBackend) -> Self {
        Self {
            backend: Some(backend),
        }
    }
}

impl Default for RetrieveMemoryTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for RetrieveMemoryTool {
    fn name(&self) -> &str {
        "retrieve_memory"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "retrieve_memory",
                "description": "Fetch a memory entry by key. Returns null content if not found.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "key": {"type": "string"}
                    },
                    "required": ["key"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(key) = args.get("key").and_then(Value::as_str) else {
            return ToolResult::err("retrieve_memory: missing 'key'");
        };

        let Some(backend) = self.backend.as_ref() else {
            let out = json!({
                "key": key,
                "content": Value::Null,
                "error": "memory store not available — RetrieveMemoryTool was constructed without a MemoryBackend."
            });
            return ToolResult::ok(out.to_string());
        };

        let storage_key = MemoryBackend::kv_key(key);
        match backend.store.get(Segment::AgentMemory, &storage_key).await {
            Ok(Some(payload)) => {
                let out = json!({
                    "key": key,
                    "content": payload.get("content").cloned().unwrap_or(Value::Null),
                    "tags": payload.get("tags").cloned().unwrap_or_else(|| json!([])),
                });
                ToolResult::ok(out.to_string())
            }
            Ok(None) => {
                let out = json!({
                    "key": key,
                    "content": Value::Null,
                });
                ToolResult::ok(out.to_string())
            }
            Err(e) => {
                let out = json!({
                    "key": key,
                    "content": Value::Null,
                    "error": format!("retrieve failed: {e}")
                });
                ToolResult::ok(out.to_string())
            }
        }
    }
}
