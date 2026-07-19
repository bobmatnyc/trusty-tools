//! Log-level regression guards for the #2857 silent-control-flow
//! instrumentation.
//!
//! Why: #2852's re-delegation cap killed an entire run while emitting nothing
//! at any level, and it was only diagnosed by cross-run forensic comparison of
//! delegation indices. The property this crate's log-level convention (see
//! [`crate::logging`]) buys us is that such a decision is diagnosable from ONE
//! run's stderr — and that property is only real if it is asserted. These
//! tests therefore pin the LEVEL, not merely the presence, of the loop's
//! outcome-changing decisions: a `warn` that silently decays to `debug`, or an
//! `info` that creeps up to `warn`, both defeat the convention and both are
//! caught here. Split into this focused child module (from `agent_loop::tests`)
//! to keep the parent test file under its SLOC cap, reusing its scripted-LLM
//! harness verbatim via `use super::*` — the `gate_intercept` precedent.
//! What: A level-generic [`CaptureLayer`] taps events into a shared buffer;
//! each test drives the loop into exactly one decision and asserts the
//! expected level fired with the expected substance — plus, for the
//! level-discipline cases, that a LOUDER level did not.
//! Test: this module is itself the test surface.

use std::sync::OnceLock;

use super::*;
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt as _;

/// One captured tracing event: its level and its formatted `message` field.
type Captured = (Level, String);

/// A layer that keeps EVERY callsite's `Interest` permissive but records
/// nothing — installed once as the process-global default.
///
/// Why: this is the crux of the anti-flake fix, and it is load-bearing.
/// `tracing` caches per-callsite `Interest` **globally**; a callsite first
/// evaluated while no permissive subscriber is active is cached `never()` and
/// compiled out for the rest of the process, so a later thread-local capture
/// subscriber never sees it. That is exactly what made the earlier
/// `set_default` + `rebuild_interest_cache()` approach still flaky (measured
/// 2/12) for the #2279 verify-gate `warn!` — a callsite the sibling
/// `gate_intercept` tests hit first, with no capture active. By registering a
/// permissive subscriber as the GLOBAL default (`register_callsite` →
/// `always()`, `max_level_hint` → `TRACE`), no callsite is ever cached
/// `never`, so the per-test thread-local [`CaptureLayer`] below can always
/// observe events regardless of test order. It records nothing itself; capture
/// is the thread-local layer's job.
/// What: interest-only; `on_event` is intentionally absent (the default is a
/// no-op).
/// Test: this module (its effect is that the capture assertions are
/// order-independent).
struct InterestKeeper;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for InterestKeeper {
    fn register_callsite(
        &self,
        _meta: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }
}

/// The per-test thread-local capture layer.
///
/// Why: `#[tokio::test]` uses the current-thread runtime, so a `set_default`
/// guard held across `.await` points captures everything the awaited future
/// emits on the test's own thread — the parent module's `install_error_capture`
/// establishes this pattern, and running per-thread keeps parallel tests
/// isolated. Distinguishing `warn` from `info` is the whole point, so it
/// records the level alongside the message.
/// What: `on_event` visits the `message` field and pushes `(level, message)`
/// onto the shared buffer.
/// Test: this module.
struct CaptureLayer {
    events: Arc<Mutex<Vec<Captured>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct MessageVisitor(String);
        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        // `expect` is deliberate: a poisoned lock means another test thread
        // panicked, which must fail loudly rather than silently drop the very
        // events under assertion.
        self.events
            .lock()
            .expect("capture lock")
            .push((*event.metadata().level(), visitor.0));
    }
}

/// Installs the permissive global exactly once per test binary.
static GLOBAL_INIT: OnceLock<()> = OnceLock::new();

/// Begin capturing this thread's tracing events into a fresh buffer.
///
/// Why: two mechanisms, both load-bearing. (1) The one-time [`InterestKeeper`]
/// global guarantees no callsite is ever cached `never`, making capture
/// order-independent — see its docs. (2) The per-test `set_default`
/// [`CaptureLayer`] receives this test's events on its own thread. The
/// `rebuild_interest_cache()` after installing the global re-evaluates any
/// callsite a sibling test already cached `never` (e.g. the verify-gate `warn!`
/// hit first by `gate_intercept`) against the now-permissive global.
/// What: On first call, installs the permissive global (tolerating the race
/// with `logging::init_tracing_for_test`, whose post-#2857 `info` filter is
/// itself permissive for `warn`/`info`) and rebuilds the interest cache. Every
/// call sets this thread's default subscriber to a fresh `CaptureLayer` and
/// returns `(guard, buffer)`; dropping the guard restores the prior subscriber.
/// Test: used by every test in this module.
fn install_capture() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<Captured>>>) {
    GLOBAL_INIT.get_or_init(|| {
        let global = tracing_subscriber::registry().with(InterestKeeper);
        let _ = tracing::subscriber::set_global_default(global);
        tracing::callsite::rebuild_interest_cache();
    });

    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer {
        events: events.clone(),
    });
    let guard = tracing::subscriber::set_default(subscriber);
    (guard, events)
}

/// Every captured message at exactly `level`.
fn at_level(events: &Arc<Mutex<Vec<Captured>>>, level: Level) -> Vec<String> {
    events
        .lock()
        .expect("capture lock")
        .iter()
        .filter(|(l, _)| *l == level)
        .map(|(_, m)| m.clone())
        .collect()
}

/// Assert some captured message at `level` contains every needle.
fn assert_logged(events: &Arc<Mutex<Vec<Captured>>>, level: Level, needles: &[&str]) {
    let msgs = at_level(events, level);
    assert!(
        msgs.iter().any(|m| needles.iter().all(|n| m.contains(n))),
        "expected a {level} log containing all of {needles:?}, but {level} messages were: {msgs:#?}"
    );
}

/// **The #2852-class acceptance test:** a cap that kills a run must be
/// diagnosable from ONE run's stderr.
///
/// Why: This is the exact shape that made #2852 cost a forensic
/// investigation — a harness cap silently ended a run and the only evidence
/// was the outcome itself, so diagnosis required comparing runs against each
/// other. A `warn` naming the cap, its value, and its consequence is what
/// turns that investigation into reading one line.
/// What: Scripts a model that never stops calling tools, so `max_turns` is the
/// only way out; asserts `TurnCapExceeded` AND a WARN naming the cap, the
/// numeric budget, and that a partial transcript is what the run returns.
/// Test: this test.
#[tokio::test]
async fn turn_cap_exhaustion_warns_naming_the_cap_and_consequence() {
    let (_guard, events) = install_capture();

    let fixtures: Vec<Value> = (0..5)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    let agent = make_loop(
        Arc::new(ScriptedLlm::from_json(&fixtures)),
        registry_with_echo(false),
        AgentLoopConfig {
            max_turns: 3,
            ..AgentLoopConfig::default()
        },
    );

    let err = agent
        .run("sys", "task")
        .await
        .expect_err("must abort on the turn cap");
    assert!(matches!(err, AgentLoopError::TurnCapExceeded { .. }));

    drop(_guard);
    assert_logged(
        &events,
        Level::WARN,
        &["turn cap exhausted", "3", "partial transcript"],
    );
}

/// The stop signal killing the PM loop warns — the *consumer* half of #2852.
///
/// Why: #2852's cap latched a signal that stopped the PM loop, and NEITHER the
/// cause nor the effect was logged. The cause is instrumented at the cap site
/// (`run_task::redelegation`); this asserts the effect — the loop actually
/// dying, and how much of its budget died with it — is independently visible,
/// because whoever latches the signal is not always who explains it.
/// What: Attaches an always-true stop signal; asserts `StoppedBySignal` and a
/// WARN naming the signal and the PM loop stopping.
/// Test: this test.
#[tokio::test]
async fn stop_signal_abort_warns_that_the_pm_loop_is_stopping() {
    let (_guard, events) = install_capture();

    let agent = make_loop(
        Arc::new(ScriptedLlm::from_json(&[stop_response("unreachable")])),
        registry_with_echo(false),
        AgentLoopConfig::default(),
    )
    .with_stop_signal(Arc::new(|| true));

    let err = agent
        .run("sys", "task")
        .await
        .expect_err("must abort as stopped-by-signal");
    assert!(matches!(err, AgentLoopError::StoppedBySignal { .. }));

    drop(_guard);
    assert_logged(
        &events,
        Level::WARN,
        &["external stop signal", "stopping the PM loop"],
    );
}

/// The verify-before-finish gate warns when it overrules a `finish_task`.
///
/// Why: The gate converts a model-reported success into another turn — the
/// highest-signal policy override in the loop. An operator watching a run take
/// extra turns after the model said "done" has no other way to learn the gate
/// caused it.
/// What: Scripts a premature `finish_task` against the real
/// `default_finish_gate` with a task naming a test command that never runs
/// (the parent module's `registry_with_finish_task_and_bash` harness); asserts
/// a WARN naming the gate and the downgrade.
/// Test: this test.
#[tokio::test]
async fn verify_gate_interception_warns() {
    let (_guard, events) = install_capture();

    let agent = make_loop(
        Arc::new(ScriptedLlm::from_json(&[
            finish_task_call_response(
                "call-premature",
                r#"{"status": "completed", "summary": "done (premature)"}"#,
            ),
            stop_response("after the gate"),
        ])),
        registry_with_finish_task_and_bash(),
        AgentLoopConfig::default(),
    )
    .with_finish_gate(crate::verify_gate::default_finish_gate());

    let _ = agent.run("sys", "run pytest tests/ -v and report").await;

    drop(_guard);
    assert_logged(
        &events,
        Level::WARN,
        &["verify-before-finish gate", "downgraded"],
    );
}

/// **Level discipline:** user-requested cancellation is `info`, never `warn`.
///
/// Why: The convention is only worth writing down if something enforces it.
/// `warn` means "the harness overrode you"; a cancellation is the user getting
/// precisely what they asked for. Promoting it to `warn` would be exactly the
/// "everything at warn is the same as nothing at warn" failure the convention
/// exists to prevent — so this test is the forcing function against that drift.
/// What: Cancels before the first turn; asserts an INFO naming the
/// cancellation AND that no WARN was emitted at all.
/// Test: this test.
#[tokio::test]
async fn cancellation_logs_at_info_and_never_warn() {
    let (_guard, events) = install_capture();

    let agent = make_loop(
        Arc::new(ScriptedLlm::from_json(&[stop_response("unreachable")])),
        registry_with_echo(false),
        AgentLoopConfig::default(),
    )
    .with_cancel_flag(Arc::new(AtomicBool::new(true)));

    let err = agent
        .run("sys", "task")
        .await
        .expect_err("must abort as cancelled");
    assert!(matches!(err, AgentLoopError::Cancelled { .. }));

    drop(_guard);
    assert_logged(&events, Level::INFO, &["cancelled"]);
    assert!(
        at_level(&events, Level::WARN).is_empty(),
        "cancellation is the user getting what they asked for and must not warn, but got: {:#?}",
        at_level(&events, Level::WARN)
    );
}
