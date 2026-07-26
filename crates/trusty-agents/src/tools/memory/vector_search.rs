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
//! #3232/#4009 (epic #4007's two-tier knowledge model) add a SECOND tier of
//! corpora alongside the one curated OKG store: `[tools].search_indexes`, a
//! plain list of trusty-search index ids attached to the agent. The tool
//! enumerates bound + attached ids in its `index_id` schema description so
//! the model no longer has to guess an id that happens to exist, and — only
//! when `[tools] enforce_search_indexes = true` — rejects an explicit id
//! outside that set. Enforcement is opt-in by owner decision (epic #4007
//! OQ-2); an agent that is unenforced AND declares no attached indexes takes
//! every path here byte-identically to pre-#4009, schema bytes included.
//! Note the conjunction: an ENFORCED agent's schema always states the
//! restriction, even with no attached indexes (bound-store-only lockdown is a
//! legal config), because the schema must describe what the tool accepts.
//!
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
    /// Arbitrary trusty-search indexes attached to this agent via
    /// `[tools].search_indexes` (#3232) — epic #4007's tier-2 knowledge
    /// mechanism, distinct from the single curated OKG store above. Empty for
    /// agents that declare none, which is the pre-#3232 state exactly.
    attached_index_ids: Vec<String>,
    /// Whether an explicit `index_id` outside the allowlist is REJECTED
    /// (#4009). `false` (the default, per epic #4007 OQ-2) = declarative-only:
    /// the allowlist informs the schema but gates nothing, byte-identically
    /// to pre-#4009 behaviour.
    enforce_index_allowlist: bool,
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
            attached_index_ids: Vec::new(),
            enforce_index_allowlist: false,
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

    /// Attach the agent's arbitrary tier-2 search indexes (#3232/#4009).
    ///
    /// Why: `[tools].search_indexes` is where an agent declares corpora it
    /// may query BESIDES its one curated OKG store. Passing the resolved list
    /// in at registry-build time (where `AgentConfig` is in scope) keeps the
    /// tool free of config loading, exactly as `with_default_index` does.
    /// What: Sets `attached_index_ids`, dropping blanks and any id already
    /// present (the caller normally passes
    /// `ToolsConfig::resolved_search_indexes`, which has done this already —
    /// this is belt-and-braces for direct callers and tests).
    /// Test: `vector_search_schema_lists_attached_indexes`.
    pub fn with_attached_indexes(mut self, ids: Vec<String>) -> Self {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            let id = id.trim();
            if id.is_empty() || out.iter().any(|e| e == id) {
                continue;
            }
            out.push(id.to_string());
        }
        self.attached_index_ids = out;
        self
    }

    /// Turn the allowlist from declarative into fail-closed (#4009).
    ///
    /// Why: Owner decision on epic #4007 OQ-2 — opt-in, default off. See
    /// `ToolsConfig::enforce_search_indexes`.
    /// What: Sets `enforce_index_allowlist`; `false` leaves every code path
    /// byte-identical to pre-#4009.
    /// Test: `vector_search_enforcement_rejects_undeclared_index`,
    /// `vector_search_default_is_unenforced`.
    pub fn with_index_enforcement(mut self, enforce: bool) -> Self {
        self.enforce_index_allowlist = enforce;
        self
    }

    /// The full set of indexes this agent may query: its bound OKG store
    /// first, then its attached tier-2 indexes (#3232/#4009).
    ///
    /// Why: ONE derivation feeds both halves of #4009 — the schema
    /// enumeration the model reads and the enforcement check — so what the
    /// tool advertises and what it accepts can never drift apart. That
    /// drift is precisely the #3864 class of defect this ticket must not
    /// reintroduce in the other direction.
    /// What: Bound index (when set) followed by the attached ids, deduped,
    /// declaration order preserved. Empty for an agent with neither.
    /// Test: `vector_search_allowed_ids_put_bound_index_first`.
    pub fn allowed_index_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self
            .default_index_id
            .iter()
            .map(String::as_str)
            .chain(self.attached_index_ids.iter().map(String::as_str))
        {
            let id = id.trim();
            if id.is_empty() || out.iter().any(|e| e == id) {
                continue;
            }
            out.push(id.to_string());
        }
        out
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

    /// [`Self::effective_index_id`] plus the #4009 allowlist gate — the entry
    /// point `execute` uses.
    ///
    /// Why: Resolution (which index does this call mean?) and authorization
    /// (may this agent query it?) are separate concerns, and only the second
    /// is opt-in. Keeping them as two functions lets the resolution contract
    /// stay pinned by its pre-existing tests while enforcement is layered on
    /// top; keeping the gate on the path `execute` actually calls means an
    /// enforced agent cannot reach `daemon_query` with an undeclared id.
    /// What: With enforcement OFF (the default) this is exactly
    /// `Ok(effective_index_id(args))` — no allowlist is consulted, so the
    /// default path is byte-identical to pre-#4009. With enforcement ON, an
    /// EXPLICIT `index_id` outside [`Self::allowed_index_ids`] returns `Err`
    /// with a message naming the permitted set; the agent's own bound default
    /// is never rejected (it is always a member), and an omitted `index_id`
    /// therefore always resolves.
    /// Test: `vector_search_enforcement_rejects_undeclared_index`,
    /// `vector_search_enforcement_allows_bound_and_attached`,
    /// `vector_search_default_is_unenforced`,
    /// `vector_search_execute_rejects_undeclared_index_when_enforced`.
    pub fn authorized_index_id(&self, args: &Value) -> Result<Option<String>, String> {
        let resolved = self.effective_index_id(args);
        if !self.enforce_index_allowlist {
            return Ok(resolved);
        }
        let Some(id) = resolved else {
            return Ok(None);
        };
        let allowed = self.allowed_index_ids();
        if allowed.iter().any(|a| a == &id) {
            return Ok(Some(id));
        }
        let allowed_list = if allowed.is_empty() {
            "(none — this agent declares no bound store and no [tools].search_indexes)".to_string()
        } else {
            allowed
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Err(format!(
            "vector_search: index_id `{id}` is not attached to this agent. Allowed indexes: \
             {allowed_list}. Attach it via [tools].search_indexes in the agent config, or omit \
             index_id to search the agent's own store."
        ))
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
        let mut index_desc = match &self.default_index_id {
            Some(id) => format!(
                "trusty-search index to search. Defaults to this agent's bound OKG store index \
                 (`{id}`) when omitted. Pass an explicit id to search a different corpus."
            ),
            None => "trusty-search index to search. When omitted, searches the local embedded \
                     project code index."
                .to_string(),
        };
        // #4009: enumerate the agent's attached tier-2 indexes
        // (`[tools].search_indexes`, #3232) alongside the bound OKG store, so
        // the model picks from real queryable ids instead of guessing one that
        // happens to exist.
        //
        // The `|| enforce` half of this gate is load-bearing, not defensive.
        // A bound store with NO `search_indexes` and `enforce = true` is a
        // legal config — an operator locking an agent to only its own corpus —
        // and there `allowed_index_ids()` is `[bound]` while
        // `authorized_index_id()` genuinely refuses everything else. Gating on
        // the attached list alone would leave that agent's schema saying
        // "Pass an explicit id to search a different corpus" over closed
        // behaviour: the #3864 advertise/accept drift, in the
        // schema-says-open direction. The schema must describe what the tool
        // ACCEPTS, so the note appears whenever enforcement is active.
        //
        // Unenforced agents with no attached indexes still take neither branch
        // and keep the exact pre-#4009 description, prompt bytes unchanged —
        // that, and not "enforce is invisible", is the compatibility promise.
        if !self.attached_index_ids.is_empty() || self.enforce_index_allowlist {
            let allowed = self.allowed_index_ids();
            if allowed.is_empty() {
                // Only reachable under enforcement (an unenforced agent with
                // no ids took neither branch): nothing is queryable at all, so
                // say that rather than emit a dangling empty list.
                index_desc
                    .push_str(" This agent has no queryable trusty-search index; omit index_id.");
            } else {
                let listed = allowed
                    .iter()
                    .map(|id| format!("`{id}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                index_desc.push_str(&format!(" Indexes available to this agent: {listed}."));
                if self.enforce_index_allowlist {
                    index_desc
                        .push_str(" Only these indexes may be queried; any other id is rejected.");
                }
            }
        }
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
        // #4009: authorize before routing. With enforcement off (the default)
        // this is the pre-existing `effective_index_id` result unchanged; with
        // it on, an undeclared explicit id is a hard tool error rather than a
        // silent daemon call — and deliberately NOT a fall-through to the
        // local index, which would leak an answer from the wrong corpus in
        // response to a rejected request.
        let index_id = match self.authorized_index_id(&args) {
            Ok(id) => id,
            Err(msg) => return ToolResult::err(msg),
        };
        if let Some(index_id) = index_id {
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
