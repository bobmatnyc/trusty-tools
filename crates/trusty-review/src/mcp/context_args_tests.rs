//! Tests for `review_pr`'s optional PR-context parameters (#9192).
//!
//! Why: the old text names must stay lenient (Ruling B) and the new boolean
//! strict, and a call without any of them must parse to the defaults.
//! What: drives [`parse_review_pr_context`] over each shape.
//! Test: this module.

use serde_json::json;

use super::*;

/// #9192: the three text params land in `CallerContext`; blanks are `None`.
#[test]
fn review_pr_accepts_the_three_text_params() {
    let args = json!({
        "owner": "o", "repo": "r", "pr": 1,
        "pr_description": "why", "pr_discussion": "talk", "referenced_code": "  ",
    });
    let parsed = parse_review_pr_context(&args).expect("parses");
    assert_eq!(parsed.caller.pr_description.as_deref(), Some("why"));
    assert_eq!(parsed.caller.pr_discussion.as_deref(), Some("talk"));
    assert_eq!(parsed.caller.referenced_code, None);
    assert!(parsed.warnings.is_empty());
    // Ruling 2026-10-06 03:42Z (plan §3.1): the text params are new to
    // `review_pr`, so they turn the ledger on.
    assert!(parsed.request.requested_new() && parsed.request.caller_text);
}

/// #9192: a call with none of the params parses to the defaults.
#[test]
fn review_pr_without_context_params_parses_to_defaults() {
    let parsed =
        parse_review_pr_context(&json!({"owner": "o", "repo": "r", "pr": 1})).expect("parses");
    assert!(parsed.caller.pr_description.is_none());
    assert!(parsed.caller.pr_discussion.is_none());
    assert!(parsed.caller.referenced_code.is_none());
    assert_eq!(parsed.request, OptionalContextRequest::default());
    assert!(parsed.warnings.is_empty());
}

/// #9192 (Ruling B): a mistyped OLD text param is ignored, with a warning
/// naming it and the expected type; the call still runs.
#[test]
fn review_pr_ignores_a_mistyped_text_param_with_a_warning() {
    let args = json!({"pr_description": 42, "pr_discussion": ["a"], "referenced_code": "x"});
    let parsed = parse_review_pr_context(&args).expect("a legacy name never fails the call");
    assert!(parsed.caller.pr_description.is_none());
    assert!(parsed.caller.pr_discussion.is_none());
    assert_eq!(parsed.caller.referenced_code.as_deref(), Some("x"));
    assert_eq!(parsed.warnings.len(), 2, "{:?}", parsed.warnings);
    assert!(parsed.warnings[0].contains("'pr_description'"));
    assert!(parsed.warnings[0].contains("expected a string, got a number"));
    assert!(parsed.warnings[1].contains("'pr_discussion'"));
}

/// #9192 (Ruling B): a mistyped NEW param is `InvalidParams`.
#[test]
fn review_pr_rejects_a_mistyped_include_pr_body() {
    for bad in [json!("true"), json!(1), json!({})] {
        let err = parse_review_pr_context(&json!({ "include_pr_body": bad }))
            .expect_err("a new name is strict");
        match err {
            ToolError::InvalidParams(msg) => assert!(msg.contains("'include_pr_body'"), "{msg}"),
            ToolError::UnknownTool => panic!("wrong error"),
        }
    }
}

/// #9192: `include_pr_body: true` asks for the body and turns the ledger on;
/// `false` and `null` ask for nothing.
#[test]
fn include_pr_body_marks_the_request() {
    let on = parse_review_pr_context(&json!({"include_pr_body": true})).expect("parses");
    assert!(on.request.include_pr_body && on.request.requested_new());
    for off in [json!(false), json!(null)] {
        let parsed = parse_review_pr_context(&json!({ "include_pr_body": off })).expect("parses");
        assert!(!parsed.request.requested_new());
    }
}

/// #9197: `issue_docs` parses into the request with normalised ids, and
/// sending it counts as a new input; `null` is absent.
#[test]
fn review_pr_parses_issue_docs_into_the_request() {
    let args = json!({
        "owner": "o", "repo": "r", "pr": 1,
        "issue_docs": [{"id": "42", "title": "T", "body": "b"}],
    });
    let parsed = parse_review_pr_context(&args).expect("parses");
    let docs = parsed.request.issue_docs.as_deref().expect("docs");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id, "#42");
    assert!(parsed.request.requested_new());
    assert_eq!(
        parse_issue_docs(&json!({"issue_docs": null})).expect("null"),
        None
    );
}

/// #9197 (Ruling B): a wrong `issue_docs` shape is `InvalidParams` naming
/// the parameter, never ignored.
#[test]
fn issue_docs_mistyped_is_invalid_params() {
    for bad in [
        json!("x"),
        json!([{"id": "1"}]),
        json!({"id": "1", "body": "b"}),
        json!([1]),
    ] {
        let args = json!({"owner": "o", "repo": "r", "pr": 1, "issue_docs": bad.clone()});
        match parse_review_pr_context(&args) {
            Err(ToolError::InvalidParams(msg)) => {
                assert!(msg.starts_with("issue_docs:"), "{bad}: {msg}");
            }
            other => panic!("{bad}: expected InvalidParams, got {other:?}"),
        }
    }
}

/// #9197: GitHub issue numbers only in v1; a JIRA-shaped id is refused.
#[test]
fn jira_shaped_issue_doc_id_is_invalid_params() {
    let args = json!({"issue_docs": [{"id": "PROJ-12", "body": "b"}]});
    match parse_issue_docs(&args) {
        Err(ToolError::InvalidParams(msg)) => assert!(msg.contains("'id'"), "{msg}"),
        other => panic!("expected InvalidParams, got {other:?}"),
    }
}
