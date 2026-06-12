//! Search tool arms: `search`, `search_lexical`, `search_semantic`,
//! `search_kg`, `search_all`, and `search_similar`.
//!
//! Why: the six search-related tool arms — and the lane-routing machinery
//! they share (`SearchLane`, `run_lane_search`, `summarise_stages`) — form
//! a cohesive, self-contained group. Isolating them here keeps each file
//! focused and makes it easy to evolve lane behaviour without touching index
//! management or admin logic.
//! What: exports `dispatch_search_tool` (called from `call_tool` in `mod.rs`)
//! and the private helpers `run_lane_search`, `SearchLane`, `summarise_stages`.
//! Test: `tests_lane.rs` covers every lane's happy path, stage-not-ready
//! path, and routing shape; `tests.rs` covers `search_all` fan-out and
//! `search_similar`.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};

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
            //   * with `index_id`  → per-index full hybrid (alias for
            //     `search`; matches the #138 ticket spec).
            //   * without `index_id` → cross-project fan-out (legacy
            //     issue #10 behaviour preserved).
            if args.get("index_id").and_then(Value::as_str).is_some() {
                return Some(server.run_lane_search(args, SearchLane::All).await);
            }
            let query = match require_str(args, "query") {
                Ok(q) => q,
                Err(e) => return Some(Err(e)),
            };
            let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(10);
            let full_content = args
                .get("full_content")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let body = serde_json::json!({
                "query": query,
                "top_k": top_k,
                "full_content": full_content,
            });
            Some(server.post("/search", &body).await)
        }
        "search" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Accept the spec form `{query: string, top_k?: int}` and
            // also a pre-built `{query: object}` body for callers that
            // need to pass advanced search parameters directly.
            let body = match args.get("query") {
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
                    if let Some(ea) = args.get("exclude_archived").and_then(Value::as_bool) {
                        b["exclude_archived"] = Value::Bool(ea);
                    }
                    b
                }
                _ => {
                    return Some(Err(DispatchError::InvalidParams(
                        "missing or invalid 'query' (expected string or object)".into(),
                    )))
                }
            };
            let resp = match server
                .post(&format!("/indexes/{index_id}/search"), &body)
                .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Mirror the daemon's per-query INFO log (issue #125) so the
            // MCP transport surfaces the same query/intent/latency line.
            let query_text = body.get("text").and_then(Value::as_str).unwrap_or_default();
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
                query = %&query_text[..query_text.len().min(80)],
                "search"
            );
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
            Some(
                server
                    .post(&format!("/indexes/{index_id}/search_similar"), &body)
                    .await,
            )
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
    /// 1 (`Semantic` needs `vector`, `Graph` needs `kg`), fetches `GET
    /// /indexes/:id/status` and inspects `search_capabilities`. If the
    /// prerequisite lane is missing, returns
    /// `DispatchError::StageNotReady` carrying a human-readable message,
    /// the full stages snapshot, and a `suggested_tools` retry hint.
    /// Otherwise constructs the search body (including lane-specific
    /// `stage` and `expand_graph` settings), forwards optional caller
    /// fields (`top_k`, `mode`, branch-aware boost, archive filter),
    /// POSTs the request, and mirrors the daemon's per-query INFO log.
    ///
    /// Test: unit tests in `tests_lane.rs` exercise each tool's happy
    /// path, stage-not-ready path, and routing shape.
    pub(super) async fn run_lane_search(
        &self,
        args: &Value,
        lane: SearchLane,
    ) -> Result<Value, DispatchError> {
        let index_id = require_str(args, "index_id")?;
        let query_text = require_str(args, "query")?;

        // Pre-flight stage check for lanes that need Stage 2 or Stage 3.
        // The lexical and `search_all` tools always work — they degrade
        // gracefully through the daemon's adaptive routing.
        if let Some(required) = lane.required_capability() {
            let status = self.get(&format!("/indexes/{index_id}/status")).await?;
            let caps: Vec<String> = status
                .get("search_capabilities")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if !caps.iter().any(|c| c == required) {
                let stages = status
                    .get("stages")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let stages_summary = summarise_stages(&stages);
                let suggested = lane.suggested_fallback_tools(&caps);
                let suggested_list = suggested.join(", ");
                let stage_label = lane.stage_label();
                let message = format!(
                    "{tool} requires Stage {stage_num} ({stage_name}), which is not yet \
                     ready on index '{index_id}'. Current stages: {stages_summary}. \
                     Suggested: use {suggested_list}, or wait for the reindex to complete.",
                    tool = lane.tool_name(),
                    stage_num = lane.stage_number(),
                    stage_name = stage_label,
                );
                return Err(DispatchError::StageNotReady {
                    message,
                    current_stages: stages,
                    suggested_tools: suggested,
                });
            }
        }

        // Build the SearchQuery body honouring the lane pin plus optional
        // caller-supplied fields. The bare-string `query` form is the
        // primary shape; advanced callers may still pass `query` as an
        // object to override defaults (kept for symmetry with `search`).
        let mut body = match args.get("query") {
            Some(v @ Value::Object(_)) => v.clone(),
            _ => serde_json::json!({ "text": query_text }),
        };
        // Lane-specific `stage` and `expand_graph` pinning. Always
        // overwrites whatever the caller may have set so the LLM's tool
        // selection is honoured (a `search_lexical` call must NOT silently
        // run the full hybrid pipeline because the caller forgot to clear
        // `stage`).
        if let Some(stage_str) = lane.stage_serde_value() {
            body["stage"] = Value::String(stage_str.into());
        } else {
            body.as_object_mut().map(|m| m.remove("stage"));
        }
        body["expand_graph"] = Value::Bool(lane.expand_graph_default());

        // Pass-through optional caller fields.
        if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
            body["top_k"] = Value::from(k);
        }
        if let Some(bf) = args.get("branch_files") {
            body["branch_files"] = bf.clone();
        }
        if let Some(bb) = args.get("branch_boost") {
            body["branch_boost"] = bb.clone();
        }
        if let Some(br) = args.get("branch").and_then(Value::as_str) {
            body["branch"] = Value::String(br.to_string());
        }
        if let Some(m) = args.get("mode").and_then(Value::as_str) {
            body["mode"] = Value::String(m.to_string());
        }
        if let Some(ea) = args.get("exclude_archived").and_then(Value::as_bool) {
            body["exclude_archived"] = Value::Bool(ea);
        }
        // Issue #147: pass refine_query through to the search body for the
        // KG lane. The field is ignored by lexical / semantic lanes since
        // `expand_with_kg` only reads it when the graph stage is active.
        if let Some(rq) = args.get("refine_query").and_then(Value::as_str) {
            body["refine_query"] = Value::String(rq.to_string());
        }

        let resp = self
            .post(&format!("/indexes/{index_id}/search"), &body)
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
            query = %&query_text[..query_text.len().min(80)],
            "search"
        );
        Ok(resp)
    }
}

/// Per-lane router (issue #138).
///
/// Why: maps the four MCP tool names to their canonical lane combination
/// without scattering the policy across the dispatcher arms.
/// What: a unit-like enum that knows its `SearchStage` serde value,
/// `expand_graph` default, prerequisite `search_capabilities` entry, and
/// human-readable stage label / number for STAGE_NOT_READY messages.
/// Test: per-lane routing covered by `search_*_tool_routes_to_*_stage` in
/// `tests_lane.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchLane {
    /// BM25 + grep-fallback only. Always available; no prerequisite.
    Lexical,
    /// BM25 + HNSW via RRF. Requires Stage 2 (`vector`).
    Semantic,
    /// BM25 + HNSW + KG expansion. Requires Stage 3 (`kg`).
    Graph,
    /// Full hybrid pipeline; adaptive routing chooses what to run.
    All,
}

impl SearchLane {
    pub(super) fn tool_name(self) -> &'static str {
        match self {
            SearchLane::Lexical => "search_lexical",
            SearchLane::Semantic => "search_semantic",
            SearchLane::Graph => "search_kg",
            SearchLane::All => "search_all",
        }
    }

    /// Serialised value for `SearchQuery::stage`, or `None` for adaptive
    /// (no pin).
    pub(super) fn stage_serde_value(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical => Some("lexical"),
            SearchLane::Semantic => Some("semantic"),
            SearchLane::Graph => Some("graph"),
            SearchLane::All => None,
        }
    }

    pub(super) fn expand_graph_default(self) -> bool {
        matches!(self, SearchLane::Graph | SearchLane::All)
    }

    /// Required entry in `search_capabilities` (returned by
    /// `GET /indexes/:id/status`). `None` for lanes that always work.
    pub(super) fn required_capability(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical | SearchLane::All => None,
            SearchLane::Semantic => Some("vector"),
            SearchLane::Graph => Some("kg"),
        }
    }

    pub(super) fn stage_number(self) -> u8 {
        match self {
            SearchLane::Lexical => 1,
            SearchLane::Semantic => 2,
            SearchLane::Graph => 3,
            SearchLane::All => 0,
        }
    }

    pub(super) fn stage_label(self) -> &'static str {
        match self {
            SearchLane::Lexical => "lexical",
            SearchLane::Semantic => "embeddings",
            SearchLane::Graph => "symbol graph",
            SearchLane::All => "all",
        }
    }

    /// Compute the LLM's retry hint when this lane is unavailable.
    ///
    /// Why: the STAGE_NOT_READY error payload must carry actionable fallback
    /// suggestions so the LLM can retry without a second probe.
    /// What: returns a small ordered list of alternative tools that are likely
    /// to succeed given the current `caps` snapshot.
    /// Test: `search_semantic_tool_returns_stage_not_ready_when_stage_2_missing`
    /// and `search_kg_tool_returns_stage_not_ready_when_stage_3_missing` in
    /// `tests_lane.rs`.
    pub(super) fn suggested_fallback_tools(self, caps: &[String]) -> Vec<&'static str> {
        let has_vector = caps.iter().any(|c| c == "vector");
        match self {
            // Semantic missing: lexical always works; full hybrid degrades
            // to whatever's ready.
            SearchLane::Semantic => vec!["search_lexical", "search_all"],
            // Graph missing: semantic if vector ready, else lexical.
            SearchLane::Graph => {
                if has_vector {
                    vec!["search_semantic", "search_lexical", "search_all"]
                } else {
                    vec!["search_lexical", "search_all"]
                }
            }
            // Lexical / All never fall through this path (no prereq).
            SearchLane::Lexical | SearchLane::All => vec!["search_lexical"],
        }
    }
}

/// Render an `IndexStages` JSON value as a compact debug-style string for
/// the human-readable STAGE_NOT_READY message.
///
/// Why: the `current_stages` JSON snapshot already lives in the structured
/// `_meta` / `error.data` field; the textual message just needs a short
/// summary like `lexical=Ready, semantic=InProgress, graph=Pending` so a
/// human reader (or LLM) can see at a glance which stages are blocking.
/// What: walks the three known stage keys (`lexical`, `semantic`, `graph`)
/// and pulls each one's `.status` field, falling back to `"unknown"` when
/// the field is missing or non-string.
/// Test: `summarise_stages_renders_in_order` in `tests.rs`.
pub(super) fn summarise_stages(stages: &Value) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    for key in ["lexical", "semantic", "graph"] {
        let status = stages
            .get(key)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // Title-case the status for human readability (snake_case → CamelCase).
        let pretty = status
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<String>();
        parts.push(format!("{key}={pretty}"));
    }
    parts.join(", ")
}
