//! The manager's action seam for the proposal-and-confirm flow (WI-9, #2586).
//!
//! Why: DOC-36 §3.2 lets the Layer-3 manager ACT on a resolved route — launch a
//! session for a project, or inject/summarize a specific session — but DOC-35 §11
//! forbids any silent/background mutation: every action must be one deliberate,
//! traceable call. The `/manager/act` handler (`act.rs`) owns the propose→confirm
//! protocol; this module owns the EXECUTION seam it drives on confirm, built so it
//! never reimplements the two subsystems it composes. A session launch calls
//! #2108's launch verb ([`spawn_managed`], DOC-35 §3.2); a session-directed
//! message routes through L2's existing [`SessionProxy`] (`client/proxy.rs`) —
//! never a direct tmux mutation. Splitting the two composed operations behind
//! their own small seams ([`SessionLauncher`] + the injected [`SessionProxy`])
//! is exactly what lets the hermetic suite (#2586 AC) drive the whole flow with a
//! test-double launcher and a test-double [`crate::client::proxy::ManagedBackend`]
//! under a real [`SessionProxy`], with no live session or channel.
//! What: [`ManagerActuator`] (the trait the handler consumes, overridable on
//! [`super::ManagerState`] for tests), [`ProxyActuator`] (the ONE concrete impl,
//! used in production over [`DaemonLauncher`] + the daemon's real proxy and in
//! tests over doubles), the [`SessionLauncher`] launch seam + its production
//! [`DaemonLauncher`], and the structured [`LaunchOutcome`]/[`InjectOutcome`]/
//! [`SummarizeOutcome`] results the handler renders.
//! Test: `daemon_launcher_unknown_project_errors` here; the propose→confirm HTTP
//! flow (with a test-double launcher + `SessionProxy` over a mock backend) in
//! `tests/manager_routing.rs`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::client::proxy::{
    FocusOutcome, FocusTarget, InjectOutcome as ProxyInjectOutcome, SessionProxy,
    SummarizeOutcome as ProxySummarizeOutcome,
};
use crate::daemon::managed_routes::{SpawnParams, spawn_managed};
use crate::daemon::state::DaemonState;
use crate::session_manager::record::ManagedSessionId;

/// A launched session, as reported back to a confirming caller.
///
/// Why: the confirm response echoes just enough of the newly-created session for
/// the caller (CLI/channel) to name and reach it, without leaking the full record.
/// What: the canonical session id, its tmux/display name, and its lifecycle state.
/// Test: exercised via the launch HTTP path in `tests/manager_routing.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOutcome {
    /// Canonical managed-session id.
    pub session_id: String,
    /// Friendly session name.
    pub name: String,
    /// Lifecycle state immediately after launch.
    pub state: String,
}

/// Result of a confirmed INJECT (focus-then-send through [`SessionProxy`]).
///
/// Why: focusing then injecting has genuinely distinct outcomes a caller must
/// render differently — the target could not be resolved, it was sent, the
/// session vanished mid-send (auto-unfocused), or a transient failure preserved
/// focus. Modelling all four keeps the handler exhaustive.
/// What: mirrors the proxy's own [`ProxyInjectOutcome`] plus a `NotFound` for the
/// focus-resolution failure that precedes the send.
/// Test: `tests/manager_routing.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectOutcome {
    /// The text was injected into the resolved session.
    Sent {
        /// The session it was sent to.
        target: FocusTarget,
        /// The injected text.
        text: String,
    },
    /// The session could not be resolved (focus failed) — nothing was sent.
    NotFound {
        /// The unresolved target.
        session: String,
        /// The backend error.
        error: String,
    },
    /// The session vanished during send; focus was auto-cleared.
    Vanished {
        /// The session that was targeted.
        target: FocusTarget,
        /// The "not found" error that triggered the auto-unfocus.
        error: String,
    },
    /// A transient failure during send.
    Failed {
        /// The session that was targeted.
        target: FocusTarget,
        /// The transport/daemon error.
        error: String,
    },
}

/// Result of a confirmed SUMMARIZE (focus-then-summarize through [`SessionProxy`]).
///
/// Why: mirrors [`InjectOutcome`]'s four-way split for the summarize direction.
/// What: the digest on success, or the same resolution/vanish/transient failures.
/// Test: `tests/manager_routing.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizeOutcome {
    /// A digest of the resolved session's recent activity.
    Summary {
        /// The summarized session.
        target: FocusTarget,
        /// The session's lifecycle state.
        state: String,
        /// The activity summary text.
        summary: String,
        /// Any decision the session is blocked on.
        pending_decision: Option<String>,
    },
    /// The session could not be resolved.
    NotFound {
        /// The unresolved target.
        session: String,
        /// The backend error.
        error: String,
    },
    /// The session vanished; focus was auto-cleared.
    Vanished {
        /// The session that was targeted.
        target: FocusTarget,
        /// The "not found" error.
        error: String,
    },
    /// A transient failure.
    Failed {
        /// The session that was targeted.
        target: FocusTarget,
        /// The transport/daemon error.
        error: String,
    },
}

/// The launch seam: create a session for a named project (#2108 launch verb).
///
/// Why: launching is the ONE genuinely stateful, environment-touching action the
/// manager can take, so it sits behind its own trait — the production impl calls
/// the real [`spawn_managed`] verb, while the hermetic suite injects a recording
/// double so the propose→confirm flow is testable with no provisioning.
/// What: `launch` maps a project name + task to a [`LaunchOutcome`] or an error
/// string (unknown project, spawn failure).
/// Test: `daemon_launcher_unknown_project_errors`; the double in
/// `tests/manager_routing.rs`.
#[async_trait]
pub trait SessionLauncher: Send + Sync {
    /// Launch a session for `project` with the given `task`.
    async fn launch(&self, project: &str, task: &str) -> Result<LaunchOutcome, String>;
}

/// Production [`SessionLauncher`] over this daemon's real launch verb.
///
/// Why: the manager must call #2108's launch verb EXPLICITLY (DOC-35 §3.2), never
/// a bespoke spawn path — so this resolves the project from the registry and calls
/// the shared [`spawn_managed`] the HTTP `spawn_session` route also uses.
/// What: wraps `Arc<DaemonState>`; `launch` looks up the project by exact name,
/// builds a turnkey [`SpawnParams`] from its `repo_url`/`default_branch`, and
/// spawns via [`spawn_managed`] with a fresh id.
/// Test: `daemon_launcher_unknown_project_errors` (the no-provisioning error
/// path); the happy path is covered by the existing spawn integration tests.
pub struct DaemonLauncher {
    state: Arc<DaemonState>,
}

impl DaemonLauncher {
    /// Wrap `state` for real-launch access.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl SessionLauncher for DaemonLauncher {
    async fn launch(&self, project: &str, task: &str) -> Result<LaunchOutcome, String> {
        let registry = self.state.project_registry().await;
        let projects = registry
            .list()
            .await
            .map_err(|e| format!("project registry read failed: {e}"))?;
        let Some(found) = projects.iter().find(|p| p.name == project) else {
            return Err(format!("project '{project}' is not registered"));
        };
        let params = SpawnParams {
            repo_url: found.repo_url.clone(),
            git_ref: found.default_branch.clone(),
            task: task.to_string(),
            name_hint: None,
            runtime: None,
            ephemeral: None,
            // Operator-confirmed manager action — a trusted, explicit path, never
            // the MCP spawn gate (SpawnParams::mcp_initiated).
            mcp_initiated: false,
            // Turnkey: inject the task once the runtime is ready (the default).
            inject_task: None,
            deliverable_id: None,
            // A confirmed "launch" is an explicit new-session intent.
            force_new: true,
        };
        let record = spawn_managed(&self.state, ManagedSessionId::new(), params).await?;
        Ok(LaunchOutcome {
            session_id: record.id.to_string(),
            name: record.tmux_name.clone(),
            state: record.state.to_string(),
        })
    }
}

/// Resolve the `/manager/act` + chat-confirm execution seam.
///
/// Why: BOTH `act.rs`'s confirm branch and `chat.rs`'s in-conversation confirm
/// turn (#2586) need the identical "test override, else fresh production
/// actuator" resolution — centralising it here is what makes the confirm-turn
/// wiring `reuse the actuator, not duplicate it` (coordinator review finding 1).
/// What: returns [`super::ManagerState::actuator_override`] when installed
/// (hermetic suite), else a fresh [`ProxyActuator::production`].
/// Test: exercised via `tests/manager_routing.rs` (act) and the chat
/// propose-confirm suite in the same file.
pub fn resolve_actuator(state: &Arc<DaemonState>) -> Arc<dyn ManagerActuator> {
    match state.manager_state().actuator_override() {
        Some(actuator) => actuator,
        None => Arc::new(ProxyActuator::production(state)),
    }
}

/// The action seam the `/manager/act` handler executes on confirm.
///
/// Why: type-erasing the concrete executor behind a trait is what lets
/// [`super::ManagerState`] carry an optional TEST override (installed via
/// [`super::ManagerState::set_actuator`]) so the hermetic suite swaps in a
/// [`ProxyActuator`] built over doubles, exactly as the inference seam is swapped
/// for a `ScriptedAdapter`. Production builds a fresh [`ProxyActuator`] per request.
/// What: three async verbs — `launch`, `inject`, `summarize` — each returning a
/// structured outcome the handler renders as JSON.
/// Test: `tests/manager_routing.rs` (over a double); production wiring in `act.rs`.
#[async_trait]
pub trait ManagerActuator: Send + Sync {
    /// Launch a session for `project` with `task` (the #2108 launch verb).
    async fn launch(&self, project: &str, task: &str) -> Result<LaunchOutcome, String>;
    /// Inject `text` into `session` for `conversation_key`, via [`SessionProxy`].
    async fn inject(&self, conversation_key: &str, session: &str, text: &str) -> InjectOutcome;
    /// Summarize `session` for `conversation_key`, via [`SessionProxy`].
    async fn summarize(&self, conversation_key: &str, session: &str) -> SummarizeOutcome;
}

/// The ONE concrete [`ManagerActuator`]: launch via a [`SessionLauncher`], and
/// inject/summarize via a real [`SessionProxy`].
///
/// Why: keeping a single implementation used in BOTH production and tests (only
/// its two seams differ) means the hermetic test exercises the SAME code path the
/// daemon runs — the real [`SessionProxy`] focus/inject/summarize state machine,
/// just over a test-double backend and launcher (#2586 AC). No parallel test-only
/// executor to drift from production.
/// What: holds an `Arc<dyn SessionLauncher>` and a [`SessionProxy`]; `inject`/
/// `summarize` focus the target for the conversation then act, mapping the proxy's
/// own outcomes (including the focus-resolution failure) into [`InjectOutcome`]/
/// [`SummarizeOutcome`].
/// Test: `tests/manager_routing.rs`.
pub struct ProxyActuator {
    launcher: Arc<dyn SessionLauncher>,
    proxy: SessionProxy,
}

impl ProxyActuator {
    /// Build a [`ProxyActuator`] from its two seams.
    ///
    /// Why: production passes [`DaemonLauncher`] + the daemon's real
    /// `local_proxy`; the hermetic test passes a recording launcher +
    /// `SessionProxy::new(mock_backend)`.
    /// What: stores both seams.
    /// Test: `tests/manager_routing.rs`.
    pub fn new(launcher: Arc<dyn SessionLauncher>, proxy: SessionProxy) -> Self {
        Self { launcher, proxy }
    }

    /// Build the production actuator from daemon state.
    ///
    /// Why: the `/manager/act` handler builds this fresh per request when no test
    /// override is installed — wiring the real launch verb and the daemon's shared
    /// proxy focus store in one place.
    /// What: a [`ProxyActuator`] over [`DaemonLauncher`] and
    /// [`crate::daemon::managed_routes::proxy::local_proxy`].
    /// Test: production wiring exercised via `act.rs`'s handler.
    pub fn production(state: &Arc<DaemonState>) -> Self {
        Self::new(
            Arc::new(DaemonLauncher::new(Arc::clone(state))),
            crate::daemon::managed_routes::proxy::local_proxy(state),
        )
    }
}

#[async_trait]
impl ManagerActuator for ProxyActuator {
    async fn launch(&self, project: &str, task: &str) -> Result<LaunchOutcome, String> {
        self.launcher.launch(project, task).await
    }

    async fn inject(&self, conversation_key: &str, session: &str, text: &str) -> InjectOutcome {
        // Focus the target first — resolution failure is a distinct, no-send
        // outcome — then inject through the SAME SessionProxy state machine every
        // channel uses (never a direct tmux mutation).
        match self.proxy.focus(conversation_key, session).await {
            FocusOutcome::NotFound { target, error } => InjectOutcome::NotFound {
                session: target,
                error,
            },
            FocusOutcome::Focused(_) | FocusOutcome::Current(_) => {
                match self.proxy.inject(conversation_key, text).await {
                    ProxyInjectOutcome::Sent { target, text } => {
                        InjectOutcome::Sent { target, text }
                    }
                    ProxyInjectOutcome::AutoUnfocused { target, error } => {
                        InjectOutcome::Vanished { target, error }
                    }
                    ProxyInjectOutcome::Failed { target, error } => {
                        InjectOutcome::Failed { target, error }
                    }
                    // Focus succeeded immediately above, so NoFocus is unreachable
                    // in practice; treat it as a resolution failure defensively.
                    ProxyInjectOutcome::NoFocus => InjectOutcome::NotFound {
                        session: session.to_string(),
                        error: "focus was cleared before inject".to_string(),
                    },
                }
            }
        }
    }

    async fn summarize(&self, conversation_key: &str, session: &str) -> SummarizeOutcome {
        match self.proxy.focus(conversation_key, session).await {
            FocusOutcome::NotFound { target, error } => SummarizeOutcome::NotFound {
                session: target,
                error,
            },
            FocusOutcome::Focused(_) | FocusOutcome::Current(_) => {
                match self.proxy.summarize(conversation_key).await {
                    ProxySummarizeOutcome::Summary {
                        target,
                        state,
                        summary,
                        pending_decision,
                    } => SummarizeOutcome::Summary {
                        target,
                        state,
                        summary,
                        pending_decision,
                    },
                    ProxySummarizeOutcome::AutoUnfocused { target, error } => {
                        SummarizeOutcome::Vanished { target, error }
                    }
                    ProxySummarizeOutcome::Failed { target, error } => {
                        SummarizeOutcome::Failed { target, error }
                    }
                    ProxySummarizeOutcome::NoFocus => SummarizeOutcome::NotFound {
                        session: session.to_string(),
                        error: "focus was cleared before summarize".to_string(),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production launcher rejects an unknown project BEFORE any provisioning
    /// side effect — a hermetic assertion needing no tmux/worktree.
    #[tokio::test]
    async fn daemon_launcher_unknown_project_errors() {
        let root = tempfile::tempdir().unwrap().keep();
        let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
        let launcher = DaemonLauncher::new(Arc::clone(&state));
        let err = launcher
            .launch("does-not-exist", "some task")
            .await
            .expect_err("unknown project must error");
        assert!(err.contains("not registered"), "{err}");
    }
}
