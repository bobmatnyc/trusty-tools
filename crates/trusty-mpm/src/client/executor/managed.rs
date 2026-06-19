//! Managed session-manager command methods (`/api/v1/sessions/managed/*`).
//!
//! Why: the managed verb set is the convergence target for every managed-session
//! UI (the `tm` CLI today; STUI #1272, TELUI #1433, Web/Slack next). Each verb
//! maps one-to-one onto a typed [`crate::client::DaemonClient`] managed method,
//! so wiring them here once gives every adapter the full surface through the
//! shared [`CommandResult`]. Splitting these out of the dispatch facade keeps
//! every executor file under the 500-SLOC cap.
//! What: inherent `CommandExecutor` methods, one per managed verb. Verbs that
//! take a target resolve the fuzzy id-or-name against the live managed list via
//! the canonical [`resolve_target`] resolver before calling the endpoint, so a
//! friendly name or prefix works everywhere. Transport/HTTP failures become a
//! renderable [`CommandResult::Error`], never a panic.
//! Test: the `execute_managed_*` tests in `tests.rs` (wire-shape + dispatch).

use super::CommandExecutor;
use crate::client::http_client::{ManagedSessionSummary, ManagedSpawnRequest};
use crate::client::resolver::resolve_target;
use crate::client::result::{CommandResult, ManagedSessionView};

impl CommandExecutor {
    /// Resolve a fuzzy managed target to its canonical id.
    ///
    /// Why: every target-taking managed verb accepts an id, a friendly name, or
    /// an unambiguous prefix; resolving once against the live list keeps the
    /// endpoint call unambiguous and the precedence identical across verbs.
    /// What: fetches the managed list, applies [`resolve_target`], and returns
    /// the matched session's canonical `id`. Returns `Err(CommandResult::Error)`
    /// when the daemon is unreachable or nothing matches, so callers can `?`-style
    /// short-circuit with a renderable error.
    /// Test: covered by `execute_managed_get_unknown_errors` and the happy-path
    /// managed tests.
    async fn resolve_managed(&self, target: &str) -> Result<ManagedSessionSummary, CommandResult> {
        let sessions = self
            .client()
            .list_managed_sessions()
            .await
            .map_err(|e| CommandResult::Error(format!("daemon unreachable: {e}")))?;
        match resolve_target(&sessions, target) {
            Some(found) => Ok(found.clone()),
            None => Err(CommandResult::Error(format!(
                "managed session {target} not found"
            ))),
        }
    }

    /// `managed-new` — spawn a managed session from a repo + ref + task.
    pub(super) async fn managed_new(
        &self,
        repo_url: String,
        git_ref: String,
        task: String,
        name_hint: Option<String>,
        runtime: Option<String>,
    ) -> CommandResult {
        let req = ManagedSpawnRequest {
            repo_url,
            git_ref,
            task,
            name_hint,
            runtime,
        };
        match self.client().spawn_managed_session(&req).await {
            Ok(resp) => CommandResult::ManagedSpawned {
                id: resp.id,
                name: resp.name,
                state: resp.state,
                runtime: resp.runtime,
                attach_cmd: resp.attach_cmd,
            },
            Err(e) => CommandResult::Error(format!("spawn failed: {e}")),
        }
    }

    /// `managed-list` — list every managed session.
    pub(super) async fn managed_list(&self) -> CommandResult {
        match self.client().list_managed_sessions().await {
            Ok(sessions) => CommandResult::ManagedSessions(
                sessions.into_iter().map(ManagedSessionView::from).collect(),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `managed-get` — fetch one managed session's record.
    ///
    /// Why: resolves a fuzzy target then fetches the authoritative per-session
    /// record (rather than reusing the list row) so the detail view reflects the
    /// freshest state.
    /// What: resolves `target`, GETs the per-session endpoint, and returns a
    /// [`ManagedSessionView`].
    /// Test: `execute_managed_get_unknown_errors`.
    pub(super) async fn managed_get(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().get_managed_session(&id).await {
            Ok(summary) => CommandResult::ManagedSession(ManagedSessionView::from(summary)),
            Err(e) => CommandResult::Error(format!("get failed: {e}")),
        }
    }

    /// `managed-send` — inject text into a managed session's pane.
    pub(super) async fn managed_send(&self, target: &str, text: &str) -> CommandResult {
        if text.trim().is_empty() {
            return CommandResult::Error("send: text is required".to_string());
        }
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().send_to_managed_session(&id, text).await {
            Ok(resp) => CommandResult::ManagedSent {
                id,
                tmux_name: resp.tmux_name,
            },
            Err(e) => CommandResult::Error(format!("send failed: {e}")),
        }
    }

    /// `managed-answer` — answer a managed session's pending decision.
    ///
    /// Why: the direct decision-answer verb. It reaches the REAL
    /// `POST .../{id}/answer` endpoint so the harness actually unblocks (the
    /// `/approve`/`/deny` family routes here too via the sessions module).
    /// What: resolves `target`, POSTs `{ answer }` to the answer endpoint, and
    /// returns a [`CommandResult::ManagedAnswered`].
    /// Test: `execute_managed_answer_unknown_errors`,
    /// `execute_managed_answer_routes_to_answer_endpoint`.
    pub(super) async fn managed_answer(&self, target: &str, answer: &str) -> CommandResult {
        if answer.trim().is_empty() {
            return CommandResult::Error("answer: text is required".to_string());
        }
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().answer_managed_session(&id, answer).await {
            Ok(resp) => CommandResult::ManagedAnswered {
                id,
                answer: answer.to_string(),
                tmux_name: resp.tmux_name,
            },
            Err(e) => CommandResult::Error(format!("answer failed: {e}")),
        }
    }

    /// `managed-activity` — inspect a managed session's activity.
    pub(super) async fn managed_activity(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().managed_session_activity(&id).await {
            Ok(act) => CommandResult::ManagedActivity {
                id,
                state: act.state,
                summary: act.summary,
                raw_pane: act.raw_pane,
                runtime_active: act.runtime_active,
                pending_decision: act.pending_decision,
            },
            Err(e) => CommandResult::Error(format!("activity failed: {e}")),
        }
    }

    /// `managed-attach` — show a managed session's tmux attach command.
    pub(super) async fn managed_attach_cmd(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().managed_session_attach_cmd(&id).await {
            Ok(resp) => CommandResult::ManagedAttachCmd {
                id,
                attach_cmd: resp.attach_cmd,
            },
            Err(e) => CommandResult::Error(format!("attach-cmd failed: {e}")),
        }
    }

    /// `managed-stop` — stop a managed session's runtime (keeps the workspace).
    pub(super) async fn managed_runtime_stop(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().runtime_stop_managed_session(&id).await {
            Ok(summary) => lifecycle(summary, "stopped"),
            Err(e) => CommandResult::Error(format!("runtime-stop failed: {e}")),
        }
    }

    /// `managed-resume` — resume a stopped managed session.
    pub(super) async fn managed_resume(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().resume_managed_session(&id).await {
            Ok(summary) => lifecycle(summary, "resumed"),
            Err(e) => CommandResult::Error(format!("resume failed: {e}")),
        }
    }

    /// `managed-decommission` — decommission a session (terminal; removes the
    /// workspace).
    pub(super) async fn managed_decommission(&self, target: &str) -> CommandResult {
        let id = match self.resolve_managed(target).await {
            Ok(s) => s.id,
            Err(err) => return err,
        };
        match self.client().decommission_managed_session(&id).await {
            Ok(summary) => lifecycle(summary, "decommissioned"),
            Err(e) => CommandResult::Error(format!("decommission failed: {e}")),
        }
    }
}

/// Map a managed summary + verb to a [`CommandResult::ManagedLifecycle`].
///
/// Why: the three lifecycle verbs (stop/resume/decommission) all report the same
/// id/name/state shape; one helper keeps their result construction identical.
/// What: builds the lifecycle result from the updated summary and the verb that
/// produced it.
/// Test: covered by the managed lifecycle dispatch tests.
fn lifecycle(summary: ManagedSessionSummary, action: &str) -> CommandResult {
    CommandResult::ManagedLifecycle {
        id: summary.id,
        name: summary.name,
        state: summary.state,
        action: action.to_string(),
    }
}
