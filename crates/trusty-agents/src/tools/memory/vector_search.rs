//! `vector_search` tool — semantic search over an agent's bound OKG store or
//! the embedded local code index.
//!
//! Why: Research agents benefit from fuzzy, intent-based lookups; the
//! exact-regex `grep_files` tool complements it but doesn't match
//! paraphrases. Until #3864 this tool could ONLY search a CWD-relative
//! embedded index (`.trusty-agents/state/code/`) — it advertised no way to
//! name a corpus, so a persona whose knowledge lives in a trusty-search index
//! (`cto-assistant`, `bob-kb`) silently searched the wrong thing and returned
//! nothing. The documented `vector_search(index_id=…)` call shape had no
//! implementation behind it; the live workaround was to grant `grep` instead.
//! What: `VectorSearchTool` now takes an `index_id`:
//!   1. explicit `index_id` argument, else
//!   2. the agent's bound OKG store index (`default_index_id`, wired from
//!      `AgentConfig::stores` at registry-build time), else
//!   3. no index → the pre-#3864 behaviour, unchanged.
//! When an index id resolves, the query is routed to the shared trusty-search
//! daemon (`POST /indexes/{id}/search`, the same endpoint the index-aware
//! `grep`/`search` tools use). Every failure — daemon undiscoverable, index
//! missing, transport error — degrades to the embedded index and then to the
//! regex fallback, so the tool never hard-fails an agent turn.
//! Test: See `super::tests` — `vector_search_returns_graceful_error_without_index`,
//! `vector_search_schema_advertises_index_id`,
//! `vector_search_prefers_explicit_index_over_default`,
//! `vector_search_routes_to_daemon_index`,
//! `vector_search_falls_back_when_daemon_index_missing`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{CodeStore, Embedder, FastEmbedder};
use crate::tools::fs_reader::GrepFilesTool;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::recall::{EMBED_DIM, HIT_MAX_CHARS};

/// Per-call timeout for the trusty-search daemon round trip.
///
/// Why: A tool call must feel synchronous inside an agent turn. 5s matches
/// `crate::search::service_client::REQUEST_TIMEOUT`'s budget for the same
/// reason — generous for a cold query, short enough that a wedged daemon
/// falls through to the local path instead of stalling the loop.
const DAEMON_TIMEOUT: Duration = Duration::from_secs(5);

/// Tool: `vector_search` — semantic search over a named index or the
/// embedded local code index.
///
/// Why/What: see the module doc comment.
/// Test: `vector_search_returns_graceful_error_without_index`,
/// `vector_search_routes_to_daemon_index`.
pub struct VectorSearchTool {
    code_dir: PathBuf,
    fallback: Arc<GrepFilesTool>,
    /// The agent's bound OKG store index, used when the caller passes no
    /// explicit `index_id` (#3864). `None` for agents with no `[[stores]]`
    /// binding — those keep the pre-#3864 local-index behaviour exactly.
    default_index_id: Option<String>,
    /// trusty-search daemon base URL override. `None` = discover at call time
    /// via `trusty_common::resolve_daemon_base_url` (tests inject a mock).
    search_base_url: Option<String>,
}

impl VectorSearchTool {
    /// Construct with the default index path (`.trusty-agents/state/code/`)
    /// and no bound store.
    ///
    /// Why: Matches the indexer's default output location so the tool finds
    /// the embedded code without requiring explicit config. Agents with no
    /// OKG store binding behave exactly as they did before #3864.
    /// What: Plain struct literal with the bundled `GrepFilesTool` fallback.
    /// Test: `vector_search_returns_graceful_error_without_index`.
    pub fn new() -> Self {
        Self {
            code_dir: PathBuf::from(".trusty-agents").join("state").join("code"),
            fallback: Arc::new(GrepFilesTool::new()),
            default_index_id: None,
            search_base_url: None,
        }
    }

    /// Bind this tool to the agent's own OKG store index (#3864).
    ///
    /// Why: An unqualified `vector_search(query)` from a persona with a bound
    /// store must search THAT persona's knowledge. Passing the id in at
    /// registry-build time (where `AgentConfig` is in scope) keeps the tool
    /// itself free of config loading.
    /// What: Sets `default_index_id`; a blank/whitespace id is treated as
    /// absent so a malformed binding can't produce a `/indexes//search` URL.
    /// Test: `vector_search_routes_to_daemon_index`,
    /// `vector_search_prefers_explicit_index_over_default`.
    pub fn with_default_index(mut self, index_id: Option<String>) -> Self {
        self.default_index_id = index_id.filter(|s| !s.trim().is_empty());
        self
    }

    /// Override the on-disk index location (used by tests).
    #[allow(dead_code)]
    pub fn with_code_dir(mut self, path: PathBuf) -> Self {
        self.code_dir = path;
        self
    }

    /// Override the trusty-search daemon base URL (used by tests).
    #[allow(dead_code)]
    pub fn with_search_base_url(mut self, base: Option<String>) -> Self {
        self.search_base_url = base;
        self
    }

    /// Path accessor used by tests.
    #[allow(dead_code)]
    pub fn code_dir(&self) -> &Path {
        &self.code_dir
    }

    /// The index this call should target: explicit argument first, then the
    /// agent's bound store, then none.
    ///
    /// Why: This precedence IS the #3864 contract — an unqualified search
    /// hits the agent's own knowledge, and an explicit `index_id` always
    /// wins so cross-index lookups stay possible.
    /// What: Trims and discards blank strings at both levels.
    /// Test: `vector_search_prefers_explicit_index_over_default`,
    /// `vector_search_index_defaults_to_bound_store`,
    /// `vector_search_index_none_without_binding`.
    pub fn effective_index_id(&self, args: &Value) -> Option<String> {
        args.get("index_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.default_index_id.clone())
    }
}

impl Default for VectorSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for VectorSearchTool {
    fn name(&self) -> &str {
        "vector_search"
    }

    fn schema(&self) -> Value {
        // #3864: `index_id`'s description names the agent's bound default
        // explicitly when there is one, so the model can see which corpus an
        // unqualified call will actually search rather than guessing.
        let index_desc = match &self.default_index_id {
            Some(id) => format!(
                "trusty-search index to search. Defaults to this agent's bound OKG store index \
                 (`{id}`) when omitted. Pass an explicit id to search a different corpus."
            ),
            None => "trusty-search index to search. When omitted, searches the local embedded \
                     project code index."
                .to_string(),
        };
        json!({
            "type": "function",
            "function": {
                "name": "vector_search",
                "description": "Semantic search over an indexed corpus (trusty-search index, or the local embedded project code index). Falls back to regex search when no index is available. Returns JSON array of hits with path and snippet.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language or keyword query describing what to find."
                        },
                        "index_id": {
                            "type": "string",
                            "description": index_desc
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of hits to return (default 5).",
                            "minimum": 1,
                            "maximum": 50
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("vector_search: missing required 'query' string");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 50) as usize)
            .unwrap_or(5);

        // #3864 fast path: a named trusty-search index (explicit arg, or the
        // agent's bound OKG store). Any failure falls through to the
        // pre-existing local paths rather than erroring the turn.
        if let Some(index_id) = self.effective_index_id(&args) {
            match self.daemon_query(&index_id, query, limit).await {
                Ok(payload) => return ToolResult::ok(payload),
                Err(e) => tracing::warn!(
                    error = %e,
                    index_id = %index_id,
                    "vector_search: trusty-search index query failed; falling back to local index"
                ),
            }
        }

        // Fast path: embedded index available.
        if self.code_dir.exists() {
            match semantic_query(&self.code_dir, query, limit).await {
                Ok(payload) => return ToolResult::ok(payload),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        code_dir = %self.code_dir.display(),
                        "vector_search: semantic query failed; falling back to grep"
                    );
                    // Fall through to the grep fallback.
                }
            }
        }

        // Fallback: delegate to grep_files, but dress the result in the same
        // JSON envelope so the agent sees a consistent shape.
        let grep_args = json!({
            "pattern": query,
            "max_results": limit,
        });
        let r = self.fallback.execute(grep_args).await;
        let body = r.content().to_string();
        ToolResult::ok(format!(
            "{{\"mode\":\"grep_fallback\",\"reason\":\"no vector index at {}\",\"results\":{}}}",
            self.code_dir.display(),
            serde_json::to_string(&body).unwrap_or_else(|_| "\"\"".to_string())
        ))
    }
}

impl VectorSearchTool {
    /// Query a named index on the shared trusty-search daemon.
    ///
    /// Why: This is the routing #3864 asked for. `POST /indexes/{id}/search`
    /// with `{text, top_k}` is the daemon's own contract (see
    /// `trusty_common::monitor::search_client::SearchClient::search`) — going
    /// straight to it keeps this tool independent of whether the trusty-search
    /// MCP plugin happens to be spawned.
    /// What: Returns the same `[{path, score, snippet}]` envelope the local
    /// path emits, so the model sees ONE result shape regardless of which
    /// backend answered. Errors (undiscoverable daemon, non-2xx, transport)
    /// propagate to the caller, which falls back rather than failing.
    /// Test: `vector_search_routes_to_daemon_index`,
    /// `vector_search_falls_back_when_daemon_index_missing`.
    async fn daemon_query(
        &self,
        index_id: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<String> {
        let base = match &self.search_base_url {
            Some(b) => b.clone(),
            None => trusty_common::resolve_daemon_base_url("trusty-search")
                .ok_or_else(|| anyhow::anyhow!("trusty-search daemon not discoverable"))?,
        };
        let url = format!("{}/indexes/{}/search", base.trim_end_matches('/'), index_id);
        let client = reqwest::Client::builder().timeout(DAEMON_TIMEOUT).build()?;
        let resp = client
            .post(&url)
            .json(&json!({ "text": query, "top_k": limit }))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("trusty-search returned HTTP {} for {url}", resp.status());
        }
        let body: Value = resp.json().await?;
        Ok(serde_json::to_string(&normalize_daemon_hits(&body, limit))?)
    }
}

/// Reshape the daemon's search response into this tool's hit envelope.
///
/// Why: The daemon wraps hits under `results`/`hits` depending on the route
/// version and names the text field `content`/`snippet`/`text`; collapsing
/// that here means the model always sees `{path, score, snippet}` — the same
/// shape `semantic_query` produces for the local index.
/// What: Accepts either a bare array or an object with a `results`/`hits`
/// array; truncates snippets to `HIT_MAX_CHARS`; caps at `limit`.
/// Test: `normalize_daemon_hits_handles_wrapped_and_bare_arrays`.
fn normalize_daemon_hits(body: &Value, limit: usize) -> Vec<Value> {
    let arr = body
        .as_array()
        .or_else(|| body.get("results").and_then(Value::as_array))
        .or_else(|| body.get("hits").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    arr.into_iter()
        .take(limit)
        .map(|h| {
            let path = h
                .get("path")
                .or_else(|| h.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let snippet_raw = h
                .get("content")
                .or_else(|| h.get("snippet"))
                .or_else(|| h.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| h.to_string());
            let snippet = snippet_raw.chars().take(HIT_MAX_CHARS).collect::<String>();
            json!({
                "path": path,
                "score": h.get("score").cloned().unwrap_or(Value::Null),
                "snippet": snippet,
            })
        })
        .collect()
}

/// Run a semantic query against the on-disk `CodeStore`.
///
/// Why: Keeping the embedding + search plumbing in one helper means the
/// tool's `execute` path stays small and easy to read.
/// What: Opens `CodeStore` at `code_dir`, embeds the query, returns up to
/// `limit` hits as a JSON array `[{path, score, snippet}, ...]`.
/// Test: Covered via the `vector_search` tool's graceful-error test today;
/// a populated-index test is future work.
async fn semantic_query(code_dir: &Path, query: &str, limit: usize) -> anyhow::Result<String> {
    let store = CodeStore::open(code_dir, EMBED_DIM)?;
    let embedder = FastEmbedder::new()?;
    let vec = embedder.embed_single(query)?;
    let hits = store.search(Segment::CodeIndex, &vec, limit).await?;

    let out: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            // Payload shape depends on the indexer — pull common fields if
            // present, otherwise stringify the whole payload as snippet.
            let path = h.payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let snippet_raw = h
                .payload
                .get("content")
                .or_else(|| h.payload.get("snippet"))
                .or_else(|| h.payload.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| h.payload.to_string());
            let snippet = snippet_raw.chars().take(HIT_MAX_CHARS).collect::<String>();
            json!({
                "id": h.id,
                "path": path,
                "score": h.score,
                "snippet": snippet,
            })
        })
        .collect();

    Ok(serde_json::to_string(&out)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_daemon_hits_handles_wrapped_and_bare_arrays() {
        let bare = json!([{ "path": "a.rs", "score": 0.9, "content": "fn main() {}" }]);
        let out = normalize_daemon_hits(&bare, 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["path"], "a.rs");
        assert_eq!(out[0]["snippet"], "fn main() {}");

        let wrapped = json!({ "results": [{ "path": "b.rs", "snippet": "x" }] });
        assert_eq!(normalize_daemon_hits(&wrapped, 5)[0]["path"], "b.rs");

        let hits_key = json!({ "hits": [{ "file_path": "c.rs", "text": "y" }] });
        let out = normalize_daemon_hits(&hits_key, 5);
        assert_eq!(out[0]["path"], "c.rs");
        assert_eq!(out[0]["snippet"], "y");
    }

    #[test]
    fn normalize_daemon_hits_respects_limit_and_missing_fields() {
        let body = json!([{"path": "a"}, {"path": "b"}, {"path": "c"}]);
        assert_eq!(normalize_daemon_hits(&body, 2).len(), 2);
        assert_eq!(normalize_daemon_hits(&json!({"other": 1}), 5).len(), 0);
    }
}
