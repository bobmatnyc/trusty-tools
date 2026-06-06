//! `memory_recall` tool — semantic search over the embedded agent memory store.
//!
//! Why: Agents need to ask the project's own memory for facts previously
//! stored (architecture decisions, APIs, conventions). The harness already
//! runs an embedded `RedbUsearchStore` (redb + usearch + fastembed) for
//! agent memory; this tool exposes its semantic search to LLMs.
//! What: `MemoryRecallTool` embeds the query via the injected embedder and
//! runs HNSW search against the chosen `Segment` in the injected
//! `MemoryBackend`, applying scope/tag/identity filters.
//! Test: See `super::tests` — graceful error without a backend, plus scope,
//! tag, segment, and identity-ceiling coverage.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::identity::{CallerIdentity, RecallCeiling};
use crate::memory::store::Segment;
use crate::tools::native_memory::MemoryBackend;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Dimension of embedded code vectors — mirrors `search::build_file_watcher`.
/// Kept in sync manually; a mismatch here will cause `CodeStore::open` to
/// error, which `VectorSearchTool` surfaces to the agent as a graceful
/// "index unavailable" message.
pub(super) const EMBED_DIM: usize = 384;

/// Max characters of content returned per hit in `memory_recall` /
/// `vector_search`, so a single call can't blow the context window.
pub(super) const HIT_MAX_CHARS: usize = 600;

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
    /// `TAGENT_CALLER` env vars so unit tests don't fight over process
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
        // TAGENT_CALLER + IDs at spawn time; agents cannot self-elevate.
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
