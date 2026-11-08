//! The tmux server a CLI launch starts its PM on (#10101, slice G).
//!
//! Why: `tm launch`, `tm connect` and the in-place `tm session start` create
//! the PM's tmux session themselves. A fresh PM belongs on its workspace's own
//! server (#8928 owner ruling, 2026-09-30), the one the daemon's create path
//! picks, and every later tmux call for it must reach that server: the
//! default server can hold an unrelated session with the same name.
//! What: [`LaunchHost`] is the chosen server and its socket, with the tmux
//! calls a launch makes on it; [`refuse_held_name`] refuses a name a managed
//! record holds (Architect ruling, 2026-11-08 14:47Z, matching the rename
//! check); [`LaunchSessionGuard`] kills a half-launched session on its server.
//! Test: `launch_host_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::core::tmux::{TmuxCommand, resolve_tmux_binary_or_bare, run_tmux_on};
use trusty_mpm::session_manager::ManagedSessionState;

/// The tmux server one CLI launch creates its PM on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LaunchHost {
    /// The `-L` server name; `None` is the default server.
    server: Option<String>,
    /// That server's socket, stored on the registration; `None` with `server`.
    socket: Option<PathBuf>,
}

impl LaunchHost {
    /// The server a fresh PM at `workspace` launches on: the daemon's rule,
    /// [`trusty_mpm::session_manager::launch_server::cli_launch_server`]
    /// (`TM_WORKSPACE_TMUX=0` keeps the default server).
    /// Test: `a_cli_host_is_the_workspace_server`.
    pub(crate) fn for_workspace(workspace: &Path) -> Self {
        match trusty_mpm::session_manager::launch_server::cli_launch_server(workspace) {
            Some((server, socket)) => Self {
                server: Some(server),
                socket: Some(socket),
            },
            None => Self::default(),
        }
    }

    /// The server a registered session lives on, from its stored `socket`.
    ///
    /// # Errors
    ///
    /// The socket names no server in this process's socket directory, so its
    /// server cannot be reached; the default server is never used instead.
    ///
    /// Test: `a_row_socket_routes_to_its_server`.
    pub(crate) fn of_socket(socket: Option<&Path>) -> anyhow::Result<Self> {
        let dir = trusty_mpm::core::tmux_server::ambient_socket_dir();
        let server = trusty_mpm::core::tmux_server::server_of_socket(socket, &dir)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(Self {
            server,
            socket: socket.map(Path::to_path_buf),
        })
    }

    /// The `-L` server name; `None` is the default server.
    pub(crate) fn server(&self) -> Option<&str> {
        self.server.as_deref()
    }

    /// The socket the registration records; `None` is the default server.
    pub(crate) fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// The server, as an operator reads it in an error.
    pub(crate) fn describe(&self) -> String {
        match &self.server {
            Some(server) => format!("tmux server {server}"),
            None => "the default tmux server".to_owned(),
        }
    }

    /// Create detached session `name` in `workdir` on this server, through
    /// the crate's one creation choke point (#2398) with `-A` as before.
    ///
    /// What: an unverified scrollback limit is a printed warning (#3386).
    ///
    /// # Errors
    ///
    /// tmux could not run or `new-session` failed; the error names the server.
    ///
    /// Test: `a_cli_create_reaches_the_workspace_server_only`.
    pub(crate) fn create(&self, name: &str, workdir: &str) -> anyhow::Result<()> {
        let outcome = trusty_mpm::core::tmux::create_managed_session_on(
            None,
            self.server(),
            name,
            Some(workdir),
            true,
        );
        let outcome = match outcome {
            Ok(o) if o.output.status.success() => o,
            Ok(o) => anyhow::bail!(
                "failed to create tmux session {name} in {workdir} on {}: {}",
                self.describe(),
                String::from_utf8_lossy(&o.output.stderr).trim()
            ),
            Err(e) => anyhow::bail!(
                "failed to create tmux session {name} in {workdir} on {}: {e}",
                self.describe()
            ),
        };
        if !outcome.options_verified {
            eprintln!(
                "warning: tmux scrollback limit could not be verified for session {name} — \
                 the pane may be capped at tmux's factory 2000-line history-limit instead of \
                 the configured value; see issue #3386"
            );
        }
        Ok(())
    }

    /// Whether session `name` exists on this server.
    /// Test: `a_cli_create_reaches_the_workspace_server_only`.
    pub(crate) fn has_session(&self, name: &str) -> bool {
        let cmd = TmuxCommand::HasSession {
            name: name.to_owned(),
        };
        run_tmux_on(&resolve_tmux_binary_or_bare(), self.server(), &cmd)
            .is_ok_and(|o| o.status.success())
    }

    /// Kill session `name` on this server, best-effort.
    /// Test: `a_dropped_guard_kills_on_its_own_server`.
    pub(crate) fn kill(&self, name: &str) {
        let cmd = TmuxCommand::KillSession {
            name: name.to_owned(),
        };
        let _ = run_tmux_on(&resolve_tmux_binary_or_bare(), self.server(), &cmd);
    }

    /// The `claude` pid in session `name` on this server, retried as
    /// [`trusty_mpm::core::process::find_claude_pid_in_tmux_on`] does.
    pub(crate) fn claude_pid(&self, name: &str) -> Option<u32> {
        trusty_mpm::core::process::find_claude_pid_in_tmux_on(
            self.server(),
            name,
            10,
            std::time::Duration::from_millis(500),
        )
    }

    /// Stdout of one tmux `argv` on this server, on a zero exit.
    pub(crate) fn stdout(&self, argv: &[String]) -> Option<String> {
        let argv = trusty_mpm::core::tmux_server::server_argv(self.server(), argv.to_vec());
        let output = trusty_mpm::core::tmux::run_tmux_argv(&argv).ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// RAII guard that kills a tmux session on drop unless explicitly disarmed.
///
/// Why: `launch()` and `connect()` create real tmux sessions; if any step after
/// creation fails (e.g. `send-keys` error, or `attach-session` failing because
/// there is no TTY available — which is always the case in integration tests),
/// the session would otherwise be permanently orphaned (#1815). #10101: the
/// kill goes to the server the session was created on.
/// What: on `drop` while armed, [`LaunchHost::kill`]s the session (errors
/// ignored: it may already be gone). `disarm` turns `drop` into a no-op; call
/// it once the session is handed over so it persists for re-attachment.
/// Test: `a_dropped_guard_kills_on_its_own_server`.
pub(crate) struct LaunchSessionGuard {
    host: LaunchHost,
    name: String,
    armed: bool,
}

impl LaunchSessionGuard {
    /// An armed guard owning session `name` on `host`.
    pub(crate) fn new(host: &LaunchHost, name: impl Into<String>) -> Self {
        Self {
            host: host.clone(),
            name: name.into(),
            armed: true,
        }
    }

    /// Transfer ownership away: `drop` becomes a no-op. Idempotent.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LaunchSessionGuard {
    /// Reaps the session when still armed; never panics.
    fn drop(&mut self) {
        if self.armed {
            self.host.kill(&self.name);
        }
    }
}

/// The non-terminal managed record that holds tmux name `name`, if any.
///
/// What: the rename rule (`rename.rs`, a name any other non-terminal record
/// holds is refused) over the daemon's wire summaries. The raw persisted
/// state decides (#3531); a state word this build does not know is treated
/// as live, so an unknown never frees the name.
/// Test: `a_live_managed_record_holds_its_name`.
pub(crate) fn name_holder<'a>(
    sessions: &'a [ManagedSessionSummary],
    name: &str,
) -> Option<&'a ManagedSessionSummary> {
    sessions.iter().find(|s| {
        let state = s.persisted_state.as_deref().unwrap_or(&s.state);
        s.name == name && !ManagedSessionState::from_wire(state).is_some_and(|st| st.is_terminal())
    })
}

/// Refuse a fresh CLI launch under a name a managed record holds (#10101).
///
/// Why: the Architect's 14:47Z ruling — a CLI launch whose name collides with
/// an existing managed record is refused, fail closed: two sessions of one
/// name on two servers leave every by-name lookup unable to pick one.
/// What: reads `GET /api/v1/sessions/managed`. A holder is an error naming
/// it. A daemon that cannot be reached returns `Ok`: the launch then runs
/// unregistered, as before, and the record list it would check is not
/// readable. Any other read failure refuses.
///
/// # Errors
///
/// The name is held, or the daemon answered but the list could not be read.
///
/// Test: `a_held_name_refuses_the_launch_before_tmux_or_registration`.
pub(crate) async fn refuse_held_name(daemon: &DaemonClient, name: &str) -> anyhow::Result<()> {
    let sessions = match daemon.list_managed_sessions().await {
        Ok(sessions) => sessions,
        Err(e) if unreachable(&e) => return Ok(()),
        Err(e) => anyhow::bail!(
            "cannot check whether a managed session already holds the name {name}: {e:#}; \
             refusing to launch under it (#10101)"
        ),
    };
    if let Some(holder) = name_holder(&sessions, name) {
        anyhow::bail!(
            "tmux session name {name} is already held by managed session {} (state {}); \
             refusing to launch a second session under it. Stop or rename that session \
             (`tm session rename`), or launch from another directory.",
            holder.id,
            holder.persisted_state.as_deref().unwrap_or(&holder.state)
        );
    }
    Ok(())
}

/// Whether `err` is a failure to reach the daemon at all, on either
/// transport (`DaemonCallError::Unreachable`).
fn unreachable(err: &anyhow::Error) -> bool {
    use trusty_mpm::client::http_client::DaemonCallError;
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<DaemonCallError>(),
            Some(DaemonCallError::Unreachable { .. })
        )
    })
}

/// A registration to undo when the launch fails after it (#10101).
///
/// Why: a record left behind names a session that never ran; the failure
/// must say what it left, or leave nothing.
/// What: [`Self::fail`] sends `DELETE /sessions/{id}` and returns `err`
/// extended with the outcome. No id (the daemon was not reached, or the
/// session already existed) undoes nothing.
/// Test: `a_failed_create_removes_the_registration`.
pub(crate) struct Registered<'a> {
    daemon: &'a DaemonClient,
    id: Option<trusty_mpm::core::session::SessionId>,
}

impl<'a> Registered<'a> {
    /// The registration `id` made through `daemon`.
    pub(crate) fn new(
        daemon: &'a DaemonClient,
        id: Option<trusty_mpm::core::session::SessionId>,
    ) -> Self {
        Self { daemon, id }
    }

    /// `err`, after removing the registration; the message names what is left.
    pub(crate) async fn fail(&self, err: anyhow::Error) -> anyhow::Error {
        let Some(id) = &self.id else {
            return err;
        };
        let removed = match self
            .daemon
            .delete(format!("/sessions/{}", id.0))
            .send()
            .await
        {
            Ok(resp) => resp.error_for_status().map(drop),
            Err(e) => Err(e.into()),
        };
        match removed {
            Ok(()) => anyhow::anyhow!("{err:#}; its registration {} was removed", id.0),
            Err(e) => anyhow::anyhow!(
                "{err:#}; its registration {} could not be removed ({e:#}), so the daemon \
                 still lists it",
                id.0
            ),
        }
    }
}

#[cfg(test)]
#[path = "launch_host_tests.rs"]
mod tests;
