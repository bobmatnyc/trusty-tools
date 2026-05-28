//! `memory_recall` and `vector_search` tools — agent-side memory access.
//!
//! Why: #53 — agents need two complementary lookup surfaces: a semantic
//! `memory_recall` that queries the embedded `RedbUsearchStore`
//! (`Segment::AgentMemory`) for previously-stored facts/decisions, and a
//! `vector_search` that queries the local embedded code index at
//! `.open-mpm/code/` (semantic code search). Both are optional — when the
//! underlying store is unavailable, the tool returns a structured payload
//! rather than failing the LLM loop, so agents can gracefully skip the
//! lookup and proceed with the task.
//! What:
//!   - `MemoryRecallTool` embeds the query via `FastEmbedder` and runs HNSW
//!     search against `Segment::AgentMemory` in the injected `MemoryBackend`.
//!     When constructed without a backend it returns a graceful "not
//!     available" JSON payload. (Replaces the legacy `KuzuRecallTool` which
//!     shelled out to a Python `kuzu` interpreter — KùzuDB was archived by
//!     Apple in Oct 2025 and is unmaintained.)
//!   - `VectorSearchTool` tries to open the embedded `CodeStore` at
//!     `.open-mpm/code/`; if absent or unreadable, it falls back to a plain
//!     `grep`-style scan over the working tree via the existing `GrepFilesTool`
//!     so the agent still gets *something* usable.
//! Test: See `tests` submodule — both tools return a graceful error when the
//! underlying store is missing, and both appear in the research-agent registry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::identity::{CallerIdentity, RecallCeiling};
use crate::memory::store::{MemoryStore, Segment};
use crate::memory::{CodeStore, Embedder, FastEmbedder};
use crate::tools::fs_reader::GrepFilesTool;
use crate::tools::native_memory::MemoryBackend;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Dimension of embedded code vectors — mirrors `search::build_file_watcher`.
/// Kept in sync manually; a mismatch here will cause `CodeStore::open` to
/// error, which `VectorSearchTool` surfaces to the agent as a graceful
/// "index unavailable" message.
const EMBED_DIM: usize = 384;

/// Max characters of content returned per hit in `memory_recall` /
/// `vector_search`, so a single call can't blow the context window.
const HIT_MAX_CHARS: usize = 600;

/// Tool: `memory_recall` — semantic search over the embedded agent memory store.
///
/// Why: Agents need to ask the project's own memory for facts previously
/// stored (architecture decisions, APIs, conventions). The harness already
/// runs an embedded `RedbUsearchStore` (redb + usearch + fastembed) for
/// agent memory; this tool exposes its semantic search to LLMs.
/// What: Accepts `{"query": "..."}` plus optional `"limit"`. Embeds the
/// query via `FastEmbedder`, searches `Segment::AgentMemory`, returns a JSON
/// array `[{id, score, content}]`. When constructed without a backend
/// (default for unwired callers), returns a graceful `{"error": "..."}`
/// payload so the LLM loop can continue.
/// Test: `memory_recall_returns_graceful_error_without_backend`,
/// `memory_recall_searches_embedded_store`.
pub struct MemoryRecallTool {
    backend: Option<MemoryBackend>,
    /// Explicit identity override. When `None`, the tool falls back to
    /// `CallerIdentity::from_env()` at execute time. Tests pin this to a
    /// known value to isolate from process-wide env state. (#193)
    identity_override: Option<CallerIdentity>,
}

impl MemoryRecallTool {
    /// Construct without a backend — returns graceful "not available"
    /// payloads on every call. Used in registries that are wired before the
    /// memory store is ready.
    pub fn new() -> Self {
        Self {
            backend: None,
            identity_override: None,
        }
    }

    /// Construct with an injected `MemoryBackend` (store + embedder).
    ///
    /// Why: Production wiring needs to share the same `RedbUsearchStore` and
    /// `FastEmbedder` across `store_memory`, `retrieve_memory`,
    /// `list_memory_keys`, and `memory_recall`. Constructor injection keeps
    /// that explicit.
    /// What: Plain struct literal.
    /// Test: `memory_recall_searches_embedded_store`.
    #[allow(dead_code)]
    pub fn with_backend(backend: MemoryBackend) -> Self {
        Self {
            backend: Some(backend),
            identity_override: None,
        }
    }

    /// Pin an explicit identity (test-only). When set, `execute` ignores the
    /// `OPEN_MPM_CALLER` env vars so unit tests don't fight over process
    /// state. (#193)
    #[allow(dead_code)]
    pub fn with_identity(mut self, identity: Option<CallerIdentity>) -> Self {
        self.identity_override = identity;
        self
    }
}

impl Default for MemoryRecallTool {
    fn default() -> Self {
        Self::new()
    }
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
                "description": "Semantic search over the embedded agent memory store (redb + usearch + fastembed). Returns a JSON array of hits {id, score, content} or a graceful error when the store isn't available. Use the `scope` parameter to control breadth: 'session' (default — current session only, lowest noise), 'all' (every local session), 'imported' (cross-machine sessions imported from teammates). Use the optional `tag` parameter to restrict to a hierarchical namespace — prefix matching is supported. Examples: tag='configuration' returns all skills + MCP servers, tag='configuration/skill' returns only skills, tag='configuration/mcp' returns only MCP servers, tag='docs' returns all documentation (user/developer/design), tag='docs/user' returns only user docs, tag='memories' returns agent-run memories.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language query to recall relevant memories."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default 5).",
                            "minimum": 1,
                            "maximum": 50
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["session", "all", "imported"],
                            "description": "Search scope: 'session' (current session only, default), 'all' (all local sessions), 'imported' (cross-machine imported sessions only)."
                        },
                        "tag": {
                            "type": "string",
                            "description": "Optional hierarchical tag filter (prefix-matched). Top-level namespaces: 'memories' (agent-run notes), 'configuration' (skills + MCP), 'docs' (project documentation). Subtags: 'configuration/skill', 'configuration/mcp', 'docs/user', 'docs/developer', 'docs/design', 'memories/session', 'memories/decision', 'memories/observation'. Prefix match: tag='configuration' returns both skills and MCP."
                        },
                        "segment": {
                            "type": "string",
                            "description": "Memory tier to search: context (stable architecture facts), brief (active goals), history (past decisions), or agent_memory (default catch-all).",
                            "enum": ["context", "brief", "history", "agent_memory"]
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
            return ToolResult::err("memory_recall: missing required 'query' string");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 50) as usize)
            .unwrap_or(5);
        // Resolve scope (default = "session"): determines which memories the
        // search will return. Default keeps noise lowest by restricting to the
        // current session; agents broaden explicitly via "all" or "imported".
        let mut scope = args
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "session".to_string());

        // #193: Apply the caller-identity ceiling. The harness sets
        // OPEN_MPM_CALLER + IDs at spawn time; agents cannot self-elevate.
        // CTRL = All (no cap), PM = Session (downgrade "all" -> "session"),
        // Agent = Agent (force agent-tag filter on top of session). When no
        // identity is present (legacy callers, tests) we leave the scope as
        // requested for backward compat.
        let identity = self
            .identity_override
            .clone()
            .or_else(CallerIdentity::from_env);
        let ceiling = identity
            .as_ref()
            .map(CallerIdentity::max_recall_scope)
            .unwrap_or(RecallCeiling::All);
        // Silently downgrade "all"/"imported" requests when ceiling forbids
        // them — agents shouldn't be able to detect the cap.
        if matches!(ceiling, RecallCeiling::Session | RecallCeiling::Agent)
            && (scope == "all" || scope == "imported")
        {
            scope = "session".to_string();
        }
        // Optional tag filter: when present, restricts results to payloads
        // whose `tag` field starts with the supplied prefix. The hierarchical
        // taxonomy (e.g. `configuration/skill`, `docs/user`,
        // `memories/decision`) means a filter of `configuration` matches both
        // `configuration/skill` and `configuration/mcp`. Empty == no filter.
        let tag_filter = args
            .get("tag")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        // #277: optional `segment` arg routes search to a specific memory tier
        // (Context / Brief / History / AgentMemory). Unknown values fall back
        // to `AgentMemory` so the tool degrades gracefully when an agent sends
        // a typo or future-but-unsupported tier name.
        let target_segment = args
            .get("segment")
            .and_then(Value::as_str)
            .and_then(Segment::from_name)
            .unwrap_or(Segment::AgentMemory);

        let Some(backend) = self.backend.as_ref() else {
            let out = json!({
                "error": "memory store not available — MemoryRecallTool was constructed without a MemoryBackend; agent should proceed without recall.",
                "results": []
            });
            return ToolResult::ok(out.to_string());
        };

        // Embed the query. Fail soft if the embedder errors out so the LLM
        // sees a structured payload rather than a hard tool failure.
        let qvec = match backend.embedder.embed_single(query) {
            Ok(v) => v,
            Err(e) => {
                let out = json!({
                    "error": format!("embed failed: {e}"),
                    "results": []
                });
                return ToolResult::ok(out.to_string());
            }
        };

        // Over-fetch when filtering so we still return up to `limit` matches
        // after client-side filtering drops non-matching rows. Cap the
        // over-fetch so a tiny index still works.
        let needs_filter = scope == "session" || scope == "imported" || tag_filter.is_some();
        let search_limit = if needs_filter {
            (limit * 4).clamp(20, 50)
        } else {
            limit
        };

        let hits = match backend
            .store
            .search(target_segment, &qvec, search_limit)
            .await
        {
            Ok(h) => h,
            Err(e) => {
                let out = json!({
                    "error": format!("memory search failed: {e}"),
                    "results": []
                });
                return ToolResult::ok(out.to_string());
            }
        };

        // Apply tag filter first (independent of scope) — it's the cheapest
        // and lets us drop irrelevant rows before scope-narrowing further.
        // Prefix-match so `configuration` matches `configuration/skill` and
        // `configuration/mcp`; an exact tag like `configuration/skill` still
        // works because every string starts with itself.
        let hits: Vec<_> = if let Some(want_tag) = tag_filter.as_deref() {
            hits.into_iter()
                .filter(|h| {
                    h.payload
                        .get("tag")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.starts_with(want_tag))
                })
                .collect()
        } else {
            hits
        };

        // #193: Resolve the effective session_id from either the backend (legacy
        // wiring) or the caller identity (env-var bridge). Identity wins when
        // both are present so the scope-ceiling check stays consistent.
        let effective_session_id: Option<String> = identity
            .as_ref()
            .and_then(|i| i.session_id().map(str::to_string))
            .or_else(|| backend.session_id.clone());

        // Apply scope filter:
        //  - "session": only payloads whose session_id matches the effective sid
        //  - "imported": only payloads with imported == true
        //  - "all": no filter
        let hits: Vec<_> = match scope.as_str() {
            "imported" => hits
                .into_iter()
                .filter(|h| {
                    h.payload
                        .get("imported")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .take(limit)
                .collect(),
            "all" => hits.into_iter().take(limit).collect(),
            // Default: session-scoped. If neither identity nor backend
            // provides a session_id, fall back to "all" rather than returning
            // zero hits — this preserves legacy behavior for unwired callers.
            _ => {
                if let Some(sid) = effective_session_id.as_deref() {
                    hits.into_iter()
                        .filter(|h| {
                            h.payload
                                .get("session_id")
                                .and_then(|v| v.as_str())
                                .is_some_and(|s| s == sid)
                        })
                        .take(limit * 4)
                        .collect()
                } else {
                    hits.into_iter().take(limit * 4).collect()
                }
            }
        };

        // #193: When the caller is an Agent, additionally restrict to its own
        // writes — payloads must carry `agent/<agent_id>` in their tags array.
        // Memories without any `agent/*` tag (e.g. legacy entries written
        // before this change, or those written by PM/CTRL) are *allowed* so
        // we don't break backward compat. Memories with a *different*
        // `agent/<other>` tag are filtered out.
        let hits: Vec<_> = if ceiling == RecallCeiling::Agent {
            let want_agent = identity
                .as_ref()
                .and_then(|i| i.agent_id().map(str::to_string));
            hits.into_iter()
                .filter(|h| {
                    let tags = h.payload.get("tags").and_then(|v| v.as_array());
                    let agent_tags: Vec<&str> = tags
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .filter(|s| s.starts_with("agent/"))
                                .collect()
                        })
                        .unwrap_or_default();
                    if agent_tags.is_empty() {
                        // Legacy / non-agent memory — visible to agents too.
                        return true;
                    }
                    // Must contain the calling agent's tag.
                    if let Some(self_id) = want_agent.as_deref() {
                        let want = format!("agent/{self_id}");
                        agent_tags.iter().any(|t| *t == want)
                    } else {
                        // Agent ceiling but no agent_id available — be safe
                        // and drop foreign-agent memories.
                        false
                    }
                })
                .take(limit)
                .collect()
        } else {
            hits.into_iter().take(limit).collect()
        };

        let results: Vec<Value> = hits
            .into_iter()
            .map(|h| {
                let content_raw = h
                    .payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| h.payload.to_string());
                let content: String = content_raw.chars().take(HIT_MAX_CHARS).collect();
                json!({
                    "id": h.id,
                    "score": h.score,
                    "content": content,
                })
            })
            .collect();

        ToolResult::ok(serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string()))
    }
}

/// Tool: `vector_search` — semantic code search via embedded index.
///
/// Why: Research agents benefit from fuzzy, intent-based lookups over the
/// codebase; the exact-regex `grep_files` tool complements it but doesn't
/// match paraphrases. When no index exists yet (first run, or a project
/// that hasn't run `--reindex`), the tool degrades to a regex fallback so
/// the agent still gets useful results.
/// What: Opens `.open-mpm/code/` if present, embeds the query via
/// `FastEmbedder`, returns top-k hits; otherwise falls back to
/// `GrepFilesTool` with the query treated as a regex.
/// Test: `vector_search_returns_graceful_error_without_index`.
pub struct VectorSearchTool {
    code_dir: PathBuf,
    fallback: Arc<GrepFilesTool>,
}

impl VectorSearchTool {
    /// Construct with the default index path (`.open-mpm/state/code/`).
    ///
    /// Why: Matches the indexer's default output location so the tool finds
    /// the embedded code without requiring explicit config.
    /// What: Plain struct literal with the bundled `GrepFilesTool` fallback.
    /// Test: `vector_search_returns_graceful_error_without_index`.
    pub fn new() -> Self {
        Self {
            code_dir: PathBuf::from(".open-mpm").join("state").join("code"),
            fallback: Arc::new(GrepFilesTool::new()),
        }
    }

    /// Override the on-disk index location (used by tests).
    #[allow(dead_code)]
    pub fn with_code_dir(mut self, path: PathBuf) -> Self {
        self.code_dir = path;
        self
    }

    /// Path accessor used by tests.
    #[allow(dead_code)]
    pub fn code_dir(&self) -> &Path {
        &self.code_dir
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
        json!({
            "type": "function",
            "function": {
                "name": "vector_search",
                "description": "Semantic search over indexed project code (embedded vector index). Falls back to regex search when the index is unavailable. Returns JSON array of hits with path and snippet.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language or keyword query describing the code to find."
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
    use crate::memory::redb_usearch::RedbUsearchStore;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// Tiny deterministic embedder that maps every text to a fixed-length
    /// vector by hashing characters. Avoids loading the real ONNX model in
    /// unit tests.
    struct StubEmbedder {
        dim: usize,
    }

    impl Embedder for StubEmbedder {
        fn dimension(&self) -> usize {
            self.dim
        }

        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed_single(t)).collect()
        }

        fn embed_single(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; self.dim];
            for (i, b) in text.bytes().enumerate() {
                v[i % self.dim] += (b as f32) / 255.0;
            }
            // Normalize so cosine similarity behaves.
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            for x in &mut v {
                *x /= norm;
            }
            Ok(v)
        }
    }

    #[tokio::test]
    async fn memory_recall_returns_graceful_error_without_backend() {
        let tool = MemoryRecallTool::new();
        let out = tool.execute(json!({"query": "anything"})).await;
        // Degrades gracefully: returns Success (not error), with an error
        // key embedded in the JSON payload. This lets the LLM decide to
        // skip memory and continue the task.
        assert!(!out.is_error());
        let body = out.content();
        assert!(body.contains("error"));
        assert!(body.contains("not available") || body.contains("proceed"));
    }

    #[tokio::test]
    async fn memory_recall_requires_query() {
        let tool = MemoryRecallTool::new();
        let out = tool.execute(json!({})).await;
        assert!(out.is_error());
        assert!(out.content().contains("query"));
    }

    #[tokio::test]
    async fn memory_recall_searches_embedded_store() {
        // Wire a real RedbUsearchStore in a tempdir + a stub embedder; insert
        // a memory and confirm `memory_recall` returns it.
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });
        let backend = MemoryBackend::new(store.clone(), embedder.clone());

        let content = "PM uses delegate_to_agent to spawn sub-agents over NDJSON.";
        let vec = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "fact-1",
                &vec,
                json!({ "content": content }),
            )
            .await
            .unwrap();

        let tool = MemoryRecallTool::with_backend(backend);
        let out = tool
            .execute(json!({"query": "delegate_to_agent NDJSON", "limit": 5}))
            .await;
        assert!(!out.is_error());
        let body = out.content();
        assert!(
            body.contains("fact-1") && body.contains("delegate_to_agent"),
            "expected hit content in payload, got: {body}"
        );
    }

    #[tokio::test]
    async fn memory_recall_defaults_to_session_scope() {
        // Two memories with different session_ids; without scope, the tool
        // must default to "session" and return only the current session's hit.
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content_a = "Auth flow uses bearer tokens.";
        let content_b = "Auth flow uses bearer tokens.";
        let vec_a = embedder.embed_single(content_a).unwrap();
        let vec_b = embedder.embed_single(content_b).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "fact-a",
                &vec_a,
                json!({ "content": content_a, "session_id": "session-aaa" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "fact-b",
                &vec_b,
                json!({ "content": content_b, "session_id": "session-bbb" }),
            )
            .await
            .unwrap();

        let backend =
            MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("session-aaa");
        let tool = MemoryRecallTool::with_backend(backend);

        // Omit scope: must default to current session.
        let out = tool
            .execute(json!({"query": "auth bearer tokens", "limit": 5}))
            .await;
        let body = out.content();
        assert!(
            body.contains("fact-a") && !body.contains("fact-b"),
            "default scope should be 'session' (current); got: {body}"
        );
    }

    #[tokio::test]
    async fn memory_recall_all_scope_returns_cross_session() {
        // scope=all returns memories from every session in the store.
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "Auth flow uses bearer tokens.";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "fact-a",
                &v,
                json!({ "content": content, "session_id": "session-aaa" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "fact-b",
                &v,
                json!({ "content": content, "session_id": "session-bbb" }),
            )
            .await
            .unwrap();

        let backend =
            MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("session-aaa");
        let tool = MemoryRecallTool::with_backend(backend);

        let out = tool
            .execute(json!({"query": "auth bearer tokens", "limit": 5, "scope": "all"}))
            .await;
        let body = out.content();
        assert!(
            body.contains("fact-a") && body.contains("fact-b"),
            "scope=all should return both sessions; got: {body}"
        );
    }

    #[tokio::test]
    async fn memory_recall_imported_scope_filters_by_imported_flag() {
        // scope=imported returns only memories whose payload has imported=true.
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "Cross-machine fact.";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "local-fact",
                &v,
                json!({ "content": content, "session_id": "local-1" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "remote-fact",
                &v,
                json!({
                    "content": content,
                    "session_id": "remote-x",
                    "imported": true,
                    "machine_id": "remote-host"
                }),
            )
            .await
            .unwrap();

        let backend =
            MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("local-1");
        let tool = MemoryRecallTool::with_backend(backend);

        let out = tool
            .execute(json!({"query": "cross machine", "limit": 5, "scope": "imported"}))
            .await;
        let body = out.content();
        assert!(
            body.contains("remote-fact") && !body.contains("local-fact"),
            "scope=imported should return only imported=true memories; got: {body}"
        );
    }

    #[tokio::test]
    async fn memory_recall_filters_by_tag() {
        // Insert three memories with different tags; queries with tag filter
        // should return only the matching one.
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "Useful information about deployment.";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "doc-1",
                &v,
                json!({ "content": content, "tag": "docs/user", "session_id": "s" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "skill-1",
                &v,
                json!({ "content": content, "tag": "configuration/skill", "session_id": "s" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "mcp-1",
                &v,
                json!({ "content": content, "tag": "configuration/mcp", "session_id": "s" }),
            )
            .await
            .unwrap();

        let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
        let tool = MemoryRecallTool::with_backend(backend);

        // tag=configuration/skill (with scope=all so the session filter doesn't interfere).
        let out = tool
            .execute(
                json!({"query": "deployment info", "scope": "all", "tag": "configuration/skill"}),
            )
            .await;
        let body = out.content();
        assert!(
            body.contains("skill-1"),
            "expected skill-1 in payload: {body}"
        );
        assert!(
            !body.contains("doc-1"),
            "doc-1 should be filtered out: {body}"
        );
        assert!(
            !body.contains("mcp-1"),
            "mcp-1 should be filtered out: {body}"
        );

        // tag=configuration/mcp
        let out2 = tool
            .execute(
                json!({"query": "deployment info", "scope": "all", "tag": "configuration/mcp"}),
            )
            .await;
        let body2 = out2.content();
        assert!(body2.contains("mcp-1"), "expected mcp-1: {body2}");
        assert!(!body2.contains("skill-1"));
        assert!(!body2.contains("doc-1"));

        // Prefix match: tag=configuration returns both skills AND MCP, but not docs.
        let out_prefix = tool
            .execute(json!({"query": "deployment info", "scope": "all", "tag": "configuration"}))
            .await;
        let body_prefix = out_prefix.content();
        assert!(
            body_prefix.contains("skill-1"),
            "prefix tag=configuration should match skill-1: {body_prefix}"
        );
        assert!(
            body_prefix.contains("mcp-1"),
            "prefix tag=configuration should match mcp-1: {body_prefix}"
        );
        assert!(
            !body_prefix.contains("doc-1"),
            "prefix tag=configuration should NOT match doc-1: {body_prefix}"
        );

        // Prefix match: tag=docs returns only docs.
        let out_docs = tool
            .execute(json!({"query": "deployment info", "scope": "all", "tag": "docs"}))
            .await;
        let body_docs = out_docs.content();
        assert!(body_docs.contains("doc-1"));
        assert!(!body_docs.contains("skill-1"));
        assert!(!body_docs.contains("mcp-1"));

        // No tag = all matches returned.
        let out3 = tool
            .execute(json!({"query": "deployment info", "scope": "all"}))
            .await;
        let body3 = out3.content();
        assert!(body3.contains("doc-1"));
        assert!(body3.contains("skill-1"));
        assert!(body3.contains("mcp-1"));
    }

    /// #193: An agent caller MUST be capped at `RecallCeiling::Agent` —
    /// even when it requests `scope: "all"`, only memories tagged with its
    /// own `agent/<id>` should come back (untagged legacy rows are still
    /// allowed for back-compat, but a foreign-agent tag must be filtered).
    ///
    /// We use std::env::set_var inside a serial test guarded with a Mutex
    /// because the env var bridge is global. Running in a single tokio
    /// runtime per test prevents interleaving with other env-reading tests.
    #[tokio::test]
    async fn agent_ceiling_filters_foreign_agent_tags() {
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "Useful agent memory.";
        let v = embedder.embed_single(content).unwrap();
        // Memory written by `code-agent` (foreign).
        store
            .insert(
                Segment::AgentMemory,
                "foreign",
                &v,
                json!({
                    "content": content,
                    "session_id": "sess-1",
                    "tags": ["session/sess-1", "agent/code-agent"]
                }),
            )
            .await
            .unwrap();
        // Memory written by `research-agent` (self).
        store
            .insert(
                Segment::AgentMemory,
                "self",
                &v,
                json!({
                    "content": content,
                    "session_id": "sess-1",
                    "tags": ["session/sess-1", "agent/research-agent"]
                }),
            )
            .await
            .unwrap();
        // Legacy untagged memory — allowed under back-compat.
        store
            .insert(
                Segment::AgentMemory,
                "legacy",
                &v,
                json!({
                    "content": content,
                    "session_id": "sess-1"
                }),
            )
            .await
            .unwrap();

        // Pin identity explicitly via the builder so this test doesn't
        // pollute process-wide env vars (other parallel tests would observe
        // them and erroneously apply the agent ceiling).
        let identity = crate::identity::CallerIdentity::Agent {
            session_id: "sess-1".into(),
            project_id: "proj".into(),
            agent_id: "research-agent".into(),
        };
        let backend = MemoryBackend::new(store.clone(), embedder.clone());
        let tool = MemoryRecallTool::with_backend(backend).with_identity(Some(identity));
        // Even when the agent asks for scope=all, the ceiling must downgrade
        // to session and the agent-tag filter must drop "foreign".
        let out = tool
            .execute(json!({"query": "useful", "limit": 50, "scope": "all"}))
            .await;
        let body = out.content();

        assert!(
            body.contains("\"self\""),
            "self-agent memory should be returned: {body}"
        );
        assert!(
            body.contains("\"legacy\""),
            "legacy untagged memory should be returned (back-compat): {body}"
        );
        assert!(
            !body.contains("\"foreign\""),
            "foreign-agent memory must be filtered: {body}"
        );
    }

    /// #277: With no `segment` arg, `memory_recall` searches `AgentMemory`
    /// (the legacy default). Memories inserted into `Context` must NOT come
    /// back from a default-segment query.
    #[tokio::test]
    async fn memory_recall_defaults_to_agent_memory_segment() {
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "shared content text";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "in-agent",
                &v,
                json!({ "content": content, "session_id": "s" }),
            )
            .await
            .unwrap();
        store
            .insert(
                Segment::Context,
                "in-context",
                &v,
                json!({ "content": content, "session_id": "s" }),
            )
            .await
            .unwrap();

        let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
        let tool = MemoryRecallTool::with_backend(backend);
        let out = tool
            .execute(json!({"query": "shared content", "scope": "all"}))
            .await;
        let body = out.content();
        assert!(
            body.contains("in-agent") && !body.contains("in-context"),
            "default segment must be AgentMemory; got: {body}"
        );
    }

    /// #277: With `segment: "context"`, `memory_recall` searches
    /// `Segment::Context` and returns rows stored there.
    #[tokio::test]
    async fn memory_recall_routes_to_context_segment() {
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "architecture fact";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::Context,
                "ctx-1",
                &v,
                json!({ "content": content, "session_id": "s" }),
            )
            .await
            .unwrap();

        let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
        let tool = MemoryRecallTool::with_backend(backend);
        let out = tool
            .execute(json!({"query": "architecture", "scope": "all", "segment": "context"}))
            .await;
        let body = out.content();
        assert!(
            body.contains("ctx-1"),
            "segment=context should return Context rows: {body}"
        );
    }

    /// #277: With `segment: "brief"`, recall hits `Segment::Brief`.
    #[tokio::test]
    async fn memory_recall_routes_to_brief_segment() {
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "active goal";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::Brief,
                "brief-1",
                &v,
                json!({ "content": content, "session_id": "s" }),
            )
            .await
            .unwrap();

        let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
        let tool = MemoryRecallTool::with_backend(backend);
        let out = tool
            .execute(json!({"query": "active goal", "scope": "all", "segment": "brief"}))
            .await;
        let body = out.content();
        assert!(
            body.contains("brief-1"),
            "segment=brief should return Brief rows: {body}"
        );
    }

    /// #277: An unknown segment string falls back to `AgentMemory` rather
    /// than failing the call (graceful degradation).
    #[tokio::test]
    async fn memory_recall_unknown_segment_falls_back_to_agent_memory() {
        let dir = tempdir().unwrap();
        let dim = 16;
        let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
        let embedder = Arc::new(StubEmbedder { dim });

        let content = "fallback content";
        let v = embedder.embed_single(content).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "agent-1",
                &v,
                json!({ "content": content, "session_id": "s" }),
            )
            .await
            .unwrap();

        let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
        let tool = MemoryRecallTool::with_backend(backend);
        let out = tool
            .execute(json!({"query": "fallback", "scope": "all", "segment": "totally_unknown"}))
            .await;
        let body = out.content();
        assert!(!out.is_error());
        assert!(
            body.contains("agent-1"),
            "unknown segment should fall back to AgentMemory: {body}"
        );
    }

    #[tokio::test]
    async fn vector_search_returns_graceful_error_without_index() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("no-index");
        let tool = VectorSearchTool::new().with_code_dir(missing);
        let out = tool.execute(json!({"query": "foo"})).await;
        // Falls back to grep mode rather than erroring out.
        assert!(!out.is_error());
        let body = out.content();
        assert!(body.contains("grep_fallback"));
    }

    #[tokio::test]
    async fn vector_search_requires_query() {
        let tool = VectorSearchTool::new();
        let out = tool.execute(json!({})).await;
        assert!(out.is_error());
        assert!(out.content().contains("query"));
    }

    #[test]
    fn memory_recall_schema_names_tool() {
        let t = MemoryRecallTool::new();
        assert_eq!(t.name(), "memory_recall");
        let s = t.schema();
        assert_eq!(s["function"]["name"], "memory_recall");
    }

    #[test]
    fn vector_search_schema_names_tool() {
        let t = VectorSearchTool::new();
        assert_eq!(t.name(), "vector_search");
        let s = t.schema();
        assert_eq!(s["function"]["name"], "vector_search");
    }
}
