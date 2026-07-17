//! Re-delegation cap: bounds total engineer delegation attempts per run (#2265).
//!
//! Why: (#2265, completes #2233) Before this module, the ONLY ceiling on how
//! many times the PM could re-delegate a failing engineer task was the PM's
//! OWN `AgentLoopConfig::default().max_turns` (8) — because #2233 raised the
//! delegated ENGINEER's turn budget to 40 without raising or decoupling the
//! PM's, each blind PM-driven retry could burn up to 40 engineer turns while
//! the PM itself had only 8 total attempts to notice, react, and finish. That
//! asymmetry produced non-deterministic failure storms (session turn counts
//! blowing up from ~40 to ~111) that hit the PM's turn cap and were reported
//! as an opaque `run_failure`, even when a fully working solution already sat
//! on disk from an earlier attempt. This module makes an explicit,
//! independent cap ([`MAX_REDELEGATIONS`]) the thing that governs retry
//! count, and — critically — enforces it INSIDE a single `delegate_to_agent`
//! tool dispatch via [`RedelegatingRunner`], so retries never consume
//! additional PM turns at all (the "decouple" design chosen for fix #3: see
//! `run_task::mod`'s `build_engineer_runner` docs for why this was preferred
//! over raising the PM's `max_turns`).
//! What: [`RedelegationCapSignal`] is a small `Arc`-shared attempt
//! counter/flag, constructed once per `execute_run_task` call and threaded
//! into both [`RedelegatingRunner`] (which increments it and retries) and
//! `assemble_report` (which reads `is_cap_reached()` to produce a clean
//! terminal report instead of riding on a coincidental PM-turn-cap failure).
//! [`RedelegatingRunner`] wraps the engineer `AgentRunner`: on a failure whose
//! [`crate::tools::delegate::redelegation_hint`] returns `Some` (partial work
//! may exist — turn cap, timeout, cancellation, or a retryable LLM/transport
//! error, per that function's #2265-updated docs), it retries the SAME
//! engineer agent with the task text augmented by the reuse hint, up to
//! [`MAX_REDELEGATIONS`] attempts **for that one delegation** before giving up
//! with a clean, quoted "re-delegation limit reached" error. A non-retryable
//! failure (unknown agent, bad config) propagates immediately without
//! consuming the budget.
//!
//! # #2852: the budget counts RETRIES, not delegations
//!
//! The original #2265 implementation shared ONE run-wide counter across every
//! `delegate_to_agent` call and refused the 4th, whatever it was. That
//! conflated two unrelated things: a *failing engineer being retried* (what
//! the cap is for) and a *PM using delegation to read files before building*
//! (the normal PM shape). A PM that spent three successful delegations on
//! reconnaissance had its FOURTH call — the actual build — refused without the
//! inner runner ever being invoked, and the latched signal then stopped the PM
//! loop, making it unrecoverable. Measured cost: 2-in-7 total-loss L4 bake-off
//! runs, caused by the harness rather than the model.
//!
//! So the two concerns are now counted separately:
//!
//! * [`MAX_REDELEGATIONS`] bounds attempts **within a single delegation**, via
//!   a local counter reset at every [`RedelegatingRunner::run_with_retries`]
//!   entry. A successful delegation therefore consumes NO budget from a later
//!   one. Exhausting it latches `retry_budget_exhausted` (informational: it
//!   labels the report) but deliberately does NOT stop the PM loop — a fresh,
//!   different delegation is a legitimate move that could well succeed, so the
//!   error is recoverable, exactly like #2683/#2805's post-completion refusal.
//! * [`MAX_FAILED_INVOCATIONS`] bounds **failed** engineer invocations
//!   run-wide. It is the surviving purpose of the shared signal: a genuinely
//!   broken run whose delegations keep failing must still terminate. It counts
//!   only FAILURES, never successes — otherwise the very bug above would simply
//!   reappear one level up, with a deep-but-successful recon sequence
//!   guillotined by a total-cost ceiling instead of by a per-run retry counter.
//!   Only THIS ceiling latches `cap_reached`, and hence only this one fires
//!   `run_task::mod`'s `with_stop_signal` — at which point stopping really is
//!   correct, because delegations have demonstrably stopped clearing it.
//!
//! Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
//! `successful_delegations_never_consume_a_later_delegations_budget`,
//! `many_successful_delegations_never_latch_the_failure_ceiling`,
//! `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
//! `failure_ceiling_latches_cap_reached`,
//! `redelegating_runner_propagates_non_retryable_errors_immediately`,
//! `cap_reached_message_names_the_project_path`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::Result;
use async_trait::async_trait;

use crate::tools::delegate::redelegation_hint;
use crate::tools::{AgentOutput, AgentRunner, RunContext};

/// Maximum engineer attempts for ONE `delegate_to_agent` call — 1 initial
/// invocation plus up to 2 reuse-aware retries (#2265, re-scoped by #2852).
///
/// Why: This — not the PM's own turn budget — is the ceiling on retry count.
/// 3 attempts is deliberately modest: each retry costs up to 40 engineer turns
/// (#2233's raised default), so a generous bound would reintroduce the
/// runaway-cost failure mode #2265 closed. #2852 re-scoped it from "per run"
/// to "per delegation": as a RUN-wide bound it silently guillotined PMs that
/// used delegation for legitimate reconnaissance before building, since a
/// successful delegation consumed budget a later one then lacked. Retries are
/// what needs bounding; delegations are not.
/// What: Checked against a counter local to each
/// [`RedelegatingRunner::run_with_retries`] call, reset at loop entry. Once
/// exceeded, that delegation stops without invoking the inner runner and
/// returns a recoverable error — the PM's loop continues and may delegate
/// again with a full, fresh budget (see [`MAX_FAILED_INVOCATIONS`] for the
/// run-wide backstop that keeps "again" from being unbounded).
/// Test: `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
/// `successful_delegations_never_consume_a_later_delegations_budget`.
pub const MAX_REDELEGATIONS: u32 = 3;

/// Maximum FAILED engineer invocations across a whole `execute_run_task` run
/// (#2852).
///
/// Why: With [`MAX_REDELEGATIONS`] scoped per-delegation, something must still
/// bound a pathological run in which delegations keep FAILING — otherwise a PM
/// stuck in a failing loop could re-delegate once per turn forever, which is
/// the runaway #2265 existed to prevent. Only FAILURES count toward it. This
/// fix's central thesis is that a successful delegation must never consume
/// budget a later one needs; that has to hold at the run-wide ceiling too, not
/// just at the per-delegation counter, or the same guillotine simply moves up
/// one level. A PM issuing many successful read-only recon delegations — one
/// per atomic recon step, the normal shape, and `tcode` is an interactive
/// daily driver rather than only a bake-off runner — must never be terminated
/// for it, no matter how deep the recon goes.
/// 12 is 4× the per-delegation retry budget: four separate delegations may
/// each fail completely before the run is declared hopeless.
/// What: Checked against the shared [`RedelegationCapSignal`] after an engineer
/// invocation FAILS. Reaching it is the ONLY thing that latches `cap_reached`,
/// and hence the only thing that fires `run_task::mod`'s `with_stop_signal` to
/// halt the PM loop — correct here precisely because a run that has failed this
/// many times has demonstrated that fresh delegations are not clearing it.
/// Successful invocations are counted only by
/// [`RedelegationCapSignal::attempts`], which is reporting-only and governs
/// nothing.
/// Test: `failure_ceiling_latches_cap_reached`,
/// `many_successful_delegations_never_latch_the_failure_ceiling`.
pub const MAX_FAILED_INVOCATIONS: u32 = 12;

/// Shared, run-scoped state behind [`RedelegationCapSignal`].
///
/// Why: Kept as a private inner type so the public handle stays a cheap,
/// `Clone`-able `Arc` wrapper — see [`RedelegationCapSignal`]'s own docs.
/// What: `attempts` counts every engineer invocation actually dispatched across
/// the whole run — a truthful cost measure for REPORTING only; it governs
/// nothing (#2852). `failed_invocations` counts only the subset that failed,
/// and is the sole input to the terminal ceiling, so that no number of
/// successes can ever end a run. `retry_budget_exhausted` latches `true` the
/// first time any ONE delegation burns all [`MAX_REDELEGATIONS`] attempts —
/// informational only. `cap_reached` latches `true` only when
/// `failed_invocations` reaches [`MAX_FAILED_INVOCATIONS`]; that one is
/// terminal (#2852).
/// Test: Exercised indirectly through `RedelegationCapSignal`'s own tests.
#[derive(Debug, Default)]
struct RedelegationCapState {
    attempts: AtomicU32,
    failed_invocations: AtomicU32,
    retry_budget_exhausted: AtomicBool,
    cap_reached: AtomicBool,
}

/// Shared signal tracking total re-delegation attempts across one run (#2265).
///
/// Why: [`RedelegatingRunner`] and `run_task::assemble_report` are different
/// call sites that both need to observe the SAME attempt count / cap state —
/// the runner to enforce the cap, the report assembler to recognise a
/// cap-triggered terminal state even when the PM's own loop error doesn't
/// literally say so (e.g. the PM's NEXT turn separately exhausts its own
/// budget after seeing the cap-reached tool error). An `Arc`-wrapped group of
/// atomics is the simplest thing that is `Send + Sync` and cheap to clone into
/// both places.
/// What: `new()` starts at zero attempts with both flags clear.
/// `is_cap_reached()`, `is_retry_budget_exhausted()` and `attempts()` are
/// read-only accessors for `assemble_report`; `record_attempt`,
/// `mark_retry_budget_exhausted` and `mark_cap_reached` are private, called
/// only by [`RedelegatingRunner`]. The two flags are deliberately distinct:
/// only `cap_reached` is terminal (#2852).
/// Test: `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
/// `failure_ceiling_latches_cap_reached`,
/// `successful_delegations_never_consume_a_later_delegations_budget`.
#[derive(Debug, Clone, Default)]
pub struct RedelegationCapSignal(Arc<RedelegationCapState>);

impl RedelegationCapSignal {
    /// Build a fresh signal with zero recorded attempts.
    ///
    /// Why: Each `execute_run_task` call needs its OWN signal — attempts must
    /// never leak across independent runs.
    /// What: `Self::default()`.
    /// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one more dispatched engineer invocation.
    ///
    /// Why: Reporting only (#2852) — an operator wants to know how much
    /// engineer work the run actually bought. It deliberately governs NOTHING:
    /// making total cost terminal is precisely the bug this fix removes.
    /// What: `fetch_add(1)`. Called only for invocations that actually reach
    /// the inner runner, so the count stays a truthful cost measure.
    /// Test: `many_successful_delegations_never_latch_the_failure_ceiling`.
    fn record_attempt(&self) {
        self.0.attempts.fetch_add(1, Ordering::SeqCst);
    }

    /// Record one FAILED engineer invocation and return the new run-wide total.
    ///
    /// Why: This is the terminal ceiling's only input. The runner must compare
    /// the POST-increment total against [`MAX_FAILED_INVOCATIONS`] atomically
    /// with recording it, so two concurrent failures can never both observe
    /// "one under the ceiling" and both proceed.
    /// What: `fetch_add(1) + 1`. Called once per invocation that returned an
    /// error, retryable or not — both shapes are failures the ceiling exists
    /// to bound.
    /// Test: `failure_ceiling_latches_cap_reached`.
    fn record_failure(&self) -> u32 {
        self.0.failed_invocations.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Latch the "some delegation burned its whole retry budget" flag (#2852).
    ///
    /// Why: `assemble_report` needs this to keep labelling a genuinely
    /// retry-exhausted run "re-delegation limit reached" (and to map it to
    /// `Partial` when a deliverable exists) — diagnostics #2852 must not lose.
    /// It is deliberately SEPARATE from `cap_reached`: this condition is
    /// recoverable, so it must never reach the PM loop's stop signal.
    /// What: Stores `true`.
    /// Test: `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`.
    fn mark_retry_budget_exhausted(&self) {
        self.0.retry_budget_exhausted.store(true, Ordering::SeqCst);
    }

    /// Latch the terminal run-wide failure-ceiling flag.
    ///
    /// Why: Called exactly once, the moment failed invocations reach
    /// [`MAX_FAILED_INVOCATIONS`] — the run has failed too many times for a
    /// fresh delegation to be worth betting on, so this is what both
    /// `assemble_report` and the PM loop's stop signal key off.
    /// What: Stores `true`.
    /// Test: `failure_ceiling_latches_cap_reached`.
    fn mark_cap_reached(&self) {
        self.0.cap_reached.store(true, Ordering::SeqCst);
    }

    /// Whether the run-wide FAILED-invocation ceiling was hit (#2852).
    ///
    /// Why: Drives `run_task::mod`'s `with_stop_signal`, so it must be `true`
    /// ONLY for conditions a fresh delegation genuinely cannot clear. Neither
    /// per-call retry exhaustion (see [`Self::is_retry_budget_exhausted`]) nor
    /// sheer invocation COUNT is one of those — a run of successful recon
    /// delegations must never land here.
    /// What: Reads the latched flag.
    /// Test: `failure_ceiling_latches_cap_reached`,
    /// `many_successful_delegations_never_latch_the_failure_ceiling`.
    pub fn is_cap_reached(&self) -> bool {
        self.0.cap_reached.load(Ordering::SeqCst)
    }

    /// Test-only: a signal with `retry_budget_exhausted` already latched.
    ///
    /// Why: `run_task::mod`'s `assemble_report` must be unit-testable against
    /// the retry-exhausted-with-deliverable shape — the most common real-world
    /// outcome once #2852 makes retry exhaustion recoverable — without
    /// standing up a whole scripted `RedelegatingRunner` from a sibling module.
    /// Kept `#[cfg(test)]` so the production API kept its invariant that only
    /// [`RedelegatingRunner`] can latch this.
    /// What: `Self::default()` with the one flag set.
    /// Test: `assemble_report_maps_retry_exhausted_with_deliverable_to_partial`.
    #[cfg(test)]
    pub(crate) fn retry_exhausted_for_test() -> Self {
        let signal = Self::default();
        signal.mark_retry_budget_exhausted();
        signal
    }

    /// Total FAILED engineer invocations recorded so far, run-wide.
    ///
    /// Why: The number the terminal report must quote — it is the ceiling's
    /// actual input, and unlike a pre-incremented dispatch counter it is
    /// exactly the count of invocations that really failed, so an operator is
    /// never told "13" when 12 occurred.
    /// What: Reads the atomic counter.
    /// Test: `failure_ceiling_latches_cap_reached`.
    pub fn failed_invocations(&self) -> u32 {
        self.0.failed_invocations.load(Ordering::SeqCst)
    }

    /// Whether any single delegation exhausted its [`MAX_REDELEGATIONS`]
    /// retry budget during this run (#2852).
    ///
    /// Why: Lets `assemble_report` distinguish "the engineer really was
    /// retried to exhaustion" from an unrelated PM-loop error, WITHOUT that
    /// observation stopping the loop. Read-only for report assembly.
    /// What: Reads the latched flag.
    /// Test: `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`.
    pub fn is_retry_budget_exhausted(&self) -> bool {
        self.0.retry_budget_exhausted.load(Ordering::SeqCst)
    }

    /// Total engineer invocations (successes AND failures) recorded so far,
    /// run-wide.
    ///
    /// Why: A truthful total-cost measure for callers that want it (e.g.
    /// future observability/metrics) — but it is reporting-only and governs
    /// nothing (#2852): the terminal report's message quotes
    /// [`Self::failed_invocations`] instead, precisely so a run of successful
    /// delegations is never described as having "failed" anything.
    /// What: Reads the atomic counter.
    /// Test: `many_successful_delegations_never_latch_the_failure_ceiling`,
    /// `redelegating_runner_retries_on_llm_error_then_succeeds`.
    pub fn attempts(&self) -> u32 {
        self.0.attempts.load(Ordering::SeqCst)
    }
}

/// `AgentRunner` decorator that internally retries a failed engineer
/// delegation, reuse-hint-augmented, up to [`MAX_REDELEGATIONS`] times before
/// giving up (#2265).
///
/// Why: This is the concrete "decouple retries from PM turns" implementation
/// chosen for fix #3 — every retry this decorator performs happens INSIDE one
/// `AgentRunner::run`/`run_with_context` call, i.e. inside one
/// `delegate_to_agent` tool dispatch from the PM's perspective, so the PM's
/// own `AgentLoopConfig::max_turns` is never consumed by them. Wrapping at the
/// `AgentRunner` seam (rather than inside `DelegateToAgentTool` itself) keeps
/// the tool free of retry policy and makes the retry/cap behaviour unit
/// testable in isolation with a scripted inner runner.
/// What: Holds the inner (real) engineer runner, a shared
/// [`RedelegationCapSignal`], and the project path (quoted in the
/// cap-reached message per the required wording, "…partial work preserved at
/// <path>"). If the run-wide ceiling has already latched, the delegation is
/// refused up front without dispatching anything. Otherwise, per delegation, a
/// LOCAL attempt counter starts at zero (#2852):
/// if it would exceed [`MAX_REDELEGATIONS`], stop WITHOUT invoking the inner
/// runner, latch `retry_budget_exhausted`, warn, and return a RECOVERABLE
/// error (the PM may delegate again with a fresh budget). Otherwise invoke the
/// inner runner; on success, return it. On failure, record the failure
/// run-wide; if that reaches [`MAX_FAILED_INVOCATIONS`], latch `cap_reached`
/// (terminal — stops the PM loop) and return. Otherwise, on a failure whose
/// [`redelegation_hint`] is `Some`, retry with the task text augmented by
/// that hint; on a failure whose hint is `None` (no partial work to reuse),
/// propagate immediately without consuming further budget.
/// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
/// `successful_delegations_never_consume_a_later_delegations_budget`,
/// `many_successful_delegations_never_latch_the_failure_ceiling`,
/// `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
/// `failure_ceiling_latches_cap_reached`,
/// `redelegating_runner_propagates_non_retryable_errors_immediately`.
pub struct RedelegatingRunner {
    inner: Arc<dyn AgentRunner>,
    signal: RedelegationCapSignal,
    project: PathBuf,
}

impl RedelegatingRunner {
    /// Construct from the inner engineer runner, a shared cap signal, and the
    /// project root (used only for the cap-reached message).
    ///
    /// Why: Constructor injection keeps this testable with a scripted inner
    /// runner, exactly like the crate's other `AgentRunner` decorators
    /// (`ModelPinningRunner`).
    /// What: Stores all three fields verbatim.
    /// Test: Every test in this module constructs one directly.
    pub fn new(
        inner: Arc<dyn AgentRunner>,
        signal: RedelegationCapSignal,
        project: PathBuf,
    ) -> Self {
        Self {
            inner,
            signal,
            project,
        }
    }

    /// Drive the retry loop for one logical delegation.
    ///
    /// Why: Shared by both `AgentRunner` methods so `run` and
    /// `run_with_context` can never drift on retry policy.
    /// What: See the type-level docs for the full attempt/retry/cap
    /// contract. `attempt` is LOCAL to this call and starts at zero every time
    /// (#2852) — that is the whole fix: a delegation's retry budget belongs to
    /// that delegation, so an earlier successful one cannot spend it. Returns
    /// the first successful `AgentOutput`; or, once this call's attempts would
    /// exceed [`MAX_REDELEGATIONS`], a recoverable error naming the count and
    /// project path; or, once the run's FAILED invocations reach
    /// [`MAX_FAILED_INVOCATIONS`], a terminal error that also latches the
    /// stop signal; or, on a non-retryable failure, that failure verbatim.
    /// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
    /// `successful_delegations_never_consume_a_later_delegations_budget`,
    /// `many_successful_delegations_never_latch_the_failure_ceiling`,
    /// `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
    /// `failure_ceiling_latches_cap_reached`,
    /// `redelegating_runner_propagates_non_retryable_errors_immediately`,
    /// `cap_reached_message_names_the_project_path`.
    async fn run_with_retries(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // Once the run-wide ceiling has latched, refuse WITHOUT dispatching.
        // The PM loop is already being stopped, so further engineer work is
        // pure waste — and dispatching it would also inflate the failure count
        // the terminal report quotes, telling an operator more invocations
        // failed than the ceiling actually allowed.
        if self.signal.is_cap_reached() {
            return Err(anyhow::anyhow!(
                "engineer failure ceiling reached after {} failed invocations this run; \
                 partial work preserved at {}",
                self.signal.failed_invocations(),
                self.project.display()
            ));
        }

        let mut current_task = task.to_string();
        // #2852: per-CALL, not per-run. Reset at entry so reconnaissance
        // delegations that succeed never starve the build that follows.
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            if attempt > MAX_REDELEGATIONS {
                self.signal.mark_retry_budget_exhausted();
                // #2852: this cap used to be entirely silent — run-6's stderr
                // carried no trace of it and the mechanism was only findable
                // by cross-run forensics on the report's `task` label. Say so.
                // Recoverable: the PM's loop continues (this does NOT latch
                // `cap_reached`), so it may delegate afresh.
                tracing::warn!(
                    agent = agent_name,
                    attempts = MAX_REDELEGATIONS,
                    run_invocations = self.signal.attempts(),
                    "re-delegation retry budget exhausted for this delegation; refusing \
                     further retries of it. The PM loop continues — a fresh delegation \
                     gets a full budget."
                );
                return Err(anyhow::anyhow!(
                    "re-delegation limit reached after {MAX_REDELEGATIONS} attempts; \
                     partial work preserved at {}",
                    self.project.display()
                ));
            }

            // Reporting only (#2852): total invocations govern nothing, so a
            // run of successful delegations can never be terminated for its
            // count. Recorded before dispatch so the cost measure stays
            // truthful even if the invocation panics or fails.
            self.signal.record_attempt();

            match self
                .inner
                .run_with_context(agent_name, &current_task, ctx)
                .await
            {
                Ok(out) => return Ok(out),
                Err(e) => {
                    // Only FAILURES feed the terminal ceiling — the whole point
                    // of #2852, applied at the run-wide level too and not just
                    // to the per-delegation counter.
                    let failures = self.signal.record_failure();
                    if failures >= MAX_FAILED_INVOCATIONS {
                        self.signal.mark_cap_reached();
                        // Terminal, unlike the per-call budget above: the run
                        // has failed too many times for a fresh delegation to
                        // be worth betting on, so `run_task::mod`'s stop signal
                        // halts the PM loop at its next turn boundary. Log
                        // loudly — this ends the run.
                        tracing::warn!(
                            agent = agent_name,
                            failed_invocations = failures,
                            ceiling = MAX_FAILED_INVOCATIONS,
                            "run-wide engineer failure ceiling reached; stopping the PM \
                             loop. Delegations this run kept failing rather than \
                             succeeding."
                        );
                        return Err(anyhow::anyhow!(
                            "engineer failure ceiling reached after {failures} failed \
                             invocations this run; partial work preserved at {}",
                            self.project.display()
                        ));
                    }

                    let Some(hint) = redelegation_hint(&e) else {
                        // No partial work to reuse — this is a hard failure
                        // (unknown agent, bad config), not a re-delegation
                        // candidate. Propagate immediately.
                        return Err(e);
                    };
                    current_task = format!("{task}{hint}");
                }
            }
        }
    }
}

#[async_trait]
impl AgentRunner for RedelegatingRunner {
    /// See [`Self::run_with_retries`]; forwards with a default `RunContext`.
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        self.run_with_retries(agent_name, task, &RunContext::default())
            .await
    }

    /// See [`Self::run_with_retries`]; forwards the caller's `RunContext`
    /// unchanged to every attempt.
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.run_with_retries(agent_name, task, ctx).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
