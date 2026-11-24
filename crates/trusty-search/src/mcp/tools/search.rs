//! Search tool arms: `search`, `search_lexical`, `search_semantic`,
//! `search_kg`, `search_all`, and `search_similar`.
//!
//! Why: the six search-related tool arms — and the lane-routing machinery
//! they share (`SearchLane`, `run_lane_search`, `summarise_stages`) — form
//! a cohesive, self-contained group. Isolating them here keeps each file
//! focused and makes it easy to evolve lane behaviour without touching index
//! management or admin logic.
//! What: exports `dispatch_search_tool` (called from `call_tool` in `mod.rs`)
//! and `run_lane_search`. `SearchLane` and the shared readiness check and
//! body builder live in `lane.rs` (#11625).
//! Test: `tests_lane.rs` covers every lane's happy path, stage-not-ready
//! path, and routing shape; `tests.rs` covers `search_all` fan-out and
//! `search_similar`.

use serde_json::Value;

use super::{
    compact,
    types::{optional_bool, require_str, DispatchError},
    McpServer,
};
use crate::service::rpc::queries::{METHOD_QUERY, METHOD_QUERY_ALL, METHOD_SIMILAR};
use crate::service::rpc::reads::METHOD_INDEX_STATUS;

pub(super) use super::lane::{lane_query_params, SearchLane};

/// What `exclude_archived: true` does, for the rejection message (#7927).
///
/// Shared by the `search` arm and [`McpServer::run_lane_search`] so the two
/// spellings of the same flag cannot describe themselves differently.
pub(super) const EXCLUDE_ARCHIVED_HINT: &str =
    "true drops archived/deprecated/legacy chunks instead of downranking them";

/// Route one of the six search-related tool names to the correct daemon call.
///
/// Why: factoring search dispatch out of the monolithic `call_tool` match
/// keeps each logical group in its own file without needing to split `McpServer`
/// into multiple `impl` blocks that live in separate files (which Rust allows
/// but makes navigation harder).
/// What: returns `None` when `tool` is not a search tool (so `call_tool` can
/// try the next group), `Some(Ok(value))` on success, or
/// `Some(Err(DispatchError))` on failure.
/// Test: all search-tool tests in `tests.rs` and `tests_lane.rs`.
pub(super) async fn dispatch_search_tool(
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, DispatchError>> {
    match tool {
        // Issue #138 — per-lane search tools.
        "search_lexical" => Some(server.run_lane_search(args, SearchLane::Lexical).await),
        "search_semantic" => Some(server.run_lane_search(args, SearchLane::Semantic).await),
        "search_kg" => Some(server.run_lane_search(args, SearchLane::Graph).await),
        "search_all" => {
            // Polymorphic for back-compat (issue #138):
            //   * with `index_id` (explicit or pinned, #1373) → per-index full
            //     hybrid (alias for `search`; matches the #138 ticket spec).
            //   * without any index → cross-project fan-out (legacy issue #10
            //     behaviour preserved).
            // The pinned-index resolution (#1373) means a trusty-mpm session
            // scoped to one project never silently sweeps every registered
            // index — it runs the per-index hybrid against its own index.
            // #9168: a `project` names an index too, so it never fans out.
            if server.names_a_target(args) {
                return Some(server.run_lane_search(args, SearchLane::All).await);
            }
            let query = match require_str(args, "query") {
                Ok(q) => q,
                Err(e) => return Some(Err(e)),
            };
            let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(10);
            // #7676: compact mode drops `content` outright, so asking for the
            // full chunk alongside it is a contradiction — compact wins, and
            // the fan-out's `compact: !full_content` then guarantees the
            // `compact_snippet` that replaces it.
            let want_compact = match compact::wants_compact(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // #7927: a wrong-typed `full_content` is rejected, not read as
            // `false` — a dropped `true` returns snippets where the caller
            // asked for whole chunks.
            let asked_full_content = match optional_bool(
                args,
                "full_content",
                "true includes full chunk content in each fan-out hit",
            ) {
                Ok(v) => v.unwrap_or(false),
                Err(e) => return Some(Err(e)),
            };
            let full_content = !want_compact && asked_full_content;
            let mut body = serde_json::json!({
                "query": query,
                "top_k": top_k,
                "full_content": full_content,
            });
            // Issue #2845: forward the optional fan-out bounding overrides so
            // a caller can request serial / a custom cap per call without
            // restarting the daemon.
            // #7927: same strict read as every other flag.
            match optional_bool(
                args,
                "serial",
                "true runs the per-index fan-out searches one at a time",
            ) {
                Ok(Some(serial)) => body["serial"] = Value::Bool(serial),
                Ok(None) => {}
                Err(e) => return Some(Err(e)),
            }
            if let Some(n) = args.get("max_fanout_concurrency").and_then(Value::as_u64) {
                body["max_fanout_concurrency"] = Value::from(n);
            }
            // Issue #3401: forward optional path/repo scoping to every
            // per-index search in the fan-out.
            if let Some(pp) = args.get("path_prefix").and_then(Value::as_str) {
                body["path_prefix"] = Value::String(pp.to_string());
            }
            if let Some(repos) = args.get("repos") {
                body["repos"] = repos.clone();
            }
            // #9258: the lexical-lane pair reaches every per-index search.
            if let Err(e) = super::lexical_args::forward(args, &mut body) {
                return Some(Err(e));
            }
            // #9168: `POST /search`'s socket twin.
            let mut resp = match server.call(METHOD_QUERY_ALL, body).await {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if want_compact {
                compact::apply(&mut resp);
            }
            Some(Ok(resp))
        }
        "search" => {
            // Default `index_id` to the session's pinned index when omitted
            // (#1373); an explicit caller-supplied id still wins, and #9168 adds
            // `project`, resolved by the daemon, between the two.
            let index_id = match server.resolve_target(args).await {
                Ok(Some(v)) => v,
                Err(e) => return Some(Err(e)),
                // #6317: nothing resolved — answer with the indexes that exist
                // plus a retry hint, as a success. #5213's error named
                // `list_indexes`; this returns what that call would have said.
                Ok(None) => {
                    return Some(super::index_directory::index_directory(server, "search").await)
                }
            };
            // Accept the spec form `{query: string, top_k?: int}` and
            // also a pre-built `{query: object}` body for callers that
            // need to pass advanced search parameters directly.
            let mut body = match args.get("query") {
                Some(v @ Value::Object(_)) => v.clone(),
                Some(Value::String(text)) => {
                    let mut b = serde_json::json!({ "text": text });
                    if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
                        b["top_k"] = Value::from(k);
                    }
                    // Issue #122 — branch-aware search: forward optional
                    // branch context fields through to the daemon body so
                    // MCP callers can boost on-branch matches without
                    // building the `{query: object}` shape themselves.
                    if let Some(bf) = args.get("branch_files") {
                        b["branch_files"] = bf.clone();
                    }
                    if let Some(bb) = args.get("branch_boost") {
                        b["branch_boost"] = bb.clone();
                    }
                    if let Some(br) = args.get("branch").and_then(Value::as_str) {
                        b["branch"] = Value::String(br.to_string());
                    }
                    // Issue #77 (final design): forward optional `mode`
                    // ("code" | "text" | "data"; default "code") so the
                    // daemon applies the matching penalty matrix.
                    if let Some(m) = args.get("mode").and_then(Value::as_str) {
                        b["mode"] = Value::String(m.to_string());
                    }
                    // Issue #74: forward optional `exclude_archived`
                    // (default false) so callers can hard-filter archived
                    // / deprecated / legacy chunks instead of only
                    // downranking them.
                    // #7927: rejected when wrong-typed — a dropped `true`
                    // returns archived hits the caller asked to exclude.
                    match optional_bool(args, "exclude_archived", EXCLUDE_ARCHIVED_HINT) {
                        Ok(Some(ea)) => b["exclude_archived"] = Value::Bool(ea),
                        Ok(None) => {}
                        Err(e) => return Some(Err(e)),
                    }
                    // Issue #3401: forward optional path/repo scoping —
                    // applied server-side BEFORE top_k truncation in every
                    // retrieval lane.
                    if let Some(pp) = args.get("path_prefix").and_then(Value::as_str) {
                        b["path_prefix"] = Value::String(pp.to_string());
                    }
                    if let Some(repos) = args.get("repos") {
                        b["repos"] = repos.clone();
                    }
                    b
                }
                _ => {
                    return Some(Err(DispatchError::InvalidParams(
                        "missing or invalid 'query' (expected string or object)".into(),
                    )))
                }
            };
            // #7676: compact hits need the snippet that replaces `content`,
            // so pin the daemon's own compact flag on. A `{query: object}`
            // caller that set `compact: false` is overridden, not obeyed —
            // the tool-level flag is the one the caller just asked for.
            let want_compact = match compact::wants_compact(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if want_compact {
                body["compact"] = Value::Bool(true);
            }
            // #9258: forwarded for both `query` shapes.
            if let Err(e) = super::lexical_args::forward(args, &mut body) {
                return Some(Err(e));
            }
            // Issue #4715: scoped call so a not-found on the session's
            // advertised index surfaces as INDEX_NOT_READY, not "unknown index".
            let query_text = body
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let params = serde_json::json!({ "index_id": index_id, "body": body });
            let mut resp = match server
                .call_scoped(METHOD_QUERY, params, Some(&index_id))
                .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Mirror the daemon's per-query INFO log (issue #125) so the
            // MCP transport surfaces the same query/intent/latency line.
            let log_intent = resp
                .get("intent")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let log_latency = resp.get("latency_ms").and_then(Value::as_u64).unwrap_or(0);
            let log_results = resp
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            tracing::info!(
                index_id = %index_id,
                intent = %log_intent,
                latency_ms = log_latency,
                results = log_results,
                query = %crate::truncate_at_char_boundary(&query_text, 80),
                "search"
            );
            if want_compact {
                compact::apply(&mut resp);
            }
            Some(Ok(resp))
        }
        "search_similar" => {
            // Code-to-code similarity (issue #31). Index defaults to "default"
            // so simple call sites don't need to specify it.
            let index_id = args
                .get("index")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let file = match require_str(args, "file") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut body = serde_json::json!({ "file": file });
            if let Some(func) = args.get("function").and_then(Value::as_str) {
                body["function"] = Value::String(func.to_string());
            }
            if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
                body["top_k"] = Value::from(k);
            }
            let params = serde_json::json!({ "index_id": index_id, "body": body });
            Some(server.call(METHOD_SIMILAR, params).await)
        }
        _ => None,
    }
}

impl McpServer {
    /// Common dispatcher for the four per-lane search tools (issue #138).
    ///
    /// Why: every per-lane tool follows the same recipe — parse `index_id`
    /// plus `query`, optionally pre-check `search_capabilities` against
    /// the lane's prerequisite stage, build a `SearchQuery` body with the
    /// correct `stage` / `expand_graph` pinning, and POST to the
    /// per-index search endpoint. Factoring this out keeps `call_tool`
    /// readable and guarantees all four tools share identical schema,
    /// error shape, and logging.
    ///
    /// What: validates args; for lanes that require a stage beyond Stage
    /// 1 (`Semantic` needs `vector`, `Graph` needs `kg`), calls
    /// `search.index.status` and inspects `search_capabilities`. If the
    /// prerequisite lane is missing, returns
    /// `DispatchError::StageNotReady` carrying a human-readable message,
    /// the full stages snapshot, and a `suggested_tools` retry hint.
    /// Otherwise constructs the search body (including lane-specific
    /// `stage` and `expand_graph` settings), forwards optional caller
    /// fields (`top_k`, `mode`, branch-aware boost, archive filter),
    /// calls `search.query`, and mirrors the daemon's per-query INFO log.
    ///
    /// Test: unit tests in `tests_lane.rs` exercise each tool's happy
    /// path, stage-not-ready path, and routing shape.
    pub(super) async fn run_lane_search(
        &self,
        args: &Value,
        lane: SearchLane,
    ) -> Result<Value, DispatchError> {
        // Default `index_id` to the session's pinned index when omitted (#1373);
        // an explicit caller-supplied id still wins.
        // #6317: with neither, return the index directory as a success rather
        // than #5213's error — these lanes are reads.
        // #9168: `project` resolves through the daemon between the two.
        let Some(index_id) = self.resolve_target(args).await? else {
            return super::index_directory::index_directory(self, lane.tool_name()).await;
        };
        let query_text = require_str(args, "query")?;
        // #7927: validate the caller's flags before the pre-flight probe
        // below, so a wrong-typed argument answers as a parameter error
        // instead of costing a daemon round trip first.
        optional_bool(args, "exclude_archived", EXCLUDE_ARCHIVED_HINT)?;

        // #11625: the readiness check and body builder live in `lane.rs`,
        // shared with the CLI lane verbs. Lexical and `search_all` need no
        // probe; #4715: the probe is index-scoped.
        let status = match lane.required_capability() {
            Some(_) => Some(
                self.call_scoped(
                    METHOD_INDEX_STATUS,
                    serde_json::json!({ "index_id": index_id }),
                    Some(&index_id),
                )
                .await?,
            ),
            None => None,
        };
        let params = lane_query_params(lane, &index_id, status.as_ref(), args)?;
        let want_compact = compact::wants_compact(args)?;
        let mut resp = self
            .call_scoped(METHOD_QUERY, params, Some(&index_id))
            .await?;
        // Mirror the daemon's per-query INFO log.
        let log_intent = resp
            .get("intent")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let log_latency = resp.get("latency_ms").and_then(Value::as_u64).unwrap_or(0);
        let log_results = resp
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let tool_name = lane.tool_name();
        tracing::info!(
            tool = %tool_name,
            index_id = %index_id,
            intent = %log_intent,
            latency_ms = log_latency,
            results = log_results,
            query = %crate::truncate_at_char_boundary(query_text, 80),
            "search"
        );
        if want_compact {
            compact::apply(&mut resp);
        }
        Ok(resp)
    }
}
