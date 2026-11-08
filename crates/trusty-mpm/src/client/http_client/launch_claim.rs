//! Who may launch a fresh PM under a tmux name (#10101, slice G and D3c).
//!
//! Why: a fresh PM launch from the CLI (`tm launch`, `tm connect`, the
//! in-place `tm session start`) or the TUI `/connect` path must not start a
//! second session under a name another record already holds, must not run
//! unregistered when the daemon is gone, and must not leave a registration
//! behind when it fails (owner ruling, 2026-11-08 17:41Z; Architect ruling
//! 14:47Z). Every launch path reads its decision from this one module.
//! What: [`DaemonClient::claim_launch_name`] reads the managed list and the
//! legacy `Session` registry and answers a [`LaunchClaim`];
//! [`DaemonClient::require_reachable`] refuses an unreachable daemon before a
//! launch writes anything; [`Registered`] undoes a registration on failure.
//! Test: `launch_claim_tests.rs`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{DaemonCallError, DaemonClient, ManagedSessionSummary};
use crate::core::session::SessionId;
use crate::session_manager::ManagedSessionState;

/// What a launch under one tmux name may do (#10101 D3c).
#[derive(Debug, Clone)]
pub enum LaunchClaim {
    /// No record holds the name: launch a fresh PM under it.
    Free,
    /// A managed record in `Stopped` or `Errored` holds the name: resume that
    /// record through the daemon's resume route instead of launching.
    Resume(Box<ManagedSessionSummary>),
    /// A live legacy record for this same directory holds the name and its
    /// tmux session runs on its server: hand that session over, register
    /// nothing, type nothing.
    Running {
        /// The tmux session name.
        name: String,
        /// The socket of the server it runs on; `None` is the default server.
        socket: Option<PathBuf>,
    },
}

/// One row of `GET /sessions` (the legacy `Session` registry), as far as the
/// name rule reads it.
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyRow {
    /// The record id.
    #[serde(default)]
    pub id: String,
    /// The directory the session was registered for.
    #[serde(default)]
    pub workdir: String,
    /// The tmux session name.
    #[serde(default)]
    pub tmux_name: String,
    /// The status word (`Active`, `Stopped`, ...); kept raw, so a word this
    /// build does not know still holds the name.
    #[serde(default)]
    pub status: Option<String>,
    /// The workspace server's socket; `None` is the default server.
    #[serde(default)]
    pub tmux_socket: Option<PathBuf>,
}

/// The non-terminal managed record that holds tmux name `name`, if any.
///
/// What: the rename rule (`rename.rs`, a name any other non-terminal record
/// holds is refused) over the daemon's wire summaries. The raw persisted
/// state decides (#3531); a state word this build does not know is treated
/// as live, so an unknown never frees the name.
/// Test: `a_live_managed_record_holds_its_name`.
pub fn name_holder<'a>(
    sessions: &'a [ManagedSessionSummary],
    name: &str,
) -> Option<&'a ManagedSessionSummary> {
    sessions.iter().find(|s| {
        s.name == name
            && !ManagedSessionState::from_wire(raw_state(s)).is_some_and(|st| st.is_terminal())
    })
}

/// The legacy record that holds tmux name `name`: any record of that name
/// whose status is not `Stopped` (#10101 D3c). An unknown or missing status
/// holds the name (fail closed).
/// Test: `a_live_legacy_record_holds_its_name_10101`.
pub fn legacy_name_holder<'a>(rows: &'a [LegacyRow], name: &str) -> Option<&'a LegacyRow> {
    rows.iter()
        .find(|r| r.tmux_name == name && r.status.as_deref() != Some("Stopped"))
}

/// The persisted state word a record is judged by (#3531).
fn raw_state(s: &ManagedSessionSummary) -> &str {
    s.persisted_state.as_deref().unwrap_or(&s.state)
}

/// Whether a managed holder in this state is resumed rather than refused:
/// `Stopped` or `Errored`, the two states the daemon's resume accepts.
fn resumable(s: &ManagedSessionSummary) -> bool {
    matches!(
        ManagedSessionState::from_wire(raw_state(s)),
        Some(ManagedSessionState::Stopped | ManagedSessionState::Errored)
    )
}

/// Whether two directory strings name the same directory, trailing `/` aside.
fn same_dir(a: &str, b: &Path) -> bool {
    let b = b.to_string_lossy();
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Whether tmux session `name` runs on the server at `socket`. A socket that
/// names no server here, or a probe that fails, answers `false`, so the
/// caller refuses rather than adopts.
fn running_on(socket: Option<&Path>, name: &str) -> bool {
    let dir = crate::core::tmux_server::ambient_socket_dir();
    let Ok(server) = crate::core::tmux_server::server_of_socket(socket, &dir) else {
        return false;
    };
    let cmd = crate::core::tmux::TmuxCommand::HasSession {
        name: name.to_owned(),
    };
    crate::core::tmux::run_tmux_on(
        &crate::core::tmux::resolve_tmux_binary_or_bare(),
        server.as_deref(),
        &cmd,
    )
    .is_ok_and(|o| o.status.success())
}

/// Whether `err` is a failure to reach the daemon at all, on either
/// transport (`DaemonCallError::Unreachable`).
fn unreachable(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<DaemonCallError>(),
            Some(DaemonCallError::Unreachable { .. })
        )
    })
}

/// The refusal for a daemon that could not be read, worded for `err`.
fn read_refusal(client: &DaemonClient, name: &str, err: &anyhow::Error) -> anyhow::Error {
    if unreachable(err) {
        anyhow::anyhow!(
            "the tm daemon at {} cannot be reached ({err:#}); refusing to launch session {name} \
             unregistered. Start it with `tm start` and retry (#10101).",
            client.base_url()
        )
    } else {
        anyhow::anyhow!(
            "cannot check whether a session already holds the name {name}: {err:#}; \
             refusing to launch under it (#10101)"
        )
    }
}

impl DaemonClient {
    /// Refuse a launch when the daemon cannot be reached (#10101 D3c).
    ///
    /// Why: a PM the daemon never registered is invisible to every list,
    /// reaper and name check; the owner ruled such a launch refused, before
    /// it writes anything.
    /// What: `GET /health`; a transport failure or a non-success status is
    /// the error.
    ///
    /// # Errors
    ///
    /// The daemon is unreachable or unhealthy.
    ///
    /// Test: `an_unreachable_daemon_refuses_the_claim_10101`,
    /// `connect_refuses_an_unreachable_daemon_before_any_write_10101`.
    pub async fn require_reachable(&self) -> anyhow::Result<()> {
        let probe = async {
            self.get("/health").send().await?.error_for_status()?;
            anyhow::Ok(())
        };
        probe.await.map_err(|e| {
            if unreachable(&e) {
                anyhow::anyhow!(
                    "the tm daemon at {} cannot be reached ({e:#}); refusing to launch a session \
                     it cannot register. Start it with `tm start` and retry (#10101).",
                    self.base_url()
                )
            } else {
                anyhow::anyhow!("the tm daemon at {} is not healthy: {e:#}", self.base_url())
            }
        })
    }

    /// Decide what a fresh launch of `name` in `workdir` may do (#10101).
    ///
    /// Why: one rule for every launch path. A name a live record holds is
    /// refused (Architect ruling 14:47Z); a `Stopped`/`Errored` managed holder
    /// is resumed (owner ruling 17:41Z); a daemon that cannot be read refuses
    /// (fail closed).
    /// What: reads `GET /api/v1/sessions/managed` and `GET /sessions`. A
    /// non-terminal managed holder in any other state, or in a state this
    /// build does not know, refuses. A legacy holder ([`legacy_name_holder`])
    /// refuses, unless it is this same directory's session and runs on its
    /// server, which answers [`LaunchClaim::Running`]. A resumable managed
    /// holder answers [`LaunchClaim::Resume`] when no live legacy record
    /// shares the name. Otherwise [`LaunchClaim::Free`].
    ///
    /// # Errors
    ///
    /// The daemon cannot be reached or read, or a live record holds `name`.
    ///
    /// Test: `a_held_name_refuses_the_launch_before_tmux_or_registration`,
    /// `a_live_legacy_record_refuses_the_launch_10101`,
    /// `a_stopped_holder_is_resumed_not_launched_10101`.
    pub async fn claim_launch_name(
        &self,
        name: &str,
        workdir: &Path,
    ) -> anyhow::Result<LaunchClaim> {
        let managed = self
            .list_managed_sessions()
            .await
            .map_err(|e| read_refusal(self, name, &e))?;
        let legacy = self
            .legacy_rows()
            .await
            .map_err(|e| read_refusal(self, name, &e))?;
        let resume = match name_holder(&managed, name) {
            Some(holder) if resumable(holder) => Some(Box::new(holder.clone())),
            Some(holder) => anyhow::bail!(
                "tmux session name {name} is already held by managed session {} (state {}); \
                 refusing to launch a second session under it. Stop or rename that session \
                 (`tm session rename`), or launch from another directory.",
                holder.id,
                raw_state(holder)
            ),
            None => None,
        };
        if let Some(row) = legacy_name_holder(&legacy, name) {
            if resume.is_none()
                && same_dir(&row.workdir, workdir)
                && running_on(row.tmux_socket.as_deref(), name)
            {
                return Ok(LaunchClaim::Running {
                    name: name.to_owned(),
                    socket: row.tmux_socket.clone(),
                });
            }
            anyhow::bail!(
                "tmux session name {name} is already held by session {} (status {}, in {}); \
                 refusing to launch a second session under it. Remove that record \
                 (`tm session delete {name}`) or launch from another directory; a record \
                 whose tmux session is gone is reaped within a minute.",
                row.id,
                row.status.as_deref().unwrap_or("unknown"),
                row.workdir
            );
        }
        Ok(resume.map_or(LaunchClaim::Free, LaunchClaim::Resume))
    }

    /// Every row of the legacy registry (`GET /sessions`).
    async fn legacy_rows(&self) -> anyhow::Result<Vec<LegacyRow>> {
        #[derive(Deserialize)]
        struct Body {
            sessions: Vec<LegacyRow>,
        }
        let body: Body = self
            .get("/sessions")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body.sessions)
    }
}

/// A registration to undo when the launch fails after it (#10101).
///
/// Why: a record left behind names a session that never ran; the failure
/// must say what it left, or leave nothing.
/// What: [`Self::fail`] sends `DELETE /sessions/{id}` and returns `err`
/// extended with the outcome. No id (the session already existed) undoes
/// nothing.
/// Test: `a_failed_create_removes_the_registration`.
pub struct Registered<'a> {
    daemon: &'a DaemonClient,
    id: Option<SessionId>,
}

impl<'a> Registered<'a> {
    /// The registration `id` made through `daemon`.
    pub fn new(daemon: &'a DaemonClient, id: Option<SessionId>) -> Self {
        Self { daemon, id }
    }

    /// `err`, after removing the registration; the message names what is left.
    pub async fn fail(&self, err: anyhow::Error) -> anyhow::Error {
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
#[path = "launch_claim_tests.rs"]
mod tests;
