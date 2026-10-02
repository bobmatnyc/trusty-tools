//! Whether a pane operation may act on a record's pane (#9101).
//!
//! Why: send, inject, answer, observe, capture and resume address a record's
//! tmux pane by its `%N` id. A restarted tmux server hands the same ids out
//! again, so a stale record's `%N` can name a pane in an unrelated live
//! session, and those operations then typed into it or read it. #9004 proved
//! ownership for the teardown paths only.
//! What: [`SessionManager::owned_pane`], the one gate every pane operation
//! runs before it touches tmux. It reuses the #9004 `same_server` verdict.
//! Test: `pane_ops_identity_tests.rs`; the pane-gone refusal in
//! `pane_scoped_tests.rs`.

use tracing::warn;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, SessionRecord};
use super::runtime_identity::{RuntimeOwnership, same_server};

impl SessionManager {
    /// The record's own pane id, proven to be on the live tmux server, or the
    /// refusal to return before any pane operation (#9101).
    ///
    /// Why: see the module doc. A pane id alone is not proof of ownership.
    /// What: `PaneGone` when the session's pane list lacks `record.pane_id`.
    /// `InvalidState` when the record has no pane id, the pane list cannot be
    /// read, or `same_server` is not `Owned`: no server identity, an
    /// unreadable identity, another server, or another session. `Ok` carries
    /// the pane id only when the verdict is `Owned`.
    /// Test: `a_stale_record_after_a_server_restart_never_sends_keys`,
    /// `an_unreadable_pane_identity_refuses_every_pane_operation`,
    /// `a_record_without_a_pane_id_refuses_every_pane_operation`.
    pub(crate) fn owned_pane(
        &self,
        id: &ManagedSessionId,
        record: &SessionRecord,
    ) -> Result<String, ManagedError> {
        let name = record.tmux_name.as_str();
        let Some(pane_id) = record.pane_id.as_deref() else {
            return Err(refusal(
                id,
                record,
                format!("this record carries no pane id to prove tmux session '{name}' is its own"),
            ));
        };
        match self.tmux.pane_exists_checked(name, pane_id) {
            Some(true) => {}
            Some(false) => {
                warn!(
                    id = %id, name = %name, pane_id = %pane_id,
                    "recorded pane is gone but the tmux session is still alive (sibling window) \
                     — refusing to operate on an unrelated active pane"
                );
                return Err(ManagedError::PaneGone(id.to_string(), pane_id.to_owned()));
            }
            None => {
                return Err(refusal(
                    id,
                    record,
                    format!(
                        "the panes of tmux session '{name}' could not be listed to find {pane_id}"
                    ),
                ));
            }
        }
        match same_server(record, name, pane_id, self.tmux.as_ref()) {
            RuntimeOwnership::Owned { pane_id, .. } => Ok(pane_id),
            RuntimeOwnership::Foreign(why) => Err(refusal(
                id,
                record,
                format!("{why}; the record is stale, so delete it"),
            )),
            RuntimeOwnership::Unverifiable(why) => Err(refusal(
                id,
                record,
                format!(
                    "{why}; if that session is this record's, end it yourself and \
                     resume to recreate it"
                ),
            )),
            // `same_server` never answers `Absent`; refuse rather than act.
            RuntimeOwnership::Absent => Err(refusal(
                id,
                record,
                format!("no tmux session '{name}' holds pane {pane_id}"),
            )),
        }
    }
}

/// The typed refusal for an unproven pane, logged at `warn` (#9101).
fn refusal(id: &ManagedSessionId, record: &SessionRecord, why: String) -> ManagedError {
    warn!(id = %id, name = %record.tmux_name, "{why}; no pane operation ran (#9101)");
    ManagedError::InvalidState(
        id.to_string(),
        format!("refusing to act on the recorded pane: {why}"),
    )
}
