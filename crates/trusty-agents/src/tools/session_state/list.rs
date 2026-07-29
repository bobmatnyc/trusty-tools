//! `session_state_list` — read-only enumeration of managed sessions (#4171).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

use super::store::{SessionView, default_store_path, load_sessions};

/// Default number of sessions returned when the caller names no `limit`.
///
/// Why: the shipped store on a working machine holds hundreds of records
/// (including long-dead ones); returning all of them would bury the in-flight
/// work an orchestrator actually asked about and burn the turn's context.
/// Twenty-five is roughly a screen of the most-recently-active sessions.
/// What: the `limit` default; `limit` is clamped to [`MAX_LIMIT`].
/// Test: `list_defaults_to_twenty_five_rows`.
const DEFAULT_LIMIT: usize = 25;

/// Hard ceiling on `limit`, whatever the caller asks for.
///
/// Why: a model that passes `limit = 100000` should get a large answer, not an
/// unbounded one — this tool's output goes straight into the next LLM request.
/// What: the clamp applied to any caller-supplied `limit`.
/// Test: `list_clamps_an_oversized_limit`.
const MAX_LIMIT: usize = 200;

/// How much of a session's free-text task description is shown per row.
///
/// Why: task strings are frequently a full paragraph of pasted instructions;
/// a one-line-per-session listing has to truncate to stay legible.
/// What: characters kept before an ellipsis is appended.
/// Test: `list_truncates_long_task_text`.
const TASK_PREVIEW_CHARS: usize = 100;

/// `session_state_list` — list managed sessions, read-only.
///
/// Why: The first thing PM orchestration needs is "what is in flight?". This
/// tool answers it from the on-disk session store without a daemon, a
/// subprocess or a socket, so it works (and is testable) whether or not the
/// orchestration harness is currently running.
/// What: renders one line per session — id, tmux name, state, branch,
/// last activity, truncated task — newest activity first. Optional `state`
/// and `project` filters and a `limit` narrow the result. L0-ONLY: this
/// executor is constructed exclusively by
/// [`super::session_state_tools`] for [`crate::agents::AgentTier::L0Orchestration`],
/// and its name is additionally stripped for every other tier by
/// [`super::retain_tier_permitted`].
/// Test: `list_renders_sessions_newest_first`, `list_filters_by_state`,
/// `list_filters_by_project_substring`, `list_defaults_to_twenty_five_rows`,
/// `list_clamps_an_oversized_limit`, `list_truncates_long_task_text`,
/// `list_reports_empty_store_legibly`.
pub struct SessionStateListTool {
    /// Overrides the default store location. `None` in production; set by
    /// tests so they never read the developer's real store.
    store_path: Option<std::path::PathBuf>,
}

impl SessionStateListTool {
    /// Construct the production tool, reading the canonical store path.
    pub fn new() -> Self {
        Self { store_path: None }
    }

    /// Construct a tool bound to an explicit store file (tests only).
    ///
    /// Why: the executors resolve `~/.trusty-mpm/session-manager/sessions.json`
    /// lazily at `execute` time, which is right for production but untestable
    /// without mutating process-global `HOME`. An explicit override keeps the
    /// tests hermetic and parallel-safe.
    /// What: stores `path` and uses it instead of [`default_store_path`].
    /// Test: every `list_*` test in `tests.rs` constructs through this.
    #[cfg(test)]
    pub fn with_store_path(path: std::path::PathBuf) -> Self {
        Self {
            store_path: Some(path),
        }
    }

    /// Resolve the store file to read.
    fn path(&self) -> Option<std::path::PathBuf> {
        self.store_path.clone().or_else(default_store_path)
    }
}

impl Default for SessionStateListTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one session as a single legible line.
///
/// Why: pulled out of `execute` so the row format is unit-testable without
/// building a store file, and so `session_state_status`'s multi-line detail
/// view cannot accidentally diverge from the list's field names.
/// What: `<id-12>  <tmux_name>  [<state>]  <branch>  <last-activity>  <task>`,
/// with absent optional fields rendered as `-`.
/// Test: `list_renders_sessions_newest_first`, `list_truncates_long_task_text`.
fn render_row(s: &SessionView) -> String {
    let id_short: String = s.id.chars().take(12).collect();
    let task = truncate(&s.task, TASK_PREVIEW_CHARS);
    format!(
        "{id_short}  {}  [{}]  branch={}  active={}  {}",
        if s.tmux_name.is_empty() {
            "-"
        } else {
            s.tmux_name.as_str()
        },
        if s.state.is_empty() {
            "unknown"
        } else {
            s.state.as_str()
        },
        s.branch.as_deref().unwrap_or("-"),
        s.last_activity_at.as_deref().unwrap_or("-"),
        if task.is_empty() { "-".into() } else { task },
    )
}

/// Cut `text` to `max` characters, appending an ellipsis when cut.
///
/// Why: `char`-based (not byte-based) so a multi-byte task description cannot
/// panic the renderer mid-character.
/// What: returns `text` unchanged when short enough, else the first `max`
/// chars plus `…`. Newlines are collapsed so one session stays one line.
/// Test: `list_truncates_long_task_text`.
fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max).collect();
    format!("{kept}…")
}

#[async_trait]
impl ToolExecutor for SessionStateListTool {
    fn name(&self) -> &str {
        "session_state_list"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "session_state_list",
                "description": "List the orchestration sessions recorded on this machine, most recently active first. Read-only: this reports state and never starts, stops, messages or modifies a session. Use it to answer 'what work is in flight?' before reconciling against git/PR/CI.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "state": {
                            "type": "string",
                            "description": "Keep only sessions whose lifecycle state matches this value, case-insensitively (e.g. running, paused, stopped)."
                        },
                        "project": {
                            "type": "string",
                            "description": "Keep only sessions whose project identity, working directory or workspace path contains this substring."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum rows to return. Defaults to 25, capped at 200.",
                            "minimum": 1
                        }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(path) = self.path() else {
            return ToolResult::err(
                "cannot resolve the session store: no home directory".to_string(),
            );
        };
        let sessions = match load_sessions(&path) {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let state_filter = args
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        let project_filter = args
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let matched: Vec<&SessionView> = sessions
            .iter()
            .filter(|s| match &state_filter {
                Some(want) => s.state.to_ascii_lowercase() == *want,
                None => true,
            })
            .filter(|s| match &project_filter {
                Some(want) => {
                    let haystack = format!(
                        "{} {} {}",
                        s.source_id.as_deref().unwrap_or(""),
                        s.cwd,
                        s.workspace_path.as_deref().unwrap_or("")
                    )
                    .to_ascii_lowercase();
                    haystack.contains(want)
                }
                None => true,
            })
            .collect();

        if matched.is_empty() {
            return ToolResult::ok(
                "No orchestration sessions matched. (The session store is empty, absent, or every \
                 record was filtered out.)"
                    .to_string(),
            );
        }
        let shown = matched.len().min(limit);
        let mut out = format!(
            "{} session(s) matched; showing {shown} (most recently active first):\n",
            matched.len()
        );
        for s in matched.iter().take(limit) {
            out.push_str(&render_row(s));
            out.push('\n');
        }
        ToolResult::ok(out)
    }
}
