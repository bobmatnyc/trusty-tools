//! The daemon side of hook-fed worktree recording (#8994 S2a).
//!
//! Why: the hook pipeline sees every subagent's `cwd` the moment the agent
//! acts, so it is the one place that can record an agent-isolation tree
//! without waiting for `tm worktrees`. It runs inside the 5 s `PreToolUse`
//! budget, so everything that touches disk or git happens off that path.
//! What: [`spawn_for_event`] decides, in memory, whether an event needs ledger
//! work — the first event naming a tree (observe) and every `SubagentStop`,
//! `SubagentStopFailure` or `SessionEnd` (reconcile) — and runs that work on
//! a detached task. [`IngestTracker`] is the per-daemon dedup and
//! serialization state.
//!
//! Failure contract: the hook verdict never depends on this module. All
//! ledger I/O runs after the verdict is decided, on a detached task. A ledger
//! read or write error is (1) logged at `error`, (2) recorded in the
//! `worktrees.ingest-failure.json` sidecar beside the ledger, which the
//! `worktree_registry` doctor row reports as `Warn`, and (3) returned in the
//! task's [`IngestReport`]. A tree whose `observed` write (or whose registry
//! read) failed joins the tracker's `pending` set. The next reconcile — at most
//! one retry per stop event, never per tool call — observes every pending tree
//! again. The sidecar is cleared only by an ingest that appends successfully
//! while `pending` is empty: an unrelated success never hides a tree that is
//! still missing. A registry-read failure skips only the observe step; the
//! reconcile still runs, and the `reconcile_queued` flag is reset on every exit
//! path (a drop guard), so a failed task cannot absorb later stop events.
//! Nothing here deletes a worktree.
//! Test: `daemon::services::worktree_ledger_ingest::tests`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::core::worktree_ledger::backfill::registered_checkouts;
use crate::core::worktree_ledger::hook_ingest::{
    ObserveOutcome, agent_tree_of, clear_ingest_failure, observe_agent_tree, reconcile_agent_trees,
    record_ingest_failure,
};
use crate::core::worktree_ledger::reconcile::ReconcileReport;
use crate::core::worktree_ledger::{LEDGER_FILE, WorktreeLedger};
use crate::daemon::state::DaemonState;

/// Per-daemon state for hook-fed ledger ingest.
///
/// Why: the hot path must answer "already handled this tree?" without
/// reading the ledger, and two detached tasks must not interleave their
/// read-fold-append steps.
/// What: the trees already claimed, the claimed trees whose record failed and
/// await a retry (`pending`), a flag that coalesces queued reconciles, and a
/// lock held for the whole of one task's ledger work.
/// Test: `repeated_events_record_a_tree_once`,
/// `a_failed_registry_read_does_not_absorb_later_stops`,
/// `a_failed_tree_is_retried_and_the_alarm_outlives_an_unrelated_success`.
#[derive(Debug, Default)]
pub(crate) struct IngestTracker {
    seen: Mutex<HashSet<PathBuf>>,
    pending: Mutex<HashSet<PathBuf>>,
    reconcile_queued: AtomicBool,
    write: Mutex<()>,
}

impl IngestTracker {
    /// `true` the first time `tree` is claimed in this daemon's lifetime.
    fn claim(&self, tree: &Path) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tree.to_path_buf())
    }

    fn release(&self, tree: &Path) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(tree);
    }

    fn mark_pending(&self, tree: &Path) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tree.to_path_buf());
    }

    fn take_pending(&self) -> Vec<PathBuf> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .collect()
    }

    fn has_pending(&self) -> bool {
        !self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }
}

/// Resets `reconcile_queued` when a queued reconcile's task ends, however it ends.
///
/// Why: #8994 — an early return or a panic left the flag set, so every later
/// stop event was absorbed and no `removed` was written until restart.
/// What: [`Self::release`] resets the flag early (before the ledger read) and
/// disarms the guard; otherwise `Drop` resets it.
/// Test: `a_failed_registry_read_does_not_absorb_later_stops`.
struct QueueGuard {
    tracker: Option<Arc<IngestTracker>>,
}

impl QueueGuard {
    fn release(&mut self) {
        if let Some(tracker) = self.tracker.take() {
            tracker.reconcile_queued.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for QueueGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// What one detached ingest task did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// The observe step's outcome, when the event named a new tree.
    pub observed: Option<ObserveOutcome>,
    /// The reconcile step's report, when the event was a stop or session end.
    pub reconciled: Option<ReconcileReport>,
    /// The ledger or registry error, when a step failed.
    pub error: Option<String>,
}

/// `true` for the events after which an agent may have finished.
fn is_reconcile_event(event: HookEvent) -> bool {
    matches!(
        event,
        HookEvent::SubagentStop | HookEvent::SubagentStopFailure | HookEvent::SessionEnd
    )
}

/// Start the ledger work one hook event needs, if any.
///
/// Why: see the module doc.
/// What: claims the agent-isolation tree named by `payload.cwd` (first claim
/// only) and, on a stop or session-end event, queues one reconcile (a queued,
/// not-yet-started reconcile absorbs further requests). Returns `None` with no
/// work, with no tokio runtime (the claim is released so a later event
/// retries), or with a relative framework root. Otherwise returns the detached
/// task's handle; nothing on the hook path awaits it.
/// Test: `repeated_events_record_a_tree_once`,
/// `stop_marks_removed_only_after_the_tree_is_gone`,
/// `odd_payloads_never_panic_and_record_nothing`,
/// `an_unwritable_ledger_leaves_the_verdict_and_reaches_the_doctor_row`.
pub fn spawn_for_event(
    state: &Arc<DaemonState>,
    session: SessionId,
    event: HookEvent,
    payload: &Value,
) -> Option<tokio::task::JoinHandle<IngestReport>> {
    let tracker = state.worktree_ingest();
    let tree = payload
        .get("cwd")
        .and_then(Value::as_str)
        .and_then(|cwd| agent_tree_of(Path::new(cwd)))
        .filter(|tree| tracker.claim(tree));
    let reconcile =
        is_reconcile_event(event) && !tracker.reconcile_queued.swap(true, Ordering::SeqCst);
    if tree.is_none() && !reconcile {
        return None;
    }
    let root = state.framework_root();
    let runtime = tokio::runtime::Handle::try_current();
    let (Ok(runtime), true) = (runtime, root.is_absolute()) else {
        if let Some(tree) = &tree {
            tracker.release(tree);
        }
        if reconcile {
            tracker.reconcile_queued.store(false, Ordering::SeqCst);
        }
        tracing::warn!(
            root = %root.display(),
            "worktree ledger: hook ingest skipped — no async runtime or no absolute \
             framework root (#8994)"
        );
        return None;
    };
    let ledger = WorktreeLedger::at(root.join(LEDGER_FILE));
    let registry_dir = crate::project::registry_data_dir_under(root);
    let session = session.0.to_string();
    // #8994: armed before the task exists, so every exit path resets the flag.
    let guard = reconcile.then(|| QueueGuard {
        tracker: Some(Arc::clone(&tracker)),
    });
    Some(runtime.spawn(async move {
        // Only the observe and retry steps need the project registry.
        let needs_checkouts = tree.is_some() || (reconcile && tracker.has_pending());
        let checkouts = if needs_checkouts {
            registered_checkouts(&registry_dir)
                .await
                .map_err(|e| format!("project registry unreadable: {e}"))
        } else {
            Ok(Vec::new())
        };
        let work = move || ingest(&tracker, &ledger, tree, guard, checkouts, &session);
        tokio::task::spawn_blocking(work)
            .await
            .unwrap_or_else(|e| IngestReport {
                error: Some(format!("worktree ledger ingest task failed: {e}")),
                ..IngestReport::default()
            })
    }))
}

/// One blocking ingest pass: the ledger, the registry read and the report.
struct Pass<'a> {
    tracker: &'a IngestTracker,
    ledger: &'a WorktreeLedger,
    checkouts: &'a Result<Vec<PathBuf>, String>,
    session: &'a str,
    report: IngestReport,
    appended: bool,
}

impl Pass<'_> {
    /// Observe one tree. A registry or ledger failure leaves it `pending`.
    ///
    /// Why: #8994 — a failed tree must be retried, and must keep the sidecar.
    fn observe(&mut self, tree: &Path) -> Option<ObserveOutcome> {
        let checkouts = match self.checkouts {
            Ok(checkouts) => checkouts,
            Err(message) => {
                self.tracker.mark_pending(tree);
                fail(
                    self.ledger,
                    "read the project registry for",
                    tree,
                    message,
                    &mut self.report,
                );
                return None;
            }
        };
        match observe_agent_tree(self.ledger, tree, checkouts, Some(self.session)) {
            Ok(outcome) => {
                if let ObserveOutcome::Recorded { branch } = &outcome {
                    self.appended = true;
                    tracing::info!(
                        tree = %tree.display(),
                        branch = branch.as_deref().unwrap_or("-"),
                        "worktree ledger: observed an agent-isolation tree (#8994)"
                    );
                }
                Some(outcome)
            }
            Err(e) => {
                self.tracker.mark_pending(tree);
                fail(self.ledger, "observe", tree, &e, &mut self.report);
                None
            }
        }
    }
}

/// The blocking half: observe, retry failed trees, reconcile, under the lock.
///
/// Why: see the module doc; `guard` is `Some` for a queued reconcile.
/// What: drains `pending` first (so this event's own failure waits for the
/// next stop), observes the event's tree, retries the drained trees, reconciles
/// (resetting `reconcile_queued` before the ledger read), and clears the
/// sidecar only when something was appended, nothing failed and nothing is
/// still pending.
/// Test: `a_failed_tree_is_retried_and_the_alarm_outlives_an_unrelated_success`,
/// `a_failed_registry_read_does_not_absorb_later_stops`.
fn ingest(
    tracker: &IngestTracker,
    ledger: &WorktreeLedger,
    tree: Option<PathBuf>,
    mut guard: Option<QueueGuard>,
    checkouts: Result<Vec<PathBuf>, String>,
    session: &str,
) -> IngestReport {
    let _write = tracker.write.lock().unwrap_or_else(PoisonError::into_inner);
    let retry = if guard.is_some() {
        tracker.take_pending()
    } else {
        Vec::new()
    };
    let mut pass = Pass {
        tracker,
        ledger,
        checkouts: &checkouts,
        session,
        report: IngestReport::default(),
        appended: false,
    };
    if let Some(tree) = &tree {
        pass.report.observed = pass.observe(tree);
    }
    for tree in &retry {
        // #8994: at most one retry per stop event.
        pass.observe(tree);
    }
    if let Some(guard) = guard.as_mut() {
        // Cleared before the read, so a stop arriving during this pass queues
        // another one instead of being absorbed by a read that predates it.
        guard.release();
        match reconcile_agent_trees(ledger) {
            Ok(r) => {
                pass.appended |= r.removed > 0;
                pass.report.reconciled = Some(r);
            }
            Err(e) => fail(ledger, "reconcile", ledger.path(), &e, &mut pass.report),
        }
    }
    if pass.appended
        && pass.report.error.is_none()
        && !tracker.has_pending()
        && let Err(e) = clear_ingest_failure(ledger)
    {
        tracing::warn!("worktree ledger: could not clear the ingest-failure sidecar: {e} (#8994)");
    }
    pass.report
}

/// Log a ledger error and record it in the sidecar; never panics.
fn fail(
    ledger: &WorktreeLedger,
    step: &str,
    path: &Path,
    e: &dyn std::fmt::Display,
    out: &mut IngestReport,
) {
    let message = format!("hook ingest could not {step} {}: {e}", path.display());
    tracing::error!(
        "worktree ledger: {message} — the hook verdict is unaffected; `tm worktrees` \
         backfills and reconciles what was missed (#8994)"
    );
    if let Err(side) = record_ingest_failure(ledger, &message) {
        tracing::error!(
            "worktree ledger: could not record the ingest failure either: {side} (#8994)"
        );
    }
    out.error = Some(message);
}

#[cfg(test)]
#[path = "worktree_ledger_ingest_tests.rs"]
mod tests;
