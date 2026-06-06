//! `memory_search` — hybrid retrieval + consolidation tool (#71).
//!
//! Why: Agents that run across multiple workflow phases benefit from recalling
//! prior research, decisions, and code patterns without the PM having to
//! re-feed them. Combining vector + BM25 retrieval with a cheap consolidation
//! LLM pass gives the agent a compact paragraph to work from instead of 5
//! raw turn snippets.
//! What: `MemorySearchTool` loads `entries.jsonl` + `clusters.jsonl`, embeds
//! the query via OpenRouter, scores with `MemoryRetriever`, passes the top
//! snippets through a cheap haiku-backed consolidation prompt, and saves the
//! consolidated paragraph as a new cluster for future boosts.
//! Test: `memory_search_returns_empty_when_no_history` — offline-safe.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::context::cluster::ClusterStore;
use crate::context::indexer::{IndexedEntry, tokenize};
use crate::context::retrieval::MemoryRetriever;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `memory_search` tool executor — hybrid retrieval + LLM consolidation (#71).
///
/// # Intent
/// Carries the two pieces of runtime state the tool needs without threading
/// them through every call: the on-disk path to the turn/cluster JSONL store
/// and the OpenRouter API key used for query embedding + consolidation.
///
/// Test: `memory_search_returns_empty_when_no_history`.
pub struct MemorySearchTool {
    pub store_dir: PathBuf,
    pub api_key: String,
}

impl MemorySearchTool {
    /// Construct against an explicit store directory (used by the engine and
    /// by tests).
    pub fn new(store_dir: PathBuf, api_key: String) -> Self {
        Self { store_dir, api_key }
    }

    /// Convenience constructor that reads `OPENROUTER_API_KEY` from env and
    /// defaults the store dir to `.trusty-agents/history`.
    #[allow(dead_code)]
    pub fn from_env() -> Self {
        Self::new(
            PathBuf::from(".trusty-agents").join("history"),
            std::env::var("OPENROUTER_API_KEY").unwrap_or_default(),
        )
    }
}

#[async_trait]
impl ToolExecutor for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "memory_search",
                "description": "Search past agent turn history for relevant context. Returns up to 5 consolidated results. Use when you need to recall previous research, decisions, or code patterns.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What you want to recall. Be specific."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if query.is_empty() {
            return ToolResult::err("memory_search: 'query' is required");
        }

        let entries_path = self.store_dir.join("entries.jsonl");
        let entries_content = tokio::fs::read_to_string(&entries_path)
            .await
            .unwrap_or_default();
        let entries: Vec<IndexedEntry> = entries_content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        if entries.is_empty() {
            return ToolResult::ok("No history indexed yet.");
        }

        if self.api_key.is_empty() {
            return ToolResult::ok("memory_search unavailable: OPENROUTER_API_KEY not set");
        }

        let q_embedding = match embed_text(&query, &self.api_key).await {
            Ok(e) => e,
            Err(e) => {
                return ToolResult::err(format!("memory_search embed failed: {e}"));
            }
        };

        let cluster_store = ClusterStore::new(&self.store_dir);
        let clusters = cluster_store.load_all().await;

        let retriever = MemoryRetriever::default();
        let query_terms = tokenize(&query);
        let results = retriever.search(&q_embedding, &query_terms, &entries, &clusters);

        if results.is_empty() {
            return ToolResult::ok("No relevant history found.");
        }

        let snippets: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let body: String = r.entry.turn.response_text.chars().take(500).collect();
                format!(
                    "[{}] Agent: {} | {}\n{}",
                    i + 1,
                    r.entry.turn.agent,
                    r.entry.turn.timestamp.format("%Y-%m-%d %H:%M"),
                    body
                )
            })
            .collect();

        let context_text = snippets.join("\n\n---\n\n");
        let truncated: String = context_text.chars().take(4000).collect();

        // Consolidate via cheap LLM. Falls back to raw snippets on failure.
        let consolidated = consolidate(&query, &truncated, &self.api_key)
            .await
            .unwrap_or_else(|_| context_text.clone());

        // Best-effort cluster save — never fails the tool call.
        if let Ok(c_embedding) = embed_text(&consolidated, &self.api_key).await {
            let _ = cluster_store.save(consolidated.clone(), c_embedding).await;
        }

        ToolResult::ok(format!(
            "## Memory Search Results\n\n**Query:** {query}\n\n{consolidated}"
        ))
    }
}

async fn embed_text(text: &str, api_key: &str) -> anyhow::Result<Vec<f32>> {
    let prefix: String = text.chars().take(8000).collect();
    let client = reqwest::Client::new();
    let resp = client
        .post("https://openrouter.ai/api/v1/embeddings")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": "openai/text-embedding-3-small",
            "input": prefix
        }))
        .send()
        .await?
        .json::<Value>()
        .await?;

    Ok(resp["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no embedding: {resp}"))?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect())
}

async fn consolidate(query: &str, context: &str, api_key: &str) -> anyhow::Result<String> {
    let truncated: String = context.chars().take(4000).collect();
    let client = reqwest::Client::new();
    let resp = client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": "anthropic/claude-haiku-3-5",
            "max_tokens": 512,
            "messages": [
                {
                    "role": "user",
                    "content": format!(
                        "Synthesize the following retrieved memory snippets into a single concise paragraph relevant to: \"{query}\"\n\nSnippets:\n{truncated}"
                    )
                }
            ]
        }))
        .send()
        .await?
        .json::<Value>()
        .await?;

    Ok(resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_search_returns_empty_when_no_history() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = MemorySearchTool::new(tmp.path().to_path_buf(), "".to_string());
        let out = tool.execute(serde_json::json!({"query": "anything"})).await;
        assert!(!out.is_error());
        assert!(out.content().contains("No history"));
    }

    #[tokio::test]
    async fn memory_search_requires_query() {
        let tool = MemorySearchTool::new(PathBuf::from("/tmp/nope"), "".into());
        let out = tool.execute(serde_json::json!({})).await;
        assert!(out.is_error());
        assert!(out.content().contains("query"));
    }

    #[test]
    fn memory_search_schema_names_tool() {
        let tool = MemorySearchTool::new(PathBuf::from("/tmp"), "".into());
        assert_eq!(tool.name(), "memory_search");
        assert_eq!(tool.schema()["function"]["name"], "memory_search");
    }
}
