//! The daemon side of Bash-fed worktree recording (#8994 S2b).
//!
//! Why: an agent that runs `git worktree add` in a Bash tool call makes a tree
//! no tm route records. The `PostToolUse` relay (`matcher: "*"`, `async`)
//! already brings every Bash call's command and `cwd` to the hook pipeline, so
//! the tree can be recorded when it appears instead of at the next backfill.
//! A failed Bash call fires `PostToolUseFailure`, which tm does not subscribe
//! to, so only `PostToolUse` is read; the record itself rests on disk and git
//! evidence ([`crate::core::worktree_ledger::command_ingest`]).
//! What: [`spawn_for_bash`] parses the command in memory and, when it names a
//! `git worktree add|remove|prune` (or names one it cannot resolve), runs the
//! ledger work on a detached task under S2a's write lock.
//!
//! Failure contract: the hook verdict never depends on this module. Every
//! failure — a call the parser cannot resolve, an unreadable registry, a git
//! that gives no answer, a ledger read or write error — is (1) logged at
//! `warn` with its cause, (2) recorded in S2a's `worktrees.ingest-failure.json`
//! sidecar, which the `worktree_registry` doctor row reports as `Warn`, and
//! (3) returned in [`CommandReport::errors`]. It also marks the shared
//! [`super::worktree_ledger_ingest::IngestTracker`], so no later success, S2a's
//! included, clears the sidecar while this daemon runs: there is no retry
//! here, and `tm worktrees`' backfill is what records a missed tree.
//! Nothing here deletes, moves, locks or unlocks a worktree.
//! Test: `daemon::services::worktree_command_ingest::tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::worktree_ledger_ingest::IngestTracker;
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::core::worktree_ledger::backfill::registered_checkouts;
use crate::core::worktree_ledger::bash_worktree::{
    ParsedCommand, WorktreeOp, parse_worktree_command,
};
use crate::core::worktree_ledger::command_ingest::{observe_added_tree, reconcile_command_removal};
use crate::core::worktree_ledger::hook_ingest::{ObserveOutcome, record_ingest_failure};
use crate::core::worktree_ledger::reconcile::ReconcileReport;
use crate::core::worktree_ledger::{LEDGER_FILE, WorktreeLedger};
use crate::daemon::state::DaemonState;

/// What one detached Bash-command ingest did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandReport {
    /// One outcome per `git worktree add`, in command order.
    pub added: Vec<ObserveOutcome>,
    /// One report per `git worktree remove` / `prune`, in command order.
    pub reconciled: Vec<ReconcileReport>,
    /// Every failure, each already logged at `warn` and recorded in the sidecar.
    pub errors: Vec<String>,
}

/// Start the ledger work one Bash `PostToolUse` needs, if any.
///
/// Why: see the module doc.
/// What: returns `None` — touching nothing — for any other event or tool, a
/// command without the word `worktree`, or one that runs no `git worktree`
/// call. Otherwise spawns the work and returns its handle; nothing on the hook
/// path awaits it. With no tokio runtime or a relative framework root the
/// work cannot run: that is logged at `warn` and marks the tracker.
/// Test: `each_worktree_add_form_records_one_entry`,
/// `commands_that_only_mention_the_words_record_nothing`,
/// `a_failed_record_is_never_silent_and_never_blocks`.
pub fn spawn_for_bash(
    state: &Arc<DaemonState>,
    session: SessionId,
    event: HookEvent,
    payload: &Value,
) -> Option<tokio::task::JoinHandle<CommandReport>> {
    if event != HookEvent::PostToolUse
        || payload.get("tool").and_then(Value::as_str) != Some("Bash")
    {
        return None;
    }
    let command = payload
        .get("input")
        .and_then(|input| input.get("command"))
        .and_then(Value::as_str)
        .filter(|c| c.contains("worktree"))?;
    let cwd = payload.get("cwd").and_then(Value::as_str).map(Path::new);
    let parsed = parse_worktree_command(command, cwd);
    if parsed.ops.is_empty() && parsed.errors.is_empty() {
        return None;
    }
    let tracker = state.worktree_ingest();
    let root = state.framework_root();
    let (Ok(runtime), true) = (tokio::runtime::Handle::try_current(), root.is_absolute()) else {
        tracker.mark_command_failed();
        tracing::warn!(
            root = %root.display(),
            "worktree ledger: a Bash `git worktree` call was not recorded — no async \
             runtime or no absolute framework root (#8994)"
        );
        return None;
    };
    let ledger = WorktreeLedger::at(root.join(LEDGER_FILE));
    let registry_dir = crate::project::registry_data_dir_under(root);
    let session = session.0.to_string();
    Some(runtime.spawn(async move {
        let checkouts = registered_checkouts(&registry_dir)
            .await
            .map_err(|e| format!("project registry unreadable: {e}"));
        let work = move || ingest(&tracker, &ledger, parsed, &checkouts, &session);
        tokio::task::spawn_blocking(work)
            .await
            .unwrap_or_else(|e| CommandReport {
                errors: vec![format!("worktree ledger Bash ingest task failed: {e}")],
                ..CommandReport::default()
            })
    }))
}

/// The blocking half: every op of one command, under the ledger write lock.
fn ingest(
    tracker: &IngestTracker,
    ledger: &WorktreeLedger,
    parsed: ParsedCommand,
    checkouts: &Result<Vec<PathBuf>, String>,
    session: &str,
) -> CommandReport {
    let _write = tracker.lock_write();
    let mut report = CommandReport::default();
    for error in &parsed.errors {
        let what = format!("a Bash `git worktree` call: {error}");
        report.errors.push(fail(tracker, ledger, &what));
    }
    for op in &parsed.ops {
        let (what, result) = match op {
            WorktreeOp::Add(candidates) => {
                let what = format!("`git worktree add` of {}", show(candidates));
                let result = match checkouts {
                    Ok(checkouts) => {
                        observe_added_tree(ledger, candidates, checkouts, Some(session))
                            .map(|outcome| report.added.push(outcome))
                            .map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.clone()),
                };
                (what, result)
            }
            WorktreeOp::Remove(candidates) => (
                format!("`git worktree remove` of {}", show(candidates)),
                reconcile_command_removal(ledger, Some(candidates))
                    .map(|r| report.reconciled.push(r))
                    .map_err(|e| e.to_string()),
            ),
            WorktreeOp::Prune => (
                "`git worktree prune`".to_string(),
                reconcile_command_removal(ledger, None)
                    .map(|r| report.reconciled.push(r))
                    .map_err(|e| e.to_string()),
            ),
        };
        match result {
            Ok(()) => tracing::info!("worktree ledger: read {what} (#8994)"),
            Err(e) => report
                .errors
                .push(fail(tracker, ledger, &format!("{what}: {e}"))),
        }
    }
    report
}

/// Log one failure at `warn`, record it in the sidecar, mark the tracker.
fn fail(tracker: &IngestTracker, ledger: &WorktreeLedger, what: &str) -> String {
    let message = format!("hook ingest could not record {what}");
    tracing::warn!(
        "worktree ledger: {message} — the hook verdict is unaffected; `tm worktrees` \
         backfills what was missed (#8994)"
    );
    if let Err(e) = record_ingest_failure(ledger, &message) {
        tracing::warn!("worktree ledger: could not record the ingest failure either: {e} (#8994)");
    }
    tracker.mark_command_failed();
    message
}

/// The candidates, for a log line.
fn show(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|c| c.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
#[path = "worktree_command_ingest_tests.rs"]
mod tests;
