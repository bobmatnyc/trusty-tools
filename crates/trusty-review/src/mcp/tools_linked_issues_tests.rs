//! MCP `fetch_linked_issues` (#9197, B2b AC13).
//!
//! Why: a new boolean with no legacy callers, so a wrong type is refused
//! before any work; `review_diff` has no PR body, so it neither lists nor
//! reads the flag (ruling Q6).
//! What: drives `parse_review_pr_context`, `call_tool("review_diff", ...)`
//! offline with the dispatch tests' fakes, and `tool_descriptors()`.
//! Test: this module.

use super::*;
use crate::mcp::tools::{context_args::parse_review_pr_context, tool_descriptors};

/// AC13: a mistyped flag is `InvalidParams` on `review_pr`; `null` is off;
/// `true` turns the ledger on; `review_diff` ignores the name, even mistyped.
#[serial_test::serial]
#[tokio::test]
async fn fetch_linked_issues_mistyped_is_invalid_params() {
    let want = "'fetch_linked_issues' must be a boolean, got a string";
    let pr = parse_review_pr_context(&json!({"fetch_linked_issues": "yes"}));
    assert!(
        matches!(&pr, Err(ToolError::InvalidParams(m)) if m == want),
        "{pr:?}"
    );
    let parsed =
        parse_review_pr_context(&json!({"fetch_linked_issues": null})).expect("null is off");
    assert!(!parsed.request.fetch_linked_issues && !parsed.request.ledger_enabled());
    let parsed = parse_review_pr_context(&json!({})).expect("absent is off");
    assert!(!parsed.request.fetch_linked_issues);
    let parsed = parse_review_pr_context(&json!({"fetch_linked_issues": true})).expect("valid");
    assert!(parsed.request.fetch_linked_issues && parsed.request.requested_new());

    let (state, llm, _pin) = capturing_state();
    let args = json!({"diff": ISSUE_DIFF, "fetch_linked_issues": "yes"});
    let envelope = call_tool("review_diff", &args, &state)
        .await
        .expect("ignored on review_diff");
    assert!(envelope.get("context_sources").is_none(), "{envelope}");
    assert!(llm.0.lock().map(|p| !p.is_empty()).unwrap_or(false));
}

/// AC13: `review_pr` lists the flag; `review_diff` does not.
#[test]
fn only_review_pr_lists_fetch_linked_issues() {
    let tools = tool_descriptors();
    let flag = |name: &str| {
        let tool = tools
            .as_array()
            .and_then(|t| t.iter().find(|t| t["name"] == name));
        tool.map(|t| t["inputSchema"]["properties"]["fetch_linked_issues"].clone())
    };
    let on_pr = flag("review_pr").expect("review_pr");
    assert_eq!(on_pr["type"], "boolean", "{on_pr}");
    let text = on_pr["description"].as_str().unwrap_or_default();
    assert!(
        text.contains("at most 5 issues") && text.contains("unavailable"),
        "{text}"
    );
    assert_eq!(
        flag("review_diff"),
        Some(Value::Null),
        "review_diff must not list it"
    );
}
