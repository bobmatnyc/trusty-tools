//! `search_code` — semantic code search over the local vector index.
//!
//! Why: Agents exploring an unfamiliar codebase need semantic (not regex)
//! lookups. Wraps `CodeIndexer` (or a daemon client) so the tool can be
//! injected with a shared index without every caller re-opening the on-disk
//! store.
//! What: `SearchCodeTool` dispatches across `SearchBackend` (Remote daemon >
//! Local indexer > grep fallback) and shapes hits via `super::helpers`.
//! Test: See `super::tests` — real indexer, daemon-empty/error, and grep
//! fallback paths are all covered.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::search::indexer::CodeIndexer;
use crate::search::service_client::SearchDaemonClient;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::{chunk_to_hit_json, grep_fallback_search};

pub(super) const DEFAULT_TOP_N: usize = 5;
pub(super) const DEFAULT_SKILL_TOP_N: usize = 3;
/// Max characters of snippet returned per hit so a single call can't blow the
/// context window.
pub(super) const SNIPPET_MAX_CHARS: usize = 600;
/// Max characters of skill body included in `search_skills` previews.
pub(super) const SKILL_PREVIEW_MAX_CHARS: usize = 400;

/// `search_code` — semantic code search over the local vector index.
///
/// Why: Agents exploring an unfamiliar codebase need semantic (not regex)
/// lookups. Wraps `CodeIndexer` so the tool can be injected with a shared
/// index without every caller re-opening the on-disk store.
/// What: Holds an optional `Arc<CodeIndexer>`. When `Some`, forwards the
/// query to `indexer.search`. When `None`, returns a graceful "unavailable"
/// payload so the agent can proceed.
/// Test: `search_code_returns_hits_from_indexer` runs a real indexer over a
/// mock store + embedder; `search_code_degrades_gracefully` verifies the
/// `None`-backend path.
pub struct SearchCodeTool {
    backend: SearchBackend,
    /// When true, replace the full chunk text with a 7-line snippet
    /// centred on the function start so a single search call can't
    /// blow the context window (#376 C1). Default `true` for the
    /// `Remote` backend (where snippets travel over HTTP), `false`
    /// for `Local`/`None` to preserve existing local behaviour.
    compact: bool,
}

/// Where `SearchCodeTool` actually runs the query.
///
/// Why: The tool started life with only a local in-process index. Issue
/// #374 introduces a daemon variant; modeling the choice as an enum
/// keeps `execute()` a small match instead of two near-duplicate paths
/// and makes the priority order (Remote > Local > None) explicit.
/// What: `Remote` talks HTTP to the search daemon, `Local` calls
/// `CodeIndexer` in-process, `None` falls back to grep over CWD.
/// Test: All three arms are exercised in `super::tests`.
enum SearchBackend {
    None,
    Local(Arc<CodeIndexer>),
    Remote(SearchDaemonClient),
}

impl SearchCodeTool {
    /// Construct without a backend (graceful-degradation grep mode).
    pub fn new() -> Self {
        Self {
            backend: SearchBackend::None,
            compact: false,
        }
    }

    /// Construct with a shared `CodeIndexer` that will service real queries.
    pub fn with_indexer(indexer: Arc<CodeIndexer>) -> Self {
        Self {
            backend: SearchBackend::Local(indexer),
            compact: false,
        }
    }

    /// Construct with a daemon client that will forward queries over HTTP.
    pub fn with_daemon(client: SearchDaemonClient) -> Self {
        Self {
            backend: SearchBackend::Remote(client),
            compact: true,
        }
    }

    /// Override the compact-snippet flag (mostly for tests + advanced
    /// callers that want the full chunk text).
    pub fn with_compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Auto-select the best available backend for `project_root`.
    ///
    /// Why: Callers (the ctrl tool registry) shouldn't have to reach
    /// into pid files themselves. The priority order — daemon, local
    /// indexer, grep — matches the desired UX: zero-cost when the
    /// daemon is up, full-fidelity when it isn't, never an error.
    /// What: Probes the daemon first; on miss, returns a `None`
    /// backend (the caller can later upgrade to `Local` via
    /// `with_indexer`). Returns a future to keep `new_auto` callable
    /// from sync code via `.await`.
    /// Test: `new_auto_returns_none_backend_when_no_daemon`.
    pub async fn new_auto(project_root: &Path) -> Self {
        if let Some(client) = SearchDaemonClient::connect_if_running(project_root).await {
            return Self::with_daemon(client);
        }
        Self::new()
    }

    /// Auto-select with a local fallback when no daemon is running.
    ///
    /// Why: The ctrl tool registry already has an `Arc<CodeIndexer>` it
    /// would otherwise wire directly. Letting it pass that in lets us
    /// prefer the daemon first and only fall back to the local index
    /// when the daemon is absent — without ever dropping to the bare
    /// grep path that was the source of bug #374.
    /// What: Tries the daemon; on miss returns `with_indexer(local)`.
    pub async fn new_auto_with_local(project_root: &Path, local: Arc<CodeIndexer>) -> Self {
        if let Some(client) = SearchDaemonClient::connect_if_running(project_root).await {
            return Self::with_daemon(client);
        }
        Self::with_indexer(local)
    }
}

impl Default for SearchCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for SearchCodeTool {
    fn name(&self) -> &str {
        "search_code"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_code",
                "description": "Semantic code search over the project's local vector index. Returns an array of {path, function_name, start_line, end_line, language, score, snippet} hits.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Natural-language query."},
                        "top_n": {"type": "integer", "description": "Max number of results (default 5)."}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("search_code: missing 'query'");
        };
        let top_n = args
            .get("top_n")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TOP_N);

        // Backend dispatch: prefer daemon, then local indexer, then grep.
        let indexer = match &self.backend {
            SearchBackend::Remote(client) => {
                // Daemon path: forward the query over HTTP. On any error,
                // fall through to grep so the agent always gets *something*.
                match client.search(query, top_n).await {
                    Ok(chunks) if !chunks.is_empty() => {
                        let hits: Vec<Value> = chunks
                            .into_iter()
                            .map(|c| chunk_to_hit_json(&c, self.compact))
                            .collect();
                        let out = json!({
                            "query": query,
                            "hits": hits,
                            "engine": "daemon",
                        });
                        return ToolResult::ok(out.to_string());
                    }
                    Ok(_) => {
                        // Daemon returned empty — fall through to grep below.
                        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let hits = grep_fallback_search(&cwd, query, top_n);
                        let out = json!({
                            "query": query,
                            "hits": hits,
                            "fallback": "grep",
                            "note": "search daemon returned no hits; results from substring grep"
                        });
                        return ToolResult::ok(out.to_string());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "daemon search failed; falling back to grep");
                        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let hits = grep_fallback_search(&cwd, query, top_n);
                        let out = json!({
                            "query": query,
                            "hits": hits,
                            "fallback": "grep",
                            "note": format!("search daemon error ({e}); results from substring grep")
                        });
                        return ToolResult::ok(out.to_string());
                    }
                }
            }
            SearchBackend::Local(indexer) => indexer,
            SearchBackend::None => {
                // Graceful fallback: walk the project directory and substring-match
                // the query against file contents. This isn't as smart as vector
                // search, but it lets the in-process CTRL path (which never
                // initialises a vector index) return useful results instead of an
                // error. See bug #213.
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let hits = grep_fallback_search(&cwd, query, top_n);
                let out = json!({
                    "query": query,
                    "hits": hits,
                    "fallback": "grep",
                    "note": "vector index not available; results from case-insensitive substring grep over project files"
                });
                return ToolResult::ok(out.to_string());
            }
        };

        // Why: Hybrid (vector + BM25 RRF) search is strictly better than
        // either signal alone for code lookups, so we always prefer it when
        // an indexer is wired. Ripgrep is only invoked when hybrid returned
        // zero hits — never as a competing signal — so well-indexed projects
        // never pay the walkdir cost. Pass `expand_graph: true` so KG
        // callers/callees of top-K hits also surface (#376 B1).
        match indexer.search_hybrid(query, top_n, true).await {
            Ok(chunks) if !chunks.is_empty() => {
                let hits: Vec<Value> = chunks
                    .into_iter()
                    .map(|c| chunk_to_hit_json(&c, self.compact))
                    .collect();
                let out = json!({
                    "query": query,
                    "hits": hits,
                    "engine": "hybrid",
                });
                ToolResult::ok(out.to_string())
            }
            Ok(_empty) => {
                // Hybrid returned nothing — fall back to walkdir+grep so the
                // caller still gets something for tokens that aren't in the
                // index (newly-added files, generated code, README content).
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let hits = grep_fallback_search(&cwd, query, top_n);
                let out = json!({
                    "query": query,
                    "hits": hits,
                    "fallback": "ripgrep",
                    "note": "hybrid search returned no hits; results from case-insensitive substring grep"
                });
                ToolResult::ok(out.to_string())
            }
            Err(e) => {
                // Hybrid failed unexpectedly. Try the legacy vector-only
                // path as a safety net before giving up — keeps the tool
                // useful even if the BM25/tokenizer path regresses.
                tracing::warn!(error = %e, "search_hybrid failed; falling back to vector-only search");
                match indexer.search(query, top_n).await {
                    Ok(chunks) => {
                        let hits: Vec<Value> = chunks
                            .into_iter()
                            .map(|c| chunk_to_hit_json(&c, self.compact))
                            .collect();
                        let out = json!({
                            "query": query,
                            "hits": hits,
                            "engine": "vector",
                            "note": "hybrid path errored; fell back to vector-only search",
                        });
                        ToolResult::ok(out.to_string())
                    }
                    Err(e2) => {
                        let out = json!({
                            "error": format!("search_code backend failed: hybrid={e}; vector={e2}"),
                            "query": query,
                            "hits": []
                        });
                        ToolResult::ok(out.to_string())
                    }
                }
            }
        }
    }
}
