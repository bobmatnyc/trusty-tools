//! `search_code` tool: trusty-search-backed code discovery (PR A).
//!
//! Why: local `glob`/`grep`/`list_dir` (#2685) find code by literal text only;
//! the engineer routinely needs CONCEPTUAL discovery — "where is X", "how does
//! Y work", "what calls Z" — which literal search cannot answer. trusty-search
//! already indexes every registered project for semantic (embedding) and
//! call-graph (KG) retrieval and ships as an MCP binary. This tool makes that
//! the PRIMARY discovery path for the engineer while the local literal tools
//! stay as the graceful-degrade FALLBACK: when trusty-search is not installed,
//! not running, or its index is not ready, the tool returns a SUCCESSFUL
//! "unavailable — use grep/glob" result rather than an error that would derail
//! the agent loop (mirrors `recall_session`'s fail-open contract).
//! What: [`TrustySearchTool`] implements `ToolExecutor`. It lazily spawns
//! `trusty-search serve` via the SHARED `trusty_common::stdio_mcp_client`
//! (never a bespoke MCP/HTTP client — common-entry-point principle, mirroring
//! trusty-agents' `TrustySearchPlugin`), runs the MCP handshake, and resolves
//! the project's index id from `list_indexes`. A single `search_code` tool with
//! a `mode` param routes to trusty-search's real lanes: `semantic`
//! (`search_semantic`), `symbol` (`search_kg`), and `grep` (`grep`). One tool
//! keeps the advertised schema surface minimal and mirrors trusty-search's own
//! lane taxonomy. Any spawn/handshake/timeout/`STAGE_NOT_READY`/missing-index
//! condition is fail-open: a non-error `ToolResult` telling the model to use
//! the local `grep`/`glob` tools instead.
//! Test: `tests::pick_index_prefers_exact_match`,
//! `tests::pick_index_falls_back_to_longest_ancestor`,
//! `tests::execute_fail_open_when_binary_absent`,
//! `tests::execute_rejects_malformed_args_recoverably`,
//! `tests::schema_advertises_mode_and_query`, `tests::mcp_text_extracts_content`,
//! `tests::is_mcp_error_detects_error_flag`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};
use tracing::warn;
use trusty_common::stdio_mcp_client::StdioMcpClient;

use crate::tools::traits::{ToolExecutor, ToolResult};

/// The tool's registered/advertised name.
pub const SEARCH_CODE_TOOL_NAME: &str = "search_code";

/// Binary spawned in MCP stdio mode (`trusty-search serve`).
const SEARCH_BINARY: &str = "trusty-search";

/// `clientInfo.name` advertised during the MCP handshake.
const CLIENT_NAME: &str = "trusty-code";

/// Default `top_k` when the model omits it.
const DEFAULT_TOP_K: usize = 10;

/// Hard cap on `top_k` regardless of what the model requests.
const MAX_TOP_K: usize = 25;

/// The fail-open message steering the model back to the local literal tools.
const FALLBACK_HINT: &str =
    "trusty-search is unavailable; use the local `grep` and `glob` tools for this query instead.";

/// Parsed `search_code` tool-call arguments.
#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
}

/// A live trusty-search MCP session plus the resolved project index.
///
/// Why: MCP requests are not concurrency-safe (one in-flight request per stdio
/// pair), so the client sits behind an async `Mutex`. `index_id` is resolved
/// once at spawn from `list_indexes` because the semantic/KG lanes require it.
/// What: `index_id` is `None` when no registered index covers the project root
/// — the search lanes then fail open rather than guess a wrong index.
/// Test: exercised via `execute_*` and `pick_index_*` in `tests`.
struct SearchSession {
    client: Mutex<StdioMcpClient>,
    index_id: Option<String>,
}

/// `ToolExecutor` for the `search_code` discovery tool (PR A).
///
/// Why: gives the engineer a semantic/call-graph discovery path backed by
/// trusty-search, scoped to the delegating run's project root.
/// What: holds the project root (for index resolution + scoping) and lazily
/// initialises one [`SearchSession`] on first use, caching the (possibly
/// absent) session in a `OnceCell` so a missing/unreachable daemon is probed at
/// most once per tool instance.
/// Test: `tests::execute_fail_open_when_binary_absent`.
pub struct TrustySearchTool {
    project: PathBuf,
    binary: String,
    session: OnceCell<Option<SearchSession>>,
}

impl TrustySearchTool {
    /// Construct a tool scoped to `project`, spawning the real `trusty-search`.
    ///
    /// Why: the engineer's registry builds one per delegation, scoped to the
    /// project working tree.
    /// What: defers all process spawning to first `execute` — construction is
    /// cheap and side-effect-free.
    /// Test: `tests::schema_advertises_mode_and_query`.
    pub fn new(project: impl Into<PathBuf>) -> Self {
        Self {
            project: project.into(),
            binary: SEARCH_BINARY.to_string(),
            session: OnceCell::new(),
        }
    }

    /// Override the spawned binary name (tests point this at a name that is
    /// guaranteed absent to exercise the fail-open path deterministically).
    ///
    /// Why: the daemon-absent path must be unit-testable without depending on
    /// whether `trusty-search` happens to be on the dev machine's PATH.
    /// What: replaces the binary the lazy spawn invokes.
    /// Test: `tests::execute_fail_open_when_binary_absent`.
    #[cfg(test)]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Lazily spawn + handshake + resolve the index, caching the result.
    ///
    /// Why: probing a missing/broken daemon on every call would add per-turn
    /// latency; caching the `Option` probes at most once.
    /// What: returns `&None` when spawn or handshake fails (fail-open).
    /// Test: `tests::execute_fail_open_when_binary_absent`.
    async fn session(&self) -> &Option<SearchSession> {
        self.session
            .get_or_init(|| spawn_session(&self.binary, &self.project))
            .await
    }
}

/// Spawn `trusty-search serve`, run the handshake, and resolve the index id.
///
/// Why: isolates the fallible startup so `session` stays a thin cache and the
/// whole flow degrades to `None` on the first failure.
/// What: returns `None` if the binary is absent, the handshake fails, or the
/// process cannot start; otherwise a [`SearchSession`] whose `index_id` may
/// still be `None` when no index covers the project.
/// Test: covered opportunistically — a missing binary yields `None`, asserted
/// via `tests::execute_fail_open_when_binary_absent`.
async fn spawn_session(binary: &str, project: &Path) -> Option<SearchSession> {
    let mut client = match StdioMcpClient::spawn(binary, &["serve"], CLIENT_NAME).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "search_code: trusty-search spawn failed (fail-open)");
            return None;
        }
    };
    if let Err(e) = client.initialize().await {
        warn!(error = %e, "search_code: trusty-search handshake failed (fail-open)");
        return None;
    }
    let index_id = resolve_index_id(&mut client, project).await;
    Some(SearchSession {
        client: Mutex::new(client),
        index_id,
    })
}

/// Resolve the project's index id from `list_indexes`.
///
/// Why: the semantic/KG lanes require an `index_id`; deriving it from the
/// project root avoids making the model call `list_indexes` and guess.
/// What: calls the `list_indexes` MCP tool, parses the `{ "indexes": [...] }`
/// detail payload, and matches the project root against each index `root_path`
/// via [`pick_index`]. Returns `None` on any RPC/parse failure or no match.
/// Test: `tests::pick_index_prefers_exact_match`.
async fn resolve_index_id(client: &mut StdioMcpClient, project: &Path) -> Option<String> {
    let result = client.call_tool("list_indexes", json!({})).await.ok()?;
    let text = mcp_text(&result)?;
    let body: Value = serde_json::from_str(&text).ok()?;
    let indexes = body.get("indexes")?.as_array()?;
    pick_index(indexes, project)
}

/// Pick the index whose `root_path` best covers `project`.
///
/// Why: a worktree/subdirectory of a registered repo should resolve to that
/// repo's index even when the paths are not byte-identical.
/// What: prefers an exact canonical match; otherwise the registered index whose
/// canonical `root_path` is the longest ancestor of `project`. `None` when no
/// index covers the project.
/// Test: `tests::pick_index_prefers_exact_match`,
/// `tests::pick_index_falls_back_to_longest_ancestor`.
fn pick_index(indexes: &[Value], project: &Path) -> Option<String> {
    let target = canonicalize_best_effort(project);
    let mut best: Option<(usize, String)> = None;
    for entry in indexes {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(root) = entry.get("root_path").and_then(Value::as_str) else {
            continue;
        };
        let root = canonicalize_best_effort(Path::new(root));
        if root == target {
            return Some(id.to_string());
        }
        if target.starts_with(&root) {
            let depth = root.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, id.to_string()));
            }
        }
    }
    best.map(|(_, id)| id)
}

/// Canonicalise `p`, falling back to the path as given when it does not exist.
///
/// Why: index roots and the project path may be symlinked (worktrees under
/// `$HOME`); canonicalising both makes ancestor comparison robust.
/// What: `std::fs::canonicalize` with a lossless fallback.
/// Test: exercised via `pick_index_*`.
fn canonicalize_best_effort(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Extract the first text frame from an MCP `tools/call` result.
///
/// Why: trusty-search wraps every payload as `content[0].text` (a JSON string).
/// What: returns that string, or `None` when the frame is missing.
/// Test: `tests::mcp_text_extracts_content`.
fn mcp_text(result: &Value) -> Option<String> {
    result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Whether an MCP result carries the in-band `isError` flag (e.g.
/// `STAGE_NOT_READY` when the semantic/KG lane is not yet built).
///
/// Why: trusty-search reports stage-not-ready as `isError: true` content rather
/// than a JSON-RPC error, so `call_tool` returns `Ok`; we must inspect the flag
/// to fail open.
/// What: `true` when `result.isError == true`.
/// Test: `tests::is_mcp_error_detects_error_flag`.
fn is_mcp_error(result: &Value) -> bool {
    result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Build the MCP arguments for `mode`, returning the tool name + params.
///
/// Why: keeps `execute` linear — each mode maps to one trusty-search lane with
/// its own argument shape (`grep` takes `pattern`/`max_results` and an optional
/// `index_id`; the semantic/KG lanes take `index_id`/`query`/`top_k`).
/// What: returns `Err` with a fail-open message when a semantic/KG lane is
/// requested but no index covers the project.
/// Test: `tests::execute_fail_open_when_binary_absent` (mode routing is covered
/// end-to-end where a live daemon is available).
fn build_call(
    mode: &str,
    query: &str,
    top_k: usize,
    index_id: Option<&str>,
) -> Result<(&'static str, Value), String> {
    match mode {
        "grep" => {
            let mut params = json!({ "pattern": query, "max_results": top_k });
            if let Some(id) = index_id {
                params["index_id"] = json!(id);
            }
            Ok(("grep", params))
        }
        "symbol" => {
            let id = index_id.ok_or_else(|| {
                format!("no trusty-search index covers this project. {FALLBACK_HINT}")
            })?;
            Ok((
                "search_kg",
                json!({ "index_id": id, "query": query, "top_k": top_k }),
            ))
        }
        // Default and "semantic".
        _ => {
            let id = index_id.ok_or_else(|| {
                format!("no trusty-search index covers this project. {FALLBACK_HINT}")
            })?;
            Ok((
                "search_semantic",
                json!({ "index_id": id, "query": query, "top_k": top_k }),
            ))
        }
    }
}

#[async_trait]
impl ToolExecutor for TrustySearchTool {
    fn name(&self) -> &str {
        SEARCH_CODE_TOOL_NAME
    }

    /// JSON schema for `search_code`.
    ///
    /// Why: the description is a first-class LLM prompt — it tells the model to
    /// prefer this tool for conceptual discovery and steers each `mode` to the
    /// right lane, reusing trusty-search's own lane wording.
    /// Test: `tests::schema_advertises_mode_and_query`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": SEARCH_CODE_TOOL_NAME,
                "description": "PRIMARY code-discovery tool, backed by trusty-search's semantic + call-graph index. Use it for CONCEPTUAL queries where you don't already know the exact text: \"where is X\", \"how does Y work\", \"find the code that does <behaviour>\", \"what calls Z\". For exact symbol names, literal strings, or filename/path globs you already know, prefer the local `grep`/`glob` tools. If this tool reports trusty-search is unavailable, silently fall back to `grep`/`glob` — do not retry.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "For semantic: a natural-language description of the behaviour/concept. For symbol: a function or type name to expand its callers/callees. For grep: a regex or literal pattern."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["semantic", "symbol", "grep"],
                            "default": "semantic",
                            "description": "semantic (default): find code by meaning. symbol: explore what calls / is called by a known symbol (call graph). grep: regex/literal search over indexed files."
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Maximum results (default 10, max 25)."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a discovery query, fail-open on any unavailability.
    ///
    /// Why: an explicit model tool call must never derail the agent loop — a
    /// missing/unready trusty-search surfaces as a SUCCESSFUL "use grep/glob"
    /// result, not a tool error the model might retry-loop on.
    /// Test: `tests::execute_fail_open_when_binary_absent`,
    /// `tests::execute_rejects_malformed_args_recoverably`.
    async fn execute(&self, args: Value) -> ToolResult {
        let parsed: SearchArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!(
                    "search_code arguments did not match the expected shape ({e}). \
                     'query' (string) is required."
                ));
            }
        };
        let top_k = parsed.top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K);
        let mode = parsed.mode.as_deref().unwrap_or("semantic");

        let Some(session) = self.session().await else {
            return ToolResult::ok(FALLBACK_HINT.to_string());
        };

        let (tool, params) =
            match build_call(mode, &parsed.query, top_k, session.index_id.as_deref()) {
                Ok(call) => call,
                Err(msg) => return ToolResult::ok(msg),
            };

        let mut client = session.client.lock().await;
        let result = match client.call_tool(tool, params).await {
            Ok(v) => v,
            Err(e) => {
                warn!(tool, error = %e, "search_code: trusty-search call failed (fail-open)");
                return ToolResult::ok(format!("trusty-search call failed. {FALLBACK_HINT}"));
            }
        };

        if is_mcp_error(&result) {
            let detail = mcp_text(&result).unwrap_or_default();
            warn!(
                tool,
                detail, "search_code: trusty-search returned an error (fail-open)"
            );
            return ToolResult::ok(format!(
                "trusty-search could not serve this query (its index may still be building). \
                 {FALLBACK_HINT}"
            ));
        }

        match mcp_text(&result) {
            Some(text) => ToolResult::ok(format!("trusty-search ({mode}) results:\n{text}")),
            None => ToolResult::ok(format!(
                "trusty-search returned no results. {FALLBACK_HINT}"
            )),
        }
    }
}

#[cfg(test)]
#[path = "trusty_search_tests.rs"]
mod tests;
