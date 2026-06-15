//! Argument parsing + routing for the six session-lifecycle MCP tools (#1221).
//!
//! Why: the session-lifecycle tools more than half-again the catalog; routing
//! them inline in `mcp/mod.rs`'s `dispatch_tool_call` would push that file over
//! the 500-SLOC production cap. Extracting the parse-and-call arms into a sibling
//! module keeps `mod.rs` focused on the handshake + core tools and gives the
//! session tools one auditable home.
//! What: [`try_dispatch`] matches a tool name against the six session tools;
//! when it matches it parses arguments (via the shared `required_str` helper),
//! calls the corresponding [`super::OrchestratorBackend`] method, and returns
//! `Some(result)`. A non-session tool name returns `None` so the caller can
//! report "unknown tool".
//! Test: the `super::tests` `dispatch_session_*` cases drive this module through
//! the public `dispatch` entry point with a mock backend.

use serde_json::Value;

use super::{OrchestratorBackend, required_str};

/// Default trailing-pane line count for `session_activity` when unspecified.
///
/// Why: the HTTP activity route captures the last 60 lines; the MCP tool keeps
/// the same default so both transports behave identically.
/// What: the `u32` line count used when the caller omits `lines`.
/// Test: `super::tests::dispatch_session_activity_default_lines`.
const DEFAULT_ACTIVITY_LINES: u32 = 60;

/// Route a session-lifecycle tool call to the backend.
///
/// Why: a single entry point lets `dispatch_tool_call` delegate every
/// session-tool name in one arm, keeping the core dispatch match small.
/// What: returns `Some(Result)` for the six session tool names (parsing args and
/// calling the matching backend method), or `None` when `name` is not a session
/// tool — signalling the caller to fall through to its "unknown tool" branch.
/// Errors from argument parsing are returned as `Some(Err(_))` so they surface
/// to the client as a tool-error result, identical to the core tools.
/// Test: exercised by every `dispatch_session_*` test in `super::tests`.
pub async fn try_dispatch<B: OrchestratorBackend>(
    backend: &B,
    name: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    let result = match name {
        "session_new" => session_new(backend, args).await,
        "session_stop" => match required_str(args, "session_id") {
            Ok(id) => backend.session_stop(&id).await,
            Err(e) => Err(e),
        },
        "session_resume" => match required_str(args, "session_id") {
            Ok(id) => backend.session_resume(&id).await,
            Err(e) => Err(e),
        },
        "session_decommission" => match required_str(args, "session_id") {
            Ok(id) => backend.session_decommission(&id).await,
            Err(e) => Err(e),
        },
        "session_activity" => match required_str(args, "session_id") {
            Ok(id) => {
                let lines = args
                    .get("lines")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32)
                    .unwrap_or(DEFAULT_ACTIVITY_LINES);
                backend.session_activity(&id, lines).await
            }
            Err(e) => Err(e),
        },
        "session_send" => match (required_str(args, "session_id"), required_str(args, "text")) {
            (Ok(id), Ok(text)) => backend.session_send(&id, &text).await,
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        // Not a session tool — let the caller report "unknown tool".
        _ => return None,
    };
    Some(result)
}

/// Parse `session_new` arguments and call the backend.
///
/// Why: `session_new` has the widest argument surface (repo/ref/task + two
/// optionals); pulling it into its own helper keeps [`try_dispatch`] readable.
/// What: requires `repo_url`, `ref`, and `task`; reads the optional `name_hint`
/// and `runtime`; then calls [`OrchestratorBackend::session_new`]. Any missing
/// required field yields a descriptive error string.
/// Test: `super::tests::dispatch_session_new_tool`,
/// `super::tests::dispatch_session_new_requires_repo_url`.
async fn session_new<B: OrchestratorBackend>(backend: &B, args: &Value) -> Result<Value, String> {
    let repo_url = required_str(args, "repo_url")?;
    let git_ref = required_str(args, "ref")?;
    let task = required_str(args, "task")?;
    let name_hint = args.get("name_hint").and_then(Value::as_str);
    let runtime = args.get("runtime").and_then(Value::as_str);
    backend
        .session_new(&repo_url, &git_ref, &task, name_hint, runtime)
        .await
}
