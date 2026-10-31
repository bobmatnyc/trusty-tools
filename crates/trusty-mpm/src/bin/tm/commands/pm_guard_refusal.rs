//! How `tm hook --pm-guard` prints its decision, and why a guard that fails
//! still denies (#10600, #10641).
//!
//! Why: Claude Code reads a non-zero hook exit as a non-blocking error, so a
//! guard that panicked out of `main` let the tool call through. `onFailure:
//! "block"` on the hook entry (#10600, `session_launch::settings`) covers that
//! on Claude Code v2.1.295 and later; this module is the in-process layer that
//! holds on any version. It also gives every deny site one call that audits
//! under the policy's own label and prints the policy-tagged refusal (#10641).
//! What: [`emit`] prints one stdout decision object and records that one was
//! printed. [`refuse`] audits and emits a deny for a policy entry. [`run`]
//! wraps `pm_guard::pm_guard` with [`fail_closed`]: a panic or an error with
//! no decision printed yet prints the [`guard_policy::GUARD_FAILURE`] deny; a
//! failure after a decision was printed leaves that decision as the one
//! stdout object.
//! Test: `a_panicking_guard_denies`, `an_erroring_guard_denies`,
//! `a_completed_guard_keeps_its_own_answer`,
//! `a_failure_after_a_printed_decision_prints_nothing_more`.

use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt as _;
use trusty_mpm::core::guard_policy::{self, GuardPolicy};

use crate::commands::pm_guard_deny_log::{DenyContext, audit_denied_tool};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;

/// Whether this process has printed its one `PreToolUse` decision object.
static DECISION_PRINTED: AtomicBool = AtomicBool::new(false);

/// Print one `PreToolUse` decision object on stdout and record that it was.
///
/// Why (#10600): the fail-closed wrapper must not add a second object after a
/// decision is out — stdout may carry exactly one. The flag is set only after
/// the print returns, so a print that panics (a closed pipe) still leaves the
/// wrapper free to deny.
/// What: `println!`, a flush (its error ignored: the object is written or the
/// process is going down), then the flag.
/// Test: `a_failure_after_a_printed_decision_prints_nothing_more`.
pub(crate) fn emit(decision: impl std::fmt::Display) {
    println!("{decision}");
    let _ = std::io::stdout().flush();
    DECISION_PRINTED.store(true, Ordering::SeqCst);
}

/// Whether [`emit`] has printed a decision in this process.
pub(crate) fn decision_printed() -> bool {
    DECISION_PRINTED.load(Ordering::SeqCst)
}

/// Audit a refusal under `policy`'s audit label, then print its deny (#10641).
///
/// Why: the audit label and the printed policy id come from the same entry,
/// so they cannot disagree, and a deny site is one statement.
/// What: `audit_denied_tool(refused, policy.rule, reason)`, then [`emit`] of
/// the policy-tagged deny.
/// Test: `every_pm_guard_deny_names_its_policy_id` (the deny body); the
/// audit label in `pm_guard_records_every_deny_where_list_recent_errors_reads`.
pub(crate) async fn refuse(refused: &DenyContext<'_>, policy: &GuardPolicy, reason: &str) {
    audit_denied_tool(refused, policy.rule, reason).await;
    emit(build_pm_guard_deny_response(policy, reason));
}

/// How one guard run ended.
#[derive(Debug)]
enum GuardOutcome {
    Completed,
    Errored(String),
    Panicked,
}

/// The deny to print for a guard run that failed, or `None`.
///
/// Why (#10600): a failure before any decision must deny, never fall through
/// to an allow; a failure after one must not print a second object.
/// What: `None` for a completed run or when `already_printed`; otherwise the
/// [`guard_policy::GUARD_FAILURE`] deny naming how the run failed.
/// Test: as the module doc.
fn failure_deny(outcome: &GuardOutcome, already_printed: bool) -> Option<serde_json::Value> {
    let how = match outcome {
        GuardOutcome::Completed => return None,
        _ if already_printed => return None,
        GuardOutcome::Errored(err) => format!("it failed ({err})"),
        GuardOutcome::Panicked => "it panicked".to_owned(),
    };
    let reason = format!(
        "tm hook --pm-guard could not decide this tool call because {how}, so it denies it \
         (#10600). A repeat is a bug in the guard; report it with the exact command."
    );
    Some(build_pm_guard_deny_response(
        &guard_policy::GUARD_FAILURE,
        &reason,
    ))
}

/// Run one guard pass; on a panic or an error return the deny to print.
///
/// Why: see the module doc. `already_printed` is a parameter so a test can
/// drive both arms without touching the process-wide flag.
/// What: polls `run` under `catch_unwind`, then [`failure_deny`].
/// Test: `a_panicking_guard_denies`, `an_erroring_guard_denies`,
/// `a_completed_guard_keeps_its_own_answer`.
pub(crate) async fn fail_closed<F>(
    run: F,
    already_printed: impl Fn() -> bool,
) -> Option<serde_json::Value>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let outcome = match AssertUnwindSafe(run).catch_unwind().await {
        Ok(Ok(())) => GuardOutcome::Completed,
        Ok(Err(err)) => GuardOutcome::Errored(format!("{err:#}")),
        Err(_) => GuardOutcome::Panicked,
    };
    failure_deny(&outcome, already_printed())
}

/// `tm hook --pm-guard` entry point: `pm_guard::pm_guard` that fails closed.
///
/// Why (#10600): see the module doc.
/// What: [`fail_closed`] over the guard; a returned deny is [`emit`]ted.
/// Always `Ok`, so the exit is 0 and Claude Code reads the printed object.
/// Test: `a_panicking_guard_denies`; the guard itself end to end in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn run(url: &str, started: std::time::Instant) -> anyhow::Result<()> {
    // #10600: a guard that panics or errors denies; it never exits non-zero.
    let guard = crate::commands::pm_guard::pm_guard(url, started);
    if let Some(deny) = fail_closed(guard, decision_printed).await {
        emit(deny);
    }
    Ok(())
}

#[cfg(test)]
#[path = "pm_guard_refusal_tests.rs"]
mod tests;
