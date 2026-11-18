//! Which tmux server a resumed session's pane is recreated on (#10101).
//!
//! Why: the owner ruling of 2026-11-18 removes secondary tmux servers. A
//! stopped record that still names a workspace (`tm-ws-*`) server would
//! restart that server on resume, so resume moves it to the default server.
//! What: [`rehome_to_default`] clears a stopped record's stored server when
//! [`super::launch_server::WORKSPACE_TMUX_ENV`] is not `1` and no session of
//! the record's name is live on that server. A live pane is never re-routed,
//! and a server whose state cannot be read keeps the record where it is.
//! Test: `resume_server_tests.rs`.

use std::time::Duration;

use super::manager::{ManagedError, ManagedTmuxDriver};
use super::record::SessionRecord;

/// How long the crashed-server probe may wait for the socket to answer.
const SOCKET_PROBE: Duration = Duration::from_millis(500);

/// Move `record` (Stopped or Errored, checked by the caller) to the default
/// server unless the workspace server is opted in or still holds its pane.
///
/// What: reads the opt-in from the process environment and calls
/// [`rehome_with`].
/// Test: `resuming_a_stopped_workspace_record_recreates_it_on_the_default_server`.
pub(crate) fn rehome_to_default(
    tmux: &dyn ManagedTmuxDriver,
    record: &mut SessionRecord,
) -> Result<bool, ManagedError> {
    let opt_in = std::env::var(super::launch_server::WORKSPACE_TMUX_ENV).ok();
    rehome_with(tmux, record, opt_in.as_deref())
}

/// [`rehome_to_default`] with the opt-in value passed in. Returns whether the
/// record moved.
///
/// What: no move when opted in or when the record has no stored socket. The
/// record's server is probed without starting it: a listing that names the
/// session keeps it (a live pane); a listing without it moves it; a failed
/// listing moves it only when the socket refuses a connection (a crashed
/// server), and otherwise keeps it for the existing resume path to decide.
/// A move clears the socket and the pane identity read on that server; the
/// project, account and `claude_session_id` are untouched.
/// Test: `a_live_pane_on_a_workspace_server_is_never_rerouted`,
/// `the_opt_in_keeps_a_resumed_record_on_its_workspace_server`,
/// `a_crashed_workspace_server_moves_the_record_to_the_default_server`,
/// `a_failed_listing_on_an_answering_socket_keeps_the_record`.
pub(crate) fn rehome_with(
    tmux: &dyn ManagedTmuxDriver,
    record: &mut SessionRecord,
    opt_in: Option<&str>,
) -> Result<bool, ManagedError> {
    if super::launch_server::opted_in(opt_in) {
        return Ok(false);
    }
    let Some(socket) = record.tmux_socket.clone() else {
        return Ok(false);
    };
    let routed = super::tmux_route::route(tmux, record)?;
    let dead = match routed.session_exists_checked(&record.tmux_name) {
        Ok(live) => !live,
        Err(_) => crate::core::tmux_server::socket_answers(&socket, SOCKET_PROBE) == Some(false),
    };
    if !dead {
        return Ok(false);
    }
    record.tmux_socket = None;
    record.tmux_server = None;
    record.pane_id = None;
    record.tmux_session_id = None;
    Ok(true)
}

#[cfg(test)]
#[path = "resume_server_tests.rs"]
mod tests;
