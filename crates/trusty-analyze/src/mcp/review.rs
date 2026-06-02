//! Feature-gated bridge that exposes the trusty-review LLM pipeline as MCP tools.
//!
//! Why (#630): collapses two MCP servers into one. With the optional `review`
//! cargo feature on, the trusty-analyze MCP dispatcher gains three LLM-backed
//! review tools (`tr_review_pr`, `tr_review_diff`, `tr_review_health`) that
//! delegate into the embedded trusty-review pipeline. The `tr_` prefix avoids
//! colliding with trusty-analyze's existing *deterministic* `review_diff` /
//! `review_github_pr` tools (which forward to analyze's own `/review` HTTP
//! endpoints). When the feature is off this module is not compiled and the
//! `tr_*` names fall through to `UnknownTool`.
//!
//! What: holds the three review-tool descriptors, a process-wide lazily-built
//! `AppState` cache (`OnceCell`) so the expensive AWS-credential / provider
//! build happens at most once, and async handlers that map trusty-review's
//! `ToolError` onto the dispatcher's [`DispatchError`].
//!
//! Test: `mod.rs` tests `tools_list_includes_tr_review_tools` (feature on) and
//! `tr_review_health_routes` exercise the descriptor list and routing; the
//! credential-bound build path is covered by the live smoke test.

use serde_json::Value;
use tokio::sync::OnceCell;

use super::DispatchError;

/// Process-wide cache of the assembled trusty-review `AppState`.
///
/// Why: `trusty_review::mcp::build_review_state()` loads AWS credentials and
/// builds LLM providers, which is slow and should happen once per process, not
/// per tool call. A `tokio::sync::OnceCell` gives us async-safe lazy init that
/// shares a single build across concurrent first calls.
/// What: stores the built `AppState`; populated on the first review tool call.
/// Test: indirectly via the live smoke test (a second call reuses the cache).
static REVIEW_STATE: OnceCell<trusty_review::mcp::ReviewAppState> = OnceCell::const_new();

/// Return the descriptors for the three `tr_review_*` tools.
///
/// Why: `mod.rs::tool_descriptors()` appends these to the base descriptor set
/// only when the `review` feature is on, so `tools/list` advertises the review
/// tools exactly when they are callable.
/// What: returns a `Vec<Value>`, one descriptor object per tool, mirroring the
/// trusty-review tool schemas but with the `tr_` name prefix.
/// Test: `mod.rs::tools_list_includes_tr_review_tools` asserts all three names
/// appear in the `tools/list` response.
pub(super) fn review_tool_descriptors() -> Vec<Value> {
    vec![
        serde_json::json!({
            "name": "tr_review_pr",
            "description": "LLM-backed review of a GitHub pull request via the embedded trusty-review pipeline. Fetches the PR diff, retrieves code context from trusty-search, augments with this analyzer daemon's static-analysis context (loopback), and returns a structured verdict (APPROVE / APPROVE* / REQUEST_CHANGES / BLOCK / UNKNOWN) with actionable findings. Requires GITHUB_TOKEN and AWS Bedrock credentials (or OPENROUTER_API_KEY). Always dry-run — never posts a GitHub comment.",
            "inputSchema": {
                "type": "object",
                "required": ["owner", "repo", "pr"],
                "properties": {
                    "owner": { "type": "string", "description": "GitHub organisation or user that owns the repository" },
                    "repo":  { "type": "string", "description": "GitHub repository name" },
                    "pr":    { "type": "integer", "description": "Pull request number" },
                    "reviewer_model": { "type": "string", "description": "Override the reviewer model slug (e.g. 'bedrock/us.anthropic.claude-sonnet-4-6')" }
                }
            }
        }),
        serde_json::json!({
            "name": "tr_review_diff",
            "description": "LLM-backed review of a raw unified diff string via the embedded trusty-review pipeline. No GitHub credentials required. Useful for reviewing local changes, staged diffs, or patches. Requires AWS Bedrock credentials (or OPENROUTER_API_KEY). trusty-search + this analyzer daemon supply code and static-analysis context.",
            "inputSchema": {
                "type": "object",
                "required": ["diff"],
                "properties": {
                    "diff":    { "type": "string", "description": "Unified diff string (output of `git diff` or similar)" },
                    "context": { "type": "string", "description": "Optional human-readable context (PR title/description, ticket, intent)" },
                    "reviewer_model": { "type": "string", "description": "Override the reviewer model slug (same format as tr_review_pr)" }
                }
            }
        }),
        serde_json::json!({
            "name": "tr_review_health",
            "description": "Probe the embedded trusty-review pipeline's liveness and configuration (dry_run mode, reviewer model, dependency URLs). Safe to call without any credentials.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

/// Dispatch a `tr_review_*` tool name into the embedded trusty-review pipeline.
///
/// Why: `mod.rs::call_tool` routes the three feature-gated names here so the
/// bridge owns the lazy `AppState` build, the name mapping (`tr_` → bare), and
/// the error translation in one place.
/// What: lazily builds (or reuses) the shared `AppState`, strips the `tr_`
/// prefix to recover the trusty-review tool name, delegates to
/// `trusty_review::mcp::call_review_tool`, and maps the result/error onto
/// `Result<Value, DispatchError>`.
/// Test: `mod.rs::tr_review_health_routes` checks the routing reaches this
/// handler; the full pipeline is covered by the live smoke test.
pub(super) async fn handle_tr_review(tool: &str, args: &Value) -> Result<Value, DispatchError> {
    // Strip the `tr_` prefix to recover the trusty-review tool name.
    let inner = tool.strip_prefix("tr_").unwrap_or(tool);

    let state = REVIEW_STATE
        .get_or_try_init(trusty_review::mcp::build_review_state)
        .await
        .map_err(|e| {
            DispatchError::Transport(format!("failed to build trusty-review state: {e}"))
        })?;

    match trusty_review::mcp::call_review_tool(inner, args, state).await {
        Ok(value) => Ok(value),
        Err(trusty_review::mcp::ReviewToolError::UnknownTool) => Err(DispatchError::UnknownTool),
        Err(trusty_review::mcp::ReviewToolError::InvalidParams(msg)) => {
            Err(DispatchError::InvalidParams(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_descriptors_are_tr_prefixed_and_complete() {
        let descs = review_tool_descriptors();
        let names: Vec<&str> = descs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names.len(), 3, "expected 3 tr_ tools, got {names:?}");
        for required in ["tr_review_pr", "tr_review_diff", "tr_review_health"] {
            assert!(names.contains(&required), "missing {required} in {names:?}");
        }
        // Every tr_ tool carries an inputSchema.
        for d in &descs {
            assert!(d.get("inputSchema").is_some(), "missing inputSchema: {d}");
        }
    }
}
