//! Unit tests for pure helper functions in `mcp/mod.rs`.
//!
//! Why: Extracted from `mod.rs` to keep that file within its frozen line-cap
//! budget (#610). Tests for `index_id_or_default`, `build_query`, and the
//! MCP client timeout constant live here.
//! What: Pure-logic tests; no I/O, no tokio runtime needed.
//! Test: `cargo test -p trusty-analyze`.

use super::{build_query, index_id_or_default};

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

/// Why: the MCP client timeout has two floors and used to clear only one.
/// Issue #528 fixed a 30 s timeout that silently killed slow LLM responses,
/// settling on a flat 150 s. #6018 then gave the diagnostics endpoint a 180 s
/// deadline and left that 150 s alone, so `run_diagnostics` over a large index
/// wrote its structured 504 into a socket the client had already abandoned —
/// the body-less transport failure the whole fix exists to remove.
/// What: asserts the live client timeout exceeds BOTH the OpenRouter 120 s
/// ceiling and the diagnostics handler budget (deadline + grace).
/// Test: this test. Fails against the pre-fix flat 150 s, which sits below the
/// 210 s default handler budget.
#[test]
fn mcp_client_timeout_outlives_the_daemon_and_openrouter() {
    // The OpenRouter request timeout in trusty-common/src/chat.rs is 120 s.
    const OPENROUTER_CEILING: std::time::Duration = std::time::Duration::from_secs(120);

    let client = crate::core::mcp_client_timeout();
    assert!(
        client > OPENROUTER_CEILING,
        "MCP client timeout {client:?} must exceed the OpenRouter ceiling \
         {OPENROUTER_CEILING:?} so a slow deep_analysis is not killed at the \
         transport layer (#528)"
    );

    let handler = crate::core::diagnostics_handler_budget();
    assert!(
        client > handler,
        "MCP client timeout {client:?} must exceed the diagnostics handler \
         budget {handler:?}, or run_diagnostics answers into an abandoned \
         socket (#6018)"
    );
}
