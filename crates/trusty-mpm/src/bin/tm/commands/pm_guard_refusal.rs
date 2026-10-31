//! How `tm hook --pm-guard` prints its decision, and what a guard that fails
//! still lets through (#10600, #10641).
//!
//! Why: Claude Code reads a non-zero hook exit as a non-blocking error, so a
//! guard that panicked out of `main` let the tool call through, and a guard
//! still judging at the 10 s hook timeout is cancelled. `onFailure: "block"`
//! on the hook entry (#10600, `session_launch::settings`) turns both into a
//! refusal of EVERY tool, which locks a session out of its own repair. This
//! module decides in-process first: a failure refuses writes and lets the
//! fixed recovery allowlist through (`pm_guard_recovery`). It also gives every
//! deny site one call that audits under the policy's own label (#10641).
//! What: [`Decision`] lets exactly one decision object reach stdout; [`emit`]
//! prints through the process-wide one. [`refuse`] audits and emits a deny.
//! [`run`] records nothing itself: `pm_guard` records its payload with
//! [`record_payload`], [`spawn_watchdog`] decides at [`GUARD_DEADLINE`] from
//! process start, and [`fail_closed`] decides a panic or an error.
//! Test: `pm_guard_refusal_tests.rs`.

use std::fmt::Display;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use futures::FutureExt as _;
use serde_json::Value;
use trusty_mpm::core::guard_policy::{self, GuardPolicy};

use crate::commands::pm_guard_deny_log::{DenyContext, audit_denied_tool};
use crate::commands::pm_guard_recovery::failure_verdict;
use crate::commands::pm_guard_response::build_pm_guard_deny_response;

/// By when, from process start, the guard decides or the watchdog does.
///
/// Why (#10641): Claude Code cancels the hook 10 s after it SPAWNS it, and
/// `started` is taken at the first line of `main`, after exec and loading. The
/// guard's own budgets sum past 8 s only when a call stacks the 5 s stdin read
/// (`hook_stdin::PM_GUARD_STDIN_TIMEOUT`) on more than one 2–2.5 s daemon call
/// plus the 2 s audit POST; one daemon call after a slow read (7.5 s) still
/// decides itself. The 2 s left covers spawn-to-`main` latency under load and
/// the print, flush and exit, which take milliseconds.
/// Test: `the_deadline_leaves_a_margin_inside_the_hook_timeout`.
pub(crate) const GUARD_DEADLINE: Duration = Duration::from_secs(8);

/// One process's single `PreToolUse` decision.
///
/// Why: stdout may carry exactly one object, and both the guard and the
/// watchdog may want to decide. The first to take the lock decides; the other
/// prints nothing.
/// Test: `a_slow_judgement_is_decided_by_the_watchdog`,
/// `emit_marks_the_decision_so_a_later_panic_prints_nothing`.
pub(crate) struct Decision {
    decided: Mutex<bool>,
}

impl Decision {
    /// An undecided decision.
    pub(crate) const fn new() -> Self {
        Self {
            decided: Mutex::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, bool> {
        self.decided.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write `decision` on `out` and mark it decided, unless already decided.
    ///
    /// What: `true` when this call wrote. The lock is held across the write,
    /// so a second decider waits and then finds the first one's answer. A
    /// write error is ignored: the reader is gone, so no object can reach it.
    /// #10641: pass an UNLOCKED writer; the one lock order is this mutex,
    /// then the writer's own lock (`emit` and `spawn_watchdog` both comply).
    pub(crate) fn print_once(&self, out: &mut dyn Write, decision: &dyn Display) -> bool {
        let mut decided = self.lock();
        if *decided {
            return false;
        }
        let _ = writeln!(out, "{decision}");
        let _ = out.flush();
        *decided = true;
        true
    }

    /// Mark it decided with nothing printed (an allow); `true` when this call did.
    pub(crate) fn claim(&self) -> bool {
        let mut decided = self.lock();
        !std::mem::replace(&mut *decided, true)
    }

    /// Whether a decision has been made.
    pub(crate) fn is_decided(&self) -> bool {
        *self.lock()
    }
}

/// The process-wide decision every pm-guard print goes through.
static DECISION: Decision = Decision::new();

/// The payload `pm_guard` read, for the failure arms' allowlist.
static PAYLOAD: OnceLock<Value> = OnceLock::new();

/// Record the payload the guard read (#10641): the failure arms judge it.
pub(crate) fn record_payload(payload: &Value) {
    let _ = PAYLOAD.set(payload.clone());
}

/// Print one `PreToolUse` decision object on stdout, unless one is out.
///
/// Test: `emit_marks_the_decision_so_a_later_panic_prints_nothing`.
pub(crate) fn emit(decision: impl Display) {
    // #10641: lock order is DECISION, then stdout, everywhere: an unlocked
    // handle here, so `print_once` takes DECISION before `writeln!` takes stdout.
    DECISION.print_once(&mut std::io::stdout(), &decision);
}

/// Audit a refusal under `policy`'s audit label, then print its deny (#10641).
///
/// Why: the audit label and the printed policy id come from the same entry.
/// What: `audit_denied_tool(refused, policy.rule, reason)`, then [`emit`].
/// Test: `every_pm_guard_deny_names_its_policy_id` (the deny body).
pub(crate) async fn refuse(refused: &DenyContext<'_>, policy: &GuardPolicy, reason: &str) {
    audit_denied_tool(refused, policy.rule, reason).await;
    emit(build_pm_guard_deny_response(policy, reason));
}

/// Start the thread that decides at `wait` if nothing has (#10641).
///
/// Why: a judgement can block its thread (a CPU-bound scan, a slow
/// filesystem), so only another OS thread can decide in time.
/// What: after `wait`, applies [`failure_verdict`] under
/// [`guard_policy::GUARD_TIMEOUT`] to `payload()`: a deny is printed through
/// `decision`, an allow claims it. Both do nothing once decided. Then
/// `on_expire` runs — `exit(0)` in production, so a printed decision is not
/// discarded by Claude Code's cancel.
/// Test: `a_slow_judgement_is_decided_by_the_watchdog`.
pub(crate) fn spawn_watchdog(
    decision: &'static Decision,
    wait: Duration,
    payload: impl FnOnce() -> Option<Value> + Send + 'static,
    mut out: impl Write + Send + 'static,
    on_expire: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        std::thread::sleep(wait);
        let payload = payload();
        let how = "it did not decide before its deadline";
        match failure_verdict(&guard_policy::GUARD_TIMEOUT, how, payload.as_ref()) {
            Some(deny) => {
                decision.print_once(&mut out, &deny);
            }
            None => {
                decision.claim();
            }
        }
        on_expire();
    });
}

/// Run one guard pass; on a panic or an error return the deny to print.
///
/// Why: see the module doc. `already_printed` and `payload` are parameters
/// so a test can drive every arm without the process-wide state.
/// What: polls `run` under `catch_unwind`. A completed run, or a failure
/// after a decision was printed, is `None`; otherwise [`failure_verdict`]
/// under [`guard_policy::GUARD_FAILURE`] — `None` (allow) for the recovery
/// allowlist, else the deny.
/// Test: `a_panicking_guard_denies`, `an_erroring_guard_denies`,
/// `a_failing_guard_allows_a_recovery_command`,
/// `a_completed_guard_keeps_its_own_answer`.
pub(crate) async fn fail_closed<F>(
    run: F,
    already_printed: impl Fn() -> bool,
    payload: impl Fn() -> Option<Value>,
) -> Option<Value>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let how = match AssertUnwindSafe(run).catch_unwind().await {
        Ok(Ok(())) => return None,
        Ok(Err(err)) => format!("it failed ({err:#})"),
        Err(_) => "it panicked".to_owned(),
    };
    if already_printed() {
        return None;
    }
    failure_verdict(&guard_policy::GUARD_FAILURE, &how, payload().as_ref())
}

/// `tm hook --pm-guard` entry point: `pm_guard::pm_guard` that fails closed,
/// bar the recovery allowlist.
///
/// What: arms [`spawn_watchdog`] for what is left of [`GUARD_DEADLINE`],
/// runs the guard under [`fail_closed`], emits a returned deny, then claims
/// the decision so the watchdog cannot decide during shutdown. Always `Ok`.
/// Test: `hook_mode_runs_the_guard_through_this_wrapper`; the guard end to
/// end in `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn run(url: &str, started: std::time::Instant) -> anyhow::Result<()> {
    // #10641: decide before Claude Code's 10 s cancel, so the allowlist applies.
    let wait = GUARD_DEADLINE.saturating_sub(started.elapsed());
    let payload = || PAYLOAD.get().cloned();
    spawn_watchdog(&DECISION, wait, payload, std::io::stdout(), || {
        std::process::exit(0)
    });
    // #10600: a guard that panics or errors decides here; it never exits non-zero.
    let guard = crate::commands::pm_guard::pm_guard(url, started);
    if let Some(deny) = fail_closed(guard, || DECISION.is_decided(), payload).await {
        emit(deny);
    }
    DECISION.claim();
    Ok(())
}

#[cfg(test)]
#[path = "pm_guard_refusal_tests.rs"]
mod tests;
