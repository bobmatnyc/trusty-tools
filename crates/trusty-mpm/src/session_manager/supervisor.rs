//! The session manager's half of the #8942 Architect protections.
//!
//! Why: the daemon must never stop, decommission, resume, delete, nudge or
//! auto-tear-down the Architect's sessions, and a kill it cannot clear must
//! abort the teardown rather than half-run it. Each lifecycle path makes one
//! call into this module, so the rule lives in one place and the crowded
//! lifecycle files stay under the line cap.
//! What: [`refuse_protected`] and [`skip_protected`] for the record kind;
//! [`SessionManager::kill_gate`] for the name floor; the record-only
//! [`SessionManager::mark_stopped_record_only`] for the tmux-gone reaper; and
//! the stable Architect id [`ManagedSessionId::for_supervisor`].
//! Test: `supervisor_tests.rs`.

use std::path::Path;

use tracing::{debug, info};
use uuid::Uuid;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};
use super::supervisor_floor::SupervisorFloor;

/// An operator verb the daemon refuses on a protected session (#8942).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedVerb {
    /// `tm session stop` and every other stop request.
    Stop,
    /// `tm session decommission`, prune, and the record-only tombstone.
    Decommission,
    /// An operator or automatic resume.
    Resume,
    /// `tm sessions delete`.
    Delete,
}

impl ProtectedVerb {
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            Self::Stop => ("stop", "exit claude in its pane to stop it"),
            Self::Decommission => ("decommission", "exit claude in its pane to stop it"),
            Self::Resume => ("resume", "relaunch it with `tm fleet init`"),
            Self::Delete => ("delete", "`tm fleet init` replaces its record"),
        }
    }
}

/// Refuse `verb` on a live record of a protected kind (#8942).
///
/// Why: ruling 1 — the only stops for the Architect are exiting claude or a
/// manual `tmux kill-session`; the daemon's own verbs all refuse.
/// What: `Err(InvalidState)` naming the kind and the way out when the record
/// is protected and not terminal; `Ok(())` otherwise.
/// Test: `stop_refuses_a_supervisor_record_and_never_signals_it`,
/// `resume_refuses_a_supervisor_record`, `delete_refuses_a_live_supervisor_record`.
pub(crate) fn refuse_protected(
    record: &SessionRecord,
    verb: ProtectedVerb,
) -> Result<(), ManagedError> {
    if !record.kind.is_protected() || record.state.is_terminal() {
        return Ok(());
    }
    let (verb, way_out) = verb.describe();
    Err(ManagedError::InvalidState(
        record.id.to_string(),
        format!(
            "session '{}' is the Architect's (kind {:?}); tm does not {verb} it — {way_out}",
            record.tmux_name, record.kind
        ),
    ))
}

/// Whether a daemon sweep named `path` must leave `record` alone (#8942).
///
/// What: `true`, logged at debug, for a live record of a protected kind.
/// Test: `idle_reaper_never_stops_a_supervisor_record`,
/// `shutdown_skips_a_supervisor_session`,
/// `prune_include_active_never_decommissions_a_supervisor_record`.
pub(crate) fn skip_protected(record: &SessionRecord, path: &str) -> bool {
    let skip = record.kind.is_protected() && !record.state.is_terminal();
    if skip {
        debug!(id = %record.id, name = %record.tmux_name, kind = ?record.kind, "{path}: left the Architect's session alone (#8942)");
    }
    skip
}

impl SessionManager {
    /// The #8942 floor this manager's teardowns ask, over its own store.
    pub(crate) fn kill_floor(&self) -> SupervisorFloor {
        SupervisorFloor::for_data_dir(self.data_dir())
    }

    /// Refuse a signal or kill of tmux session `name` the floor cannot clear.
    ///
    /// Why: critic HIGH — a refusal must abort the teardown, typed, before any
    /// signal, workspace removal or record transition; and the warning must
    /// name the real requester, `caller`, not the primitive's call site.
    /// What: `Ok(())` on a `Permit` verdict; otherwise
    /// [`ManagedError::KillRefused`] with the floor's logged reason.
    /// Test: `an_undeterminable_floor_aborts_stop_and_decommission_of_an_ordinary_session`.
    pub(crate) fn kill_gate(&self, name: &str, caller: &str) -> Result<(), ManagedError> {
        match self.kill_floor().refuse(name, caller) {
            None => Ok(()),
            Some(why) => Err(ManagedError::KillRefused(why)),
        }
    }

    /// Mark a record `Stopped` without touching tmux (#8942).
    ///
    /// Why: the tmux-gone reaper observes that a protected session's pane is
    /// gone; the record must say so, but no teardown may run for it.
    /// What: refuses a terminal record like `stop`; otherwise sets `Stopped`
    /// with `cause`, persists, and bumps the residency generation.
    /// Test: `the_tmux_gone_reaper_marks_a_supervisor_record_stopped_without_teardown`.
    pub async fn mark_stopped_record_only(
        &self,
        id: &ManagedSessionId,
        cause: StopCause,
    ) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if record.state.is_terminal() {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!("cannot stop a session in terminal state '{}'", record.state),
            ));
        }
        record.state = ManagedSessionState::Stopped;
        record.stop_cause = Some(cause);
        self.store.write().await.upsert(record.clone()).await?;
        self.bump_residency_generation();
        info!(id = %id, name = %record.tmux_name, cause = ?cause, "managed session marked Stopped (record only, #8942)");
        Ok(record)
    }
}

impl ManagedSessionId {
    /// The stable id of the Architect's session (or a helper) for `dir`.
    ///
    /// Why: a relaunch of the same Architect must upsert the same record,
    /// never mint a second one.
    /// What: UUIDv5, under a fixed namespace, of the canonical `dir`, a NUL,
    /// and `role` (`supervisor`, or a helper suffix such as `-poll`).
    /// Test: `the_supervisor_id_is_stable_per_dir_and_role`.
    pub fn for_supervisor(dir: &Path, role: &str) -> Self {
        /// Never change it: the value IS the identity of every Architect record.
        const SUPERVISOR_NAMESPACE: Uuid =
            Uuid::from_u128(0x5269_9d4e_d450_4bfc_84ab_e660_9efa_f288);
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let mut key = dir.to_string_lossy().into_owned().into_bytes();
        key.push(0);
        key.extend_from_slice(role.as_bytes());
        Self(Uuid::new_v5(&SUPERVISOR_NAMESPACE, &key))
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
