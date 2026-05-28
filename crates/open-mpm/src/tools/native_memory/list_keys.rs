//! `list_memory_keys` — enumerate available memory keys, optionally filtered.
//!
//! Why: Agents need to discover which kv entries exist without a semantic
//! search, e.g. to decide whether to overwrite or read a known key.
//! What: `ListMemoryKeysTool` reads the kv-index manifest and returns the keys
//! (optionally filtered by prefix). Degrades gracefully without a backend.
//! Test: See `super::tests` — enumeration, prefix-filter, and
//! graceful-degradation cases.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

use super::backend::MemoryBackend;

/// `list_memory_keys` — enumerate available memory keys, optionally filtered.
pub struct ListMemoryKeysTool {
    backend: Option<MemoryBackend>,
}

impl ListMemoryKeysTool {
    pub fn new() -> Self {
        Self { backend: None }
    }

    pub fn with_backend(backend: MemoryBackend) -> Self {
        Self {
            backend: Some(backend),
        }
    }
}

impl Default for ListMemoryKeysTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for ListMemoryKeysTool {
    fn name(&self) -> &str {
        "list_memory_keys"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_memory_keys",
                "description": "List memory keys, optionally filtered by a prefix.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prefix": {"type": "string", "description": "Optional key prefix filter."}
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let prefix = args
            .get("prefix")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let Some(backend) = self.backend.as_ref() else {
            let empty: Vec<String> = Vec::new();
            let out = json!({
                "keys": empty,
                "prefix": prefix,
                "error": "memory store not available — ListMemoryKeysTool was constructed without a MemoryBackend."
            });
            return ToolResult::ok(out.to_string());
        };

        match backend.load_keys().await {
            Ok(keys) => {
                let filtered: Vec<String> = keys
                    .into_iter()
                    .filter(|k| k.starts_with(&prefix))
                    .collect();
                let out = json!({
                    "keys": filtered,
                    "prefix": prefix,
                });
                ToolResult::ok(out.to_string())
            }
            Err(e) => {
                let empty: Vec<String> = Vec::new();
                let out = json!({
                    "keys": empty,
                    "prefix": prefix,
                    "error": format!("list failed: {e}")
                });
                ToolResult::ok(out.to_string())
            }
        }
    }
}
