//! `tm doctor` row for a record whose pane is live under another tmux name
//! (#9238).
//!
//! Why: a `tmux rename-session` leaves the record naming a session that no
//! longer exists. The reaper and the list route now follow the pane, but a
//! record that cannot be followed — another record holds the name — or one
//! already decommissioned while its pane runs on is invisible everywhere
//! else: `tm ls` hides tombstones and shows the other as stopped.
//! What: [`check_session_names`], one read-only `session_names` row over
//! [`SessionManager::name_drifts`]. It never writes the store or touches a
//! tmux session.
//! Test: `the_doctor_row_flags_a_live_pane_under_another_name`.

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::session_manager::rename_follow::NameDrift;
use crate::session_manager::{ManagedError, SessionManager};

/// The row's name.
pub(crate) const NAME: &str = "session_names";

/// Grade every managed record's name against the session holding its pane.
///
/// Why: see the module doc.
/// What: [`session_names_row`] over [`SessionManager::name_drifts`].
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`.
pub(crate) async fn check_session_names(mgr: &SessionManager) -> DoctorCheck {
    session_names_row(mgr.name_drifts().await)
}

/// The row for a [`SessionManager::name_drifts`] answer.
///
/// What: `Unknown` when tmux could not be listed; `Ok` when no record's pane
/// is live under another name; otherwise `Warn`, one line per record naming
/// its id, recorded name, pane and current name. A terminal record's line
/// names the reactivate route, since it is hidden from `tm ls`; a live
/// record's line says the next `tm ls` follows the rename unless another
/// record holds the name.
/// Test: `the_doctor_row_flags_a_live_pane_under_another_name`.
pub(crate) fn session_names_row(drifts: Result<Vec<NameDrift>, ManagedError>) -> DoctorCheck {
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
    if drifts.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            "every managed record whose pane is live carries its tmux session's name",
        );
    }
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
    DoctorCheck::new(
        NAME,
        CheckStatus::Warn,
        format!(
            "{} record(s) name a tmux session their pane is not in (#9238): {}",
            drifts.len(),
            lines.join(" | ")
        ),
    )
}
