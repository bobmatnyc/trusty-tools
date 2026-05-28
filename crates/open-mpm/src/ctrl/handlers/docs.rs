//! `search_docs` CTRL tool — TF-IDF over project documentation (#187).
//!
//! Why: Lets CTRL answer "how does open-mpm work?" questions by scanning the
//! project's own `docs/` tree without an LLM call. The tool relies on a
//! TF-IDF index built at CTRL startup.
//! What: `SearchDocsTool` wraps an `Arc<Mutex<Option<Arc<DocsIndex>>>>` so the
//! background indexer can install the index after the REPL is already running.
//! Test: `search_docs_returns_results_when_index_present`,
//! `search_docs_falls_back_when_index_missing`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// `search_docs(query)` — semantic search over project documentation. (#187)
///
/// Why: Lets CTRL answer "how does open-mpm work?" questions by scanning the
/// project's own `docs/` tree without an LLM call. The tool relies on a
/// TF-IDF index built at CTRL startup.
/// What: Returns top-5 matches as a JSON array of `{path, title, snippet,
/// score}`. Falls back to a graceful message when the index is still
/// building or the docs directory is empty.
/// Test: `search_docs_returns_results_when_index_present` and
/// `search_docs_falls_back_when_index_missing`.
pub(crate) struct SearchDocsTool {
    pub(crate) index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
}

#[async_trait]
impl ToolExecutor for SearchDocsTool {
    fn name(&self) -> &str {
        "search_docs"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_docs",
                "description": "Search project documentation semantically. Use this to answer questions about how open-mpm works, its configuration, agents, skills, and workflows.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Free-text query (e.g. 'how do I write a skill', 'what is the workflow JSON format')."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(q) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("search_docs: missing 'query'");
        };
        let idx = match self.index.lock() {
            Ok(g) => g.clone(),
            Err(_) => return ToolResult::err("search_docs: index lock poisoned"),
        };
        let Some(idx) = idx else {
            return ToolResult::ok(
                "search_docs: docs index not yet built (try again in a moment)".to_string(),
            );
        };
        if idx.is_empty() {
            return ToolResult::ok("search_docs: no documents indexed".to_string());
        }
        let hits = idx.search(q, 5);
        match serde_json::to_string(&hits) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("search_docs: serialize: {e}")),
        }
    }
}
