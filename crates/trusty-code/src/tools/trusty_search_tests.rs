//! Tests for the `search_code` trusty-search discovery tool.
//!
//! Why: pins the pure index-resolution/parsing helpers and the fail-open
//! contract (a missing daemon must yield a SUCCESSFUL "use grep/glob" result,
//! never an error) so a regression in either can't silently derail the agent
//! loop.
//! What: covers `pick_index`, `mcp_text`, `is_mcp_error`, the tool schema, and
//! `execute`'s daemon-absent / malformed-args paths. The live search lanes are
//! not exercised here (they require a running trusty-search daemon + index);
//! their routing is covered by `build_call`'s construction being unit-visible.
//! Test: this module.

use serde_json::json;

use super::*;

/// An index detail entry as returned by `list_indexes?details=true`.
fn index_entry(id: &str, root_path: &str) -> Value {
    json!({ "id": id, "root_path": root_path, "size_bytes": 0 })
}

// ── pick_index ──────────────────────────────────────────────────────────────

/// Why: the common case — the project root is exactly a registered index root.
#[test]
fn pick_index_prefers_exact_match() {
    let tmp = std::env::temp_dir();
    let root = tmp.to_str().expect("utf8 temp dir");
    let indexes = vec![
        index_entry("other", "/nonexistent/elsewhere"),
        index_entry("mine", root),
    ];
    assert_eq!(pick_index(&indexes, &tmp).as_deref(), Some("mine"));
}

/// Why: a subdirectory/worktree of a registered repo must resolve to that
/// repo's index (longest ancestor wins over a shallower one).
#[test]
fn pick_index_falls_back_to_longest_ancestor() {
    let base = std::env::temp_dir();
    let nested = base.join("tcode-pick-index-nested");
    std::fs::create_dir_all(&nested).expect("mkdir nested");

    let shallow = base.to_str().expect("utf8");
    let deep = nested.to_str().expect("utf8");
    // Register both an ancestor (temp dir) and the exact nested dir; the exact
    // match must win. Then drop the exact entry and the ancestor must be used.
    let with_exact = vec![index_entry("ancestor", shallow), index_entry("exact", deep)];
    assert_eq!(pick_index(&with_exact, &nested).as_deref(), Some("exact"));

    let ancestor_only = vec![index_entry("ancestor", shallow)];
    assert_eq!(
        pick_index(&ancestor_only, &nested).as_deref(),
        Some("ancestor")
    );

    let _ = std::fs::remove_dir_all(&nested);
}

/// Why: no registered index covers the project → `None`, so the search lanes
/// fail open rather than guess a wrong index.
#[test]
fn pick_index_returns_none_when_uncovered() {
    let indexes = vec![index_entry("elsewhere", "/nonexistent/elsewhere")];
    assert_eq!(
        pick_index(&indexes, Path::new("/nonexistent/project")),
        None
    );
}

// ── mcp_text / is_mcp_error ─────────────────────────────────────────────────

/// Why: every trusty-search payload arrives as `content[0].text`; the extractor
/// must return that inner string.
#[test]
fn mcp_text_extracts_content() {
    let result = json!({ "content": [{ "type": "text", "text": "{\"indexes\":[]}" }] });
    assert_eq!(mcp_text(&result).as_deref(), Some("{\"indexes\":[]}"));

    let empty = json!({ "isError": false });
    assert_eq!(mcp_text(&empty), None);
}

/// Why: `STAGE_NOT_READY` and friends arrive as in-band `isError: true`
/// content (not a JSON-RPC error), so the flag must be detected to fail open.
#[test]
fn is_mcp_error_detects_error_flag() {
    assert!(is_mcp_error(&json!({ "isError": true, "content": [] })));
    assert!(!is_mcp_error(&json!({ "isError": false })));
    assert!(!is_mcp_error(&json!({ "content": [] })));
}

// ── build_call ──────────────────────────────────────────────────────────────

/// Why: each mode must route to trusty-search's real lane with the right
/// argument shape; a wrong tool name or missing arg would silently 400.
#[test]
fn build_call_routes_each_mode() {
    let (tool, params) = build_call("semantic", "how does auth work", 5, Some("idx")).expect("ok");
    assert_eq!(tool, "search_semantic");
    assert_eq!(params["index_id"], "idx");
    assert_eq!(params["query"], "how does auth work");
    assert_eq!(params["top_k"], 5);

    let (tool, params) = build_call("symbol", "validate_token", 5, Some("idx")).expect("ok");
    assert_eq!(tool, "search_kg");
    assert_eq!(params["query"], "validate_token");

    // grep works without an index (the daemon fans out across all indexes).
    let (tool, params) = build_call("grep", "TODO", 7, None).expect("ok");
    assert_eq!(tool, "grep");
    assert_eq!(params["pattern"], "TODO");
    assert_eq!(params["max_results"], 7);
    assert!(params.get("index_id").is_none());
}

/// Why: semantic/symbol need an index; without one the call is a fail-open
/// message steering the model back to the local tools.
#[test]
fn build_call_semantic_without_index_is_fallback() {
    let err = build_call("semantic", "q", 5, None).expect_err("needs index");
    assert!(err.contains("grep"), "must steer to local tools: {err}");
}

// ── schema ──────────────────────────────────────────────────────────────────

/// Why: the model must see `query` as required and `mode` as an enum defaulting
/// to semantic.
#[test]
fn schema_advertises_mode_and_query() {
    let tool = TrustySearchTool::new(std::env::temp_dir());
    let schema = tool.schema();
    assert_eq!(schema["function"]["name"], SEARCH_CODE_TOOL_NAME);
    let params = &schema["function"]["parameters"];
    let required: Vec<&str> = params["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, vec!["query"]);
    assert_eq!(params["properties"]["mode"]["default"], "semantic");
    assert_eq!(params["additionalProperties"], json!(false));
}

// ── execute (fail-open) ─────────────────────────────────────────────────────

/// Why: the daemon-absent path is the whole point of fail-open — a missing
/// binary must yield a SUCCESSFUL "use grep/glob" result, never an error.
#[tokio::test]
async fn execute_fail_open_when_binary_absent() {
    let tool = TrustySearchTool::new(std::env::temp_dir())
        .with_binary("trusty-search-definitely-not-installed-xyzzy");
    let result = tool.execute(json!({ "query": "how does auth work" })).await;
    assert!(!result.is_error(), "must not surface as a tool error");
    assert!(
        result.content().contains("grep"),
        "must steer to local tools: {}",
        result.content()
    );
}

/// Why: malformed args (missing `query`) must be a recoverable, non-fatal error
/// the model can correct — mirrors `recall_session`'s contract.
#[tokio::test]
async fn execute_rejects_malformed_args_recoverably() {
    let tool = TrustySearchTool::new(std::env::temp_dir())
        .with_binary("trusty-search-definitely-not-installed-xyzzy");
    let result = tool.execute(json!({ "mode": "semantic" })).await; // no query
    assert!(result.is_error());
    assert!(!result.is_fatal());
}
