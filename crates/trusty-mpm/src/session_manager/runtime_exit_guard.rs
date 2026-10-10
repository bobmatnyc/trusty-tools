//! The store-guard read and re-check for the runtime-exit reap (#9034).
//!
//! Why: `SessionManager::mark_runtime_exited_stopped` used to hold the store's
//! write guard across its tmux calls, and a stalled tmux subprocess then
//! blocked every `list()`/`get()`. The reap now reads the record under one
//! short guard, does its tmux work with no guard held, and re-checks the
//! record under a second guard before it writes.
//! What: [`active_for_runtime_exit`] reads the record and refuses one that is
//! not `Active`; [`unchanged`] says whether the record is still the one the
//! reap read.
//! Test: `runtime_exit_guard_tests.rs`.

use tracing::warn;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::store::{SessionStore, StoreError};

/// The `Active` record for `id`, read under the caller's store guard.
///
/// Why: the reap reads the record once before its tmux work and once after,
/// and both reads must apply the same rules.
/// What: reloads the store when its file changed. A failed reload logs and
/// falls back to the last-known record, as `SessionManager::get` does.
/// Returns `SessionNotFound` for an absent id and `InvalidState` for a record
/// that is not `Active`.
/// Test: `mark_runtime_exited_stopped_rejects_concurrently_decommissioned`,
/// `a_record_changed_during_the_runtime_exit_capture_is_not_written_9034`.
pub(super) async fn active_for_runtime_exit(
    store: &mut SessionStore,
    id: &ManagedSessionId,
) -> Result<SessionRecord, ManagedError> {
    if let Err(e) = store.reload_if_changed().await {
        warn!(id = %id, "mark_runtime_exited_stopped: reload failed: {e}; using last-known record");
    }
    let record = store.cached_get(id).map_err(|e| match e {
        StoreError::NotFound(k) => ManagedError::SessionNotFound(k),
        other => ManagedError::Store(other),
    })?;
    if record.state != ManagedSessionState::Active {
        return Err(ManagedError::InvalidState(
            id.to_string(),
            format!(
                "cannot mark runtime-exited-stopped: session is '{}', not 'active' — \
                 a concurrent operation already changed its state",
                record.state
            ),
        ));
    }
    Ok(record)
}

/// Whether `current` is still exactly the record the reap read as `observed`.
///
/// Why: the reap releases the store guard during its tmux work, so a rebind,
/// a resume or any other write can land in that gap while the record stays
/// `Active`. Writing the reap's stale copy back would undo that write.
/// What: compares the two records' serialized forms field by field. A record
/// that cannot be serialized counts as changed, so the reap writes nothing.
/// Test: `a_record_changed_during_the_runtime_exit_capture_is_not_written_9034`.
pub(super) fn unchanged(observed: &SessionRecord, current: &SessionRecord) -> bool {
    match (
        serde_json::to_value(observed),
        serde_json::to_value(current),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
