//! Unit tests for pure helper functions in `mcp/mod.rs`.
//!
//! Why: Extracted from `mod.rs` to keep that file within its frozen line-cap
//! budget (#610). Tests for `index_id_or_default`, `optional_params`, and the
//! MCP client timeout constant live here.
//! What: Pure-logic tests; no I/O, no tokio runtime needed.
//! Test: `cargo test -p trusty-analyze`.

use super::{index_id_or_default, optional_params};

#[test]
fn index_id_or_default_prefers_index_then_alias_then_default() {
    let with_index = serde_json::json!({ "index": "primary" });
    assert_eq!(index_id_or_default(&with_index), "primary");

    let with_alias = serde_json::json!({ "index_id": "alias" });
    assert_eq!(index_id_or_default(&with_alias), "alias");

    let empty = serde_json::json!({});
    assert_eq!(index_id_or_default(&empty), "default");
}

/// Why: the daemon's request structs give every optional field a serde default,
/// so forwarding a key the caller did not supply would override that default
/// with a guess. Absence has to survive the hop.
/// What: two of three keys present; the third is absent from the params object
/// entirely, not present-and-null.
/// Test: this test.
#[test]
fn optional_params_copies_only_present_keys() {
    let args = serde_json::json!({ "subject": "fn auth", "object": "JWT" });
    let p = optional_params(&args, &["subject", "predicate", "object"]);
    assert_eq!(p.get("subject").and_then(|v| v.as_str()), Some("fn auth"));
    assert_eq!(p.get("object").and_then(|v| v.as_str()), Some("JWT"));
    assert!(
        !p.contains_key("predicate"),
        "an unsupplied filter must not be sent at all: {p:?}"
    );
}

/// Why (#6287): `build_query`, which this replaced, coerced every value through
/// a string/u64/bool ladder — so `omit_content: false` travelled as the STRING
/// `"false"` and the daemon's `bool` field only accepted it because a query
/// string has no types. A JSON `params` object does, and copying the value
/// verbatim is what keeps the daemon's own `Deserialize` the single arbiter of
/// what a well-typed argument is.
/// What: a number stays a number and a bool stays a bool.
/// Test: this test.
#[test]
fn optional_params_preserves_value_types() {
    let args = serde_json::json!({
        "limit": 100u64,
        "offset": 50u64,
        "omit_content": false,
    });
    let p = optional_params(&args, &["limit", "offset", "omit_content"]);
    assert_eq!(p.get("limit").and_then(|v| v.as_u64()), Some(100));
    assert_eq!(p.get("offset").and_then(|v| v.as_u64()), Some(50));
    assert_eq!(p.get("omit_content").and_then(|v| v.as_bool()), Some(false));
}

/// Why: an MCP client that spells an unset optional argument as an explicit
/// `null` means the same thing as omitting it. Forwarding the `null` would fail
/// the decode on a `#[serde(default)]` scalar like `limit`, turning a
/// well-formed tool call into `invalid_params`.
/// What: an explicitly-null key is dropped.
/// Test: this test.
#[test]
fn optional_params_omits_an_explicit_null() {
    let args = serde_json::json!({ "limit": serde_json::Value::Null });
    let p = optional_params(&args, &["limit"]);
    assert!(p.is_empty(), "an explicit null means absent: {p:?}");
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
