//! `SessionRegistry::await_cancelled` (#8207), split out of `registry.rs` for
//! the 500-SLOC cap — a child module of `registry` (declared via
//! `#[path = ...] mod cancel_confirm;`), so it keeps full access to
//! `SessionRegistry`'s private `lock` helper and `Execution`'s private fields
//! exactly as if the method were still defined there.
//!
//! Why: `registry.rs` sits within a few lines of its cap; the same reason
//! `registry_task_result.rs` and `registry_events.rs` exist.
//! What: [`SessionRegistry::await_cancelled`] — the half of `session.cancel`
//! that turns a cancel REQUEST into a confirmed STOP.
//! Test: `registry_cancel_tests::*`.

use super::*;

impl SessionRegistry {
    /// Wait, bounded by `grace`, for `id`'s cancelled execution to ACTUALLY
    /// stop (#8207).
    ///
    /// Why: [`Self::request_cancel`] only flips a cooperative flag, so a
    /// caller that reported "cancelled" the moment it returned was reporting a
    /// request, not an outcome — the daemon-side task kept running and the
    /// next `task.run` on the same session was rejected with
    /// `invalid_argument` ("already has a task running"). "Reported cancelled"
    /// and "actually stopped" have to be the same fact, so `session.cancel`
    /// blocks here until the spawned run's `JoinHandle` resolves. The run
    /// clears its own slot as its last act (`task::executor::run_and_record`
    /// calls `finish_execution`), so a resolved handle IS the guarantee that a
    /// following `begin_execution` will be accepted.
    /// What: takes the handle out of the entry, awaits it under `grace`, and
    /// returns `Ok(())` on a clean join. Never downgrades a failure to
    /// success: a `grace` that elapses first RESTORES the handle (so a later
    /// `shutdown_executions` still awaits it) and returns `RpcError::internal`
    /// — the caller must surface "still cancelling", never "cancelled". A
    /// panicked task is a stopped task, so a `JoinError` still counts as
    /// confirmation. `Err(session_not_found)` for an unknown `id`. An entry
    /// with no execution (already finished, or never started) is `Ok(())`:
    /// there is nothing left to wait for. An execution tracked with NO handle
    /// is the one case that cannot be confirmed — the window between
    /// `begin_execution` and [`Self::attach_execution_handle`] — and is an
    /// error for the same fail-closed reason as a timeout.
    /// Test: `registry_cancel_tests::await_cancelled_returns_once_the_task_has_stopped`,
    /// `registry_cancel_tests::await_cancelled_errors_when_the_task_outlives_the_grace`,
    /// `registry_cancel_tests::await_cancelled_is_ok_when_nothing_is_executing`,
    /// `registry_cancel_tests::await_cancelled_errors_when_the_execution_has_no_handle`,
    /// `registry_cancel_tests::await_cancelled_unknown_session_errors`.
    pub async fn await_cancelled(&self, id: &str, grace: Duration) -> Result<(), RpcError> {
        let handle = {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            match entry.execution.as_mut() {
                None => return Ok(()),
                Some(execution) => execution.handle.take(),
            }
        };
        let Some(mut handle) = handle else {
            return Err(RpcError::internal(format!(
                "session {id}: cancellation could not be confirmed — its run is \
                 tracked but not yet joinable"
            )));
        };
        if tokio::time::timeout(grace, &mut handle).await.is_err() {
            // #8207: put the handle back rather than dropping it — a dropped
            // `JoinHandle` detaches the task, and graceful shutdown would then
            // have nothing left to await.
            self.attach_execution_handle(id, handle);
            return Err(RpcError::internal(format!(
                "session {id}: cancellation requested but the task did not stop \
                 within {}s",
                grace.as_secs_f32()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "registry_cancel_tests.rs"]
mod tests;
