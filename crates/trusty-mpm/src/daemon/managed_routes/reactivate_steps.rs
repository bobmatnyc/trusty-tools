//! The post-mark steps of an in-place reactivate and their rollback (#12012,
//! #12019).
//!
//! Why: split from `reactivate.rs` to keep it under the 500-SLOC cap.
//! What: [`detach`] spawns the project and index steps with the rollback that
//! keeps a departed caller's record off `Active`; [`Detached::answer`] waits
//! for the steps' answer.
//! Test: `reactivate_project_tests.rs`.

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::{SessionManager, SessionRecord};

use super::super::record_to_summary;

/// The steps task's pending answer, and what [`Detached::answer`] needs to
/// roll back a task that ended without one (#12012).
pub(super) struct Detached {
    rx: tokio::sync::oneshot::Receiver<RouteOutcome>,
    attempt: Attempt,
}

/// One reactivate attempt: the record it marked and the record to restore.
///
/// #12019: `marked` is the per-attempt token. A rollback that finds the stored
/// record changed since this mark (a later reactivate, a stop) writes nothing.
#[derive(Clone)]
struct Attempt {
    mgr: Arc<SessionManager>,
    marked: SessionRecord,
    prior: Option<SessionRecord>,
}

/// Run the post-mark steps in a task the route cannot cancel (#12012).
///
/// Why: the steps wait on trusty-search, and the client or the transport can
/// drop the route meanwhile. A dropped future would stop between the mark and
/// the rollback, leaving an `Active` record no client execs into.
/// What: a `tokio::spawn`ed task runs [`reactivate_project_and_index`] under
/// `budget` and [`roll_back`]s on its failure or timeout. #12019: the steps
/// race the answer channel closing; when the route is dropped the steps are
/// dropped and the record is rolled back at once, before the runtime reaper
/// can record a death. A success whose answer finds the route gone is rolled
/// back too, since no client will exec.
/// Test: `a_client_that_gives_up_on_a_slow_unpark_never_leaves_the_record_active`,
/// `an_unpark_slower_than_the_daemon_budget_fails_and_rolls_back`,
/// `an_early_socket_hangup_rolls_back_before_the_unpark_ends_12019`.
pub(super) fn detach(
    state: Arc<DaemonState>,
    mgr: Arc<SessionManager>,
    record: SessionRecord,
    prior: Option<SessionRecord>,
    budget: Duration,
) -> Detached {
    let (mut tx, rx) = tokio::sync::oneshot::channel();
    let attempt = Attempt {
        mgr,
        marked: record,
        prior,
    };
    let task = attempt.clone();
    tokio::spawn(async move {
        let record = &task.marked;
        let work = tokio::time::timeout(budget, reactivate_project_and_index(&state, &task));
        let steps = tokio::select! {
            steps = work => steps,
            () = tx.closed() => {
                let why = "the reactivate caller went away before the answer (#12019)";
                let rolled = roll_back(&task, why.to_string()).await;
                tracing::warn!(id = %record.id, "{rolled}");
                return;
            }
        };
        let steps = steps.unwrap_or_else(|_| {
            Err(format!(
                "session {} ({}): its project and search index did not come back within \
                 {}s (#12012)",
                record.id,
                record.tmux_name,
                budget.as_secs_f32()
            ))
        });
        let outcome = match steps {
            Ok(()) => RouteOutcome::ok(&record_to_summary(record)),
            Err(why) => RouteOutcome::text(500, roll_back(&task, why).await),
        };
        if let Err(outcome) = tx.send(outcome)
            && outcome.is_success()
        {
            let why = "the reactivate caller went away before the answer (#12012)".to_string();
            // #12019: awaited outside the macro, which skips a disabled level's args.
            let rolled = roll_back(&task, why).await;
            tracing::warn!(id = %record.id, "{rolled}");
        }
    });
    Detached { rx, attempt }
}

impl Detached {
    /// The steps task's answer; a task that ended without one (it panicked) is
    /// a 500 and is rolled back (#12012).
    pub(super) async fn answer(self) -> RouteOutcome {
        match self.rx.await {
            Ok(outcome) => outcome,
            Err(_) => {
                let why = "the in-place reactivate task ended without an answer (#12012)";
                // #12019: spawned, so a route dropped here still rolls back.
                let attempt = self.attempt;
                let rolled = tokio::spawn(async move { roll_back(&attempt, why.into()).await });
                let text = rolled
                    .await
                    .unwrap_or_else(|e| format!("{why}; rollback failed: {e}"));
                RouteOutcome::text(500, text)
            }
        }
    }
}

/// Bring back the project `record` binds to, then its search index (#12012).
///
/// Why: spawn and resume reactivate a decommissioned project and unpark its
/// index (#10988, #10989); the in-place relaunch is a third way a session of
/// that project runs again, so it owes the same two steps.
/// What: [`crate::project::ProjectRegistry::reactivate_for_session`] (the
/// spawn rule: only projects the record binds to), then
/// [`SessionManager::restore_parked_index`]. The first failure is `Err`.
/// Test: `an_inplace_reactivate_reactivates_the_bound_project_and_unparks_its_index`,
/// `an_inplace_reactivate_leaves_an_unbound_decommissioned_project_alone`.
async fn reactivate_project_and_index(
    state: &Arc<DaemonState>,
    attempt: &Attempt,
) -> Result<(), String> {
    let record = &attempt.marked;
    let registry = state.project_registry().await;
    registry.reactivate_for_session(record).await.map_err(|e| {
        format!(
            "session {} ({}): its project could not be reactivated and still reads \
             decommissioned (#12012): {e}",
            record.id, record.tmux_name
        )
    })?;
    attempt.mgr.restore_parked_index(&registry, record).await?;
    Ok(())
}

/// Roll a failed in-place reactivate back off `Active`; answers the route's
/// error text (#12012).
///
/// What: [`SessionManager::revert_reactivation`] to the attempt's prior record,
/// guarded by its marked record (#12019); a missing snapshot or a failed
/// rollback is named in the text and logged.
/// Test: `a_failed_unpark_rolls_the_reactivate_back_to_stopped`,
/// `a_late_rollback_never_undoes_a_later_reactivate_12019`.
async fn roll_back(attempt: &Attempt, why: String) -> String {
    let Some(prior) = attempt.prior.as_ref() else {
        tracing::error!("in-place reactivate failed with no prior record to roll back to: {why}");
        return format!("{why}; the session record could not be rolled back (no prior record)");
    };
    match attempt
        .mgr
        .revert_reactivation(prior, &attempt.marked)
        .await
    {
        Ok(record) => format!(
            "{why}; the session record was rolled back to {}",
            record.state
        ),
        Err(e) => {
            tracing::error!(id = %prior.id, "in-place reactivate rollback failed: {e}");
            format!("{why}; rolling the session record back also failed: {e}")
        }
    }
}
