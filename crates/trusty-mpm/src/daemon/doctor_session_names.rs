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
//! `the_doctor_row_flags_a_remote_name_that_left_its_tmux_session`.

use std::collections::BTreeSet;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::process::PaneClaude;
use crate::runtime::remote_name::{self, RemoteNameDrift};
use crate::session_manager::rename_follow::NameDrift;
use crate::session_manager::{ManagedError, SessionManager};

/// The row's name.
pub(crate) const NAME: &str = "session_names";

/// Grade every managed record's name against the session holding its pane,
/// and every managed `claude`'s remote name against its tmux session.
///
/// Why: see the module doc.
/// What: [`session_names_row`] over [`SessionManager::name_drifts`] and the
/// #9025 remote-name drifts of the live, non-terminal records. A session is
/// probed for a `claude` pid only when the tmux driver reports one present,
/// so a driver that cannot inspect panes runs no process probe. The probe
/// and the bounded registry read run off the async executor.
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`.
pub(crate) async fn check_session_names(mgr: &SessionManager) -> DoctorCheck {
    let drifts = mgr.name_drifts().await;
    let sessions = managed_sessions(mgr, drifts.as_deref().unwrap_or_default()).await;
    let tmux = mgr.tmux.clone();
    // #9025: the name each managed `claude` shows peers.
    let remote = tokio::task::spawn_blocking(move || {
        remote_name::remote_name_drifts(
            &sessions,
            |s| {
                (tmux.pane_claude(s) == PaneClaude::Present)
                    .then(|| {
                        crate::core::process::find_claude_pid_in_tmux(
                            s,
                            1,
                            std::time::Duration::ZERO,
                        )
                    })
                    .flatten()
            },
            remote_name::read_registry,
        )
    })
    .await
    .unwrap_or_else(|e| Err(format!("the remote-name probe did not finish: {e}")));
    session_names_row(drifts, remote)
}

/// The live tmux name of every non-terminal record: the drifted name when
/// its pane moved, else the recorded one.
async fn managed_sessions(mgr: &SessionManager, drifts: &[NameDrift]) -> Vec<String> {
    let names: BTreeSet<String> = mgr
        .list()
        .await
        .into_iter()
        .filter(|r| !r.state.is_terminal())
        .map(|r| match drifts.iter().find(|d| d.id == r.id) {
            Some(d) => d.current.clone(),
            None => r.tmux_name,
        })
        .collect();
    names.into_iter().collect()
}

/// The row for a [`SessionManager::name_drifts`] and a remote-name answer.
///
/// What: `Unknown` when tmux could not be listed. `Ok` when no record's pane
/// is live under another name and no managed `claude` shows another remote
/// name; a registry that could not be read says so in the `Ok` message.
/// Otherwise `Warn`: one line per record naming its id, recorded name, pane
/// and current name — a terminal record's line names the reactivate route,
/// since it is hidden from `tm ls`, and a live record's line says the next
/// `tm ls` follows the rename unless another record holds the name — then
/// one line per #9025 remote-name drift naming the `/rename` that fixes it.
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`,
/// `the_doctor_row_flags_a_remote_name_that_left_its_tmux_session`.
pub(crate) fn session_names_row(
    drifts: Result<Vec<NameDrift>, ManagedError>,
    remote: Result<Vec<RemoteNameDrift>, String>,
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
    // #9025: a remote name that left its tmux session.
    let unchecked = match remote {
        Ok(remote) if !remote.is_empty() => {
            parts.push(remote_part(&remote));
            None
        }
        Ok(_) => None,
        Err(why) => Some(why),
    };
    if parts.is_empty() {
        let mut message =
            "every managed record whose pane is live carries its tmux session's name".to_owned();
        if let Some(why) = unchecked {
            message.push_str(&format!("; remote names were not checked ({why})"));
        }
        return DoctorCheck::new(NAME, CheckStatus::Ok, message);
    }
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
