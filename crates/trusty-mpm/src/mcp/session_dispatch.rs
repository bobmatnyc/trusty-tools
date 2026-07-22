//! Argument parsing + routing for the eleven session-lifecycle MCP tools
//! (#1221 + #1508 + #2012 + the PM pause/resume context tools).
//!
//! Why: the session-lifecycle tools more than half-again the catalog; routing
//! them inline in `mcp/mod.rs`'s `dispatch_tool_call` would push that file over
//! the 500-SLOC production cap. Extracting the parse-and-call arms into a sibling
//! module keeps `mod.rs` focused on the handshake + core tools and gives the
//! session tools one auditable home.
//! What: [`try_dispatch`] matches a tool name against the eleven session tools
//! (seven per-session #1221 + #2012 ops + the two fleet-wide #1508 teardown
//! verbs + the two PM pause/resume context tools, `session_context_catchup` /
//! `session_context_pause` — a DIFFERENT concept, the calling PM session's own
//! snapshot/digest, not managed-sub-session lifecycle); when it matches it
//! parses arguments (via the shared `required_str` helper), calls the
//! corresponding [`super::OrchestratorBackend`] method, and returns
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
/// What: returns `Some(Result)` for the nine session tool names — the seven
/// per-session ops (including #2012's `session_delete`) plus the two
/// fleet-wide #1508 verbs (`session_decommission_ephemeral`, `session_prune`)
/// — parsing args and calling the matching backend method, or `None` when
/// `name` is not a session tool — signalling the caller to fall through to its
/// "unknown tool" branch.
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
        // #2012: hard-delete the record; `force` defaults to false (fail-closed).
        "session_delete" => match required_str(args, "session_id") {
            Ok(id) => {
                let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
                backend.session_delete(&id, force).await
            }
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
        // PM pause/resume context tools — DIFFERENT concept from the
        // managed-session lifecycle ops above (see `super::OrchestratorBackend`
        // doc comments on these two methods).
        "session_context_catchup" => session_context_catchup(backend, args).await,
        "session_context_pause" => session_context_pause(backend, args).await,
        // #1508 fleet-wide teardown tools — no per-session id required.
        "session_decommission_ephemeral" => backend.session_decommission_ephemeral().await,
        "session_prune" => match required_str(args, "state") {
            Ok(state) => {
                let dry_run = args
                    .get("dry_run")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let include_active = args
                    .get("include_active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                backend.session_prune(&state, dry_run, include_active).await
            }
            Err(e) => Err(e),
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
    let ephemeral = args.get("ephemeral").and_then(Value::as_bool);
    backend
        .session_new(&repo_url, &git_ref, &task, name_hint, runtime, ephemeral)
        .await
}

/// Parse `session_context_catchup` arguments and call the backend.
///
/// Why: `project_dir` is required (the stdio bridge forwards no cwd); the
/// other three fields are all optional with documented defaults.
/// What: requires `project_dir`; reads the optional `session_id`,
/// `all_projects` (default false), and `full` (default false).
/// Test: `super::tests::dispatch_session_context_catchup_tool`,
/// `super::tests::dispatch_session_context_catchup_requires_project_dir`.
async fn session_context_catchup<B: OrchestratorBackend>(
    backend: &B,
    args: &Value,
) -> Result<Value, String> {
    let project_dir = required_str(args, "project_dir")?;
    let session_id = args.get("session_id").and_then(Value::as_str);
    let all_projects = args
        .get("all_projects")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
    backend
        .session_context_catchup(&project_dir, session_id, all_projects, full)
        .await
}

/// Parse `session_context_pause` arguments and call the backend.
///
/// Why: `project_dir`/`session_id`/`summary` are required; the three
/// bullet-list sections default to empty (omitted from the written snapshot),
/// `tmux_window` defaults to absent, and `prune_worktrees` defaults to `true`
/// (pause wrap-up prunes by default, matching the bash skill it replaces).
/// What: parses the required fields, reads optional string arrays via
/// [`string_array`], and calls [`OrchestratorBackend::session_context_pause`].
/// Test: `super::tests::dispatch_session_context_pause_tool`,
/// `super::tests::dispatch_session_context_pause_requires_summary`.
async fn session_context_pause<B: OrchestratorBackend>(
    backend: &B,
    args: &Value,
) -> Result<Value, String> {
    let project_dir = required_str(args, "project_dir")?;
    let session_id = required_str(args, "session_id")?;
    let summary = required_str(args, "summary")?;
    let completed = string_array(args, "completed");
    let in_progress = string_array(args, "in_progress");
    let next_steps = string_array(args, "next_steps");
    let tmux_window = args.get("tmux_window").and_then(Value::as_str);
    let prune_worktrees = args
        .get("prune_worktrees")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    backend
        .session_context_pause(
            &project_dir,
            &session_id,
            &summary,
            completed,
            in_progress,
            next_steps,
            tmux_window,
            prune_worktrees,
        )
        .await
}

/// Read an optional array-of-strings argument, defaulting to empty.
///
/// Why: `completed`/`in_progress`/`next_steps` share the same shape; a shared
/// helper keeps [`session_context_pause`] readable.
/// What: returns the string elements of `args[key]` when it is a JSON array,
/// silently skipping any non-string element; `[]` when absent or the wrong type.
/// Test: exercised by `super::tests::dispatch_session_context_pause_tool`.
fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
