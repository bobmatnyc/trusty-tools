//! In-session memory store/recall tools.
//!
//! Why: CTRL is a top-level REPL — we don't want memory ops to hard-fail when
//! the user hasn't set up the embedded redb+usearch store. A simple shared Vec
//! of strings is good enough for the current session and stays small.
//! What: `MemoryStoreTool` appends a string; `MemoryRecallTool` filters by
//! substring. Both share an `Arc<Mutex<Vec<String>>>` owned by `Ctrl`.
//! Test: Covered indirectly by ctrl integration tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// `memory_store(content)` — append to the in-session memory vec.
pub(crate) struct MemoryStoreTool {
    pub(crate) memory: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "memory_store",
                "description": "Store a piece of content for later recall (in-memory fallback).",
                "parameters": {
                    "type": "object",
                    "properties": { "content": { "type": "string" } },
                    "required": ["content"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::err("memory_store: missing 'content'");
        };
        match self.memory.lock() {
            Ok(mut m) => {
                m.push(content.to_string());
                ToolResult::ok("stored")
            }
            Err(e) => ToolResult::err(format!("memory_store: lock poisoned: {e}")),
        }
    }
}

/// `memory_recall(query)` — naive substring match over the in-session vec.
pub(crate) struct MemoryRecallTool {
    pub(crate) memory: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "memory_recall",
                "description": "Recall previously stored content by substring (in-memory fallback).",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let q = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let mem = match self.memory.lock() {
            Ok(m) => m.clone(),
            Err(e) => return ToolResult::err(format!("memory_recall: lock poisoned: {e}")),
        };
        let hits: Vec<&String> = mem
            .iter()
            .filter(|s| q.is_empty() || s.to_lowercase().contains(&q))
            .collect();
        match serde_json::to_string(&hits) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("memory_recall: serialize: {e}")),
        }
    }
}
