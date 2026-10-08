//! MCP `report_context` on `review_diff`, the tool schemas, and the envelope
//! (#9194).
//!
//! Why: AC1 requires an MCP opt-in for the context-source ledger, strict
//! like every new parameter, with the default envelope and `tools/list`
//! unchanged apart from the new optional parameter.
//! What: drives `call_tool("review_diff", ...)` offline with the dispatch
//! tests' fakes, and reads `tool_descriptors()`.
//! Test: this module.

use super::*;
use crate::{
    mcp::tools::{context_args::parse_review_pr_context, tool_descriptors},
    models::ReviewResult,
};

const KNOWN_STATUSES: [&str; 3] = ["completed", "skipped", "degraded"];

/// `review_diff` over the dispatch fixture diff with `extra` arguments.
async fn review_diff(state: &AppState, extra: Value) -> Result<Value, ToolError> {
    let mut args = json!({ "diff": ISSUE_DIFF });
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    call_tool("review_diff", &args, state).await
}

/// The envelope's review result, parsed back from its text block.
fn result_of(envelope: &Value) -> ReviewResult {
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("a text block");
    serde_json::from_str(text).expect("the text is a ReviewResult")
}

/// #9194 AC1: `report_context: true` on `review_diff` adds every row.
#[serial_test::serial]
#[tokio::test]
async fn review_diff_report_context_turns_the_ledger_on() {
    let (state, _llm, _pin) = capturing_state();
    let envelope = review_diff(&state, json!({"report_context": true}))
        .await
        .expect("a boolean report_context is valid");
    let rows = envelope["context_sources"]
        .as_array()
        .unwrap_or_else(|| panic!("no context_sources: {envelope}"));
    assert_eq!(rows.len(), 10, "{envelope}"); // #9196: `symbol_context` is the 10th
    assert_eq!(rows[0]["source"], "pr_body", "{envelope}");
    assert_eq!(rows[0]["state"], "not_requested", "{envelope}");
}

/// #9194 P2: a non-boolean `report_context` is `InvalidParams` on both tools,
/// naming the parameter and the type it got.
#[serial_test::serial]
#[tokio::test]
async fn report_context_mistyped_is_invalid_params() {
    let (state, llm, _pin) = capturing_state();
    for (bad, got) in [
        (json!("yes"), "a string"),
        (json!(1), "a number"),
        (json!([true]), "an array"),
    ] {
        let want = format!("'report_context' must be a boolean, got {got}");
        let pr = parse_review_pr_context(&json!({ "report_context": bad }));
        assert!(
            matches!(&pr, Err(ToolError::InvalidParams(m)) if *m == want),
            "review_pr {bad}: {pr:?}"
        );
        let diff = review_diff(&state, json!({ "report_context": bad })).await;
        assert!(
            matches!(&diff, Err(ToolError::InvalidParams(m)) if *m == want),
            "review_diff {bad}: {diff:?}"
        );
    }
    assert!(llm.0.lock().map(|p| p.is_empty()).unwrap_or(false));
}

/// #9194 AC1: `null` and `false` are off; the envelope has no ledger.
#[serial_test::serial]
#[tokio::test]
async fn report_context_null_and_false_keep_the_envelope_plain() {
    let (state, _llm, _pin) = capturing_state();
    for off in [json!(null), json!(false)] {
        let envelope = review_diff(&state, json!({ "report_context": off }))
            .await
            .expect("off is valid");
        assert!(envelope.get("context_sources").is_none(), "{envelope}");
    }
}

/// #9194 AC1: with every flag off, the envelope is exactly `wrap_result`
/// of the result, the wrapper origin/main used.
#[serial_test::serial]
#[tokio::test]
async fn default_envelope_is_wrap_result_unchanged() {
    let (state, _llm, _pin) = capturing_state();
    let envelope = review_diff(&state, json!({})).await.expect("valid");
    assert_eq!(envelope, super::super::wrap_result(&result_of(&envelope)));
}

/// #9194 amendment 3: reporting was asked for and a required dependency
/// skipped the review: `context_sources` is `[]`, no row is fabricated, and
/// `isError`, `mcp_status` and the status are the off run's.
#[serial_test::serial]
#[tokio::test]
async fn envelope_status_is_error_and_mcp_status_unchanged_by_the_ledger() {
    let (mut state, _llm, _pin) = capturing_state();
    state.config.context.require_analyze = true;
    state.analyze = None;
    let off = review_diff(&state, json!({})).await.expect("valid");
    let on = review_diff(&state, json!({"report_context": true}))
        .await
        .expect("valid");
    assert_eq!(on["context_sources"], json!([]), "{on}");
    assert_eq!(on["isError"], json!(true), "{on}");
    for key in ["isError", "mcp_status", "verdict_status"] {
        assert_eq!(on.get(key), off.get(key), "{key}");
    }
    assert_eq!(result_of(&on).status, result_of(&off).status);
    assert!(off.get("context_sources").is_none(), "{off}");
}

/// #9194 R5: the ledger adds no status value.
#[serial_test::serial]
#[tokio::test]
async fn no_new_status_value() {
    let (state, _llm, _pin) = capturing_state();
    let envelope = review_diff(&state, json!({"report_context": true}))
        .await
        .expect("valid");
    let status = serde_json::to_value(result_of(&envelope).status).expect("serialises");
    assert!(
        KNOWN_STATUSES.contains(&status.as_str().unwrap_or_default()),
        "{status}"
    );
    for known in KNOWN_STATUSES {
        let parsed: crate::models::ReviewStatus =
            serde_json::from_value(json!(known)).expect("a known status parses");
        assert_eq!(
            serde_json::to_value(parsed).expect("serialises"),
            json!(known)
        );
    }
}

/// #9194: both review tools list `report_context` as a boolean.
#[test]
fn both_review_tools_list_report_context() {
    let tools = tool_descriptors();
    for name in ["review_pr", "review_diff"] {
        let tool = tools
            .as_array()
            .and_then(|t| t.iter().find(|t| t["name"] == name))
            .unwrap_or_else(|| panic!("no {name}"));
        let param = &tool["inputSchema"]["properties"]["report_context"];
        assert_eq!(param["type"], "boolean", "{name}: {param}");
        let text = param["description"].as_str().unwrap_or_default();
        assert!(text.contains("context_sources"), "{name}: {text}");
    }
}

/// #9194 byte identity: `tools/list` is the golden captured before B6 once
/// the new optional `report_context` parameter, (#9195) the
/// `changed_files` and `changed_files_budget` parameters, and (#9196)
/// `symbol_context` are set aside, and (#9197 B2b) `fetch_linked_issues`,
/// on `review_pr` only. Each
/// set-aside name must be listed, so a stripped property is never one the
/// golden did not expect. `UPDATE_GOLDEN=1` rewrites the golden.
#[test]
fn tools_list_is_unchanged_apart_from_ledger_parameters() {
    const NEW: [&str; 4] = [
        "report_context",
        "changed_files",
        "changed_files_budget",
        "symbol_context", // #9196
    ];
    let mut tools = tool_descriptors();
    if let Some(list) = tools.as_array_mut() {
        for tool in list {
            let review = tool["name"] == "review_pr" || tool["name"] == "review_diff";
            let review_pr = tool["name"] == "review_pr"; // #9197 B2b
            if let Some(props) = tool
                .pointer_mut("/inputSchema/properties")
                .and_then(Value::as_object_mut)
            {
                for name in NEW {
                    let removed = props.remove(name).is_some();
                    assert_eq!(removed, review, "{name} listed on a review tool only");
                }
                let removed = props.remove("fetch_linked_issues").is_some();
                assert_eq!(removed, review_pr, "fetch_linked_issues on review_pr only");
            }
        }
    }
    let actual = serde_json::to_string_pretty(&tools).expect("serialises") + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/optional_context_off/tools-list.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).expect("write golden");
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("golden is readable");
    assert!(
        golden == actual,
        "tools/list moved apart from report_context"
    );
}
