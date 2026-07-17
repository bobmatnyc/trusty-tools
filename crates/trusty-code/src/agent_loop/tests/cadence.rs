//! End-to-end agent-loop tests for the #2346 cadence compressor's
//! turn-boundary wiring and the #2349 threshold-compaction regression
//! signal, plus the #2857 observability guard proving a cadence fire is no
//! longer silently discarded.
//!
//! Why: `agent_loop::cadence::tests` proves the compressor's pure mechanics
//! in isolation; these tests prove the WIRING — that `AgentLoop`'s turn
//! boundary (`maybe_cadence_compress` / `maybe_compact_transcript`) actually
//! engages/stays inert per `HarnessMode` and the `cadence` config knob, and
//! that a forced degradation (threshold compaction firing when cadence was
//! supposed to keep the transcript under budget) is diagnosable. Split into
//! this focused child module (like the sibling `gate_intercept` and
//! `redundant_rerun`) to keep the parent test file under its SLOC cap while
//! reusing its scripted-LLM harness verbatim via `use super::*`. Log-content
//! assertions use `crate::test_support::begin_capture`/`captured_at_least`
//! (see that module's doc for why).
//! What: Covers cadence's mode/config gate matrix (off by default, inert
//! under `Parity`, fires under `DailyDriver` when configured), the #2857
//! INFO-level log on a cadence fire, and the #2349 forced-degradation
//! ERROR-level regression signal (with its `cadence: None` negative
//! control).
//! Test: this module is itself the test surface.

use super::*;

// ── #2346: cadence-compressor turn-boundary wiring ──────────────────────────

/// Cadence stays off by default (#2346): `AgentLoopConfig::default().cadence`
/// is `None`, and the loop never ticks the transcript's cadence counter when
/// it is `None` — the exact "zero behaviour change for run_task/delegated
/// engineer" contract this ticket requires.
///
/// Why: Every pre-#2346 call site (`run_task`, the delegated engineer's own
/// loop) constructs `AgentLoopConfig` via `..AgentLoopConfig::default()`;
/// this is the direct regression guard that doing so keeps cadence off.
/// What: Run a several-turn script under `mode: DailyDriver` (the default)
/// with `cadence: None` (the default) via `run_with_transcript` against an
/// externally-owned `Transcript`, then assert `cadence_turn_count() == 0`.
/// Test: this test.
#[tokio::test]
async fn cadence_disabled_by_default() {
    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(llm, registry, AgentLoopConfig::default());
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 0);
}

/// `Parity` never ticks cadence even when a `CadenceConfig` is explicitly
/// attached (#2346, mirrors `parity_mode_never_compacts_even_past_threshold`'s
/// mode gate for the threshold compactor).
///
/// Why: The parity-spec (D2) byte-identical guarantee must hold for cadence
/// exactly as it does for threshold compaction — an operator accidentally
/// wiring `cadence: Some(_)` onto a Parity run must not silently break
/// benchmark fairness.
/// What: Same script shape as `cadence_disabled_by_default`, but
/// `mode: Parity` with an aggressive `cadence: Some(CadenceConfig { cadence_turns: 1, .. })`
/// attached; assert `cadence_turn_count() == 0` regardless.
/// Test: this test.
#[tokio::test]
async fn cadence_never_fires_in_parity_mode() {
    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            mode: crate::mode::HarnessMode::Parity,
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 40,
            }),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 0);
}

/// `DailyDriver` mode with an explicit `CadenceConfig` attached ticks and
/// fires cadence compression at the turn boundary (#2346).
///
/// Why: The positive case completing the mode-gate matrix — cadence must
/// actually engage when both preconditions (`DailyDriver` + `Some(cadence)`)
/// hold, not just correctly stay inert in the negative cases above.
/// What: Scripts 6 tool-call round-trips then a stop (7 loop turns) under
/// `mode: DailyDriver` with `cadence_turns: 1` (fires every turn) and an
/// aggressive `compaction.keep_last_messages: 1` (small active zone, so
/// cadence has fresh entries to compact on nearly every turn). Asserts both
/// `cadence_turn_count() == 7` (one tick per loop turn) and
/// `cadence_fire_count() > 0` (at least one turn actually compacted
/// something).
/// Test: this test.
#[tokio::test]
async fn cadence_fires_in_daily_driver_when_configured() {
    let mut fixtures: Vec<Value> = (0..6)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: CompactionConfig {
                token_threshold: 1_000_000, // keep the THRESHOLD backstop inert for this test
                keep_last_messages: 1,
            },
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 99,
            }),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 7);
    assert!(
        transcript.cadence_fire_count() > 0,
        "expected at least one cadence fire to actually compact something"
    );
}

/// A cadence fire emits an INFO-level log naming the round count (#2857).
///
/// Why: `cadence::maybe_cadence_compress`'s `CadenceOutcome` was previously
/// discarded entirely at this call site — nothing observed whether the
/// #2346 mechanism keeping sessions alive was actually working. `info` (not
/// `warn`) because a scheduled cadence fire is routine, expected behaviour.
/// What: Same script/config as `cadence_fires_in_daily_driver_when_configured`,
/// captured via `crate::test_support::begin_capture`/`captured_at_least` at
/// `Level::INFO`; asserts at least one captured message reports the cadence
/// fire.
/// Test: this test.
#[tokio::test]
async fn cadence_fire_logs_info() {
    crate::test_support::begin_capture();

    let mut fixtures: Vec<Value> = (0..6)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: CompactionConfig {
                token_threshold: 1_000_000,
                keep_last_messages: 1,
            },
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 99,
            }),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    let messages = crate::test_support::captured_at_least(tracing::Level::INFO);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cadence compression fired")),
        "expected an info-level cadence-fire log, got: {messages:?}"
    );
}

// ── #2349: threshold compaction becomes a never-event regression signal ────

/// A `CadenceConfig` present (cadence enabled) but sized so it never
/// actually schedules a fire, forcing the aggressive threshold compactor to
/// trip anyway — the "cadence sizing assumptions violated" forced-
/// degradation scenario #2349's acceptance criteria call for.
fn overwhelmed_cadence() -> CadenceConfig {
    CadenceConfig {
        cadence_turns: 1_000_000, // never a scheduled-fire turn in this test
        max_overhead_fraction_pct: 99,
    }
}

/// Forced-degradation acceptance test: cadence is enabled but never actually
/// schedules a fire (`overwhelmed_cadence`), while `aggressive_compaction`'s
/// `token_threshold: 1` trips the #2308 threshold compactor on the very
/// first turn boundary. This must increment
/// `Transcript::compaction_events()` AND emit an ERROR-level log — the
/// "cadence sizing / daemon-availability / turn-size assumptions violated"
/// regression signal.
///
/// Why: This is #2349's core acceptance criterion — proving the signal
/// actually fires end-to-end through `AgentLoop::maybe_compact_transcript`,
/// not just at the `Transcript` unit level.
/// What: Runs a 3-tool-call-then-stop script under `mode: DailyDriver` with
/// both `compaction: aggressive_compaction()` and
/// `cadence: Some(overwhelmed_cadence())` attached. Asserts
/// `transcript.compaction_events() > 0` and that the captured ERROR-level
/// log messages contain the expected "regression signal" text.
/// Test: this test.
#[tokio::test]
async fn forced_degradation_increments_counter_and_logs_error() {
    crate::test_support::begin_capture();

    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            cadence: Some(overwhelmed_cadence()),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert!(
        transcript.compaction_events() > 0,
        "threshold compaction should have fired at least once under the tiny token_threshold"
    );
    let messages = crate::test_support::captured_at_least(tracing::Level::ERROR);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("regression signal") && m.contains("cadence")),
        "expected an error-level regression-signal log, got: {messages:?}"
    );
}

/// The cadence-`None` counterpart: threshold compaction firing is the
/// PRIMARY, EXPECTED mechanism for `run_task` one-shot / Parity / delegated
/// sub-agent contexts (all `cadence: None`) — it must NOT emit an
/// error-level log, and today's existing (no-log) behaviour at this call
/// site must stay exactly as it was before this ticket.
///
/// Why: §3 of the ticket's semantics is explicit that the error-log framing
/// applies ONLY when `cadence.is_some()` — this is the negative-case guard
/// against that gate regressing to "always log" or "log regardless of
/// cadence".
/// What: Identical script and `aggressive_compaction` to
/// `forced_degradation_increments_counter_and_logs_error`, but
/// `cadence: None` (the default). Asserts `compaction_events() > 0` (the
/// counter itself is NOT cadence-gated) while the captured ERROR log list is
/// empty.
/// Test: this test.
#[tokio::test]
async fn cadence_none_threshold_fire_does_not_log_error() {
    crate::test_support::begin_capture();

    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            cadence: None,
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert!(
        transcript.compaction_events() > 0,
        "the counter itself must still increment regardless of cadence"
    );
    let messages = crate::test_support::captured_at_least(tracing::Level::ERROR);
    assert!(
        messages.is_empty(),
        "cadence: None must never emit the error-level regression signal, got: {messages:?}"
    );
}
