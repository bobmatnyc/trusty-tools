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
//! task's [`IngestReport`]. The sidecar is cleared by the next ingest that
//! appends successfully. A tree whose `observed` write failed is not retried
//! in this daemon's lifetime (no retry per tool call); `tm worktrees`
//! backfills it. Nothing here deletes a worktree.
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
use crate::core::worktree_ledger::{LEDGER_FILE, LedgerError, WorktreeLedger};
use crate::daemon::state::DaemonState;

/// Per-daemon state for hook-fed ledger ingest.
///
/// Why: the hot path must answer "already handled this tree?" without
/// reading the ledger, and two detached tasks must not interleave their
/// read-fold-append steps.
/// What: the trees already claimed, a flag that coalesces queued reconciles,
/// and a lock held for the whole of one task's ledger work.
/// Test: `repeated_events_record_a_tree_once`.
#[derive(Debug, Default)]
pub(crate) struct IngestTracker {
    seen: Mutex<HashSet<PathBuf>>,
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
    Some(runtime.spawn(async move {
        let checkouts = match &tree {
            Some(tree) => match registered_checkouts(&registry_dir).await {
                Ok(checkouts) => checkouts,
                Err(e) => {
                    // A registry we cannot read says nothing about the tree;
                    // release it so a later event decides.
                    tracker.release(tree);
                    tracing::warn!("worktree ledger: project registry unreadable ({e}) (#8994)");
                    return IngestReport {
                        error: Some(format!("project registry unreadable: {e}")),
                        ..IngestReport::default()
                    };
                }
            },
            None => Vec::new(),
        };
        let work = move || ingest(&tracker, &ledger, tree, reconcile, &checkouts, &session);
        tokio::task::spawn_blocking(work)
            .await
            .unwrap_or_else(|e| IngestReport {
                error: Some(format!("worktree ledger ingest task failed: {e}")),
                ..IngestReport::default()
            })
    }))
}

/// The blocking half: observe, then reconcile, under the tracker's lock.
fn ingest(
    tracker: &IngestTracker,
    ledger: &WorktreeLedger,
    tree: Option<PathBuf>,
    reconcile: bool,
    checkouts: &[PathBuf],
    session: &str,
) -> IngestReport {
    let _write = tracker.write.lock().unwrap_or_else(PoisonError::into_inner);
    let mut report = IngestReport::default();
    let mut appended = false;
    if let Some(tree) = tree {
        match observe_agent_tree(ledger, &tree, checkouts, Some(session)) {
            Ok(outcome) => {
                if let ObserveOutcome::Recorded { branch } = &outcome {
                    appended = true;
                    tracing::info!(
                        tree = %tree.display(),
                        branch = branch.as_deref().unwrap_or("-"),
                        "worktree ledger: observed an agent-isolation tree (#8994)"
                    );
                }
                report.observed = Some(outcome);
            }
            Err(e) => fail(ledger, "observe", &tree, &e, &mut report),
        }
    }
    if reconcile {
        // Cleared before the read, so a stop arriving during this pass queues
        // another one instead of being absorbed by a read that predates it.
        tracker.reconcile_queued.store(false, Ordering::SeqCst);
        match reconcile_agent_trees(ledger) {
            Ok(r) => {
                appended |= r.removed > 0;
                report.reconciled = Some(r);
            }
            Err(e) => fail(ledger, "reconcile", ledger.path(), &e, &mut report),
        }
    }
    if appended
        && report.error.is_none()
        && let Err(e) = clear_ingest_failure(ledger)
    {
        tracing::warn!("worktree ledger: could not clear the ingest-failure sidecar: {e} (#8994)");
    }
    report
}

/// Log a ledger error and record it in the sidecar; never panics.
fn fail(ledger: &WorktreeLedger, step: &str, path: &Path, e: &LedgerError, out: &mut IngestReport) {
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
