//! MCP `symbol_context` (#9196).
//!
//! Why: a new boolean with no legacy callers, so a wrong type is refused
//! before any work (ruling Q1); `review_diff` carries it (ruling Q8), and a
//! search client that cannot read the graph leaves the review running (AC3).
//! What: drives `call_tool("review_diff", ...)` offline with the dispatch
//! tests' fakes, `parse_review_pr_context`, and `tool_descriptors()`.
//! Test: this module.

use super::*;
use crate::mcp::tools::{context_args::parse_review_pr_context, tool_descriptors};

/// Ruling Q1: a mistyped flag is `InvalidParams` on both tools, naming the
/// parameter, and no review runs; `null` is off.
#[serial_test::serial]
#[tokio::test]
async fn symbol_context_mistyped_is_invalid_params() {
    let (state, llm, _pin) = capturing_state();
    let want = "'symbol_context' must be a boolean, got a string";
    let args = json!({"symbol_context": "yes"});
    let pr = parse_review_pr_context(&args);
    assert!(
        matches!(&pr, Err(ToolError::InvalidParams(m)) if m == want),
        "{pr:?}"
    );
    let mut diff_args = args.clone();
    diff_args["diff"] = json!(ISSUE_DIFF);
    let diff = call_tool("review_diff", &diff_args, &state).await;
    assert!(
        matches!(&diff, Err(ToolError::InvalidParams(m)) if m == want),
        "{diff:?}"
    );
    assert!(llm.0.lock().map(|p| p.is_empty()).unwrap_or(false));
    let parsed = parse_review_pr_context(&json!({"symbol_context": null})).expect("null is off");
    assert!(!parsed.request.symbol_context && !parsed.request.ledger_enabled());
    let parsed = parse_review_pr_context(&json!({"symbol_context": true})).expect("valid");
    assert!(parsed.request.symbol_context && parsed.request.requested_new());
}

/// Ruling Q8 + AC3: `review_diff` accepts the flag; a search client that
/// cannot read the call graph records the row `unavailable` and the review
/// still runs.
#[serial_test::serial]
#[tokio::test]
async fn review_diff_symbol_context_turns_the_ledger_on() {
    let (state, llm, _pin) = capturing_state();
    let args = json!({"diff": ISSUE_DIFF, "symbol_context": true});
    let envelope = call_tool("review_diff", &args, &state)
        .await
        .expect("a boolean symbol_context is valid");
    let rows = envelope["context_sources"]
        .as_array()
        .unwrap_or_else(|| panic!("no context_sources: {envelope}"));
    let row = rows
        .iter()
        .find(|r| r["source"] == "symbol_context")
        .unwrap_or_else(|| panic!("no symbol_context row: {envelope}"));
    assert_eq!(row["state"], "unavailable", "{row}");
    assert!(llm.0.lock().map(|p| !p.is_empty()).unwrap_or(false));
}

/// Both review tools list the flag, with its caps and the not-citable rule.
#[test]
fn both_review_tools_list_symbol_context() {
    let tools = tool_descriptors();
    for name in ["review_pr", "review_diff"] {
        let tool = tools
            .as_array()
            .and_then(|t| t.iter().find(|t| t["name"] == name))
            .unwrap_or_else(|| panic!("no {name}"));
        let flag = &tool["inputSchema"]["properties"]["symbol_context"];
        assert_eq!(flag["type"], "boolean", "{name}: {flag}");
        let text = flag["description"].as_str().unwrap_or_default();
        assert!(
            text.contains("12 symbols") && text.contains("24,000"),
            "{text}"
        );
        assert!(text.contains("cite only lines in the diff"), "{text}");
        assert!(text.contains("may not be at the PR head"), "{text}");
    }
}
