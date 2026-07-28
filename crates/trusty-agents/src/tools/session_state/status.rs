//! `session_state_status` — read-only detail for one managed session (#4171).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

use super::store::{SessionView, default_store_path, load_sessions};

/// `session_state_status` — report one session's recorded state, read-only.
///
/// Why: after `session_state_list` narrows the field, PM orchestration needs
/// the full record for ONE session — which branch, which workspace, whether a
/// decision is pending — to reconcile it against live git/PR/CI. Reading the
/// store directly (no daemon, no subprocess) keeps this a pure file read and
/// therefore incapable of mutating anything.
/// What: resolves `session` by exact id, exact tmux name, or an id prefix of
/// at least six characters (see [`SessionView::matches`]) and renders the
/// record as labelled lines. An ambiguous prefix is reported as such rather
/// than silently resolving to the first hit. L0-ONLY: constructed exclusively
/// by [`super::session_state_tools`] for
/// [`crate::agents::AgentTier::L0Orchestration`], and its name is stripped
/// for every other tier by [`super::retain_tier_permitted`].
/// Test: `status_renders_full_record`, `status_matches_by_id_tmux_name_and_id_prefix`,
/// `status_rejects_too_short_a_prefix`, `status_reports_ambiguous_prefix`,
/// `status_unknown_session_is_a_recoverable_error`,
/// `status_requires_the_session_argument`.
pub struct SessionStateStatusTool {
    /// Overrides the default store location. `None` in production; set by
    /// tests so they never read the developer's real store.
    store_path: Option<std::path::PathBuf>,
}

impl SessionStateStatusTool {
    /// Construct the production tool, reading the canonical store path.
    pub fn new() -> Self {
        Self { store_path: None }
    }

    /// Construct a tool bound to an explicit store file (tests only).
    ///
    /// Why: same hermetic-test rationale as `SessionStateListTool`'s own
    /// `with_store_path` — see that constructor's doc comment.
    /// What: stores `path` and uses it instead of [`default_store_path`].
    /// Test: every `status_*` test in `tests.rs` constructs through this.
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

impl Default for SessionStateStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one session record as labelled lines.
///
/// Why: separated from `execute` so the field set is pinned by a test that
/// needs no store file, and so a field added to [`SessionView`] that nobody
/// renders is visible as a gap rather than hidden inside a match arm.
/// What: one `key: value` line per field, absent optionals shown as `-`.
/// Test: `status_renders_full_record`.
fn render_detail(s: &SessionView) -> String {
    let mut out = String::new();
    let mut line = |k: &str, v: &str| {
        out.push_str(k);
        out.push_str(": ");
        out.push_str(if v.is_empty() { "-" } else { v });
        out.push('\n');
    };
    line("id", &s.id);
    line("tmux_name", &s.tmux_name);
    line("state", &s.state);
    line("branch", s.branch.as_deref().unwrap_or(""));
    line("cwd", &s.cwd);
    line("workspace_path", s.workspace_path.as_deref().unwrap_or(""));
    line("project", s.source_id.as_deref().unwrap_or(""));
    line("created_at", &s.created_at);
    line(
        "last_activity_at",
        s.last_activity_at.as_deref().unwrap_or(""),
    );
    line(
        "pending_decision",
        s.pending_decision.as_deref().unwrap_or(""),
    );
    line("task", &s.task);
    out
}

#[async_trait]
impl ToolExecutor for SessionStateStatusTool {
    fn name(&self) -> &str {
        "session_state_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "session_state_status",
                "description": "Report the recorded state of ONE orchestration session — lifecycle state, git branch, workspace path, task, and any pending decision. Read-only: it never starts, stops, messages or modifies a session.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session": {
                            "type": "string",
                            "description": "The session id, its tmux name, or an id prefix of at least 6 characters."
                        }
                    },
                    "required": ["session"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(needle) = args
            .get("session")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err(
                "session_state_status requires a non-empty `session` argument (an id, a tmux name, \
                 or an id prefix of at least 6 characters)"
                    .to_string(),
            );
        };
        let Some(path) = self.path() else {
            return ToolResult::err(
                "cannot resolve the session store: no home directory".to_string(),
            );
        };
        let sessions = match load_sessions(&path) {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let hits: Vec<&SessionView> = sessions.iter().filter(|s| s.matches(needle)).collect();
        match hits.len() {
            0 => ToolResult::err(format!(
                "no orchestration session matches '{needle}'; call session_state_list to see what \
                 is recorded"
            )),
            1 => ToolResult::ok(render_detail(hits[0])),
            n => {
                let ids: Vec<&str> = hits.iter().map(|s| s.id.as_str()).collect();
                ToolResult::err(format!(
                    "'{needle}' is ambiguous — it matches {n} sessions ({}); pass a longer id",
                    ids.join(", ")
                ))
            }
        }
    }
}
