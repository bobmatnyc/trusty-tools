//! Unit tests for the #2346 cadence compressor. Split out of `cadence.rs`
//! per the crate's `_tests.rs` sibling-file convention (see
//! `transcript_tests.rs` precedent) to keep the production file under the
//! 500-SLOC cap.

use super::super::CompactionConfig;
use super::*;
use crate::llm::{ChatMessage, FunctionCall, ToolCall};

/// Serializes tests that mutate the process-wide cadence env vars, mirroring
/// `crate::mode::MODE_ENV_LOCK`'s identical rationale.
static CADENCE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn with_cadence_env<T>(turns: Option<&str>, pct: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard = CADENCE_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialized by `CADENCE_ENV_LOCK`.
    unsafe {
        match turns {
            Some(v) => std::env::set_var(CADENCE_TURNS_ENV_VAR, v),
            None => std::env::remove_var(CADENCE_TURNS_ENV_VAR),
        }
        match pct {
            Some(v) => std::env::set_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR, v),
            None => std::env::remove_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR),
        }
    }
    let result = f();
    unsafe {
        std::env::remove_var(CADENCE_TURNS_ENV_VAR);
        std::env::remove_var(CADENCE_OVERHEAD_FRACTION_ENV_VAR);
    }
    result
}

fn project_with_settings(json: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    if let Some(json) = json {
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("settings.json"), json).expect("write settings.json");
    }
    tmp
}

/// Append one turn (an assistant tool-call + its tool result) whose
/// estimated token size is `~token_size` (via `estimate_tokens`'s chars/4
/// heuristic applied to the tool-call argument body — mirrors #2261's
/// tool-call-argument accounting).
fn push_sized_turn(t: &mut Transcript, idx: usize, token_size: usize) {
    let arguments = format!(r#"{{"content":"{}"}}"#, "x".repeat(token_size * 4));
    t.push_assistant(
        None,
        &[ToolCall {
            id: format!("call_{idx}"),
            kind: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments,
            },
        }],
    );
    t.push_tool_result(&format!("call_{idx}"), "write_file", "ok");
}

#[test]
fn default_is_sane() {
    let cfg = CadenceConfig::default();
    assert_eq!(cfg.cadence_turns, DEFAULT_CADENCE_TURNS);
    assert_eq!(
        cfg.max_overhead_fraction_pct,
        DEFAULT_MAX_OVERHEAD_FRACTION_PCT
    );
}

/// The epic's stated arithmetic: a 200K window at 40% overhead -> an 80K cap.
#[test]
fn overhead_cap_matches_epic_arithmetic() {
    let cfg = CadenceConfig::default();
    assert_eq!(cfg.overhead_cap_tokens(200_000), 80_000);
}

#[test]
fn read_settings_json_cadence_missing_file_is_not_an_error() {
    let project = project_with_settings(None);
    assert_eq!(read_settings_json_cadence(project.path()), None);
}

#[tokio::test]
async fn resolve_cadence_config_settings_json_override() {
    let project = project_with_settings(Some(
        r#"{"code_harness": {"cadence_turns": 3, "max_overhead_fraction_pct": 25}}"#,
    ));
    with_cadence_env(None, None, || {
        let cfg = resolve_cadence_config(project.path());
        assert_eq!(cfg.cadence_turns, 3);
        assert_eq!(cfg.max_overhead_fraction_pct, 25);
    })
    .await;
}

#[tokio::test]
async fn resolve_cadence_config_env_wins_over_settings_json() {
    let project = project_with_settings(Some(
        r#"{"code_harness": {"cadence_turns": 3, "max_overhead_fraction_pct": 25}}"#,
    ));
    with_cadence_env(Some("5"), Some("50"), || {
        let cfg = resolve_cadence_config(project.path());
        assert_eq!(cfg.cadence_turns, 5);
        assert_eq!(cfg.max_overhead_fraction_pct, 50);
    })
    .await;
}

/// Cadence fires exactly every N turns (N=3 override) — not on any other
/// turn.
#[test]
fn fires_every_n_turns() {
    let mut t = Transcript::seed("s", "task");
    // A generous cap so only the SCHEDULED cadence fire (never the
    // continuous-enforcement loop) drives `cadence_fire_count` in this test.
    let cfg = CadenceConfig {
        cadence_turns: 3,
        max_overhead_fraction_pct: 99,
    };
    let context_window = 1_000_000;

    // keep_last_messages=1: small enough that every scheduled fire always
    // has fresh, not-yet-compacted entries to act on (each turn adds 2
    // entries), so the schedule (`outcome.fired`) and the "did work happen"
    // counter (`cadence_fire_count`) agree in this test. A larger active
    // zone can leave a scheduled fire with nothing new to compact yet (e.g.
    // right when the transcript has grown to exactly fill the zone) — a
    // real, correct distinction between "scheduled" and "did work", not a
    // bug; this test isolates the schedule itself.
    for turn in 1..=9 {
        push_sized_turn(&mut t, turn, 10);
        let outcome = maybe_cadence_compress(&mut t, &cfg, 1, context_window);
        assert_eq!(outcome.fired, turn % 3 == 0, "turn {turn} fired mismatch");
    }
    assert_eq!(t.cadence_fire_count(), 3);
    assert_eq!(t.cadence_turn_count(), 9);
}

/// The core #2346 acceptance test: a 100-turn run of 5K-token worst-case
/// turns never triggers the threshold compactor and never exceeds the
/// overhead cap on any turn.
#[test]
fn stays_under_budget_every_turn_and_threshold_never_fires() {
    let mut t = Transcript::seed("s", "task");
    let cadence_cfg = CadenceConfig::default(); // 8 turns, 40%
    let context_window = 200_000;
    let cap = cadence_cfg.overhead_cap_tokens(context_window);
    let compaction_cfg = CompactionConfig::for_context_window(context_window);
    let keep_last_messages = compaction_cfg.keep_last_messages;

    let mut compaction_events = 0usize;
    for turn in 1..=100 {
        push_sized_turn(&mut t, turn, 5_000);
        maybe_cadence_compress(&mut t, &cadence_cfg, keep_last_messages, context_window);

        // The threshold compactor is the backstop — cadence must keep the
        // transcript under its own (looser, 75%-of-window) trigger point on
        // every single turn.
        if t.maybe_compact(&compaction_cfg) {
            compaction_events += 1;
        }

        let estimate = estimate_total_tokens(&t.to_messages());
        assert!(
            estimate <= cap,
            "turn {turn}: estimate {estimate} exceeded overhead cap {cap}"
        );
    }

    assert_eq!(
        compaction_events, 0,
        "threshold compaction must never fire under cadence's continuous enforcement"
    );
}

/// A single pathologically large (60K-token) turn still ends under the
/// overhead cap once continuous enforcement runs, even though it falls
/// inside the protected active zone.
#[test]
fn single_oversized_turn_still_ends_under_budget() {
    let mut t = Transcript::seed("s", "task");
    for turn in 1..=5 {
        push_sized_turn(&mut t, turn, 500);
    }
    push_sized_turn(&mut t, 6, 60_000);

    let cfg = CadenceConfig {
        cadence_turns: 8, // not a scheduled-fire turn; enforcement alone must do the work
        max_overhead_fraction_pct: 40,
    };
    let context_window = 200_000;
    let cap = cfg.overhead_cap_tokens(context_window);

    let outcome = maybe_cadence_compress(&mut t, &cfg, 6, context_window);
    assert!(
        outcome.within_budget,
        "enforcement must bring a single oversized turn back under budget"
    );
    assert!(estimate_total_tokens(&t.to_messages()) <= cap);
}

/// When even full compaction of everything compactable cannot bring the
/// transcript under budget (the system/user preamble alone is oversized),
/// enforcement returns `within_budget: false` rather than panicking or
/// looping forever.
#[test]
fn floor_exceeded_warns_not_panics() {
    // A huge seeded user task: system/user entries are NEVER compacted, so
    // this alone exceeds a deliberately tiny cap.
    let mut t = Transcript::seed("s", &"x".repeat(40_000));
    let cfg = CadenceConfig {
        cadence_turns: 1,
        max_overhead_fraction_pct: 1,
    };
    let context_window = 1_000; // cap = 10 tokens — trivially exceeded by the seed alone.

    let outcome = maybe_cadence_compress(&mut t, &cfg, 6, context_window);
    assert!(!outcome.within_budget);
}

/// The synthesized summary for a cadence-compacted span references the
/// durable memory turn range.
#[test]
fn summary_references_turn_range() {
    let mut t = Transcript::seed("s", "task");
    for turn in 1..=10 {
        push_sized_turn(&mut t, turn, 100);
    }
    let cfg = CadenceConfig {
        cadence_turns: 1,
        max_overhead_fraction_pct: 99,
    };
    let outcome = maybe_cadence_compress(&mut t, &cfg, 2, 1_000_000);
    assert!(outcome.fired);

    let view = t.to_messages();
    let summary = view
        .iter()
        .find(|m| m.content.as_deref().unwrap_or("").contains("[compacted"))
        .expect("a compacted summary message");
    let text = summary.content.as_deref().unwrap_or_default();
    assert!(
        text.contains("session memory"),
        "summary must reference session memory: {text}"
    );
    assert!(
        text.contains("recall_session"),
        "summary must reference recall_session: {text}"
    );
    assert!(
        text.contains("turns 1-1"),
        "summary must reference the turn range: {text}"
    );
}

/// Adversarial case: a cadence-triggered cutoff landing mid multi-tool-call
/// group is pulled forward atomically instead of splitting it (#2278,
/// reused verbatim by the cadence path via `Transcript::compact_span_tagged`).
#[test]
fn cadence_boundary_never_splits_a_tool_call_group() {
    let mut t = Transcript::seed("s", "task");
    for i in 0..5 {
        t.push_assistant(Some(format!("turn {i}")), &[]);
        t.push_tool_result(&format!("c{i}"), "bash", "output");
    }
    t.push_assistant(
        None,
        &[
            ToolCall {
                id: "multi_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "multi_2".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_time".into(),
                    arguments: "{}".into(),
                },
            },
        ],
    );
    t.push_tool_result("multi_1", "get_weather", "72F");
    t.push_tool_result("multi_2", "get_time", "12:00 UTC");
    assert_eq!(t.messages().len(), 15);

    // keep_last_messages=2 lands the naive cutoff strictly between the
    // multi-call assistant entry (index 12) and its second answer (14) —
    // see `transcript::tests::maybe_compact_pulls_whole_multi_tool_call_group_forward`
    // for the exact index arithmetic this mirrors.
    let cfg = CadenceConfig {
        cadence_turns: 1,
        max_overhead_fraction_pct: 99,
    };
    let outcome = maybe_cadence_compress(&mut t, &cfg, 2, 1_000_000);
    assert!(outcome.fired);

    let view = t.to_messages();
    assert_view_pairing_intact(&view);
    assert!(
        view.iter()
            .any(|m| m.tool_calls.as_ref().is_some_and(|c| c.len() == 2)),
        "the multi-tool-call assistant entry must survive uncompacted: {view:?}"
    );
}

/// Same pairing-intact invariant `transcript_tests.rs` checks for the
/// threshold path — every `tool` entry's issuing assistant entry (and vice
/// versa) must be present in the same rendered view.
fn assert_view_pairing_intact(view: &[ChatMessage]) {
    let mut introduced: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in view {
        if let Some(calls) = &msg.tool_calls {
            for call in calls {
                introduced.insert(call.id.clone());
            }
        }
        if msg.role == "tool" {
            let id = msg.tool_call_id.clone().unwrap_or_default();
            assert!(
                introduced.contains(&id),
                "orphaned tool entry {id:?}: {view:?}"
            );
            answered.insert(id);
        }
    }
    for id in &introduced {
        assert!(
            answered.contains(id),
            "unanswered tool_calls id {id:?}: {view:?}"
        );
    }
}
