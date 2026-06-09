//! Unit tests for pure helper functions in `mcp/mod.rs`.
//!
//! Why: Extracted from `mod.rs` to keep that file within its frozen line-cap
//! budget (#610). Tests for `index_id_or_default`, `build_query`, and the
//! MCP client timeout constant live here.
//! What: Pure-logic tests; no I/O, no tokio runtime needed.
//! Test: `cargo test -p trusty-analyze`.

use super::{build_query, index_id_or_default, DEEP_ANALYSIS_MCP_TIMEOUT_SECS};

#[test]
fn index_id_or_default_prefers_index_then_alias_then_default() {
    let with_index = serde_json::json!({ "index": "primary" });
    assert_eq!(index_id_or_default(&with_index), "primary");

    let with_alias = serde_json::json!({ "index_id": "alias" });
    assert_eq!(index_id_or_default(&with_alias), "alias");

    let empty = serde_json::json!({});
    assert_eq!(index_id_or_default(&empty), "default");
}

#[test]
fn build_query_skips_missing_keys() {
    let args = serde_json::json!({ "subject": "fn auth", "object": "JWT" });
    let q = build_query(&args, &["subject", "predicate", "object"]);
    // urlencoded space → %20
    assert!(q.starts_with('?'), "expected leading '?', got {q}");
    assert!(q.contains("subject=fn%20auth"), "got {q}");
    assert!(q.contains("object=JWT"), "got {q}");
    assert!(!q.contains("predicate"), "got {q}");
}

/// Why: `find_smells`/`run_diagnostics` gained numeric (`limit`, `offset`) and
/// boolean (`omit_content`) params (#917/#918). `build_query` must encode these
/// correctly so the HTTP call to the analyzer daemon carries the right params.
/// What: passes number and bool values in args; asserts they appear as plain
/// decimal/string in the query string (no URL-encoding needed for these types).
/// Test: this test.
#[test]
fn build_query_handles_numeric_and_bool() {
    let args = serde_json::json!({
        "limit": 100u64,
        "offset": 50u64,
        "omit_content": false,
    });
    let q = build_query(&args, &["limit", "offset", "omit_content"]);
    assert!(q.starts_with('?'), "expected leading '?', got {q}");
    assert!(q.contains("limit=100"), "got {q}");
    assert!(q.contains("offset=50"), "got {q}");
    assert!(q.contains("omit_content=false"), "got {q}");
}

/// Why: integer-valued `limit` and `offset` come from JSON clients as JSON
/// numbers, which `serde_json` parses as `u64` (when they fit). Removing the
/// `as_f64` fallback must not break this common case.
/// What: passes `limit` as a JSON integer (`200u64`) and asserts the query
/// string contains `limit=200`, proving `as_u64` still handles the happy path.
/// Test: this test.
#[test]
fn build_query_integer_limit_parses_correctly() {
    let args = serde_json::json!({ "limit": 200u64 });
    let q = build_query(&args, &["limit"]);
    assert_eq!(
        q, "?limit=200",
        "integer limit must serialise as plain decimal"
    );
}

/// Verify at compile time that the MCP client timeout is strictly greater
/// than OpenRouter's 120 s maximum so deep_analysis calls are never aborted
/// at the MCP transport layer before the daemon's own timeout fires.
///
/// Why: issue #528 — a 30 s MCP timeout silently killed any LLM response
/// taking more than 30 s, even when the daemon and API key were correct.
/// What: compile-time assertion that `DEEP_ANALYSIS_MCP_TIMEOUT_SECS > 120`.
/// Test: this is the test — it fails to compile if the const regresses.
#[test]
fn mcp_client_timeout_exceeds_openrouter_ceiling() {
    // The OpenRouter request timeout in trusty-common/src/chat.rs is 120 s.
    // Our MCP client must allow more than that. Use const assertion so
    // clippy does not flag `assertions_on_constants`.
    const OPENROUTER_CEILING_SECS: u64 = 120;
    const _: () = assert!(
        DEEP_ANALYSIS_MCP_TIMEOUT_SECS > OPENROUTER_CEILING_SECS,
        "DEEP_ANALYSIS_MCP_TIMEOUT_SECS must be > OpenRouter ceiling (120 s)"
    );
}
