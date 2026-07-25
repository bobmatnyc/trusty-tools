//! Loop-level integration tests for the #3867/#3868 compression-telemetry
//! instrumentation. Split out of `tests.rs` per the crate's `_tests.rs`
//! sibling-file convention (see `budget_tests.rs`/`cadence_tests.rs`
//! precedent) to keep that file under the 1500-SLOC test cap. Declared as a
//! CHILD of `agent_loop::tests` so it reuses that module's scripted-LLM/
//! echo-tool harness (`ScriptedLlm`, `make_loop`, `registry_with_echo`,
//! `aggressive_compaction`, …) via `use super::*` rather than duplicating it.
//!
//! Why: `telemetry_tests.rs` already proves the JSONL writer and the
//! percentage/alarm helpers work in isolation; these tests prove the OTHER
//! half — that `AgentLoop::maybe_compact_transcript`/`maybe_cadence_compress`
//! actually call them at the right turn boundary with the right values,
//! through a real (scripted) loop run, using
//! `AgentLoopConfig::telemetry_data_dir` (issue #3902) to point every write
//! at an isolated temp directory instead of the real `~/.trusty-code` — a
//! plain per-instance config field, not the raced `TCODE_TELEMETRY_DATA_DIR`
//! process-global env var these tests used before.

use std::io::Read as _;

use super::*;
use crate::agent_loop::telemetry;

use telemetry::test_temp_dir as temp_dir;

fn read_jsonl(dir: &std::path::Path) -> Vec<telemetry::CompressionEvent> {
    let path = telemetry::compression_log_path(dir);
    let Ok(mut f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s.lines()
        .map(|l| serde_json::from_str(l).expect("valid CompressionEvent JSONL line"))
        .collect()
}

/// A threshold-compaction fire under `cadence: Some(_)` writes BOTH the
/// existing `tracing::error!` alarm AND a durable `compaction_event: true`
/// JSONL line, plus increments the Slice B lifetime alarm counter — pinning
/// the exact acceptance criterion issue #3868 names: "a threshold-compaction
/// event produces BOTH the ERROR log and the durable JSONL line."
///
/// Why: `agent_loop::tests::cadence::forced_degradation_increments_counter_and_logs_error`
/// pins the ERROR-log half of this exact scenario; this test is the
/// durability half — the JSONL record and the alarm log must exist on disk,
/// independent of whether anyone was tailing stderr.
/// What: runs a `DailyDriver` loop with an aggressive `CompactionConfig`
/// (guarantees a threshold fire) AND `cadence: Some(_)` (the regression
/// case), against an isolated `with_data_dir_env_fut` temp dir. Asserts at
/// least one `tcode-threshold` JSONL line with `compaction_event: true`, and
/// `lifetime_compaction_alarm_count >= 1`.
#[tokio::test]
async fn threshold_fire_under_cadence_writes_jsonl_and_alarm() {
    let dir = temp_dir("threshold-cadence-some");
    let mut fixtures: Vec<Value> = (0..5)
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
            cadence: Some(CadenceConfig {
                // A huge cadence period + generous overhead fraction means
                // cadence itself never fires, isolating this test to the
                // threshold compactor's own behaviour while still exercising
                // the `cadence.is_some()` branch that gates the ERROR log +
                // alarm line.
                cadence_turns: 1_000_000,
                max_overhead_fraction_pct: 100,
            }),
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the original task")
        .await
        .expect("run completes");

    let events = read_jsonl(&dir);
    let threshold_events: Vec<_> = events
        .iter()
        .filter(|e| e.surface == telemetry::SURFACE_TCODE_THRESHOLD)
        .collect();
    assert!(
        !threshold_events.is_empty(),
        "expected at least one tcode-threshold JSONL line, got {events:?}"
    );
    for e in &threshold_events {
        assert!(
            e.compaction_event,
            "every tcode-threshold record must have compaction_event: true: {e:?}"
        );
        // Note: unlike the cadence surface, a single threshold fire is not
        // guaranteed to shrink the transcript below its PRE-fire size — the
        // measurement here includes the turn just appended before this call,
        // so `ratio` can legitimately exceed 1.0 on an aggressive-compaction
        // test fixture. The invariant this test actually pins is
        // `compaction_event: true` + the alarm line below, per issue #3868's
        // acceptance criterion ("BOTH the ERROR log and the durable JSONL
        // line").
        let expected_ratio = if e.tokens_before == 0 {
            0.0
        } else {
            e.tokens_after as f64 / e.tokens_before as f64
        };
        assert!((e.ratio - expected_ratio).abs() < f64::EPSILON, "{e:?}");
    }

    assert!(
        telemetry::lifetime_compaction_alarm_count(&dir) >= 1,
        "a threshold fire under cadence: Some(_) must record the durable alarm line"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The SAME threshold-fire scenario, but with `cadence: None` — the
/// regression guard for issue #3868's "do not conflate the two" acceptance
/// criterion: the JSONL record must still appear (Slice A is unconditional),
/// but the alarm counter must stay at 0 (Slice B's alarm is
/// `cadence: Some(_)`-only).
#[tokio::test]
async fn threshold_fire_under_no_cadence_writes_jsonl_but_no_alarm() {
    let dir = temp_dir("threshold-cadence-none");
    let mut fixtures: Vec<Value> = (0..5)
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
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the original task")
        .await
        .expect("run completes");

    let events = read_jsonl(&dir);
    let threshold_events: Vec<_> = events
        .iter()
        .filter(|e| e.surface == telemetry::SURFACE_TCODE_THRESHOLD)
        .collect();
    assert!(
        !threshold_events.is_empty(),
        "cadence: None still uses threshold compaction as its PRIMARY mechanism \
         and must still be counted: {events:?}"
    );
    for e in &threshold_events {
        assert!(e.compaction_event, "still true: {e:?}");
    }

    assert_eq!(
        telemetry::lifetime_compaction_alarm_count(&dir),
        0,
        "cadence: None must never record the alarm line — it is the EXPECTED \
         primary mechanism there, not a regression"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A cadence fire (`outcome.rounds > 0`) writes a `tcode-cadence` JSONL
/// record with `tokens_before >= tokens_after` and a matching `ratio`.
#[tokio::test]
async fn cadence_fire_writes_compression_telemetry() {
    let dir = temp_dir("cadence-fire");
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
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                ..CadenceConfig::default()
            }),
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the task")
        .await
        .expect("run completes");

    let events = read_jsonl(&dir);
    let cadence_events: Vec<_> = events
        .iter()
        .filter(|e| e.surface == telemetry::SURFACE_TCODE_CADENCE)
        .collect();
    assert!(
        !cadence_events.is_empty(),
        "cadence_turns: 1 must fire on every turn, got {events:?}"
    );
    for e in &cadence_events {
        assert!(
            !e.compaction_event,
            "cadence is never the alarm signal: {e:?}"
        );
        assert!(e.tokens_before >= e.tokens_after, "{e:?}");
        let expected_ratio = if e.tokens_before == 0 {
            0.0
        } else {
            e.tokens_after as f64 / e.tokens_before as f64
        };
        assert!((e.ratio - expected_ratio).abs() < f64::EPSILON, "{e:?}");
        assert!(e.working_context_pct_after.is_some(), "{e:?}");
        assert!(e.overhead_pct_after.is_some(), "{e:?}");
    }

    assert_eq!(
        telemetry::lifetime_compaction_alarm_count(&dir),
        0,
        "a cadence fire must never touch the threshold-only alarm counter"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A cadence-DISABLED loop (`cadence: None`, the delegated engineer's
/// default) must never write a `tcode-cadence` JSONL record.
#[tokio::test]
async fn cadence_disabled_writes_no_cadence_telemetry() {
    let dir = temp_dir("cadence-disabled");
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
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the task")
        .await
        .expect("run completes");

    let events = read_jsonl(&dir);
    assert!(
        events
            .iter()
            .all(|e| e.surface != telemetry::SURFACE_TCODE_CADENCE),
        "cadence: None must emit no tcode-cadence records: {events:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An unwritable telemetry data directory must never fail the loop itself
/// (issue #3867 acceptance criteria: "a broken/unwritable log directory
/// must not fail the compression/summarization operation itself"). This is
/// the LOOP-level contract — `telemetry_tests.rs` already proves the
/// writer function alone doesn't panic; this proves `AgentLoop::run` still
/// returns `Ok` with a normal compaction/cadence outcome when telemetry
/// emission can't land anywhere.
///
/// Why: `/dev/null` is a character device, never a directory, on every
/// platform this crate targets — appending a path segment under it makes
/// EVERY `create_dir_all`/`open` call underneath fail with `ENOTDIR`,
/// simulating "every telemetry write fails" without touching real
/// filesystem permissions bits (unreliable to assert portably in CI).
/// What: runs the SAME aggressive-compaction + cadence:Some(_) scenario as
/// `threshold_fire_under_cadence_writes_jsonl_and_alarm`, but injects
/// `AgentLoopConfig::telemetry_data_dir` pointing at an unwritable path.
/// Asserts `agent.run` still completes `Ok`, and that the loop's own state
/// (turn count via `llm.calls()`) reflects a normal run — the
/// compaction/cadence outcome itself must be unaffected by telemetry's
/// failure.
#[tokio::test]
async fn unwritable_data_dir_does_not_fail_the_loop() {
    let bogus = std::path::PathBuf::from("/dev/null/not-a-real-dir-segment");
    let mut fixtures: Vec<Value> = (0..5)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                ..CadenceConfig::default()
            }),
            telemetry_data_dir: Some(bogus),
            ..AgentLoopConfig::default()
        },
    );

    let output = agent
        .run("system prompt", "the original task")
        .await
        .expect("an unwritable telemetry dir must not fail the run");

    assert!(
        !output.content.is_empty() || output.summary.is_some(),
        "the loop must still produce a normal completed output: {output:?}"
    );
    assert_eq!(
        llm.calls(),
        6,
        "the loop must still run its full scripted turn sequence, unaffected \
         by telemetry write failures"
    );
}

// ── #3911: cadence floor-breach backstop ────────────────────────────────────

/// A genuine cadence-level 60%-floor breach — `cadence::enforce_budget`
/// exhausts every eligible entry (the huge seeded task alone, which is
/// `user`-role and therefore NEVER compacted, exceeds a deliberately tiny
/// cap) — now writes the durable `tcode-cadence-floor-breach` JSONL record
/// AND the alarm log, AND emits an `ERROR`-level log, end to end through
/// `AgentLoop::maybe_cadence_compress`. Mirrors
/// `agent_loop::tests::cadence::forced_degradation_increments_counter_and_logs_error`'s
/// shape (real loop run, not a bare `cadence::maybe_cadence_compress` unit
/// call) for the SAME reason: this is the acceptance criterion that the
/// signal fires through the actual turn boundary, not just at the pure
/// `cadence` module level (`cadence_tests::floor_exceeded_warns_not_panics`
/// already covers that unit).
///
/// Why: before #3911, this exact scenario — proven by the load-realistic
/// soak (epic #3866, PR #3909) to occur under heavy real-world load
/// (floor 48%) — produced ZERO durable signal anywhere:
/// `lifetime_compaction_alarm_count` stayed 0 (that alarm is keyed to the
/// mechanically-independent threshold compactor, which this scenario never
/// crosses — see `telemetry::record_cadence_floor_breach`'s docs), and the
/// only trace was an in-process `tracing::warn!` inside
/// `cadence::enforce_budget`. This test pins the fix.
#[tokio::test]
async fn cadence_floor_breach_writes_alarm_and_error_log() {
    crate::test_support::begin_capture();

    let dir = telemetry::test_temp_dir("floor-breach");
    let fixtures: Vec<Value> = vec![stop_response("all done")];
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 1, // trivially tiny cap
            }),
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );
    // A huge `user`-role seed: never eligible for compaction (system/user
    // exemption), so it alone blows the tiny cap above no matter how hard
    // `enforce_budget` shrinks the active zone — the documented,
    // structural floor this backstop is about, not a tuning gap.
    let mut transcript = Transcript::seed("system prompt", &"x".repeat(200_000));

    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes even through a floor breach");

    let messages = crate::test_support::captured_at_least(tracing::Level::ERROR);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("floor breached") && m.contains("cadence")),
        "expected an error-level floor-breach log, got: {messages:?}"
    );

    let events = read_jsonl(&dir);
    let breach_events: Vec<_> = events
        .iter()
        .filter(|e| e.surface == telemetry::SURFACE_TCODE_CADENCE_FLOOR_BREACH)
        .collect();
    assert!(
        !breach_events.is_empty(),
        "expected at least one tcode-cadence-floor-breach JSONL line, got {events:?}"
    );
    for e in &breach_events {
        assert!(
            e.compaction_event,
            "a floor breach is alarm-worthy: compaction_event must be true: {e:?}"
        );
    }

    assert!(
        telemetry::lifetime_cadence_floor_breach_count(&dir) >= 1,
        "a cadence floor breach must record the durable alarm line"
    );
    assert_eq!(
        telemetry::lifetime_compaction_alarm_count(&dir),
        0,
        "the cadence floor-breach alarm must be a DISTINCT counter from the \
         threshold-under-cadence alarm — this scenario never touches the \
         threshold compactor at all"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The regression guard: a normal run that stays within budget the whole
/// time must never write the floor-breach alarm or JSONL surface.
#[tokio::test]
async fn cadence_within_budget_never_writes_floor_breach_alarm() {
    let dir = telemetry::test_temp_dir("floor-no-breach");
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
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                ..CadenceConfig::default()
            }),
            telemetry_data_dir: Some(dir.clone()),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "a small task")
        .await
        .expect("run completes");

    let events = read_jsonl(&dir);
    assert!(
        events
            .iter()
            .all(|e| e.surface != telemetry::SURFACE_TCODE_CADENCE_FLOOR_BREACH),
        "a healthy run must never write the floor-breach surface: {events:?}"
    );
    assert_eq!(
        telemetry::lifetime_cadence_floor_breach_count(&dir),
        0,
        "a healthy run must never increment the floor-breach alarm"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// (#3925) Standing regression guard for the #3902 race CLASS, not just the
/// five originally-affected call sites PR #3920 fixed.
///
/// Why: PR #3920's fix was verified only by the author's local 40x loop
/// (2/40 failures pre-fix, 0/40 post-fix) — a manual process that lives
/// nowhere in CI. At a ~5% per-run failure rate, an ordinary single-run
/// `cargo test` has a good chance of missing a REINTRODUCTION of the same
/// omission before it lands, exactly how #3902 reached `main` via PR #3896
/// in the first place: a sixth loop-integration test could trigger a real
/// cadence/threshold fire without injecting `telemetry_data_dir` and pass
/// CI outright (the `#[cfg(test)]` guard in
/// `AgentLoop::telemetry_data_dir()` only catches the "forgot to inject
/// AND env var unset" case deterministically — it does NOT prove the
/// injected path stays correct under concurrent load). Rather than a
/// brute-force `#[ignore]`d N-iteration loop (only catches the historical
/// rate probabilistically, and only if it's actually run), this test
/// DETERMINISTICALLY manufactures the exact attack: a background task
/// rewrites `TCODE_TELEMETRY_DATA_DIR` in a tight loop with NO
/// synchronization per-write — exactly what an un-injected caller does by
/// simply never touching `telemetry::DATA_DIR_ENV_LOCK` — for the FULL
/// duration of four concurrent `AgentLoop` runs that all correctly inject
/// `telemetry_data_dir`. Because an injected loop's
/// `AgentLoop::telemetry_data_dir()` never consults the env var at all,
/// every run below MUST see exactly its own cadence records no matter how
/// aggressively the global churns concurrently; if a future change makes
/// that resolution fall back to the env var again for an injected config,
/// this test starts failing on the FIRST run, deterministically, rather
/// than ~5% of the time. Mirrors the identical pattern applied to
/// `trusty-agents`' `EventStore`/`$HOME` race (issue #3922) — see that
/// crate's `listeners::store::tests::concurrent_event_store_scenarios_survive_home_env_hammering`;
/// kept as two separate, crate-local tests rather than one shared harness
/// since the two crates have no shared test-support crate today and the
/// domain types (`AgentLoopConfig` vs. a bare directory parameter) differ
/// enough that a literal shared abstraction would be more machinery than
/// the ~40 lines it would save.
/// What: Holds `telemetry::DATA_DIR_ENV_LOCK` for this test's ENTIRE body
/// (same convention `session::protocol_budget`'s tests use) so the
/// hammering below can never leak out and destabilize any OTHER
/// lock-respecting test in this crate — the chaos is fully internal to
/// this test's own scope. Spawns a background task that rewrites
/// `TCODE_TELEMETRY_DATA_DIR` on every `tokio::task::yield_now` while four
/// scripted loops (each `cadence: Some(cadence_turns: 1)`, guaranteeing a
/// fire every turn) run concurrently via `tokio::join!`, each pointed at
/// its own tempdir via `telemetry_data_dir`. Asserts each loop's own
/// directory contains at least one `tcode-cadence` record — nothing lost
/// to the concurrently-churning global.
/// Test: itself. Validated to fail deterministically (not probabilistically)
/// when one scenario's `telemetry_data_dir` injection is reverted to `None`
/// (falling back to the raced env-var path) — see PR discussion for the
/// before/after failure output.
#[tokio::test]
async fn concurrent_agent_loops_survive_data_dir_env_hammering() {
    let _guard = telemetry::DATA_DIR_ENV_LOCK.lock().await;
    let prev = std::env::var_os(telemetry::DATA_DIR_ENV_VAR);
    let stop = Arc::new(AtomicBool::new(false));

    let hammer_stop = stop.clone();
    let hammer = tokio::spawn(async move {
        let mut i: u64 = 0;
        while !hammer_stop.load(Ordering::Relaxed) {
            i += 1;
            // SAFETY: intentionally UNSYNCHRONIZED per-write — this
            // reproduces the exact #3902 attack shape (a caller that never
            // takes `DATA_DIR_ENV_LOCK`). Holding the lock for this whole
            // test (above) keeps the hammering from leaking into any OTHER
            // test; the injected scenarios below must survive it regardless.
            unsafe {
                std::env::set_var(
                    telemetry::DATA_DIR_ENV_VAR,
                    format!("/tmp/tcode-hammer-{i}"),
                );
            }
            tokio::task::yield_now().await;
        }
    });

    async fn scenario(
        label: &'static str,
    ) -> (std::path::PathBuf, Vec<telemetry::CompressionEvent>) {
        let dir = temp_dir(label);
        let mut fixtures: Vec<Value> = (0..4)
            .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
            .collect();
        fixtures.push(stop_response("all done"));
        let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
        let registry = registry_with_echo(false);

        let agent = make_loop(
            llm,
            registry,
            AgentLoopConfig {
                cadence: Some(CadenceConfig {
                    cadence_turns: 1,
                    ..CadenceConfig::default()
                }),
                telemetry_data_dir: Some(dir.clone()),
                ..AgentLoopConfig::default()
            },
        );
        agent
            .run("system prompt", "the task")
            .await
            .expect("run completes");
        let events = read_jsonl(&dir);
        (dir, events)
    }

    let (a, b, c, d) = tokio::join!(
        scenario("hammer-a"),
        scenario("hammer-b"),
        scenario("hammer-c"),
        scenario("hammer-d"),
    );

    stop.store(true, Ordering::Relaxed);
    hammer.await.unwrap();

    // SAFETY: still holding DATA_DIR_ENV_LOCK; restoring pre-test value
    // before the guard drops at function end.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(telemetry::DATA_DIR_ENV_VAR, v),
            None => std::env::remove_var(telemetry::DATA_DIR_ENV_VAR),
        }
    }

    for (label, (dir, events)) in [("a", a), ("b", b), ("c", c), ("d", d)] {
        let cadence_events: Vec<_> = events
            .iter()
            .filter(|e| e.surface == telemetry::SURFACE_TCODE_CADENCE)
            .collect();
        assert!(
            !cadence_events.is_empty(),
            "scenario {label} lost its own cadence records under \
             TCODE_TELEMETRY_DATA_DIR hammering: {events:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
