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
use crate::session::task_result::{TaskResult, TaskResultStatus};

/// How often a cancel that found a PEER already confirming re-checks whether
/// the run it is waiting on is gone (#8207).
///
/// Why: the peer owns the `JoinHandle`, so this caller has nothing to await
/// directly; the observable it can poll is the execution slot itself. Ten
/// milliseconds is far below any human-perceptible cancel latency and costs
/// one uncontended mutex acquisition per tick.
const PEER_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// What a `session.cancel` found when it went looking for the run it must
/// confirm has stopped (#8207).
///
/// Why: `handle: None` meant two opposite things — "nobody has attached a
/// handle yet" (unconfirmable, fail closed) and "a concurrent cancel is
/// already joining it" (confirmable, just not by me). Collapsing them into one
/// `Option` is what made a second Esc answer `-32603 internal`.
enum CancelClaim {
    /// This caller owns the handle and must join it itself.
    Owned(Arc<AtomicBool>, JoinHandle<()>),
    /// A concurrent cancel holds the handle; wait on the execution slot.
    Peer(Arc<AtomicBool>),
    /// Tracked, never joinable: the window between `begin_execution` and
    /// `attach_execution_handle`.
    Unjoinable,
    /// Nothing is executing — already stopped.
    Idle,
}

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
    /// What: claims the run via [`Self::claim_cancel_confirm`] and then, per
    /// claim: joins the handle it owns ([`Self::join_owned`]), waits for the
    /// peer that owns it ([`Self::await_peer_confirm`]), returns `Ok(())` for
    /// an idle session (nothing left to wait for), or fails closed for an
    /// execution that is tracked with no handle at all. Never downgrades a
    /// failure to success: every unconfirmed outcome is
    /// `RpcError::cancel_unconfirmed`, so the caller can say "still
    /// cancelling" rather than lie. `Err(session_not_found)` for an unknown
    /// `id`.
    /// Test: `registry_cancel_tests::await_cancelled_returns_once_the_task_has_stopped`,
    /// `registry_cancel_tests::await_cancelled_errors_when_the_task_outlives_the_grace`,
    /// `registry_cancel_tests::await_cancelled_is_ok_when_nothing_is_executing`,
    /// `registry_cancel_tests::await_cancelled_errors_when_the_execution_has_no_handle`,
    /// `registry_cancel_tests::await_cancelled_unknown_session_errors`,
    /// `registry_cancel_tests::a_panicked_run_lands_failed_and_releases_its_slot`,
    /// `registry_cancel_tests::a_dead_handle_never_fails_a_newer_run`,
    /// `registry_cancel_tests::the_grace_timeout_never_clobbers_a_newer_runs_handle`,
    /// `registry_cancel_tests::two_concurrent_cancels_both_learn_the_run_stopped`.
    pub async fn await_cancelled(&self, id: &str, grace: Duration) -> Result<(), RpcError> {
        match self.claim_cancel_confirm(id)? {
            CancelClaim::Idle => Ok(()),
            CancelClaim::Unjoinable => Err(RpcError::cancel_unconfirmed(format!(
                "session {id}: cancellation could not be confirmed — its run is \
                 tracked but not yet joinable"
            ))),
            CancelClaim::Owned(cancel, handle) => self.join_owned(id, &cancel, handle, grace).await,
            CancelClaim::Peer(cancel) => self.await_peer_confirm(id, &cancel, grace).await,
        }
    }

    /// Decide, under one lock, which half of the cancel-confirm protocol this
    /// caller is running (#8207).
    ///
    /// Why: taking the handle and marking the execution "being confirmed" must
    /// be one atomic step, or two concurrent cancels both see an untaken
    /// handle or both see an untouched `None`.
    /// What: takes the handle when there is one (latching `confirming`), and
    /// otherwise reports whether a peer already took it. Returns the
    /// execution's cancel flag alongside, as the identity token the restore
    /// and peer-wait paths compare against.
    /// Test: covered through [`Self::await_cancelled`]'s tests.
    fn claim_cancel_confirm(&self, id: &str) -> Result<CancelClaim, RpcError> {
        let mut sessions = self.lock();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        let Some(execution) = entry.execution.as_mut() else {
            return Ok(CancelClaim::Idle);
        };
        let cancel = Arc::clone(&execution.cancel);
        Ok(match execution.handle.take() {
            Some(handle) => {
                execution.confirming = true;
                CancelClaim::Owned(cancel, handle)
            }
            None if execution.confirming => CancelClaim::Peer(cancel),
            None => CancelClaim::Unjoinable,
        })
    }

    /// Join the handle this caller took, bounded by `grace` (#8207).
    ///
    /// Why: three outcomes hide behind `timeout(..).await`, and the original
    /// cut tested only whether the OUTER `Elapsed` fired — folding a
    /// `JoinError` into "confirmed stop". A panicked run joins instantly, so
    /// that arm answered `Ok` while `run_and_record` had never reached
    /// `finish_execution`: the slot stayed held and every later prompt on that
    /// session failed with `-32003`.
    /// What: a clean join is `Ok(())`. A `JoinError` (panic or abort) is still
    /// a stopped run, so the WAIT stays `Ok(())` — but the run itself failed,
    /// and [`Self::land_dead_run`] is what makes that visible instead of
    /// logging it. An elapsed `grace` restores the handle and fails closed.
    /// Test: `registry_cancel_tests::a_panicked_run_lands_failed_and_releases_its_slot`,
    /// `registry_cancel_tests::await_cancelled_errors_when_the_task_outlives_the_grace`.
    async fn join_owned(
        &self,
        id: &str,
        cancel: &Arc<AtomicBool>,
        mut handle: JoinHandle<()>,
        grace: Duration,
    ) -> Result<(), RpcError> {
        match tokio::time::timeout(grace, &mut handle).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(join_error)) => {
                self.land_dead_run(id, cancel, &join_error);
                Ok(())
            }
            Err(_elapsed) => {
                self.restore_claimed_handle(id, cancel, handle);
                Err(RpcError::cancel_unconfirmed(format!(
                    "session {id}: cancellation requested but the task did not stop \
                     within {}s",
                    grace.as_secs_f32()
                )))
            }
        }
    }

    /// Wait for the concurrent cancel that owns the handle to finish
    /// confirming (#8207).
    ///
    /// Why: a second Esc (or a second GUI click through
    /// `trusty-code-gui`'s `POST /sessions/{id}/cancel`) must learn the truth
    /// about the run, not `-32603 internal`. This caller cannot join a handle
    /// it does not hold, so it polls the one observable that tells it the run
    /// is over: the execution slot the run clears on its way out.
    /// What: returns `Ok(())` as soon as the claimed execution is gone or has
    /// been replaced by a newer one (identity by `Arc::ptr_eq` on the cancel
    /// flag), and `cancel_unconfirmed` if `grace` elapses first — the same
    /// fail-closed answer the owning caller gets for the same run.
    /// Test: `registry_cancel_tests::two_concurrent_cancels_both_learn_the_run_stopped`,
    /// `registry_cancel_tests::a_concurrent_cancel_that_never_confirms_fails_closed`.
    async fn await_peer_confirm(
        &self,
        id: &str,
        cancel: &Arc<AtomicBool>,
        grace: Duration,
    ) -> Result<(), RpcError> {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            if !self.execution_is(id, cancel) {
                return Ok(());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(RpcError::cancel_unconfirmed(format!(
                    "session {id}: another cancel is still confirming this run, \
                     which did not stop within {}s",
                    grace.as_secs_f32()
                )));
            }
            tokio::time::sleep(PEER_POLL_INTERVAL.min(deadline - now)).await;
        }
    }

    /// Whether `id`'s tracked execution is still the one identified by
    /// `cancel` (#8207).
    ///
    /// Why: "the slot is occupied" is not the question — "the slot is occupied
    /// by the run I am waiting on" is. A newer run holds a different cancel
    /// `Arc`, and every waiter keeps a strong clone of the one it claimed, so
    /// the old allocation cannot be freed and its address reused underneath
    /// the comparison.
    fn execution_is(&self, id: &str, cancel: &Arc<AtomicBool>) -> bool {
        self.lock()
            .get(id)
            .and_then(|entry| entry.execution.as_ref())
            .is_some_and(|execution| Arc::ptr_eq(&execution.cancel, cancel))
    }

    /// Land a run that died without unwinding: release its slot, record a
    /// `Failed` result, and finish the session `Failed` (#8207).
    ///
    /// Why: a `JoinError` means the run PANICKED (or was aborted) — it never
    /// reached `task::executor::run_and_record`'s own
    /// `set_task_result`/`finish` pair, so without this the only trace is a log
    /// line the client never sees. Landing `Cancelled` with no result was worse
    /// than silent: `Cancelled` is resumable (`SessionStatus::is_resumable`), so
    /// the next prompt resumed a crashed session as if the user had stopped it
    /// on purpose. `Failed` is the status `finish_with_failure` already uses for
    /// a run that broke, and it is not resumable, so the next prompt is refused
    /// with a reason rather than silently continuing.
    /// What: mirrors `executor::finish_with_failure`'s shape (failed
    /// `TaskResult` carrying the reason, then `finish(Failed)`) but only when
    /// [`Self::release_claimed_execution`] confirms the dead run still holds the
    /// slot — a NEWER run that has already taken it owns both the slot and the
    /// session's status, and must not be failed for its predecessor's crash.
    /// Test: `registry_cancel_tests::a_panicked_run_lands_failed_and_releases_its_slot`,
    /// `registry_cancel_tests::a_dead_handle_never_fails_a_newer_run`.
    fn land_dead_run(
        &self,
        id: &str,
        cancel: &Arc<AtomicBool>,
        join_error: &tokio::task::JoinError,
    ) {
        tracing::error!(
            session_id = %id,
            error = %join_error,
            "cancelled run did not exit cleanly"
        );
        if !self.release_claimed_execution(id, cancel) {
            // A newer run owns the slot; its lifecycle is not this one's to end.
            return;
        }
        let reason = format!("the run did not exit cleanly: {join_error}");
        self.set_task_result(
            id,
            TaskResult::new(TaskResultStatus::Failed).with_summary(Some(reason)),
        );
        // The only error `finish` returns is `session_not_found`, which a
        // session pruned mid-cancel reaches legitimately — there is nothing left
        // to land, so log it rather than inventing a caller-facing failure out
        // of a run that did in fact stop.
        if let Err(err) = self.finish(id, SessionStatus::Failed) {
            tracing::warn!(
                session_id = %id,
                error = %err.message,
                "could not land Failed for a crashed run"
            );
        }
    }

    /// Clear the execution slot still held by a run that died without clearing
    /// it (#8207).
    ///
    /// Why: `finish_execution` clears whatever is there; if a NEWER run has
    /// already taken the slot, that would cancel-by-accident a run nobody
    /// asked about.
    /// What: clears the slot only when it still holds the claimed execution,
    /// returning whether it did — the caller reads that as "the dead run is
    /// still the session's current run", which is what makes the terminal
    /// transition in [`Self::land_dead_run`] safe.
    fn release_claimed_execution(&self, id: &str, cancel: &Arc<AtomicBool>) -> bool {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id)
            && entry
                .execution
                .as_ref()
                .is_some_and(|execution| Arc::ptr_eq(&execution.cancel, cancel))
        {
            entry.execution = None;
            return true;
        }
        false
    }

    /// Put a timed-out run's handle back, but only if the slot still holds
    /// THAT run (#8207).
    ///
    /// Why: the handle has to go back or `shutdown_executions` would have
    /// nothing left to await for this session — a dropped `JoinHandle`
    /// detaches its task. But the restore used to be unconditional, and the
    /// race it loses is real: the old run can clear its slot just before the
    /// grace ends, a new `task.run` can be accepted and attach its own live
    /// handle, and the restore then overwrites that live handle with the dead
    /// one. The new run becomes invisible to `shutdown_executions`, and a
    /// later cancel joins the dead handle and reports a stop while the new run
    /// is still going.
    /// What: restores under the same lock that checks identity, clearing
    /// `confirming` with it. When the slot has moved on, the stale handle is
    /// dropped instead — that run's slot was already released by the run
    /// itself, so there is nothing left to await.
    /// Test: `registry_cancel_tests::await_cancelled_errors_when_the_task_outlives_the_grace`,
    /// `registry_cancel_tests::the_grace_timeout_never_clobbers_a_newer_runs_handle`.
    fn restore_claimed_handle(&self, id: &str, cancel: &Arc<AtomicBool>, handle: JoinHandle<()>) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id)
            && let Some(execution) = entry.execution.as_mut()
            && Arc::ptr_eq(&execution.cancel, cancel)
        {
            execution.handle = Some(handle);
            execution.confirming = false;
        }
    }
}

#[cfg(test)]
#[path = "registry_cancel_tests.rs"]
mod tests;
