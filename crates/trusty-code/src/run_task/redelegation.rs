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
//! * [`MAX_ENGINEER_INVOCATIONS`] bounds engineer invocations **run-wide**. It
//!   is the surviving purpose of the shared signal: a genuinely broken run
//!   whose every delegation burns a full retry budget must still terminate.
//!   Only THIS ceiling latches `cap_reached`, and hence only this one fires
//!   `run_task::mod`'s `with_stop_signal` — at which point stopping really is
//!   correct, because no further delegation could clear it.
//!
//! Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
//! `successful_delegations_never_consume_a_later_delegations_budget`,
//! `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
//! `run_wide_invocation_ceiling_latches_cap_reached`,
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
/// again with a full, fresh budget (see [`MAX_ENGINEER_INVOCATIONS`] for the
/// run-wide backstop that keeps "again" from being unbounded).
/// Test: `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
/// `successful_delegations_never_consume_a_later_delegations_budget`.
pub const MAX_REDELEGATIONS: u32 = 3;

/// Maximum engineer invocations across a whole `execute_run_task` run (#2852).
///
/// Why: With [`MAX_REDELEGATIONS`] scoped per-delegation, something must still
/// bound a pathological run in which EVERY delegation burns its full retry
/// budget — otherwise a PM stuck in a failing loop could re-delegate once per
/// turn forever, which is the runaway #2265 existed to prevent. 12 is 4× the
/// per-delegation budget: it lets four separate delegations fail completely
/// before the run is declared hopeless, which no legitimate recon-then-build
/// shape can reach (recon delegations succeed on their first attempt and cost
/// 1 invocation each; run-6's pathological case totalled ~6), while still
/// binding well inside the PM's 8-turn × 3-attempt worst case of 24.
/// What: Checked against the shared [`RedelegationCapSignal`] before every
/// engineer invocation. Exceeding it is the ONLY thing that latches
/// `cap_reached`, and hence the only thing that fires `run_task::mod`'s
/// `with_stop_signal` to halt the PM loop — correct here precisely because no
/// fresh delegation could clear it.
/// Test: `run_wide_invocation_ceiling_latches_cap_reached`.
pub const MAX_ENGINEER_INVOCATIONS: u32 = 12;

/// Shared, run-scoped state behind [`RedelegationCapSignal`].
///
/// Why: Kept as a private inner type so the public handle stays a cheap,
/// `Clone`-able `Arc` wrapper — see [`RedelegationCapSignal`]'s own docs.
/// What: `attempts` counts every engineer invocation actually made across the
/// whole run. `retry_budget_exhausted` latches `true` the first time any ONE
/// delegation burns all [`MAX_REDELEGATIONS`] attempts — informational only.
/// `cap_reached` latches `true` only when an invocation would exceed the
/// run-wide [`MAX_ENGINEER_INVOCATIONS`] ceiling; that one is terminal (#2852).
/// Test: Exercised indirectly through `RedelegationCapSignal`'s own tests.
#[derive(Debug, Default)]
struct RedelegationCapState {
    attempts: AtomicU32,
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
/// budget after seeing the cap-reached tool error). An `Arc`-wrapped atomic
/// pair is the simplest thing that is `Send + Sync` and cheap to clone into
/// both places.
/// What: `new()` starts at zero attempts, cap not reached. `is_cap_reached()`
/// and `attempts()` are read-only accessors for `assemble_report`;
/// `record_attempt`/`mark_cap_reached` are private, called only by
/// [`RedelegatingRunner`].
/// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
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

    /// Record one more engineer invocation and return the new run-wide total.
    ///
    /// Why: The runner must check the POST-increment total against
    /// [`MAX_ENGINEER_INVOCATIONS`] atomically with recording it, so two
    /// invocations can never both observe "one under the ceiling" and both
    /// proceed.
    /// What: `fetch_add(1) + 1`. Called only for invocations that actually
    /// reach the inner runner, so the count stays a truthful cost measure.
    /// Test: `run_wide_invocation_ceiling_latches_cap_reached`.
    fn record_attempt(&self) -> u32 {
        self.0.attempts.fetch_add(1, Ordering::SeqCst) + 1
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

    /// Latch the terminal run-wide cap flag.
    ///
    /// Why: Called exactly once, the moment an invocation would exceed
    /// [`MAX_ENGINEER_INVOCATIONS`] — the run is out of total budget and
    /// nothing the PM does next can help, so this is what both
    /// `assemble_report` and the PM loop's stop signal key off.
    /// What: Stores `true`.
    /// Test: `run_wide_invocation_ceiling_latches_cap_reached`.
    fn mark_cap_reached(&self) {
        self.0.cap_reached.store(true, Ordering::SeqCst);
    }

    /// Whether the run-wide engineer-invocation ceiling was hit (#2852).
    ///
    /// Why: Drives `run_task::mod`'s `with_stop_signal`, so it must be `true`
    /// ONLY for conditions a fresh delegation genuinely cannot clear. Per-call
    /// retry exhaustion is not one of those — see
    /// [`Self::is_retry_budget_exhausted`].
    /// What: Reads the latched flag.
    /// Test: `run_wide_invocation_ceiling_latches_cap_reached`.
    pub fn is_cap_reached(&self) -> bool {
        self.0.cap_reached.load(Ordering::SeqCst)
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

    /// Total engineer invocations recorded so far, run-wide.
    ///
    /// Why: Surfaced in the terminal report's message so an operator knows
    /// exactly how much engineer work the run actually bought.
    /// What: Reads the atomic counter.
    /// Test: `run_wide_invocation_ceiling_latches_cap_reached`.
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
/// <path>"). Per delegation, a LOCAL attempt counter starts at zero (#2852):
/// if it would exceed [`MAX_REDELEGATIONS`], stop WITHOUT invoking the inner
/// runner, latch `retry_budget_exhausted`, warn, and return a RECOVERABLE
/// error (the PM may delegate again with a fresh budget). Otherwise record the
/// invocation run-wide; if THAT would exceed [`MAX_ENGINEER_INVOCATIONS`],
/// latch `cap_reached` (terminal — stops the PM loop) and return. Otherwise
/// invoke the inner runner; on success, return it; on a failure whose
/// [`redelegation_hint`] is `Some`, retry with the task text augmented by
/// that hint; on a failure whose hint is `None` (no partial work to reuse),
/// propagate immediately without consuming further budget.
/// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
/// `successful_delegations_never_consume_a_later_delegations_budget`,
/// `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
/// `run_wide_invocation_ceiling_latches_cap_reached`,
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
    /// project path; or, once the run's invocations would exceed
    /// [`MAX_ENGINEER_INVOCATIONS`], a terminal error that also latches the
    /// stop signal; or, on a non-retryable failure, that failure verbatim.
    /// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
    /// `successful_delegations_never_consume_a_later_delegations_budget`,
    /// `redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal`,
    /// `run_wide_invocation_ceiling_latches_cap_reached`,
    /// `redelegating_runner_propagates_non_retryable_errors_immediately`,
    /// `cap_reached_message_names_the_project_path`.
    async fn run_with_retries(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
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

            let total = self.signal.record_attempt();
            if total > MAX_ENGINEER_INVOCATIONS {
                self.signal.mark_cap_reached();
                // Terminal, unlike the per-call budget above: the run has spent
                // its whole engineer allowance, so no fresh delegation can help
                // and `run_task::mod`'s stop signal will halt the PM loop at
                // its next turn boundary. Log loudly — this ends the run.
                tracing::warn!(
                    agent = agent_name,
                    invocations = total - 1,
                    ceiling = MAX_ENGINEER_INVOCATIONS,
                    "run-wide engineer invocation ceiling reached; stopping the PM loop. \
                     Every delegation this run consumed retries without succeeding."
                );
                return Err(anyhow::anyhow!(
                    "engineer invocation ceiling reached after {} invocations this run; \
                     partial work preserved at {}",
                    MAX_ENGINEER_INVOCATIONS,
                    self.project.display()
                ));
            }

            match self
                .inner
                .run_with_context(agent_name, &current_task, ctx)
                .await
            {
                Ok(out) => return Ok(out),
                Err(e) => {
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
mod tests {
    use std::sync::Mutex;

    use anyhow::anyhow;

    use super::*;
    use crate::agent_loop::AgentLoopError;
    use crate::llm::LlmError;
    use crate::runner::RunnerError;

    /// Scripted inner runner: replays a fixed queue of `Result`s in order,
    /// recording the task text it was called with each time.
    ///
    /// Why: Deterministic, offline substitute for the real engineer runner so
    /// these tests exercise ONLY the retry/cap policy in `RedelegatingRunner`.
    /// What: `outcomes` is drained front-to-back via a `Mutex<VecDeque<..>>`;
    /// `calls` records every `(agent_name, task)` pair seen.
    /// Test: Used by every test below.
    struct ScriptedInner {
        outcomes: Mutex<std::collections::VecDeque<Result<AgentOutput>>>,
        calls: Mutex<Vec<String>>,
    }

    impl ScriptedInner {
        fn new(outcomes: Vec<Result<AgentOutput>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AgentRunner for ScriptedInner {
        async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(task.to_string());
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("ScriptedInner exhausted for {agent_name}")))
        }
    }

    /// Captures every `tracing` event's level + message for assertions.
    ///
    /// Why: #2852 requires the cap to stop being SILENT — run-6's stderr had
    /// no trace of it, so the mechanism was only discoverable by cross-run
    /// forensics on the report's `task` label. Proving the log exists needs an
    /// in-process subscriber; the crate has no tracing-capture dev-dependency,
    /// and a ~15-line `Layer` is cheaper than adding one.
    /// What: A `Layer` pushing `(Level, message)` into a shared `Vec`.
    /// Test: `cap_exhaustion_is_logged_at_warn_level`.
    #[derive(Default, Clone)]
    struct CapturedLogs(Arc<Mutex<Vec<(tracing::Level, String)>>>);

    struct CaptureLayer(CapturedLogs);

    struct MessageVisitor(String);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            self.0
                .0
                .lock()
                .expect("captured logs lock")
                .push((*event.metadata().level(), visitor.0));
        }
    }

    fn llm_error() -> anyhow::Error {
        anyhow::Error::from(RunnerError::Loop {
            name: "python-engineer".to_string(),
            source: AgentLoopError::Llm(LlmError::ApiError {
                status: 500,
                body: "bedrock hiccup".to_string(),
            }),
        })
    }

    fn unknown_agent_error() -> anyhow::Error {
        anyhow::Error::from(RunnerError::UnknownAgent {
            name: "python-engineer".to_string(),
            dir: PathBuf::from("/agents"),
        })
    }

    /// A retryable `AgentLoopError::Llm` failure is retried automatically,
    /// with the reuse hint appended, and a later success is returned.
    ///
    /// Why: This is the core #2265 fix #3 behaviour — retries happen inside
    /// ONE call, invisible to the PM's own turn budget.
    /// What: Script [Err(llm), Ok(success)]; call `run`; assert the returned
    /// output is the success, the inner runner was called twice, and the
    /// SECOND call's task text carries the reuse hint.
    /// Test: this test.
    #[tokio::test]
    async fn redelegating_runner_retries_on_llm_error_then_succeeds() {
        let inner = Arc::new(ScriptedInner::new(vec![
            Err(llm_error()),
            Ok(AgentOutput::from_content("done")),
        ]));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal.clone(),
            PathBuf::from("/tmp/project"),
        );

        let out = runner
            .run("python-engineer", "build the package")
            .await
            .expect("second attempt must succeed");
        assert_eq!(out.content, "done");

        let calls = inner.calls.lock().expect("calls lock");
        assert_eq!(
            calls.len(),
            2,
            "must retry exactly once after the LLM error"
        );
        assert_eq!(calls[0], "build the package");
        assert!(
            calls[1].contains("READ and CONTINUE"),
            "the retried task must carry the reuse hint, got: {}",
            calls[1]
        );
        assert!(
            !signal.is_cap_reached(),
            "cap must not be reached on a successful retry"
        );
        assert_eq!(signal.attempts(), 2);
    }

    /// (#2852 REGRESSION) A PM that spends several SUCCESSFUL delegations on
    /// reconnaissance may still delegate the actual build afterwards — with a
    /// full, fresh retry budget.
    ///
    /// Why: This is the exact total-loss bug #2852 closes, and the shape of
    /// L4 bake-off run-6: the PM issued 3 legitimate read-only recon
    /// delegations (read PROBLEM.md, list test_suite, read test_basic.py) and
    /// its 4th call — the real build — was refused before the inner runner was
    /// ever invoked, yielding `partial`/exit 6 with 0/9 tests and 0/5
    /// deliverables. Runs 3/4/5 succeeded only by luck of issuing one fewer
    /// recon call. Recon-then-build is the NORMAL PM shape, so under the old
    /// run-wide counter any PM reading more than two files before building was
    /// killed by the harness. Against the pre-fix code this test fails on the
    /// 4th `run` call.
    /// What: Script [Ok, Ok, Ok] for three recon delegations, then
    /// [Err(retryable llm), Ok] for the build. Assert every recon call
    /// succeeds, the build ALSO runs (not refused) and succeeds on its own
    /// retry — proving both that a successful delegation consumes no later
    /// budget and that the build's budget was reset, not merely off-by-one.
    /// Test: this test.
    #[tokio::test]
    async fn successful_delegations_never_consume_a_later_delegations_budget() {
        let inner = Arc::new(ScriptedInner::new(vec![
            Ok(AgentOutput::from_content("PROBLEM.md contents")),
            Ok(AgentOutput::from_content("test_suite listing")),
            Ok(AgentOutput::from_content("test_basic.py contents")),
            // The build: fails once transiently, then succeeds.
            Err(llm_error()),
            Ok(AgentOutput::from_content("build complete")),
        ]));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal.clone(),
            PathBuf::from("/tmp/project"),
        );

        for recon in ["read PROBLEM.md", "list test_suite", "read test_basic.py"] {
            let out = runner
                .run("python-engineer", recon)
                .await
                .unwrap_or_else(|e| panic!("recon delegation {recon:?} must succeed, got: {e}"));
            assert!(!out.content.is_empty());
        }

        let out = runner
            .run("python-engineer", "implement the solution")
            .await
            .expect(
                "the BUILD delegation must run — under the pre-#2852 run-wide counter it \
                 was refused as attempt 4 without ever invoking the engineer",
            );
        assert_eq!(out.content, "build complete");

        assert!(
            !signal.is_cap_reached(),
            "no run-wide ceiling was approached, so the PM loop must not be stopped"
        );
        assert!(
            !signal.is_retry_budget_exhausted(),
            "no single delegation burned its retry budget"
        );
        assert_eq!(
            inner.calls.lock().expect("calls lock").len(),
            5,
            "3 recon invocations + 2 build invocations must all have reached the engineer"
        );
    }

    /// Once ONE delegation's attempts exceed `MAX_REDELEGATIONS`, the runner
    /// stops calling the inner runner and returns a clean error naming the
    /// attempt count — but does NOT latch the loop-stopping `cap_reached`.
    ///
    /// Why: This is the cap's real purpose (fix #1) — a genuinely failing
    /// engineer must stay bounded — held together with #2852's constraint that
    /// bounding it must not be fatal to the RUN. The PM loop's stop signal is
    /// unrecoverable by construction, so it may only fire on a condition a
    /// fresh delegation cannot clear; retry exhaustion is not one.
    /// `retry_budget_exhausted` latches instead, which labels the report
    /// without killing the loop.
    /// What: Script `MAX_REDELEGATIONS` failing `Llm` outcomes (all
    /// retryable); assert the error names the cap and count, the inner runner
    /// ran exactly `MAX_REDELEGATIONS` times (the check happens BEFORE the
    /// attempt that would exceed), `is_retry_budget_exhausted()` is set, and
    /// `is_cap_reached()` is NOT.
    /// Test: this test.
    #[tokio::test]
    async fn redelegating_runner_stops_at_per_call_cap_without_latching_stop_signal() {
        let outcomes = (0..MAX_REDELEGATIONS).map(|_| Err(llm_error())).collect();
        let inner = Arc::new(ScriptedInner::new(outcomes));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal.clone(),
            PathBuf::from("/tmp/project"),
        );

        let err = runner
            .run("python-engineer", "build the package")
            .await
            .expect_err("must fail once this delegation's retries are exhausted");

        assert!(
            err.to_string().contains("re-delegation limit reached"),
            "error must name the cap condition, got: {err}"
        );
        assert!(
            err.to_string().contains(&MAX_REDELEGATIONS.to_string()),
            "error must name the attempt count, got: {err}"
        );

        let calls = inner.calls.lock().expect("calls lock");
        assert_eq!(
            calls.len(),
            MAX_REDELEGATIONS as usize,
            "the inner runner must be called exactly MAX_REDELEGATIONS times, not more"
        );
        assert!(
            signal.is_retry_budget_exhausted(),
            "signal must latch retry-budget-exhausted so the report can label it"
        );
        assert!(
            !signal.is_cap_reached(),
            "retry exhaustion is RECOVERABLE (#2852) — it must not stop the PM loop, \
             which would kill a run whose next delegation could well succeed"
        );
        assert_eq!(
            signal.attempts(),
            MAX_REDELEGATIONS,
            "only real invocations count; the refused 4th never reached the engineer"
        );
    }

    /// Exceeding the run-wide `MAX_ENGINEER_INVOCATIONS` ceiling latches
    /// `cap_reached`, which is what stops the PM loop.
    ///
    /// Why: With `MAX_REDELEGATIONS` scoped per-delegation (#2852), this is the
    /// backstop that keeps a pathological run — every delegation burning a full
    /// retry budget — from re-delegating once per PM turn forever. Unlike
    /// per-call exhaustion, this one IS terminal, so it is correct for it to
    /// fire the stop signal.
    /// What: Fail every invocation retryably and delegate repeatedly. Assert
    /// the ceiling latches `cap_reached`, the engineer was invoked exactly
    /// `MAX_ENGINEER_INVOCATIONS` times (never more), and the final error names
    /// the ceiling.
    /// Test: this test.
    #[tokio::test]
    async fn run_wide_invocation_ceiling_latches_cap_reached() {
        // Generously more failures than the ceiling allows to be consumed.
        let outcomes = (0..MAX_ENGINEER_INVOCATIONS * 2)
            .map(|_| Err(llm_error()))
            .collect();
        let inner = Arc::new(ScriptedInner::new(outcomes));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal.clone(),
            PathBuf::from("/tmp/project"),
        );

        // Each call burns MAX_REDELEGATIONS invocations, so the ceiling is
        // reached after MAX_ENGINEER_INVOCATIONS / MAX_REDELEGATIONS calls.
        let mut last_err = None;
        for _ in 0..(MAX_ENGINEER_INVOCATIONS / MAX_REDELEGATIONS) + 1 {
            last_err = runner
                .run("python-engineer", "build the package")
                .await
                .err();
        }

        let err = last_err.expect("every delegation fails, so the last must error");
        assert!(
            signal.is_cap_reached(),
            "the run-wide ceiling must latch cap_reached to stop the PM loop"
        );
        assert!(
            err.to_string()
                .contains("engineer invocation ceiling reached"),
            "the terminal error must name the run-wide ceiling, got: {err}"
        );
        assert_eq!(
            signal.attempts(),
            MAX_ENGINEER_INVOCATIONS + 1,
            "the ceiling is detected on the invocation that would exceed it, which is \
             never dispatched"
        );
        assert_eq!(
            inner.calls.lock().expect("calls lock").len(),
            MAX_ENGINEER_INVOCATIONS as usize,
            "the engineer must never be invoked beyond the run-wide ceiling"
        );
    }

    /// A non-retryable failure (no partial work to reuse) propagates
    /// immediately, without consuming further cap budget on retries that
    /// would never help.
    ///
    /// Why: Guards the negative case — the retry loop must not blindly retry
    /// EVERY failure, only ones `redelegation_hint` says are worth reusing.
    /// What: Script a single `UnknownAgent` error; assert it propagates
    /// verbatim after exactly one attempt, and the cap is NOT marked reached.
    /// Test: this test.
    #[tokio::test]
    async fn redelegating_runner_propagates_non_retryable_errors_immediately() {
        let inner = Arc::new(ScriptedInner::new(vec![Err(unknown_agent_error())]));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal.clone(),
            PathBuf::from("/tmp/project"),
        );

        let err = runner
            .run("python-engineer", "build the package")
            .await
            .expect_err("unknown-agent failures must propagate");

        assert!(err.to_string().contains("unknown agent"));
        let calls = inner.calls.lock().expect("calls lock");
        assert_eq!(calls.len(), 1, "must not retry a non-retryable failure");
        assert!(
            !signal.is_cap_reached(),
            "cap must not be marked reached for a non-retryable failure"
        );
    }

    /// Exhausting a delegation's retry budget emits a WARN-level log naming
    /// the attempt count.
    ///
    /// Why: #2852's observability half. The cap was completely silent — no log
    /// line anywhere — so the only evidence a run had been guillotined was a
    /// label buried in the report's `task` field, which took a cross-run
    /// forensic comparison to interpret. An operator must be able to see this
    /// in stderr.
    /// What: Force per-call retry exhaustion under a capturing subscriber;
    /// assert a WARN event was emitted naming the attempt count. (Logs go to
    /// stderr via the crate's subscriber, never stdout — the tracing default.)
    ///
    /// `rebuild_interest_cache` is load-bearing, not defensive: `tracing`
    /// caches per-callsite interest GLOBALLY, and this binary's
    /// `logging::tests::init_tracing_for_test_is_idempotent` installs a
    /// process-global `EnvFilter` subscriber that (with `RUST_LOG` unset)
    /// rejects this crate's WARN events. Whichever runs first wins the cache,
    /// so without an explicit rebuild against THIS thread's subscriber the
    /// assertion is order-dependent — it flaked 1-in-N during development.
    /// Since interest is re-evaluated per callsite on the next event, the
    /// rebuild deterministically re-admits the callsite for this scoped
    /// subscriber.
    /// Test: this test.
    #[tokio::test]
    async fn cap_exhaustion_is_logged_at_warn_level() {
        use tracing_subscriber::layer::SubscriberExt;

        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(CaptureLayer(logs.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let outcomes = (0..MAX_REDELEGATIONS).map(|_| Err(llm_error())).collect();
        let runner = RedelegatingRunner::new(
            Arc::new(ScriptedInner::new(outcomes)) as Arc<dyn AgentRunner>,
            RedelegationCapSignal::new(),
            PathBuf::from("/tmp/project"),
        );
        let _ = runner.run("python-engineer", "build the package").await;

        let captured = logs.0.lock().expect("captured logs lock");
        let warned = captured.iter().any(|(level, msg)| {
            *level == tracing::Level::WARN && msg.contains("retry budget exhausted")
        });
        assert!(
            warned,
            "exhausting the retry budget must WARN — it was completely silent before \
             #2852, making total-loss runs undiagnosable. Captured: {captured:?}"
        );
    }

    /// The cap-reached error message names the exact project path passed to
    /// the constructor.
    ///
    /// Why: The required message shape is "re-delegation limit reached after
    /// N attempts; partial work preserved at <path>" — pin the `<path>` half.
    /// What: Force the cap with `MAX_REDELEGATIONS` failures against a
    /// distinctive project path; assert it appears in the error text.
    /// Test: this test.
    #[tokio::test]
    async fn cap_reached_message_names_the_project_path() {
        let outcomes = (0..MAX_REDELEGATIONS).map(|_| Err(llm_error())).collect();
        let inner = Arc::new(ScriptedInner::new(outcomes));
        let signal = RedelegationCapSignal::new();
        let runner = RedelegatingRunner::new(
            Arc::clone(&inner) as Arc<dyn AgentRunner>,
            signal,
            PathBuf::from("/very/distinctive/project/path"),
        );

        let err = runner
            .run("python-engineer", "build the package")
            .await
            .expect_err("must fail once the cap is exceeded");

        assert!(
            err.to_string().contains("/very/distinctive/project/path"),
            "error must name the project path, got: {err}"
        );
    }
}
