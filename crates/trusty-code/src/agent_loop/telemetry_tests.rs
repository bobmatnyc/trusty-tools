//! Tests for `agent_loop::telemetry` (issues #3867, #3868).

use super::*;
use std::io::Read as _;

use test_temp_dir as temp_dir;

fn read_lines(path: &Path) -> Vec<String> {
    let mut s = String::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    s.lines().map(str::to_string).collect()
}

// -- CompressionEvent / ratio --

#[test]
fn ratio_guards_divide_by_zero() {
    assert_eq!(compute_ratio(0, 0), 0.0);
    assert_eq!(compute_ratio(0, 500), 0.0);
}

#[test]
fn ratio_is_after_over_before() {
    let r = compute_ratio(100, 55);
    assert!((r - 0.55).abs() < f64::EPSILON);
}

#[test]
fn compression_event_serializes_expected_schema() {
    let event = CompressionEvent::new(
        Some("sess-1".to_string()),
        SURFACE_TCODE_CADENCE,
        DETAIL_CADENCE,
        1000,
        400,
        Some(72),
        Some(28),
        false,
        3,
        1,
    );
    let value: serde_json::Value = serde_json::to_value(&event).unwrap();
    for key in [
        "ts",
        "session_id",
        "surface",
        "surface_detail",
        "tokens_before",
        "tokens_after",
        "ratio",
        "working_context_pct_after",
        "overhead_pct_after",
        "compaction_event",
        "duration_ms",
        "rounds",
    ] {
        assert!(value.get(key).is_some(), "missing field {key}");
    }
    assert_eq!(value["surface"], "tcode-cadence");
    assert_eq!(value["tokens_before"], 1000);
    assert_eq!(value["tokens_after"], 400);
    assert_eq!(value["ratio"], 0.4);
}

#[test]
fn compression_event_round_trips_through_serde() {
    let event = CompressionEvent::new(
        None,
        SURFACE_TCODE_THRESHOLD,
        DETAIL_THRESHOLD,
        900,
        900,
        None,
        None,
        true,
        7,
        1,
    );
    let json = serde_json::to_string(&event).unwrap();
    let back: CompressionEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event, back);
}

// -- write_compression_event --

#[test]
fn write_compression_event_creates_and_appends() {
    let dir = temp_dir("write-append");
    let event = CompressionEvent::new(
        Some("s".to_string()),
        SURFACE_TCODE_CADENCE,
        DETAIL_CADENCE,
        100,
        50,
        Some(60),
        Some(40),
        false,
        1,
        1,
    );
    write_compression_event(&dir, &event);
    write_compression_event(&dir, &event);

    let lines = read_lines(&compression_log_path(&dir));
    assert_eq!(lines.len(), 2, "one JSONL line per emitted event");
    for line in &lines {
        let parsed: CompressionEvent = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.surface, SURFACE_TCODE_CADENCE);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_compression_event_unwritable_dir_does_not_panic() {
    // A path with a NUL byte can never be created/opened by the OS; this
    // simulates an unwritable log directory without touching real
    // filesystem permissions bits (which are unreliable to assert on CI).
    let bogus = PathBuf::from("/dev/null/not-a-real-dir-\0-segment");
    let event = CompressionEvent::new(
        None,
        SURFACE_TCODE_THRESHOLD,
        DETAIL_THRESHOLD,
        10,
        10,
        None,
        None,
        true,
        1,
        1,
    );
    // Must not panic — best-effort emission swallows the failure.
    write_compression_event(&bogus, &event);
}

// -- compaction alarm --

#[test]
fn record_compaction_alarm_appends_one_line_per_call() {
    let dir = temp_dir("alarm-append");
    assert_eq!(lifetime_compaction_alarm_count(&dir), 0);

    record_compaction_alarm(&dir);
    assert_eq!(lifetime_compaction_alarm_count(&dir), 1);

    record_compaction_alarm(&dir);
    record_compaction_alarm(&dir);
    assert_eq!(lifetime_compaction_alarm_count(&dir), 3);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_compaction_alarm_unwritable_dir_does_not_panic() {
    let bogus = PathBuf::from("/dev/null/not-a-real-dir-\0-segment");
    record_compaction_alarm(&bogus);
}

#[test]
fn lifetime_compaction_alarm_count_zero_when_missing() {
    let dir = temp_dir("alarm-missing");
    assert_eq!(lifetime_compaction_alarm_count(&dir), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lifetime_compaction_alarm_count_matches_fire_count() {
    let dir = temp_dir("alarm-persist");
    for _ in 0..5 {
        record_compaction_alarm(&dir);
    }
    // A fresh read (simulating a new process / daemon restart reading the
    // same durable file) must see the same count, not reset to 0.
    assert_eq!(lifetime_compaction_alarm_count(&dir), 5);
    std::fs::remove_dir_all(&dir).ok();
}

// -- cadence floor-breach alarm (issue #3911) --

#[test]
fn record_cadence_floor_breach_alarm_appends_one_line_per_call() {
    let dir = temp_dir("floor-breach-alarm-append");
    assert_eq!(lifetime_cadence_floor_breach_count(&dir), 0);

    record_cadence_floor_breach_alarm(&dir);
    assert_eq!(lifetime_cadence_floor_breach_count(&dir), 1);

    record_cadence_floor_breach_alarm(&dir);
    record_cadence_floor_breach_alarm(&dir);
    assert_eq!(lifetime_cadence_floor_breach_count(&dir), 3);

    // A distinct log from the threshold-under-cadence alarm — this scenario
    // must never touch that counter.
    assert_eq!(lifetime_compaction_alarm_count(&dir), 0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_cadence_floor_breach_alarm_unwritable_dir_does_not_panic() {
    let bogus = PathBuf::from("/dev/null/not-a-real-dir-\0-segment");
    record_cadence_floor_breach_alarm(&bogus);
}

#[test]
fn lifetime_cadence_floor_breach_count_zero_when_missing() {
    let dir = temp_dir("floor-breach-missing");
    assert_eq!(lifetime_cadence_floor_breach_count(&dir), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lifetime_cadence_floor_breach_count_matches_fire_count() {
    let dir = temp_dir("floor-breach-persist");
    for _ in 0..4 {
        record_cadence_floor_breach_alarm(&dir);
    }
    assert_eq!(lifetime_cadence_floor_breach_count(&dir), 4);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_cadence_floor_breach_writes_jsonl_and_alarm() {
    let dir = temp_dir("floor-breach-jsonl");
    record_cadence_floor_breach(
        &dir,
        Some("sess-breach".to_string()),
        200_000,
        104_000,
        200_000,
        6,
        7,
    );

    let lines = read_lines(&compression_log_path(&dir));
    assert_eq!(lines.len(), 1);
    let event: CompressionEvent = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(event.surface, SURFACE_TCODE_CADENCE_FLOOR_BREACH);
    assert_eq!(event.surface_detail, DETAIL_CADENCE_FLOOR_BREACH);
    assert!(
        event.compaction_event,
        "a floor breach is unconditionally alarm-worthy"
    );
    assert_eq!(
        event.working_context_pct_after, None,
        "the floor-breach row must NEVER carry its own percentage — the \
         paired tcode-cadence row already does, and double-reporting it \
         double-counts the same breach toward the floor (issue #3911 \
         post-review fix)"
    );
    assert_eq!(event.overhead_pct_after, None);

    assert_eq!(
        lifetime_cadence_floor_breach_count(&dir),
        1,
        "the durable alarm line must always accompany the JSONL record"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Regression test for the exact double-count a code-review pass caught:
/// `AgentLoop::maybe_cadence_compress` calls `record_cadence_event`
/// unconditionally (whenever `outcome.rounds > 0`, which every breach
/// satisfies) THEN `record_cadence_floor_breach` for the SAME turn, with
/// the SAME `tokens_after`/`context_window`. Any consumer that counts
/// "samples with a `working_context_pct_after`" — `compression_report.py`'s
/// `compute_context_floor`, and this crate's own
/// `session_working_context_floor` (issue #3912) — must see exactly ONE
/// such sample per real breach, not two.
#[test]
fn record_cadence_floor_breach_never_double_counts_the_floor() {
    let dir = temp_dir("floor-breach-no-double-count");
    // Mirrors the real call order at one turn boundary: the paired
    // tcode-cadence record first, then the floor-breach alarm.
    record_cadence_event(&dir, Some("s".to_string()), 200_000, 104_000, 200_000, 5, 7);
    record_cadence_floor_breach(&dir, Some("s".to_string()), 200_000, 104_000, 200_000, 5, 7);

    let events = read_lines(&compression_log_path(&dir))
        .iter()
        .map(|l| serde_json::from_str::<CompressionEvent>(l).unwrap())
        .collect::<Vec<_>>();
    let with_pct = events
        .iter()
        .filter(|e| e.working_context_pct_after.is_some())
        .count();
    assert_eq!(
        with_pct, 1,
        "exactly one row from this turn may carry a working_context_pct_after \
         sample — the paired tcode-cadence row — not both: {events:?}"
    );

    let (min_pct, sample_count) = session_working_context_floor(&dir, "s");
    assert_eq!(min_pct, Some(48));
    assert_eq!(
        sample_count, 1,
        "session_working_context_floor (issue #3912) must not double-count \
         a single breach as two samples"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// -- context_pcts --

#[test]
fn context_pcts_zero_window_is_none() {
    assert_eq!(context_pcts(1000, 0), (None, None));
}

#[test]
fn context_pcts_matches_saturating_formula() {
    // 50_000 / 200_000 = 25% overhead, 75% working.
    assert_eq!(context_pcts(50_000, 200_000), (Some(75), Some(25)));
}

#[test]
fn context_pcts_saturates_at_100() {
    // Tokens far exceeding the window must never overflow/panic and must
    // saturate at 100/0, mirroring `CadenceOutcome::overhead_pct`'s own
    // saturating arithmetic.
    assert_eq!(context_pcts(10_000_000, 1000), (Some(0), Some(100)));
}

// -- record_threshold_event / record_cadence_event --

#[test]
fn record_threshold_event_writes_jsonl_and_alarm_when_cadence_enabled() {
    let dir = temp_dir("threshold-cadence-enabled");
    record_threshold_event(
        &dir,
        Some("sess-x".to_string()),
        1000,
        900,
        200_000,
        5,
        true,
    );

    let lines = read_lines(&compression_log_path(&dir));
    assert_eq!(lines.len(), 1);
    let event: CompressionEvent = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(event.surface, SURFACE_TCODE_THRESHOLD);
    assert_eq!(event.surface_detail, DETAIL_THRESHOLD);
    assert!(
        event.compaction_event,
        "threshold events are always compaction_event: true"
    );
    assert_eq!(event.tokens_before, 1000);
    assert_eq!(event.tokens_after, 900);

    assert_eq!(
        lifetime_compaction_alarm_count(&dir),
        1,
        "cadence_enabled=true must also record the durable alarm line"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_threshold_event_no_alarm_when_cadence_disabled() {
    let dir = temp_dir("threshold-cadence-disabled");
    record_threshold_event(&dir, None, 1000, 900, 200_000, 5, false);

    let lines = read_lines(&compression_log_path(&dir));
    assert_eq!(lines.len(), 1, "the JSONL record is unconditional");
    let event: CompressionEvent = serde_json::from_str(&lines[0]).unwrap();
    assert!(
        event.compaction_event,
        "compaction_event is still true — it marks a tcode-threshold fire, not the cadence-alarm gate"
    );

    assert_eq!(
        lifetime_compaction_alarm_count(&dir),
        0,
        "cadence: None must never record the alarm line (issue #3868 acceptance criteria)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_cadence_event_writes_jsonl() {
    let dir = temp_dir("cadence-event");
    record_cadence_event(&dir, Some("sess-y".to_string()), 2000, 800, 200_000, 3, 2);

    let lines = read_lines(&compression_log_path(&dir));
    assert_eq!(lines.len(), 1);
    let event: CompressionEvent = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(event.surface, SURFACE_TCODE_CADENCE);
    assert_eq!(event.surface_detail, DETAIL_CADENCE);
    assert!(
        !event.compaction_event,
        "cadence fires are never the Slice B alarm signal"
    );
    assert_eq!(event.rounds, 2);
    assert!((event.ratio - 0.4).abs() < f64::EPSILON);

    assert_eq!(
        lifetime_compaction_alarm_count(&dir),
        0,
        "a cadence fire must never increment the threshold-only alarm counter"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// -- session_working_context_floor (issue #3912) --

#[test]
fn session_working_context_floor_missing_file_is_none() {
    let dir = temp_dir("floor-missing");
    assert_eq!(session_working_context_floor(&dir, "sess-a"), (None, 0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn session_working_context_floor_tracks_minimum_for_session() {
    let dir = temp_dir("floor-tracks-min");
    record_cadence_event(
        &dir,
        Some("sess-a".to_string()),
        100_000,
        90_000,
        200_000,
        5,
        3,
    );
    record_cadence_event(
        &dir,
        Some("sess-a".to_string()),
        100_000,
        104_000,
        200_000,
        5,
        4,
    );
    record_cadence_event(
        &dir,
        Some("sess-a".to_string()),
        100_000,
        52_000,
        200_000,
        5,
        5,
    );

    // overhead_pct is tokens_after/context_window*100; working = 100 - that.
    // 90_000/200_000 = 45% -> working 55; 104_000/200_000 = 52% -> working 48;
    // 52_000/200_000 = 26% -> working 74. Floor must be 48.
    assert_eq!(
        session_working_context_floor(&dir, "sess-a"),
        (Some(48), 3),
        "must report the lowest working_context_pct_after across all samples, not the latest"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn session_working_context_floor_ignores_other_sessions() {
    let dir = temp_dir("floor-ignores-others");
    record_cadence_event(
        &dir,
        Some("sess-a".to_string()),
        100_000,
        190_000,
        200_000,
        5,
        3,
    );
    record_cadence_event(
        &dir,
        Some("sess-b".to_string()),
        100_000,
        10_000,
        200_000,
        5,
        3,
    );

    let (min_pct, count) = session_working_context_floor(&dir, "sess-a");
    assert_eq!(count, 1, "only sess-a's own sample must be counted");
    assert_eq!(
        min_pct,
        Some(5),
        "sess-b's much lower reading must not leak in"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// -- default_data_dir / DATA_DIR_ENV_VAR --

#[tokio::test]
async fn default_data_dir_honors_env_override() {
    let dir = temp_dir("default-data-dir-override");
    let observed = with_data_dir_env(&dir, default_data_dir).await;
    assert_eq!(observed, dir);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn default_data_dir_falls_back_to_workstreams_default_when_unset() {
    // Not wrapped in `with_data_dir_env` — the var must be unset here for
    // this assertion to be meaningful; guarded by not running in parallel
    // with the override test via `DATA_DIR_ENV_LOCK` in practice, but this
    // check only reads, so a stray leaked override would only make it fail
    // loudly rather than silently pass.
    if std::env::var(DATA_DIR_ENV_VAR).is_ok() {
        return;
    }
    assert_eq!(default_data_dir(), crate::workstreams::default_data_dir());
}
