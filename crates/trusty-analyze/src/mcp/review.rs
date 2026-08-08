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
//! What: holds a process-wide lazily-built `AppState` cache (`OnceCell`) so the
//! expensive AWS-credential / provider build happens at most once, and async
//! handlers that map trusty-review's `ToolError` onto the dispatcher's
//! [`DispatchError`]. The three descriptors themselves live in
//! `mcp::descriptors::review_tool_descriptors` so they compile in every build —
//! the generated README/CLAUDE.md tool section must be true with and without
//! this feature, and CI does not build the crate with it.
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
