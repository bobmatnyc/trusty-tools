//! The tmux server a CLI launch starts its PM on (#10101, slice G).
//!
//! Why: `tm launch`, `tm connect` and the in-place `tm session start` create
//! the PM's tmux session themselves. A fresh PM belongs on its workspace's own
//! server (#8928 owner ruling, 2026-09-30), the one the daemon's create path
//! picks, and every later tmux call for it must reach that server: the
//! default server can hold an unrelated session with the same name.
//! What: [`LaunchHost`] is the chosen server and its socket, with the tmux
//! calls a launch makes on it; [`settle_claim`] acts on the shared name rule
//! (`trusty_mpm::client::http_client::launch_claim`, #10101 D3c);
//! [`LaunchSessionGuard`] kills a half-launched session on its server.
//! Test: `launch_host_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::client::DaemonClient;
// #10101 D3c: the name rule and the rollback live in the library, shared with
// the TUI `/connect` path.
pub(crate) use trusty_mpm::client::http_client::launch_claim::{LaunchClaim, Registered};
use trusty_mpm::core::tmux::{TmuxCommand, resolve_tmux_binary_or_bare, run_tmux_on};

use crate::commands::launch_handoff::Handoff;

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

    /// The server that holds session `name` now, as `tmux attach` finds it.
    ///
    /// # Errors
    ///
    /// tmux cannot be listed, or no single server holds `name`.
    pub(crate) fn holding(name: &str) -> anyhow::Result<Self> {
        let socket = crate::commands::tmux_attach_server::target_socket(name)?;
        Self::of_socket(Some(&socket))
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

/// Hand over what [`DaemonClient::claim_launch_name`] answered, or report
/// that the caller launches fresh (#10101 D3c).
///
/// Why: `tm launch`, `tm connect` and the in-place `tm session start` share
/// one rule (the claim) and one way to act on it. A `Stopped`/`Errored`
/// managed holder is resumed (owner ruling 2026-11-08 17:41Z), never
/// launched over; a running legacy session of this directory is handed
/// over, never registered again or typed into.
/// What: [`LaunchClaim::Free`] answers `Ok(false)`: the caller launches.
/// [`LaunchClaim::Resume`] resumes the record through the daemon's resume
/// route (the one `tm session resume` uses), so it keeps its id and its
/// server, then hands it over. [`LaunchClaim::Running`] hands over the
/// running session on its server. `handoff` `None` (the in-place start)
/// prints one line instead. Both answer `Ok(true)`.
///
/// # Errors
///
/// The resume failed (nothing was created or registered here), or the
/// handover failed.
///
/// Test: `a_stopped_holder_is_resumed_not_launched_10101`,
/// `a_failed_resume_reports_and_creates_nothing_10101`.
pub(crate) async fn settle_claim(
    daemon: &DaemonClient,
    claim: LaunchClaim,
    workdir: &str,
    handoff: Option<Handoff>,
) -> anyhow::Result<bool> {
    let (name, socket) = match claim {
        LaunchClaim::Free => return Ok(false),
        LaunchClaim::Resume(holder) => {
            eprintln!(
                "tm: tmux session name {} is held by managed session {} ({}); resuming it \
                 instead of launching a new one",
                holder.name,
                holder.id,
                holder.persisted_state.as_deref().unwrap_or(&holder.state)
            );
            let resumed =
                crate::commands::guided_resume::restart_via_daemon(daemon, &holder).await?;
            if handoff.is_none() {
                println!(
                    "resumed {} ({}) [{}]",
                    resumed.name, resumed.id, resumed.state
                );
                return Ok(true);
            }
            (resumed.name, None)
        }
        LaunchClaim::Running { name, socket } => {
            if handoff.is_none() {
                println!("session {name} is already running; left in place");
                return Ok(true);
            }
            (name, Some(socket))
        }
    };
    let Some(handoff) = handoff else {
        return Ok(true);
    };
    handoff.announce_reconnect(workdir, &name);
    // Only a detached handover reads the pane through the host.
    let host = match (handoff, socket) {
        (Handoff::Attach, _) => LaunchHost::default(),
        (Handoff::Detach, Some(socket)) => LaunchHost::of_socket(socket.as_deref())?,
        (Handoff::Detach, None) => LaunchHost::holding(&name)?,
    };
    handoff.hand_over(&host, &name)?;
    Ok(true)
}

#[cfg(test)]
#[path = "launch_host_tests.rs"]
mod tests;
