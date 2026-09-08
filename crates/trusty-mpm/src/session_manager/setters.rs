//! Post-creation field setters for [`SessionManager`] records (#7087).
//!
//! Why: extracted from `manager.rs` when residency-generation tracking (#7087
//! slice 1b) needed room under that file's 500-SLOC production cap. This
//! follows the precedent `stop.rs` / `create.rs` / `reactivate.rs` /
//! `reconcile.rs` already set for this file: a cohesive group of methods moves
//! to its own sibling module rather than the cap being gamed or raised. The
//! group is cohesive because every method here is the same shape — a plain
//! read-modify-write against one existing record, called AFTER the session
//! already exists (`create`/`stop`/`resume` cover the state-machine
//! transitions; these cover incidental fields set alongside or after them).
//! What: [`SessionManager::mark_errored`], [`SessionManager::set_workspace`],
//! [`SessionManager::set_pending_decision`],
//! [`SessionManager::set_source_id`], [`SessionManager::set_deliverable_id`],
//! and [`SessionManager::clear_pending_decision`]. No behavior change — a pure
//! relocation.
//! Test: covered by handler_spawn_wires_provision_and_spawn (`mark_errored`,
//! `set_workspace`), `front_gate_escalation_sets_pending_decision` and
//! `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs
//! (`set_pending_decision`, `clear_pending_decision`),
//! `set_source_id_succeeds_first_try` /
//! `set_source_id_returns_err_after_retries_for_missing_session` in
//! `tests.rs` (`set_source_id`), `set_deliverable_id_persists` /
//! `set_deliverable_id_missing_session_errors` in
//! `set_deliverable_id_tests.rs` (`set_deliverable_id`).

use chrono::Utc;
use tracing::{error, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState};

impl SessionManager {
    /// Mark a session as errored with a message.
    ///
    /// Why: when provisioning or spawning fails the session must not remain in
    /// `Provisioning`; marking it errored surfaces the failure to `tm session ls`.
    /// What: transitions the record to `ManagedSessionState::Errored` and appends
    /// the error message to the task field for observability, then persists.
    /// Test: covered by handler_spawn_wires_provision_and_spawn error path.
    pub async fn mark_errored(
        &self,
        id: &ManagedSessionId,
        error_msg: &str,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.state = ManagedSessionState::Errored;
        record.task = format!("{} [error: {}]", record.task, error_msg);
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Update a session's workspace path and transition to a new state.
    ///
    /// Why: after the workspace path is resolved
    /// must be persisted so `tm session ls` shows it and `activity` can infer
    /// context.
    /// What: looks up the record, sets `workspace_path` and `state`, and persists.
    /// Test: covered by handler_spawn_wires_provision_and_spawn.
    pub async fn set_workspace(
        &self,
        id: &ManagedSessionId,
        workspace_path: std::path::PathBuf,
        new_state: ManagedSessionState,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.workspace_path = Some(workspace_path);
        record.state = new_state;
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Record a pending decision (an escalation) on a session, awaiting a human.
    ///
    /// Why: the intent-conformance FRONT gate (#1360) escalates *before* the
    /// runtime is spawned. It must surface the divergence reason + the conformant
    /// default through the SAME channel the harness uses (`pending_decision` /
    /// `proposed_default`), so it appears in `GET …/activity`, MCP
    /// `session_status`, the supervisor, and the `tm` CLI with zero new UI. The
    /// session is left NOT `Active` (the runtime never started) so it reads as
    /// awaiting approval until a human resolves it via `POST …/answer`.
    /// What: looks up the record, sets `pending_decision`/`proposed_default`,
    /// leaves the lifecycle state untouched (it stays `Provisioning` — the
    /// runtime was withheld), and persists. No tmux input is sent (unlike
    /// `answer_decision`): the pane has no running harness to receive it yet.
    /// Test: `front_gate_escalation_sets_pending_decision` in
    /// tests/session_manager_mvp.rs.
    pub async fn set_pending_decision(
        &self,
        id: &ManagedSessionId,
        decision: &str,
        proposed_default: Option<&str>,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.pending_decision = Some(decision.to_string());
        record.proposed_default = proposed_default.map(str::to_string);
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Record a source project identity on a session, with a bounded retry
    /// (#2157 item 5, the #2154 remedy).
    ///
    /// Why: the in-project spawn path (#1706) associates a session with a
    /// specific `owner/repo` so callers can later filter sessions by project
    /// and reconnect instead of spawning duplicates. Setting it post-creation
    /// (rather than via `create_with_id`) keeps the manager's SINGLE generic
    /// create path clean of in-project concerns. Previously a single transient
    /// `get`/`upsert` failure here was `warn!`-and-continue at the call site —
    /// leaving `source_id: None` on the record PERMANENTLY, which makes the
    /// session invisible to every `?source_id=` filtered listing (the guided
    /// picker, `tm session ls --project`) forever, since nothing else ever
    /// retries the write.
    /// What: retries the read-modify-write up to `MAX_SET_SOURCE_ID_ATTEMPTS`
    /// times with a short linear backoff between attempts, returning `Ok(())`
    /// on the first success. If every attempt fails, logs a `tracing::error!`
    /// (loud, not `warn!`) carrying `id` and `source_id` so a future
    /// reconcile/doctor pass can self-heal a `source_id: None` record from its
    /// `repo_url`/`workspace_path`, then returns the last error.
    /// Test: `set_source_id_succeeds_first_try`,
    /// `set_source_id_returns_err_after_retries_for_missing_session` in
    /// `tests.rs`.
    pub async fn set_source_id(
        &self,
        id: &ManagedSessionId,
        source_id: &str,
    ) -> Result<(), ManagedError> {
        const MAX_SET_SOURCE_ID_ATTEMPTS: u8 = 3;
        let mut last_err: Option<ManagedError> = None;
        for attempt in 1..=MAX_SET_SOURCE_ID_ATTEMPTS {
            let result: Result<(), ManagedError> = async {
                let mut record = self.get(id).await?;
                record.source_id = Some(source_id.to_string());
                self.store.write().await.upsert(record).await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(
                        id = %id,
                        attempt,
                        max_attempts = MAX_SET_SOURCE_ID_ATTEMPTS,
                        "set_source_id: attempt failed: {e}"
                    );
                    last_err = Some(e);
                    if attempt < MAX_SET_SOURCE_ID_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * u64::from(attempt),
                        ))
                        .await;
                    }
                }
            }
        }
        error!(
            id = %id,
            source_id = %source_id,
            attempts = MAX_SET_SOURCE_ID_ATTEMPTS,
            "set_source_id: exhausted all attempts — session will be invisible to \
             project-filtered listing (source_id: None) until a reconcile/doctor pass \
             repairs it from repo_url/workspace_path"
        );
        Err(last_err.unwrap_or_else(|| ManagedError::SessionNotFound(id.to_string())))
    }

    /// Record which Deliverable a session is working on (DOC-35 §10.6, #2379).
    ///
    /// Why: `tm sessions new --deliverable <id>` binds a fresh session to a
    /// Deliverable AFTER the session record already exists (mirroring
    /// [`Self::set_source_id`]'s post-creation setter shape, which keeps
    /// [`Self::create_with_id`]'s already-long parameter list from growing
    /// further). The caller (the daemon spawn path,
    /// `daemon::managed_routes::lifecycle`) validates the Deliverable exists
    /// and belongs to the session's project via `DeliverableManager` BEFORE
    /// calling this — this setter trusts that validation and does no lookup of
    /// its own. Per §11, this is a PURE POINTER write: it never mutates the
    /// Deliverable record itself (no auto-transition of its status).
    /// What: a plain read-modify-write — look up the record, set
    /// `deliverable_id`, persist. No retry loop (unlike `set_source_id`):
    /// losing this link on a rare transient store error is a stale pointer,
    /// not a session made invisible to a whole filtered listing, so the
    /// simpler shape matches `set_workspace`/`set_pending_decision`.
    /// Test: `set_deliverable_id_persists`,
    /// `set_deliverable_id_missing_session_errors` in `set_deliverable_id_tests.rs`.
    pub async fn set_deliverable_id(
        &self,
        id: &ManagedSessionId,
        deliverable_id: crate::deliverable::DeliverableId,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.deliverable_id = Some(deliverable_id);
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Clear a pending decision WITHOUT injecting any text into the pane.
    ///
    /// Why: a FRONT-gate (#1360) escalation is resolved *before* a harness exists
    /// — the session is still `Provisioning` with no runtime in the pane. Unlike
    /// [`answer_decision`](Self::answer_decision) (which sends the answer to a
    /// LIVE harness), the FRONT-gate answer path must clear the decision and then
    /// LAUNCH the withheld runtime; sending the answer to a bare shell would be
    /// meaningless. This method does the clear half only.
    /// What: looks up the record, clears `pending_decision`/`proposed_default`,
    /// updates `last_activity_at`, and persists. No tmux I/O.
    /// Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs.
    pub async fn clear_pending_decision(&self, id: &ManagedSessionId) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.pending_decision = None;
        record.proposed_default = None;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }
}
