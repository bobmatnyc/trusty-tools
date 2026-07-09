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
//! [`MAX_REDELEGATIONS`] total attempts (shared across every
//! `delegate_to_agent` call in the run, not reset per call) before giving up
//! with a clean, quoted "re-delegation limit reached" error. A non-retryable
//! failure (unknown agent, bad config) propagates immediately without
//! consuming the cap.
//! Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
//! `redelegating_runner_stops_at_cap_and_marks_signal`,
//! `redelegating_runner_propagates_non_retryable_errors_immediately`,
//! `cap_reached_message_names_the_project_path`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::Result;
use async_trait::async_trait;

use crate::tools::delegate::redelegation_hint;
use crate::tools::{AgentOutput, AgentRunner, RunContext};

/// Maximum total engineer delegation attempts for a single `execute_run_task`
/// run, shared across every `delegate_to_agent` call the PM makes (#2265).
///
/// Why: This — not the PM's own turn budget — is the ceiling on retry count
/// going forward. 3 total attempts (1 initial + up to 2 reuse-aware retries)
/// is deliberately modest: each retry now costs up to 40 engineer turns
/// (#2233's raised default), so an unbounded or generous cap here would
/// reintroduce the same runaway-cost failure mode this fix closes.
/// What: Checked BEFORE every engineer invocation in
/// [`RedelegatingRunner::run_with_retries`]; once the shared attempt count
/// exceeds this value, no further engineer invocation happens at all.
/// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
pub const MAX_REDELEGATIONS: u32 = 3;

/// Shared, run-scoped state behind [`RedelegationCapSignal`].
///
/// Why: Kept as a private inner type so the public handle stays a cheap,
/// `Clone`-able `Arc` wrapper — see [`RedelegationCapSignal`]'s own docs.
/// What: `attempts` counts every engineer invocation attempted across the
/// whole run; `cap_reached` latches `true` the first time an attempt would
/// exceed [`MAX_REDELEGATIONS`].
/// Test: Exercised indirectly through `RedelegationCapSignal`'s own tests.
#[derive(Debug, Default)]
struct RedelegationCapState {
    attempts: AtomicU32,
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

    /// Record one more attempt and return the new total.
    ///
    /// Why: The runner must check the POST-increment total against the cap
    /// atomically with recording it, so two attempts can never both observe
    /// "one under the cap" and both proceed.
    /// What: `fetch_add(1) + 1`.
    /// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
    fn record_attempt(&self) -> u32 {
        self.0.attempts.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Latch the cap-reached flag.
    ///
    /// Why: Called exactly once, the moment an attempt would exceed
    /// [`MAX_REDELEGATIONS`] — this is the signal `assemble_report` checks.
    /// What: Stores `true`.
    /// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
    fn mark_cap_reached(&self) {
        self.0.cap_reached.store(true, Ordering::SeqCst);
    }

    /// Whether the re-delegation cap was hit during this run.
    ///
    /// Why: `assemble_report` uses this to distinguish a cap-triggered
    /// terminal state from an unrelated PM-loop error.
    /// What: Reads the latched flag.
    /// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
    pub fn is_cap_reached(&self) -> bool {
        self.0.cap_reached.load(Ordering::SeqCst)
    }

    /// Total engineer attempts recorded so far.
    ///
    /// Why: Surfaced in the clean terminal report's message so an operator
    /// knows exactly how many tries were made.
    /// What: Reads the atomic counter.
    /// Test: `redelegating_runner_stops_at_cap_and_marks_signal`.
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
/// <path>"). On each attempt: record it against the shared signal; if the
/// new total exceeds [`MAX_REDELEGATIONS`], stop WITHOUT invoking the inner
/// runner, latch the cap, and return a clean, descriptive error. Otherwise
/// invoke the inner runner; on success, return it; on a failure whose
/// [`redelegation_hint`] is `Some`, retry with the task text augmented by
/// that hint; on a failure whose hint is `None` (no partial work to reuse),
/// propagate immediately without consuming further cap budget.
/// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
/// `redelegating_runner_stops_at_cap_and_marks_signal`,
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
    /// contract. Returns the first successful `AgentOutput`, or — once the
    /// shared attempt count would exceed [`MAX_REDELEGATIONS`] — a clean
    /// error naming the attempt count and the project path, or — on a
    /// non-retryable failure — that failure verbatim.
    /// Test: `redelegating_runner_retries_on_llm_error_then_succeeds`,
    /// `redelegating_runner_stops_at_cap_and_marks_signal`,
    /// `redelegating_runner_propagates_non_retryable_errors_immediately`,
    /// `cap_reached_message_names_the_project_path`.
    async fn run_with_retries(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let mut current_task = task.to_string();
        loop {
            let attempt = self.signal.record_attempt();
            if attempt > MAX_REDELEGATIONS {
                self.signal.mark_cap_reached();
                return Err(anyhow::anyhow!(
                    "re-delegation limit reached after {MAX_REDELEGATIONS} attempts; \
                     partial work preserved at {}",
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

    /// Once attempts exceed `MAX_REDELEGATIONS`, the runner stops calling the
    /// inner runner, marks the cap reached, and returns a clean error naming
    /// the attempt count.
    ///
    /// Why: This is fix #1 — the explicit ceiling that replaces the PM's own
    /// turn budget as the retry governor.
    /// What: Script `MAX_REDELEGATIONS` failing `Llm` outcomes (all
    /// retryable); assert the final error mentions "re-delegation limit
    /// reached" and the exact attempt count, the inner runner was called
    /// exactly `MAX_REDELEGATIONS` times (not `MAX_REDELEGATIONS + 1` — the
    /// cap check happens BEFORE the attempt that would exceed it), and the
    /// signal reports `is_cap_reached() == true`.
    /// Test: this test.
    #[tokio::test]
    async fn redelegating_runner_stops_at_cap_and_marks_signal() {
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
            .expect_err("must fail once the cap is exceeded");

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
        assert!(signal.is_cap_reached(), "signal must latch cap-reached");
        assert_eq!(signal.attempts(), MAX_REDELEGATIONS + 1);
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
