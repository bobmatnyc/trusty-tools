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

/// Dispatch `tr_report` into the embedded trusty-review report pipeline
/// (#6669).
///
/// Why: this is the one `tr_` tool that does NOT forward to trusty-review's own
/// MCP surface — that surface has no report tool, and adding one there would
/// put the same pipeline behind two dispatchers. Calling the library entry
/// point directly keeps one implementation, the same one the two CLIs drive.
/// What: validates and maps the arguments onto a
/// [`trusty_review::report::ReportRequest`], runs it, and returns the written
/// paths as JSON. The run is long — minutes — and always calls inference, so a
/// missing `OPENROUTER_API_KEY` surfaces as a transport error carrying the
/// pipeline's own message naming the variable.
/// Test: `report_args` below; routing by
/// `mod.rs::tr_report_routes_to_the_report_pipeline`.
pub(super) async fn handle_tr_report(args: &Value) -> Result<Value, DispatchError> {
    let req = report_args(args)?;
    let written = trusty_review::report::run_report(None, &req)
        .await
        .map_err(|e| DispatchError::Transport(format!("report failed: {e:#}")))?;
    Ok(serde_json::json!({
        "written": written
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
    }))
}

/// Map `tr_report` arguments onto a report request.
///
/// Why: separating the mapping from the run is what makes the argument contract
/// testable without a credential or a network call.
/// What: `manifest_path` is required; `template`, `code_only`, `out`,
/// `instructions` and `analyze` are optional, with `analyze` defaulting ON for
/// the same reason the CLI verb does — this process is the analyzer.
/// Test: `tests::{report_args_requires_a_manifest_path,
/// report_args_maps_every_field}`.
fn report_args(args: &Value) -> Result<trusty_review::report::ReportRequest, DispatchError> {
    let manifest = args
        .get("manifest_path")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            DispatchError::InvalidParams("manifest_path is required and must be a path".to_string())
        })?;
    let mut req = trusty_review::report::ReportRequest::new(manifest);
    req.template = args
        .get("template")
        .and_then(Value::as_str)
        .map(str::to_owned);
    req.code_only = args
        .get("code_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(out) = args.get("out").and_then(Value::as_str) {
        req.out = std::path::PathBuf::from(out);
    }
    req.instructions = args
        .get("instructions")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    req.analyze = args.get("analyze").and_then(Value::as_bool).unwrap_or(true);
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6669): the dispatcher must refuse a call it cannot serve rather
    /// than starting a multi-minute run against a path that is not there.
    /// What: an absent or blank `manifest_path` is invalid params.
    /// Test: this test itself.
    #[test]
    fn report_args_requires_a_manifest_path() {
        for bad in [
            serde_json::json!({}),
            serde_json::json!({ "manifest_path": "" }),
            serde_json::json!({ "manifest_path": "   " }),
            serde_json::json!({ "manifest_path": 7 }),
        ] {
            assert!(
                matches!(report_args(&bad), Err(DispatchError::InvalidParams(_))),
                "must refuse {bad}"
            );
        }
    }

    /// Why: a field that parses but never reaches the pipeline gives the caller
    /// the wrong report with no error.
    /// What: every optional field maps, and the defaults match the CLI verb's.
    /// Test: this test itself.
    #[test]
    fn report_args_maps_every_field() {
        let req = report_args(&serde_json::json!({
            "manifest_path": "/e/manifest.toml",
            "template": "cast",
            "code_only": true,
            "out": "/e/out",
            "instructions": "/e/brief.md"
        }))
        .expect("valid args");
        assert_eq!(req.manifest, std::path::PathBuf::from("/e/manifest.toml"));
        assert_eq!(req.template.as_deref(), Some("cast"));
        assert!(req.code_only);
        assert_eq!(req.out, std::path::PathBuf::from("/e/out"));
        assert_eq!(
            req.instructions.as_deref(),
            Some(std::path::Path::new("/e/brief.md"))
        );
        assert!(req.analyze, "the fetch defaults on for this daemon");

        let bare = report_args(&serde_json::json!({ "manifest_path": "m.toml" })).expect("valid");
        assert!(!bare.code_only);
        assert!(bare.template.is_none());
        assert_eq!(bare.out, std::path::PathBuf::from("./reports"));

        let no_fetch =
            report_args(&serde_json::json!({ "manifest_path": "m.toml", "analyze": false }))
                .expect("valid");
        assert!(!no_fetch.analyze);
    }
}
