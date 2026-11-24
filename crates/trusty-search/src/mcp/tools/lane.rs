//! Per-lane search: the readiness check and `search.query` params shared by
//! the MCP lane tools and the CLI lane verbs (#138, #11625).
//!
//! Why: `search_lexical`, `search_semantic` and `search_kg` (MCP) and
//! `trusty-search lexical|semantic|kg` (CLI) must send one body and refuse on
//! one rule. The daemon does not refuse `stage=graph` on a KG that is not
//! ready, so the caller's check is the only guard.
//! What: [`SearchLane`] names the lane; [`lane_query_params`] checks the
//! status snapshot and builds the `{index_id, body}` params; [`LaneError`]
//! carries a refusal.
//! Test: `tests_lane.rs` (MCP) and `search_lane_verbs_tests.rs` (CLI).

use serde_json::Value;

use super::types::{optional_bool, DispatchError};

/// Why a lane query was refused before any `search.query` was sent.
#[derive(Debug, Clone, PartialEq)]
pub enum LaneError {
    /// The lane's prerequisite capability is absent from the status snapshot,
    /// or no snapshot was supplied (fail closed).
    NotReady {
        /// Human-readable refusal naming the stage and a fallback.
        message: String,
        /// The status `stages` object, or `{}` when absent.
        current_stages: Value,
        /// MCP tool names likely to succeed instead.
        suggested_tools: Vec<&'static str>,
    },
    /// A caller argument has the wrong type.
    InvalidParams(String),
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady { message, .. } => write!(f, "stage not ready: {message}"),
            Self::InvalidParams(m) => write!(f, "invalid parameters: {m}"),
        }
    }
}

impl std::error::Error for LaneError {}

impl From<LaneError> for DispatchError {
    fn from(e: LaneError) -> Self {
        match e {
            LaneError::NotReady {
                message,
                current_stages,
                suggested_tools,
            } => DispatchError::StageNotReady {
                message,
                current_stages,
                suggested_tools,
            },
            LaneError::InvalidParams(m) => DispatchError::InvalidParams(m),
        }
    }
}

/// A shared argument reader's refusal as a [`LaneError`].
fn invalid(e: DispatchError) -> LaneError {
    match e {
        DispatchError::InvalidParams(m) => LaneError::InvalidParams(m),
        other => LaneError::InvalidParams(format!("{other:?}")),
    }
}

/// Refuse `lane` unless `status` advertises its prerequisite capability.
///
/// Fails closed: a lane with a prerequisite refuses when `status` is `None`
/// or carries no `search_capabilities` array.
fn check_ready(lane: SearchLane, index_id: &str, status: Option<&Value>) -> Result<(), LaneError> {
    let Some(required) = lane.required_capability() else {
        return Ok(());
    };
    let caps: Vec<String> = status
        .and_then(|s| s.get("search_capabilities"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if caps.iter().any(|c| c == required) {
        return Ok(());
    }
    let stages = status
        .and_then(|s| s.get("stages"))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    let suggested = lane.suggested_fallback_tools(&caps);
    let message = format!(
        "{tool} requires Stage {stage_num} ({stage_name}), which is not yet \
         ready on index '{index_id}'. Current stages: {stages_summary}. \
         Suggested: use {suggested_list}, or wait for the reindex to complete.",
        tool = lane.tool_name(),
        stage_num = lane.stage_number(),
        stage_name = lane.stage_label(),
        stages_summary = summarise_stages(&stages),
        suggested_list = suggested.join(", "),
    );
    Err(LaneError::NotReady {
        message,
        current_stages: stages,
        suggested_tools: suggested,
    })
}

/// The `search.query` params for `lane`, after the readiness check.
///
/// Why: MCP and the CLI must refuse on the same rule and send the same body
/// (#11625); the daemon itself does not refuse a graph query on a KG that is
/// not ready.
/// What: refuses through `check_ready` when the lane needs a capability that
/// `status` (the `search.index.status` result) lacks — a missing snapshot or
/// a missing `search_capabilities` counts as lacking it. Otherwise builds the
/// body from the MCP-shaped `args`: `query` (string or object), the lane's
/// pinned `stage` and `expand_graph`, and the optional `top_k`, branch
/// fields, `mode`, `exclude_archived`, `refine_query`, `path_prefix`,
/// `repos`, `compact` and lexical-lane pair. Returns
/// `{"index_id": index_id, "body": body}`.
/// Test: `search_kg_tool_returns_stage_not_ready_when_stage_3_missing`,
/// `a_not_ready_lane_sends_no_query`, `each_lane_verb_pins_its_stage`.
pub fn lane_query_params(
    lane: SearchLane,
    index_id: &str,
    status: Option<&Value>,
    args: &Value,
) -> Result<Value, LaneError> {
    check_ready(lane, index_id, status)?;
    let mut body = match args.get("query") {
        Some(v @ Value::Object(_)) => v.clone(),
        other => serde_json::json!({ "text": other.and_then(Value::as_str).unwrap_or_default() }),
    };
    // The lane pin always overwrites a caller-set `stage`, so a lexical call
    // never silently runs the full hybrid pipeline.
    if let Some(stage_str) = lane.stage_serde_value() {
        body["stage"] = Value::String(stage_str.into());
    } else {
        body.as_object_mut().map(|m| m.remove("stage"));
    }
    body["expand_graph"] = Value::Bool(lane.expand_graph_default());
    if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
        body["top_k"] = Value::from(k);
    }
    for key in ["branch_files", "branch_boost", "repos"] {
        if let Some(v) = args.get(key) {
            body[key] = v.clone();
        }
    }
    // #147 refine_query (KG lane only reads it), #3401 path_prefix.
    for key in ["branch", "mode", "refine_query", "path_prefix"] {
        if let Some(s) = args.get(key).and_then(Value::as_str) {
            body[key] = Value::String(s.to_string());
        }
    }
    let ea = optional_bool(
        args,
        "exclude_archived",
        super::search::EXCLUDE_ARCHIVED_HINT,
    )
    .map_err(invalid)?;
    if let Some(ea) = ea {
        body["exclude_archived"] = Value::Bool(ea);
    }
    // #7676: pin the daemon's compact flag so `compact_snippet` is present.
    if super::compact::wants_compact(args).map_err(invalid)? {
        body["compact"] = Value::Bool(true);
    }
    // #9258: the lexical-lane pair, type-checked.
    super::lexical_args::forward(args, &mut body).map_err(invalid)?;
    Ok(serde_json::json!({ "index_id": index_id, "body": body }))
}

/// Per-lane router (issue #138).
///
/// Why: maps the four MCP tool names to their canonical lane combination
/// without scattering the policy across the dispatcher arms.
/// What: a unit-like enum that knows its `SearchStage` serde value,
/// `expand_graph` default, prerequisite `search_capabilities` entry, and
/// human-readable stage label / number for STAGE_NOT_READY messages.
/// Test: `search_lexical_tool_routes_to_lexical_stage_only`,
/// `search_kg_tool_routes_to_graph_stage_when_stage_3_ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLane {
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
    pub fn tool_name(self) -> &'static str {
        match self {
            SearchLane::Lexical => "search_lexical",
            SearchLane::Semantic => "search_semantic",
            SearchLane::Graph => "search_kg",
            SearchLane::All => "search_all",
        }
    }

    /// Serialised value for `SearchQuery::stage`, or `None` for adaptive
    /// (no pin).
    pub fn stage_serde_value(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical => Some("lexical"),
            SearchLane::Semantic => Some("semantic"),
            SearchLane::Graph => Some("graph"),
            SearchLane::All => None,
        }
    }

    pub fn expand_graph_default(self) -> bool {
        matches!(self, SearchLane::Graph | SearchLane::All)
    }

    /// Required entry in `search_capabilities` (returned by
    /// `GET /indexes/:id/status`). `None` for lanes that always work.
    pub fn required_capability(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical | SearchLane::All => None,
            SearchLane::Semantic => Some("vector"),
            SearchLane::Graph => Some("kg"),
        }
    }

    pub fn stage_number(self) -> u8 {
        match self {
            SearchLane::Lexical => 1,
            SearchLane::Semantic => 2,
            SearchLane::Graph => 3,
            SearchLane::All => 0,
        }
    }

    pub fn stage_label(self) -> &'static str {
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
    pub fn suggested_fallback_tools(self, caps: &[String]) -> Vec<&'static str> {
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
pub fn summarise_stages(stages: &Value) -> String {
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
