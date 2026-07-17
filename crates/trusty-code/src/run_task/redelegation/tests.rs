//! Unit tests for the parent module's re-delegation cap machinery (#2265, #2852).
//!
//! Why: Split out of `redelegation.rs` so the production module stays under the
//! 500-SLOC cap; this is a test file (basename `tests.rs`), capped at 1500.
//! What: Exercises `RedelegatingRunner`'s retry/cap policy and
//! `RedelegationCapSignal` against a scripted inner runner and a
//! tracing-capture layer.
//! Test: this module is itself the test surface.

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

/// Reaching the run-wide `MAX_FAILED_INVOCATIONS` ceiling latches
/// `cap_reached`, which is what stops the PM loop.
///
/// Why: With `MAX_REDELEGATIONS` scoped per-delegation (#2852), this is the
/// backstop that keeps a pathological run — delegations that keep failing —
/// from re-delegating once per PM turn forever. Unlike per-call exhaustion,
/// this one IS terminal, so it is correct for it to fire the stop signal.
/// What: Fail every invocation retryably and delegate repeatedly. Assert
/// the ceiling latches `cap_reached`, the engineer failed exactly
/// `MAX_FAILED_INVOCATIONS` times (never more), and the final error names
/// the ceiling with the TRUE failure count — the operator-visible number
/// must be the count of invocations that really failed, not a
/// pre-incremented dispatch counter reading one higher (#2852).
/// Test: this test.
#[tokio::test]
async fn failure_ceiling_latches_cap_reached() {
    // Generously more failures than the ceiling allows to be consumed.
    let outcomes = (0..MAX_FAILED_INVOCATIONS * 2)
        .map(|_| Err(llm_error()))
        .collect();
    let inner = Arc::new(ScriptedInner::new(outcomes));
    let signal = RedelegationCapSignal::new();
    let runner = RedelegatingRunner::new(
        Arc::clone(&inner) as Arc<dyn AgentRunner>,
        signal.clone(),
        PathBuf::from("/tmp/project"),
    );

    // Each call burns MAX_REDELEGATIONS failures, so the ceiling is
    // reached after MAX_FAILED_INVOCATIONS / MAX_REDELEGATIONS calls.
    let mut last_err = None;
    for _ in 0..(MAX_FAILED_INVOCATIONS / MAX_REDELEGATIONS) + 1 {
        last_err = runner
            .run("python-engineer", "build the package")
            .await
            .err();
    }

    let err = last_err.expect("every delegation fails, so the last must error");
    assert!(
        signal.is_cap_reached(),
        "the run-wide failure ceiling must latch cap_reached to stop the PM loop"
    );
    assert!(
        err.to_string().contains("engineer failure ceiling reached"),
        "the terminal error must name the run-wide ceiling, got: {err}"
    );
    assert_eq!(
        signal.failed_invocations(),
        MAX_FAILED_INVOCATIONS,
        "the ceiling latches ON the failure that reaches it — the count an operator \
             is shown must be the real number of failed invocations"
    );
    assert!(
        err.to_string()
            .contains(&format!("{MAX_FAILED_INVOCATIONS} failed invocations")),
        "the terminal error must quote the TRUE failure count (never ceiling+1), \
             got: {err}"
    );
    assert_eq!(
        inner.calls.lock().expect("calls lock").len(),
        MAX_FAILED_INVOCATIONS as usize,
        "the engineer must never be invoked beyond the run-wide failure ceiling"
    );

    // A delegation issued AFTER the ceiling latched must be refused up
    // front: the PM loop is already stopping, so dispatching more engineer
    // work would be waste that also inflates the reported failure count.
    let before = inner.calls.lock().expect("calls lock").len();
    let err = runner
        .run("python-engineer", "build the package")
        .await
        .expect_err("delegations after the ceiling latches must be refused");
    assert!(
        err.to_string().contains("engineer failure ceiling reached"),
        "the post-latch refusal must still name the ceiling, got: {err}"
    );
    assert_eq!(
        inner.calls.lock().expect("calls lock").len(),
        before,
        "a post-latch delegation must not reach the engineer at all"
    );
    assert_eq!(
        signal.failed_invocations(),
        MAX_FAILED_INVOCATIONS,
        "a refused delegation dispatches nothing, so it must not record a failure"
    );
}

/// No number of SUCCESSFUL delegations may ever latch the terminal ceiling
/// (#2852).
///
/// Why: This is the regression guard for the fix's own central thesis,
/// applied at the run-wide level. A ceiling that counted every invocation —
/// successes included — reproduced the exact bug #2852 exists to remove,
/// one level up: a PM doing legitimate deep reconnaissance (one delegation
/// per atomic recon step, all succeeding) would still be guillotined into a
/// terminal, zero-deliverable run, just needing more steps to trigger it.
/// `tcode` is an interactive daily driver, so recon-heavy shapes are normal
/// and must never be fatal.
/// What: Issue far more successful delegations than `MAX_FAILED_INVOCATIONS`
/// allows failures; assert every one succeeds, `cap_reached` never latches,
/// no failure is ever recorded, and `attempts()` still counts them all
/// truthfully as a cost measure that governs nothing.
/// Test: this test.
#[tokio::test]
async fn many_successful_delegations_never_latch_the_failure_ceiling() {
    let calls = MAX_FAILED_INVOCATIONS * 3;
    let outcomes = (0..calls)
        .map(|_| Ok(AgentOutput::from_content("recon done")))
        .collect();
    let inner = Arc::new(ScriptedInner::new(outcomes));
    let signal = RedelegationCapSignal::new();
    let runner = RedelegatingRunner::new(
        Arc::clone(&inner) as Arc<dyn AgentRunner>,
        signal.clone(),
        PathBuf::from("/tmp/project"),
    );

    for i in 0..calls {
        runner
            .run("research", "inspect one more module")
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "successful recon delegation {i} must never be \
                     refused — successes do not consume the failure ceiling: {e}"
                )
            });
        assert!(
            !signal.is_cap_reached(),
            "cap_reached latched after {} successful delegations — a run of successes \
                 must never be terminal (#2852)",
            i + 1
        );
    }

    assert_eq!(
        signal.failed_invocations(),
        0,
        "no invocation failed, so the failure ceiling's input must stay at zero"
    );
    assert!(
        !signal.is_retry_budget_exhausted(),
        "no delegation burned its retry budget"
    );
    assert_eq!(
        signal.attempts(),
        calls,
        "attempts() must still count every invocation truthfully — it is a reporting \
             cost measure, it simply governs nothing"
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

/// Reaching the run-wide failure ceiling emits a WARN-level log naming the
/// true failure count.
///
/// Why: The counterpart to `cap_exhaustion_is_logged_at_warn_level`, and
/// the more consequential of the two lines: this one is TERMINAL — it is
/// the moment the PM loop is stopped and the run ends, so it is the single
/// log an operator most needs in stderr. Its count must also match what the
/// report tells them (#2852's accurate-diagnosis goal); a log and a report
/// disagreeing by one is exactly the confusion this fix removes.
/// What: Drive the ceiling under a capturing subscriber; assert a WARN
/// event names the ceiling and quotes `MAX_FAILED_INVOCATIONS` failures.
/// See `cap_exhaustion_is_logged_at_warn_level` for why
/// `rebuild_interest_cache` is load-bearing rather than defensive.
/// Test: this test.
#[tokio::test]
async fn failure_ceiling_is_logged_at_warn_level() {
    use tracing_subscriber::layer::SubscriberExt;

    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(logs.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();

    let outcomes = (0..MAX_FAILED_INVOCATIONS * 2)
        .map(|_| Err(llm_error()))
        .collect();
    let signal = RedelegationCapSignal::new();
    let runner = RedelegatingRunner::new(
        Arc::new(ScriptedInner::new(outcomes)) as Arc<dyn AgentRunner>,
        signal.clone(),
        PathBuf::from("/tmp/project"),
    );
    for _ in 0..(MAX_FAILED_INVOCATIONS / MAX_REDELEGATIONS) + 1 {
        let _ = runner.run("python-engineer", "build the package").await;
    }

    assert!(signal.is_cap_reached(), "precondition: ceiling must latch");
    let captured = logs.0.lock().expect("captured logs lock");
    let warned = captured.iter().any(|(level, msg)| {
        *level == tracing::Level::WARN && msg.contains("failure ceiling reached")
    });
    assert!(
        warned,
        "reaching the terminal ceiling must WARN — it ends the run, so silence here \
             makes a total-loss run undiagnosable. Captured: {captured:?}"
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
