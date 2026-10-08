//! MCP `changed_files` and `changed_files_budget` (#9195).
//!
//! Why: both are new parameters with no legacy callers, so a wrong type is
//! refused before any work; `review_diff` has no head SHA, so it reports the
//! source `unavailable` and still reviews.
//! What: drives `call_tool("review_diff", ...)` offline with the dispatch
//! tests' fakes, `parse_review_pr_context`, and `tool_descriptors()`.
//! Test: this module.

use super::*;
use crate::mcp::tools::{context_args::parse_review_pr_context, tool_descriptors};

/// `review_diff` over the dispatch fixture diff with `extra` arguments.
async fn review_diff(state: &AppState, extra: Value) -> Result<Value, ToolError> {
    let mut args = json!({ "diff": ISSUE_DIFF });
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    call_tool("review_diff", &args, state).await
}

/// #9195 amendment 10: a mistyped flag or budget is `InvalidParams` on both
/// tools, naming the parameter, and no review runs.
#[serial_test::serial]
#[tokio::test]
async fn changed_files_mistyped_is_invalid_params() {
    let (state, llm, _pin) = capturing_state();
    for (args, want) in [
        (
            json!({"changed_files": "yes"}),
            "'changed_files' must be a boolean, got a string",
        ),
        (
            json!({"changed_files": true, "changed_files_budget": -1}),
            "'changed_files_budget' must be a non-negative integer, got a number",
        ),
        (
            json!({"changed_files": true, "changed_files_budget": 1.5}),
            "'changed_files_budget' must be a non-negative integer, got a number",
        ),
        (
            json!({"changed_files": true, "changed_files_budget": "1000"}),
            "'changed_files_budget' must be a non-negative integer, got a string",
        ),
    ] {
        let pr = parse_review_pr_context(&args);
        assert!(
            matches!(&pr, Err(ToolError::InvalidParams(m)) if m == want),
            "review_pr {args}: {pr:?}"
        );
        let diff = review_diff(&state, args.clone()).await;
        assert!(
            matches!(&diff, Err(ToolError::InvalidParams(m)) if m == want),
            "review_diff {args}: {diff:?}"
        );
    }
    assert!(llm.0.lock().map(|p| p.is_empty()).unwrap_or(false));
}

/// #9195 amendment 10: the flag is a new input; the budget alone is not,
/// and a budget over the ceiling is accepted (the run clamps it).
#[test]
fn changed_files_params_set_the_request() {
    let parsed = parse_review_pr_context(&json!({"changed_files": true})).expect("valid");
    assert!(parsed.request.changed_files && parsed.request.requested_new());
    assert_eq!(parsed.request.changed_files_budget, None);
    let parsed = parse_review_pr_context(&json!({"changed_files_budget": 5000})).expect("valid");
    assert!(!parsed.request.requested_new() && !parsed.request.ledger_enabled());
    assert_eq!(parsed.request.changed_files_budget, Some(5000));
    let parsed =
        parse_review_pr_context(&json!({"changed_files": true, "changed_files_budget": u64::MAX}))
            .expect("an over-ceiling budget is clamped, not refused");
    assert!(parsed.request.changed_files_budget.is_some());
    let parsed =
        parse_review_pr_context(&json!({"changed_files": null, "changed_files_budget": null}))
            .expect("null is absent");
    assert!(!parsed.request.changed_files && parsed.request.changed_files_budget.is_none());
}

/// #9195 amendment 10: `review_diff` has no head SHA; the row is
/// `unavailable` with the reason, and the review still runs.
#[serial_test::serial]
#[tokio::test]
async fn review_diff_changed_files_reports_unavailable() {
    let (state, llm, _pin) = capturing_state();
    let envelope = review_diff(&state, json!({"changed_files": true}))
        .await
        .expect("a boolean changed_files is valid");
    let rows = envelope["context_sources"]
        .as_array()
        .unwrap_or_else(|| panic!("no context_sources: {envelope}"));
    let row = rows
        .iter()
        .find(|r| r["source"] == "changed_files")
        .unwrap_or_else(|| panic!("no changed_files row: {envelope}"));
    assert_eq!(row["state"], "unavailable", "{row}");
    let detail = row["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("no PR head SHA"), "{detail}");
    assert!(llm.0.lock().map(|p| !p.is_empty()).unwrap_or(false));
}

/// #9195: both review tools list the flag and the budget, with the default,
/// the ceiling and the not-citable rule.
#[test]
fn both_review_tools_list_changed_files() {
    let tools = tool_descriptors();
    for name in ["review_pr", "review_diff"] {
        let tool = tools
            .as_array()
            .and_then(|t| t.iter().find(|t| t["name"] == name))
            .unwrap_or_else(|| panic!("no {name}"));
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["changed_files"]["type"], "boolean", "{name}");
        let budget = &props["changed_files_budget"];
        assert_eq!(budget["type"], "integer", "{name}: {budget}");
        assert_eq!(budget["minimum"], 0, "{name}: {budget}");
        let text = budget["description"].as_str().unwrap_or_default();
        assert!(
            text.contains("120,000") && text.contains("400,000"),
            "{text}"
        );
        let flag = props["changed_files"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(flag.contains("cite only lines in the diff"), "{flag}");
    }
}
