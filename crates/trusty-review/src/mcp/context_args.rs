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
//! turns the source ledger on (plan §3.1).
//! Test: `review_pr_accepts_the_three_text_params`,
//! `review_pr_ignores_a_mistyped_text_param_with_a_warning`,
//! `review_pr_rejects_a_mistyped_include_pr_body`.

use serde_json::Value;

use crate::pipeline::{CallerContext, optional_context::OptionalContextRequest};

use super::ToolError;

/// The text parameters, in `CallerContext` field order.
pub(crate) const TEXT_PARAMS: [&str; 3] = ["pr_description", "pr_discussion", "referenced_code"];

/// The new boolean parameter.
pub(crate) const INCLUDE_PR_BODY: &str = "include_pr_body";

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
/// boolean (`null` counts as absent).
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
    Ok(parsed)
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
