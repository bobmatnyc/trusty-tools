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
//! #9193: the booleans `spec_docs` and `claude_md`, on both tools, are read
//! strictly by [`with_doc_flags`]. #9194: the boolean `report_context`, on
//! both tools, is read strictly by [`with_report_context_flag`]. #9196: the
//! boolean `symbol_context`, on both tools, by [`with_symbol_context_flag`].
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

/// Read ADR/spec/SLD docs at the PR head, on both review tools (#9193).
pub(crate) const SPEC_DOCS: &str = "spec_docs";

/// Read CLAUDE.md conventions at the PR head, on both review tools (#9193).
pub(crate) const CLAUDE_MD: &str = "claude_md";

/// Ask for the context-source ledger alone, on both review tools (#9194).
pub(crate) const REPORT_CONTEXT: &str = "report_context";

/// `request` with the `report_context` flag (#9194).
///
/// Why: AC1: reporting is opt-in through `report_context`; a new boolean
/// with no legacy callers, so a wrong type is refused, as `spec_docs` is.
/// What: absent or `null` is off; `true`/`false` set the flag.
///
/// # Errors
///
/// [`ToolError::InvalidParams`] naming the parameter and the type it got.
///
/// Test: `report_context_mistyped_is_invalid_params`,
/// `review_diff_report_context_turns_the_ledger_on`,
/// `review_pr_report_context_turns_the_ledger_on`.
pub(crate) fn with_report_context_flag(
    request: OptionalContextRequest,
    args: &Value,
) -> Result<OptionalContextRequest, ToolError> {
    match args.get(REPORT_CONTEXT) {
        None | Some(Value::Null) => Ok(request),
        Some(Value::Bool(on)) => Ok(request.with_report_context(*on)),
        Some(other) => Err(ToolError::InvalidParams(format!(
            "'{REPORT_CONTEXT}' must be a boolean, got {}",
            type_name(other)
        ))),
    }
}

/// The `report_context` JSON Schema both review tools list (#9194).
///
/// Test: `both_review_tools_list_report_context`.
pub(crate) fn report_context_schema() -> Value {
    serde_json::json!({
        "type": "boolean",
        "description": "Report every context source in a context_sources ledger beside the \
                        review: pr_body, caller_context, issues, spec_docs, claude_md, \
                        changed_files, search, analyze, symbol_context and external_sources, \
                        each used, truncated, absent (asked for, nothing there), unavailable (could not \
                        be read, with the reason), or not_requested. Default false; any other new input turns it on too. \
                        The review itself is unchanged."
    })
}

/// Show the PR's changed files whole, on both review tools (#9195).
pub(crate) const CHANGED_FILES: &str = "changed_files";

/// The byte budget for `changed_files`, on both review tools (#9195).
pub(crate) const CHANGED_FILES_BUDGET: &str = "changed_files_budget";

/// `request` with `changed_files` and `changed_files_budget` (#9195).
///
/// Why: both are new parameters with no legacy callers, so a wrong type is
/// refused before any work, as `spec_docs` is (amendment 10).
/// What: absent or `null` leaves each unset. `changed_files` takes a
/// boolean; `changed_files_budget` a non-negative integer, kept as sent (one
/// over the ceiling is clamped when the review runs) and inert without the
/// flag.
///
/// # Errors
///
/// [`ToolError::InvalidParams`] naming the parameter and the type it got.
///
/// Test: `changed_files_mistyped_is_invalid_params`, `changed_files_params_set_the_request`.
pub(crate) fn with_changed_files(
    request: OptionalContextRequest,
    args: &Value,
) -> Result<OptionalContextRequest, ToolError> {
    let request = match args.get(CHANGED_FILES) {
        None | Some(Value::Null) => request,
        Some(Value::Bool(on)) => request.with_changed_files(*on),
        Some(other) => {
            return Err(ToolError::InvalidParams(format!(
                "'{CHANGED_FILES}' must be a boolean, got {}",
                type_name(other)
            )));
        }
    };
    match args.get(CHANGED_FILES_BUDGET) {
        None | Some(Value::Null) => Ok(request),
        Some(value) => match value.as_u64() {
            // A budget past `usize` is over the ceiling anyway; the run clamps it.
            Some(bytes) => {
                Ok(request.with_changed_files_budget(usize::try_from(bytes).unwrap_or(usize::MAX)))
            }
            None => Err(ToolError::InvalidParams(format!(
                "'{CHANGED_FILES_BUDGET}' must be a non-negative integer, got {}",
                type_name(value)
            ))),
        },
    }
}

/// The `changed_files` JSON Schemas both review tools list (#9195).
///
/// Test: `both_review_tools_list_changed_files`.
pub(crate) fn changed_files_schemas() -> [(&'static str, Value); 2] {
    [
        (
            CHANGED_FILES,
            serde_json::json!({
                "type": "boolean",
                "description": "Show the PR's changed files whole, read at the PR head SHA \
                                through the GitHub Contents API, for the reviewer only (the \
                                verifier never sees them), within changed_files_budget bytes. \
                                Each file is shown whole or named under \"Not shown\" with its \
                                reason; over budget, tests drop first, then generated files, \
                                then the largest. The text is context: cite only lines in the \
                                diff. Default false. review_diff has no head SHA, so it \
                                reports the source unavailable. The response then carries a \
                                context_sources ledger."
            }),
        ),
        (
            CHANGED_FILES_BUDGET,
            serde_json::json!({
                "type": "integer",
                "minimum": 0,
                "description": "Byte budget for changed_files: default 120,000, at most \
                                400,000 (a larger value is clamped and reported). The list \
                                of files not shown is paid for first. 0 reviews the diff \
                                only. Does nothing without changed_files."
            }),
        ),
    ]
}

/// Show each changed symbol's call graph, on both review tools (#9196).
pub(crate) const SYMBOL_CONTEXT: &str = "symbol_context";

/// `request` with the `symbol_context` flag (#9196).
///
/// Why: Architect ruling Q1: a new boolean with no legacy callers, so a
/// wrong type is refused before any work, as `changed_files` is; ruling Q8:
/// `review_diff` takes it too.
/// What: absent or `null` is off; `true`/`false` set the flag.
///
/// # Errors
///
/// [`ToolError::InvalidParams`] naming the parameter and the type it got.
///
/// Test: `symbol_context_mistyped_is_invalid_params`,
/// `review_diff_symbol_context_turns_the_ledger_on`.
pub(crate) fn with_symbol_context_flag(
    request: OptionalContextRequest,
    args: &Value,
) -> Result<OptionalContextRequest, ToolError> {
    match args.get(SYMBOL_CONTEXT) {
        None | Some(Value::Null) => Ok(request),
        Some(Value::Bool(on)) => Ok(request.with_symbol_context(*on)),
        Some(other) => Err(ToolError::InvalidParams(format!(
            "'{SYMBOL_CONTEXT}' must be a boolean, got {}",
            type_name(other)
        ))),
    }
}

/// The `symbol_context` JSON Schema both review tools list (#9196).
///
/// Test: `both_review_tools_list_symbol_context`.
pub(crate) fn symbol_context_schema() -> Value {
    serde_json::json!({
        "type": "boolean",
        "description": "For each function or method the diff declares or edits, show its \
                        callers, callees and tests from the trusty-search call graph, for the \
                        reviewer only (the verifier never sees them): at most 12 symbols, 6 \
                        lines per list, 2,500 characters per symbol and 24,000 in total, each \
                        cut marked and every symbol left out named. The graph is the indexed \
                        checkout's and may not be at the PR head; its lines are context: cite \
                        only lines in the diff. A search outage leaves the section out and the \
                        review runs. Default false. The response then carries a \
                        context_sources ledger."
    })
}

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
    parsed.request = with_doc_flags(parsed.request, args)?; // #9193
    parsed.request = with_report_context_flag(parsed.request, args)?; // #9194
    parsed.request = with_changed_files(parsed.request, args)?; // #9195
    parsed.request = with_symbol_context_flag(parsed.request, args)?; // #9196
    Ok(parsed)
}

/// `request` with the `spec_docs` and `claude_md` flags (#9193).
///
/// Why: both are new booleans with no legacy callers, so a wrong type is
/// refused, as `include_pr_body` is; `review_pr` and `review_diff` share it.
/// What: absent or `null` is off; `true`/`false` set the flag.
///
/// # Errors
///
/// [`ToolError::InvalidParams`] naming the parameter and the type it got.
///
/// Test: `mistyped_spec_docs_is_invalid_params`, `doc_flags_turn_the_ledger_on`.
pub(crate) fn with_doc_flags(
    request: OptionalContextRequest,
    args: &Value,
) -> Result<OptionalContextRequest, ToolError> {
    let flag = |name: &str| match args.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(on)) => Ok(*on),
        Some(other) => Err(ToolError::InvalidParams(format!(
            "'{name}' must be a boolean, got {}",
            type_name(other)
        ))),
    };
    Ok(request
        .with_spec_docs(flag(SPEC_DOCS)?)
        .with_claude_md(flag(CLAUDE_MD)?))
}

/// The `spec_docs` and `claude_md` JSON Schemas both review tools list (#9193).
///
/// Test: `review_pr_schema_lists_spec_docs_and_claude_md`.
pub(crate) fn doc_flag_schemas() -> [(&'static str, Value); 2] {
    let note = "Read at the PR head SHA through the GitHub Contents API, for the reviewer \
                only (the verifier never sees them); shown as data, and a \
                [doc: path@sha — \"excerpt\"] citation resolves only against text the \
                reviewer saw. Default false. review_diff has no head SHA, so it reports the \
                source unavailable. The response then carries a context_sources ledger.";
    [
        (
            SPEC_DOCS,
            serde_json::json!({
                "type": "boolean",
                "description": format!(
                    "Read the ADR, spec and SLD docs (docs/adr, docs/specs, docs/design, \
                     docs/prd, docs/architecture, docs/reference, crates/*/docs) the PR body \
                     names, plus trusty-search hits: at most 6 docs, 16,000 characters each \
                     and 48,000 in total, each cut with a visible marker. {note}"
                )
            }),
        ),
        (
            CLAUDE_MD,
            serde_json::json!({
                "type": "boolean",
                "description": format!(
                    "Read the root CLAUDE.md and up to 3 nested ones found by trusty-search, \
                     16,000 characters in total. {note}"
                )
            }),
        ),
    ]
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
                "url": {
                    "type": "string",
                    "description": "An http:// or https:// link, no whitespace, at most 512 characters"
                }
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
