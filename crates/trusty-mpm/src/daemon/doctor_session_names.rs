//! `tm doctor` row for a record whose pane is live under another tmux name
//! (#9238), or whose `claude` shows peers another name (#9025).
//!
//! Why: a `tmux rename-session` leaves the record naming a session that no
//! longer exists. The reaper and the list route now follow the pane, but a
//! record that cannot be followed — another record holds the name — or one
//! already decommissioned while its pane runs on is invisible everywhere
//! else: `tm ls` hides tombstones and shows the other as stopped. The same
//! rename leaves the `claude` in the pane under its launch-time remote name,
//! against the #9025 owner rule that the two are equal.
//! What: [`check_session_names`], one read-only `session_names` row over
//! [`SessionManager::name_drifts`] and
//! [`crate::runtime::remote_name::remote_name_drifts`]. It never writes the
//! store, touches a tmux session, or renames a `claude`.
//! Test: `the_doctor_row_flags_a_live_pane_under_another_name`,
//! `the_doctor_row_flags_a_remote_name_that_left_its_tmux_session`,
//! `a_pane_the_probe_could_not_read_is_reported_unchecked`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::process::PaneClaude;
use crate::core::tmux_server::{PROBE_BUDGET, bounded_probe};
use crate::runtime::remote_name::{self, RemoteNameDrift};
use crate::session_manager::rename_follow::NameDrift;
use crate::session_manager::tmux_route::{self, RoutedTmux};
use crate::session_manager::{ManagedError, ManagedTmuxDriver, SessionManager};

/// The row's name.
pub(crate) const NAME: &str = "session_names";

/// Grade every managed record's name against the session holding its pane,
/// and every managed `claude`'s remote name against its tmux session.
///
/// Why: see the module doc.
/// What: [`session_names_row`] over [`SessionManager::name_drifts`] and the
/// #9025 remote-name drifts of the live, non-terminal records. A session is
/// probed for a `claude` pid only when the tmux driver reports one present,
/// so a driver that cannot inspect panes runs no process probe. A record on
/// a workspace server is probed on that server, within
/// [`crate::core::tmux_server::PROBE_BUDGET`] and one probe at a time per
/// server (#10101). The probe and the bounded registry read run off the
/// async executor.
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`,
/// `a_pane_the_probe_could_not_read_is_reported_unchecked`,
/// `the_doctor_probes_each_record_on_its_own_server`,
/// `same_named_records_are_probed_on_their_own_servers`,
/// `a_hung_workspace_server_bounds_the_doctor_row_and_reads_unknown`.
pub(crate) async fn check_session_names(mgr: &SessionManager) -> DoctorCheck {
    let drifts = mgr.name_drifts().await;
    let sessions = managed_sessions(mgr, drifts.as_deref().unwrap_or_default()).await;
    let tmux = mgr.tmux.clone();
    // #9025: the name each managed `claude` shows peers. A pane whose
    // `pane_claude` answer is neither Present nor Absent is collected, not
    // skipped, so the row can say it was not checked.
    let (remote, unchecked) = tokio::task::spawn_blocking(move || {
        let mut unchecked = Vec::new();
        // #10101: each record on its own socket, so two records sharing a
        // name on different servers are each probed on their own server.
        let mut running = Vec::new();
        for (name, socket) in &sessions {
            match claude_pid_on_own_server(tmux.as_ref(), name, socket.as_deref(), PROBE_BUDGET) {
                Ok(Some(pid)) => running.push((name.clone(), pid)),
                Ok(None) => {}
                Err(entry) => unchecked.push(entry),
            }
        }
        let remote = remote_name::remote_name_drifts_of(running, remote_name::read_registry);
        (remote, unchecked)
    })
    .await
    .unwrap_or_else(|e| {
        (
            Err(format!("the remote-name probe did not finish: {e}")),
            Vec::new(),
        )
    });
    session_names_row(drifts, remote, &unchecked)
}

/// The live tmux name of every non-terminal record — the drifted name when
/// its pane moved, else the recorded one — with the record's stored tmux
/// socket (#10101: `None` is the default server).
async fn managed_sessions(
    mgr: &SessionManager,
    drifts: &[NameDrift],
) -> Vec<(String, Option<PathBuf>)> {
    let sessions: BTreeSet<(String, Option<PathBuf>)> = mgr
        .list()
        .await
        .into_iter()
        .filter(|r| !r.state.is_terminal())
        .map(|r| match drifts.iter().find(|d| d.id == r.id) {
            Some(d) => (d.current.clone(), r.tmux_socket),
            None => (r.tmux_name, r.tmux_socket),
        })
        .collect();
    sessions.into_iter().collect()
}

/// The `claude` pid in session `name`'s pane, read on the server its record
/// names (#10101).
///
/// Why: a PM on a workspace server is invisible to the default server, which
/// may hold an unrelated same-named session; and a hung workspace server
/// must not hang `tm doctor`.
/// What: no `socket` keeps the pre-#10101 default-server probe:
/// [`ManagedTmuxDriver::pane_claude`], then
/// [`crate::core::process::find_claude_pid_in_tmux`]. A socket routes through
/// [`tmux_route::route_to`]; the pane probe and the pid read then run on that
/// server's driver through [`crate::core::tmux_server::bounded_probe`] keyed
/// on the socket, so they finish within `budget` and never run twice at once
/// against one server. `Ok(None)` is no `claude`; `Err` is the unchecked
/// entry the row lists, naming the server when it was the server that failed.
/// Test: `the_doctor_probes_each_record_on_its_own_server`,
/// `a_hung_workspace_server_bounds_the_doctor_row_and_reads_unknown`.
pub(crate) fn claude_pid_on_own_server(
    tmux: &dyn ManagedTmuxDriver,
    name: &str,
    socket: Option<&Path>,
    budget: Duration,
) -> Result<Option<u32>, String> {
    let Some(socket) = socket else {
        return match tmux.pane_claude(name) {
            PaneClaude::Absent => Ok(None),
            PaneClaude::Present => {
                crate::core::process::find_claude_pid_in_tmux(name, 1, Duration::ZERO)
                    .map(Some)
                    .ok_or_else(|| name.to_owned())
            }
            PaneClaude::Unknown => Err(name.to_owned()),
        };
    };
    let routed = match tmux_route::route_to(tmux, name, Some(socket)) {
        Ok(RoutedTmux::Named(driver)) => driver,
        Ok(RoutedTmux::Default(_)) => return Err(name.to_owned()),
        Err(e) => return Err(format!("{name} ({e})")),
    };
    let server = socket
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let session = name.to_owned();
    // #10101: bounded, one in flight per server — a hung server reads unknown.
    // The probe answers `Err(())` for a pane it could not read.
    let probe = bounded_probe(socket, budget, move || match routed.pane_claude(&session) {
        PaneClaude::Absent => Ok(None),
        PaneClaude::Present => routed
            .get_pane_id(&session)
            .and_then(|pane| routed.pane_pid(&pane))
            .and_then(crate::core::process::claude_child_of)
            .map(Some)
            .ok_or(()),
        PaneClaude::Unknown => Err(()),
    });
    match probe {
        Some(Ok(pid)) => Ok(pid),
        Some(Err(())) => Err(format!("{name} (on tmux server '{server}')")),
        None => Err(format!(
            "{name} (tmux server '{server}' did not answer within {}s or is already being probed)",
            budget.as_secs_f32()
        )),
    }
}

/// The row for a [`SessionManager::name_drifts`] and a remote-name answer.
///
/// What: `Unknown` when tmux could not be listed. Otherwise `Warn`: one line
/// per record naming its id, recorded name, pane and current name — a
/// terminal record's line names the reactivate route, since it is hidden from
/// `tm ls`, and a live record's line says the next `tm ls` follows the rename
/// unless another record holds the name — then one line per #9025 remote-name
/// drift naming the `/rename` that fixes it. With neither, `Ok` when every
/// check ran; `Unknown` when the remote-name check did not run (`remote` is
/// `Err`) or `unchecked` names panes it could not probe, with the reason in
/// the message. A `Warn` also carries those notes.
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`,
/// `the_doctor_row_flags_a_remote_name_that_left_its_tmux_session`,
/// `a_pane_the_probe_could_not_read_is_reported_unchecked`.
pub(crate) fn session_names_row(
    drifts: Result<Vec<NameDrift>, ManagedError>,
    remote: Result<Vec<RemoteNameDrift>, String>,
    unchecked: &[String],
) -> DoctorCheck {
    let drifts = match drifts {
        Ok(d) => d,
        Err(e) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Unknown,
                format!("tmux sessions could not be listed ({e}); record names were not checked"),
            );
        }
    };
    let mut parts = Vec::new();
    if !drifts.is_empty() {
        parts.push(record_part(&drifts));
    }
    // #9025: a remote name that left its tmux session; a check that did not
    // run is `Unknown`, never `Ok`.
    let mut notes = Vec::new();
    match remote {
        Ok(remote) if !remote.is_empty() => parts.push(remote_part(&remote)),
        Ok(_) => {}
        Err(why) => notes.push(format!("remote names were not checked ({why})")),
    }
    if !unchecked.is_empty() {
        notes.push(format!(
            "{} session(s) could not be probed for a claude, so their remote names were not \
             checked: {}",
            unchecked.len(),
            unchecked.join(", ")
        ));
    }
    if parts.is_empty() {
        let mut message =
            "every managed record whose pane is live carries its tmux session's name".to_owned();
        for note in &notes {
            message.push_str("; ");
            message.push_str(note);
        }
        let status = if notes.is_empty() {
            CheckStatus::Ok
        } else {
            CheckStatus::Unknown
        };
        return DoctorCheck::new(NAME, status, message);
    }
    parts.extend(notes);
    DoctorCheck::new(NAME, CheckStatus::Warn, parts.join("; "))
}

/// The #9238 half of the `Warn` message.
fn record_part(drifts: &[NameDrift]) -> String {
    let lines: Vec<String> = drifts
        .iter()
        .map(|d| {
            let fix = if d.state.is_terminal() {
                format!(
                    "the record is {} while its pane runs; if the session is in use, revive \
                     it: curl -sS -X POST $TRUSTY_MPM_URL/api/v1/sessions/managed/{}/reactivate",
                    d.state, d.id
                )
            } else {
                "the next `tm ls` renames the record unless another record holds that name"
                    .to_string()
            };
            format!(
                "record {} names tmux session '{}', but its pane {} is live in '{}'; {fix}",
                d.id, d.recorded, d.pane_id, d.current
            )
        })
        .collect();
    format!(
        "{} record(s) name a tmux session their pane is not in (#9238): {}",
        drifts.len(),
        lines.join(" | ")
    )
}

/// The #9025 half of the `Warn` message.
fn remote_part(remote: &[RemoteNameDrift]) -> String {
    let lines: Vec<String> = remote
        .iter()
        .map(|d| {
            format!(
                "tmux session '{}' runs claude pid {} under the remote name '{}'; type \
                 `/rename {}` in that session",
                d.session, d.pid, d.remote, d.session
            )
        })
        .collect();
    format!(
        "{} session(s) show peers a remote name other than their tmux session name (#9025): {}",
        remote.len(),
        lines.join(" | ")
    )
}
