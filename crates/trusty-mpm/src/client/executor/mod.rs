//! The single command executor (dispatch facade).
//!
//! Why: before this crate, three UIs each translated operator intent into
//! daemon HTTP calls independently. [`CommandExecutor`] is the *one* place that
//! mapping lives — every UI hands it a [`TrustyCommand`] and gets back a
//! [`CommandResult`]. A new endpoint is wired here once. The executor was a
//! single ~540-line file; it is now split into focused submodules so each stays
//! well under the 500-SLOC cap and each concern (project sessions, managed
//! sessions, pairing) lives in its own file: this `mod.rs` owns only the struct,
//! the constructor, and the [`execute`](CommandExecutor::execute) dispatch.
//! What: [`CommandExecutor`] owns a [`DaemonClient`]; [`execute`] maps each
//! [`TrustyCommand`] to the corresponding submodule method. The project-session
//! family lives in [`sessions`], the managed session-manager verbs in
//! [`managed`], and the pairing handshake in [`pairing`]. Unreachable-daemon
//! errors become a [`CommandResult::Error`], never a panic.
//! Test: `cargo test -p trusty-mpm` covers the pure `/help` path and the HTTP
//! paths against an in-process test daemon (`tests.rs`).

use super::command::{TrustyCommand, help_text};
use super::http_client::DaemonClient;
use super::result::CommandResult;

mod managed;
mod pairing;
mod sessions;

#[cfg(test)]
mod tests;

// Re-export the pure helpers the executor tests reference by their historical
// paths (`executor::resolve_session`, `executor::truncate_output`,
// `executor::MAX_OUTPUT_CHARS`). Only the test module consumes these names, so
// the re-export is test-gated to avoid an unused-import warning in lib builds.
#[cfg(test)]
pub(crate) use sessions::{MAX_OUTPUT_CHARS, resolve_session, truncate_output};

/// Translates [`TrustyCommand`]s into daemon HTTP calls.
///
/// Why: the single seam between UI intent and the daemon API; isolating it here
/// means the Telegram bot, the TUI, and the CLI never embed HTTP logic.
/// What: wraps a [`DaemonClient`]; [`execute`](Self::execute) runs one command
/// end-to-end by delegating to the [`sessions`], [`managed`], and [`pairing`]
/// submodules.
/// Test: `execute_help_returns_help`, `execute_sessions_against_test_daemon`.
pub struct CommandExecutor {
    /// The shared daemon HTTP client.
    client: DaemonClient,
}

impl CommandExecutor {
    /// Build an executor targeting `daemon_url`.
    ///
    /// Why: a UI constructs one executor for the daemon it was pointed at.
    /// What: wraps a fresh [`DaemonClient`] for `daemon_url`.
    /// Test: `execute_help_returns_help`.
    pub fn new(daemon_url: impl Into<String>) -> Self {
        Self {
            client: DaemonClient::new(daemon_url),
        }
    }

    /// The underlying daemon client.
    ///
    /// Why: a UI's alert loop and pairing flow need direct client access
    /// alongside command execution.
    /// What: returns a reference to the wrapped [`DaemonClient`].
    /// Test: covered by the pairing tests.
    pub fn client(&self) -> &DaemonClient {
        &self.client
    }

    /// Execute one [`TrustyCommand`] against the daemon.
    ///
    /// Why: the single dispatch point — every UI funnels intent through here.
    /// What: maps each command to its submodule method and returns a structured
    /// [`CommandResult`]; a transport failure becomes [`CommandResult::Error`].
    /// `Pair { code: Some(_) }` cannot complete here (it needs a chat id) and is
    /// reported as a state query — UIs must call
    /// [`Self::pair_confirm`](Self::pair_confirm) instead.
    /// Test: `execute_help_returns_help`, `execute_sessions_against_test_daemon`,
    /// `execute_kill_returns_killed`, and the `managed_*` dispatch tests.
    pub async fn execute(&self, cmd: TrustyCommand) -> CommandResult {
        match cmd {
            // ── Pure / static ───────────────────────────────────────────────
            TrustyCommand::Help => CommandResult::Help(help_text().to_string()),
            TrustyCommand::Alerts => CommandResult::AlertSubscriptions(vec![
                "Categories: Permission, Agent".to_string(),
                "Memory alerts: enabled".to_string(),
            ]),
            // ── Project-session family ──────────────────────────────────────
            TrustyCommand::Sessions => self.sessions().await,
            TrustyCommand::Status { session_id } => self.status(&session_id).await,
            TrustyCommand::Approve { session_id } => self.decide(&session_id, true).await,
            TrustyCommand::Deny { session_id } => self.decide(&session_id, false).await,
            TrustyCommand::Overseer => self.overseer().await,
            TrustyCommand::Tmux => self.tmux().await,
            TrustyCommand::Projects => self.projects().await,
            TrustyCommand::Discover => self.discover().await,
            TrustyCommand::Adopt { session } => self.adopt(&session).await,
            TrustyCommand::Config { project } => self.config(&project).await,
            TrustyCommand::Snapshot { session } => self.snapshot(&session).await,
            TrustyCommand::Kill { session_id } => self.kill(&session_id).await,
            TrustyCommand::Send { session, prompt } => self.send(&session, &prompt).await,
            TrustyCommand::Launch { project, .. } => self.launch(&project).await,
            TrustyCommand::Connect { project, .. } => self.connect(&project).await,
            TrustyCommand::Start => self.pair_state().await,
            TrustyCommand::Doctor => self.doctor().await,
            TrustyCommand::CoordinatorChat { message } => self.coordinator_chat(&message).await,
            // ── Managed session-manager verbs ───────────────────────────────
            TrustyCommand::ManagedNew {
                repo_url,
                git_ref,
                task,
                name_hint,
                runtime,
            } => {
                self.managed_new(repo_url, git_ref, task, name_hint, runtime)
                    .await
            }
            TrustyCommand::ManagedList => self.managed_list().await,
            TrustyCommand::ManagedGet { target } => self.managed_get(&target).await,
            TrustyCommand::ManagedSend { target, text } => self.managed_send(&target, &text).await,
            TrustyCommand::ManagedAnswer { target, answer } => {
                self.managed_answer(&target, &answer).await
            }
            TrustyCommand::ManagedActivity { target } => self.managed_activity(&target).await,
            TrustyCommand::ManagedAttachCmd { target } => self.managed_attach_cmd(&target).await,
            TrustyCommand::ManagedRuntimeStop { target } => {
                self.managed_runtime_stop(&target).await
            }
            TrustyCommand::ManagedResume { target } => self.managed_resume(&target).await,
            TrustyCommand::ManagedDecommission { target } => {
                self.managed_decommission(&target).await
            }
            // ── Pairing (state query only; confirm needs a chat id) ──────────
            TrustyCommand::Pair { code: None } => self.pair_state().await,
            TrustyCommand::Pair { code: Some(_) } => {
                // A code-carrying pair requires the caller's chat id, which is
                // not part of the command; UIs route those to `pair_confirm`.
                self.pair_state().await
            }
        }
    }
}
