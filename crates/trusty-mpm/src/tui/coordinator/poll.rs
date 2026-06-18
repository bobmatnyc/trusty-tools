//! Live daemon polling for the coordinator TUI (Child #2).
//!
//! Why: Child #1 rendered static placeholder rows; the coordinator list must
//! instead reflect the real fleet. This module owns the one async step the run
//! loop repeats on its `--interval-ms` cadence — probe daemon health, self-heal
//! the URL on failure, pull the coordinator context, and project each session
//! into a [`SessionEntry`] — plus the PURE mapping that does the projection so
//! it can be unit-tested without a terminal or a network (DOC-13 §6, §9). The
//! daemon-context feed carries no `last_summary` yet (that is Child #3 / #1275),
//! so the summary is derived client-side here (DOC-13 §6.2 option C).
//! What: [`coord_poll_daemon`] (health → rediscover-on-fail → context → map; on
//! daemon-down clears the rows and sets `daemon_reachable = false`) and
//! [`coord_session_to_entry`] (a pure `CoordinatorSession` → `SessionEntry`
//! projection deriving id/prefix/status/summary).
//! Test: `super::tests` covers `coord_session_to_entry` (summary from the last
//! non-empty `recent_output` line, status fallback, short-id derivation); the
//! async glue mirrors the dashboard's `poll_daemon`.

use crate::client::{CoordinatorSession, DaemonClient};

use super::rows::session_short_id;
use super::state::{CoordinatorState, SessionEntry};

/// Refresh [`CoordinatorState`] from one daemon poll.
///
/// Why: keeps the poll logic out of the key-driven run loop so the loop can
/// re-poll on its timer (and, later, on demand after a mutating action) without
/// duplicating the health/rediscover/map sequence. Mirrors the dashboard's
/// `tui::poll_daemon` so both surfaces self-heal the daemon URL identically.
/// What: probes health; if the daemon looks down, re-resolves the URL from the
/// lock file via [`rediscover`] and retries one health probe. When reachable it
/// pulls `GET /api/v1/sessions/context` and maps every session into a
/// [`SessionEntry`]; on a transport error or an unreachable daemon it clears the
/// rows and sets `daemon_reachable = false`. Always clamps the selection so a
/// shrunk list never leaves `selected` past the end.
/// Test: `poll_marks_unreachable_clears_sessions` drives the daemon-down branch
/// against a guaranteed-dead client and asserts the rows clear + `daemon_reachable`
/// flips false; the pure projection is covered by the `coord_session_to_entry_*`
/// tests. The daemon-UP path requires a live daemon, so it is exercised manually /
/// by the daemon suites rather than unit-tested here.
pub(crate) async fn coord_poll_daemon(state: &mut CoordinatorState, client: &mut DaemonClient) {
    state.daemon_reachable = client.is_healthy().await;
    if rediscover(client, state.daemon_reachable) {
        state.daemon_reachable = client.is_healthy().await;
    }
    if state.daemon_reachable {
        // Read the additive HR-3 catalog-staleness flag off the SAME health
        // surface; a failure or older daemon degrades to "no updates" (false).
        state.catalog_stale = client.catalog_stale().await;
        match client.coordinator_context().await {
            Ok(context) => {
                state.sessions = context
                    .sessions
                    .into_iter()
                    .map(coord_session_to_entry)
                    .collect();
            }
            Err(_) => {
                state.daemon_reachable = false;
                state.sessions.clear();
            }
        }
    } else {
        // Daemon down: clear the session rows AND the staleness hint so neither
        // shows stale information.
        state.catalog_stale = false;
        state.sessions.clear();
    }
    state.clamp_selection();
}

/// Re-resolve the daemon URL from the lock file when the daemon is unreachable.
///
/// Why: [`DaemonClient`] is built once at startup; if the daemon later restarted
/// onto a fresh ephemeral port the client would stay pinned to a stale address
/// forever. Re-resolving on every failed poll lets the coordinator TUI self-heal
/// exactly as the dashboard's `tui::rediscover_daemon` does (DOC-13 §6.3).
/// What: when `reachable` is `false`, calls [`crate::core::resolve_daemon_url`]
/// and, if it yields a different URL than the client currently holds, re-points
/// the client and returns `true` so the caller retries one health probe.
/// Test: `rediscover_is_noop_when_reachable`.
fn rediscover(client: &mut DaemonClient, reachable: bool) -> bool {
    if reachable {
        return false;
    }
    let resolved = crate::core::resolve_daemon_url(None);
    if resolved != client.base_url() {
        client.set_base_url(resolved);
        true
    } else {
        false
    }
}

/// Project one coordinator-context session into a list [`SessionEntry`].
///
/// Why: the daemon returns a rich [`CoordinatorSession`], but the list renders
/// the leaner `SessionEntry` shape (short id + prefix + status + a one-line
/// summary). Crucially the context feed has NO `last_summary` field yet (that
/// daemon-side enrichment is Child #3 / #1275), so the summary is derived
/// client-side here: the last non-empty line of `recent_output`, falling back to
/// the status word when the pane tail is empty (DOC-13 §6.2 option C). Keeping
/// this projection pure makes the summary/fallback/short-id rules unit-testable
/// without a terminal or a network.
/// What: returns a [`SessionEntry`] whose `short_id` is the 8-hex prefix of the
/// session id (via [`session_short_id`]), `prefix` and `status` are copied
/// through, and `summary` is the last non-empty `recent_output` line trimmed, or
/// the status word when none exists.
/// Test: `coord_session_to_entry_summarizes_last_output`,
/// `coord_session_to_entry_falls_back_to_status`,
/// `coord_session_to_entry_derives_short_id` in `super::tests`.
pub(crate) fn coord_session_to_entry(s: CoordinatorSession) -> SessionEntry {
    let summary = s
        .recent_output
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| s.status.clone());
    SessionEntry::new(session_short_id(&s.id), s.prefix, s.status, summary)
}
