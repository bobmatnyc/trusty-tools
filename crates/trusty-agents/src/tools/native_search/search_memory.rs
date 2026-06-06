//! `search_memory` — query the project agent-memory graph.
//!
//! Why: Agents benefit from recall of prior decisions / research across runs.
//! Wraps `MemoryGraph::search` so the tool can share the same graph the PM
//! loop is writing to.
//! What: `SearchMemoryTool` holds an optional `Arc<MemoryGraph>`. When `None`,
//! returns a graceful "unavailable" payload.
//! Test: See `super::tests` — `search_memory_executes_with_graph` and
//! `search_memory_degrades_gracefully`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::memory::graph::MemoryGraph;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::search_code::DEFAULT_TOP_N;

/// `search_memory` — query the project agent-memory graph.
///
/// Why: Agents benefit from recall of prior decisions / research across runs.
/// Wraps `MemoryGraph::search` so the tool can share the same graph the PM
/// loop is writing to.
/// What: Holds an optional `Arc<MemoryGraph>`. When `None`, returns a graceful
/// "unavailable" payload.
/// Test: `search_memory_executes_with_graph` exercises a real graph over a
/// mock store; `search_memory_degrades_gracefully` covers the absent-graph
/// path.
pub struct SearchMemoryTool {
    graph: Option<Arc<MemoryGraph>>,
}

impl SearchMemoryTool {
    /// Construct without a backend (graceful-degradation mode).
    pub fn new() -> Self {
        Self { graph: None }
    }

    /// Construct with a shared `MemoryGraph` for real queries.
    pub fn with_graph(graph: Arc<MemoryGraph>) -> Self {
        Self { graph: Some(graph) }
    }
}

impl Default for SearchMemoryTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for SearchMemoryTool {
    fn name(&self) -> &str {
        "search_memory"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_memory",
                "description": "Query agent memory (stored sessions) for relevant prior turns. Returns {id, score, payload} hits.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "top_n": {"type": "integer", "description": "Default 5."}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("search_memory: missing 'query'");
        };
        let top_n = args
            .get("top_n")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TOP_N);

        let Some(graph) = self.graph.as_ref() else {
            let out = json!({
                "error": "memory graph not available — SearchMemoryTool was constructed without a MemoryGraph backend.",
                "query": query,
                "hits": []
            });
            return ToolResult::ok(out.to_string());
        };

        match graph.search(query, top_n).await {
            Ok(results) => {
                let hits: Vec<Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "score": r.score,
                            "segment": r.segment,
                            "payload": r.payload,
                        })
                    })
                    .collect();
                let out = json!({
                    "query": query,
                    "hits": hits,
                });
                ToolResult::ok(out.to_string())
            }
            Err(e) => {
                let out = json!({
                    "error": format!("search_memory backend failed: {e}"),
                    "query": query,
                    "hits": []
                });
                ToolResult::ok(out.to_string())
            }
        }
    }
}
