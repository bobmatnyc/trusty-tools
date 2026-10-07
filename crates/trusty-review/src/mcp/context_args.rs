//! `review_pr`'s optional PR-context parameters (#9192).
//!
//! Why: `review_pr` used `CallerContext::default()`, so an MCP caller could not
//! give the reviewer the PR description, discussion, referenced code, or the
//! fetched PR body.
//! What: [`parse_review_pr_context`] reads the four optional parameters. The
//! three text names (`pr_description`, `pr_discussion`, `referenced_code`)
//! were ignored before, so a wrong type is still ignored, with a warning. The
//! new boolean `include_pr_body` has no legacy callers, so a wrong type is
//! `InvalidParams`. Any non-blank text param, or `include_pr_body: true`,
//! turns the source ledger on (plan §3.1). #9197: `issue_docs`, on both
//! `review_pr` and `review_diff`, is parsed strictly by [`parse_issue_docs`].
//! Test: `review_pr_accepts_the_three_text_params`,
//! `review_pr_ignores_a_mistyped_text_param_with_a_warning`,
//! `review_pr_rejects_a_mistyped_include_pr_body`,
//! `issue_docs_mistyped_is_invalid_params`.

use serde_json::Value;

use crate::pipeline::{
    CallerContext,
    optional_context::{IssueDoc, OptionalContextRequest},
};

use super::ToolError;

/// The text parameters, in `CallerContext` field order.
pub(crate) const TEXT_PARAMS: [&str; 3] = ["pr_description", "pr_discussion", "referenced_code"];

/// The new boolean parameter.
pub(crate) const INCLUDE_PR_BODY: &str = "include_pr_body";

/// The caller issue-docs parameter, on both review tools (#9197).
pub(crate) const ISSUE_DOCS: &str = "issue_docs";

/// `review_pr`'s parsed optional context.
#[derive(Debug, Default)]
pub(crate) struct ParsedPrContext {
    /// The three text parameters; a blank or mistyped one is `None`.
    pub(crate) caller: CallerContext,
    /// The new inputs the call asked for.
    pub(crate) request: OptionalContextRequest,
    /// One message per ignored, mistyped text parameter, for the caller to log.
    pub(crate) warnings: Vec<String>,
}

/// Parse `review_pr`'s optional context parameters from `args`.
///
/// # Errors
///
/// [`ToolError::InvalidParams`] when `include_pr_body` is present and not a
/// boolean (`null` counts as absent), or `issue_docs` is malformed.
pub(crate) fn parse_review_pr_context(args: &Value) -> Result<ParsedPrContext, ToolError> {
    let mut parsed = ParsedPrContext::default();
    let mut texts = TEXT_PARAMS.map(|name| match args.get(name) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => (!s.trim().is_empty()).then(|| s.clone()),
        Some(other) => {
            parsed.warnings.push(format!(
                "review_pr: ignoring '{name}': expected a string, got {}",
                type_name(other)
            ));
            None
        }
    });
    parsed.caller = CallerContext {
        pr_description: texts[0].take(),
        pr_discussion: texts[1].take(),
        referenced_code: texts[2].take(),
    };
    let include = match args.get(INCLUDE_PR_BODY) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(on)) => *on,
        Some(other) => {
            return Err(ToolError::InvalidParams(format!(
                "'{INCLUDE_PR_BODY}' must be a boolean, got {}",
                type_name(other)
            )));
        }
    };
    // #9192 (plan §3.1): the text params are new to `review_pr`, so sending
    // one turns the source ledger on.
    let caller_text = [
        &parsed.caller.pr_description,
        &parsed.caller.pr_discussion,
        &parsed.caller.referenced_code,
    ]
    .iter()
    .any(|t| t.is_some());
    parsed.request = OptionalContextRequest::default()
        .with_pr_body(include)
        .with_caller_text(caller_text);
    parsed.request = with_issue_docs(parsed.request, args)?; // #9197
    Ok(parsed)
}

/// `request` carrying the `issue_docs` parameter, when sent (#9197).
///
/// Why: `review_pr` and `review_diff` take the same parameter, so both read
/// it here.
/// What: absent or `null` leaves `request` unchanged; anything else must
/// parse through [`parse_issue_docs`].
///
/// # Errors
///
/// [`ToolError::InvalidParams`] naming the bad item and key.
///
/// Test: `review_diff_issue_docs_reach_the_reviewer_prompt`,
/// `issue_docs_mistyped_is_invalid_params`.
pub(crate) fn with_issue_docs(
    request: OptionalContextRequest,
    args: &Value,
) -> Result<OptionalContextRequest, ToolError> {
    Ok(match parse_issue_docs(args)? {
        Some(docs) => request.with_issue_docs(docs),
        None => request,
    })
}

/// The `issue_docs` parameter: `None` when absent or `null` (#9197).
///
/// # Errors
///
/// [`ToolError::InvalidParams`] when it is not an array of
/// `{id, title?, body, url?}` objects with GitHub issue-number ids.
///
/// Test: `issue_docs_mistyped_is_invalid_params`, `jira_shaped_issue_doc_id_is_invalid_params`.
pub(crate) fn parse_issue_docs(args: &Value) -> Result<Option<Vec<IssueDoc>>, ToolError> {
    match args.get(ISSUE_DOCS) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => IssueDoc::list_from_json(value)
            .map(Some)
            .map_err(|e| ToolError::InvalidParams(e.to_string())),
    }
}

/// The `issue_docs` JSON Schema both review tools list (#9197).
///
/// Why: `review_pr` and `review_diff` describe one parameter one way.
/// Test: `both_review_tools_list_issue_docs`.
pub(crate) fn issue_docs_schema() -> Value {
    serde_json::json!({
        "type": "array",
        "description": "Issues this change addresses, for the reviewer only (the verifier never \
                        sees them). Each id is a GitHub issue number (\"#42\" or \"42\"). Each \
                        body is capped at 16,000 characters with a visible marker; at most 8 docs \
                        and 48,000 body characters in total are shown, and a doc past either \
                        limit is left out whole. The text is shown as data, and a \
                        [gh: #N — \"excerpt\"] citation resolves only against text the reviewer \
                        saw. The response then carries a context_sources ledger.",
        "items": {
            "type": "object",
            "required": ["id", "body"],
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string", "description": "GitHub issue number, \"#42\" or \"42\"" },
                "title": { "type": "string", "description": "One line, at most 512 characters" },
                "body": { "type": "string", "description": "The issue text" },
                "url": { "type": "string", "description": "One line, at most 512 characters" }
            }
        }
    })
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
#[path = "context_args_tests.rs"]
mod tests;
