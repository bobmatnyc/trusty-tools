//! In-place session reactivation, with NO tmux mutation (#2023 component C).
//!
//! Why: `manager.rs` was at the 500-SLOC production cap; adding this method
//! there would breach it. Following the same pattern as `hook_sync.rs` /
//! `adopt.rs` (sibling `impl SessionManager` blocks), this file isolates the
//! one write-path the bare-`tm` in-pane relaunch needs.
//! What: one inherent method, `SessionManager::mark_reactivated` — flips a
//! `Stopped` OR `Decommissioned` (#2777) record back to `Active` with no
//! `create_session`/`kill_session` call, unlike [`SessionManager::resume`]
//! (which always recreates the tmux session).
//! Test: `mark_reactivated_flips_stopped_to_active`,
//! `mark_reactivated_flips_decommissioned_to_active`,
//! `mark_reactivated_rejects_non_stopped` in `super::reactivate_tests`.

use chrono::Utc;
use tracing::info;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Reactivate a Stopped session IN PLACE, with NO tmux mutation (#2023 C).
    ///
    /// Why: [`Self::resume`] is the daemon-driven restart path — it always kills
    /// any surviving tmux session and creates a fresh one, because it assumes
    /// the caller has no live pane of its own to reuse. The bare-`tm` in-pane
    /// relaunch (#2023 component C) is the opposite case: the operator is
    /// running `tm` FROM INSIDE the very pane [`Self::mark_runtime_exited_stopped`]
    /// (#2023 A) left alive when the runtime exited, and is about to `exec`
    /// `claude` directly back into that SAME pane. Routing that reactivation
    /// through `resume` would kill the pane out from under the process that
    /// is about to relaunch into it. This method gives the in-pane path its own
    /// non-destructive transition: flip the record back to `Active` and nothing
    /// else — no `create_session`, no `kill_session`, no pane snapshot.
    /// What: requires the record be `Stopped` OR `Decommissioned` (any other
    /// state is an [`ManagedError::InvalidState`] — in particular, an `Active`
    /// record must not be silently re-marked Active by a stray call); sets
    /// `state = Active` and refreshes `last_activity_at`, then persists.
    ///
    /// `Decommissioned` is accepted (issue #2777) for the bare-`tm`-inside-a-
    /// decommissioned-session case: after a session is decommissioned, its tmux
    /// pane can linger as a bare shell printing "run `tm` to relaunch this
    /// session". When the operator does exactly that from INSIDE that pane, a
    /// switch-client reconnect to the session they are already in is a no-op
    /// dead-end. Reviving the tombstone IN PLACE — with no tmux mutation, so the
    /// operator's current pane survives to `exec` `claude` into it — is the only
    /// non-destructive way to honor that on-screen instruction. The revive is an
    /// explicit, in-pane operator action, never automatic; if decommission had
    /// removed the workspace, the subsequent in-place `exec`'s `cd` fails and the
    /// caller falls back to an actionable hint (`bin/tm`'s `guided_inplace`).
    /// Test: `mark_reactivated_flips_stopped_to_active`,
    /// `mark_reactivated_flips_decommissioned_to_active`,
    /// `mark_reactivated_rejects_non_stopped`.
    pub async fn mark_reactivated(
        &self,
        id: &ManagedSessionId,
    ) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if !matches!(
            record.state,
            ManagedSessionState::Stopped | ManagedSessionState::Decommissioned
        ) {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "cannot reactivate a session in state '{}'; only Stopped or Decommissioned \
                     sessions can be reactivated in place",
                    record.state
                ),
            ));
        }
        record.state = ManagedSessionState::Active;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record.clone()).await?;
        info!(
            id = %id,
            name = %record.tmux_name,
            "managed session reactivated in place (#2023 C, no tmux mutation)"
        );
        Ok(record)
    }
}
