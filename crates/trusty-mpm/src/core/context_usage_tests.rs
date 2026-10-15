//! Tests for the per-session context-usage store (#8394).

use chrono::{Duration, TimeZone};
use serde_json::json;

use super::*;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .expect("valid timestamp")
}

/// Why: every figure the issue names must survive extraction unchanged.
/// Test: itself.
#[test]
fn extracts_every_figure_from_a_full_payload() {
    let payload = json!({
        "session_id": "sess-1",
        "context_window": {
            "total_input_tokens": 82_000,
            "context_window_size": 200_000,
            "used_percentage": 41.0,
            "remaining_percentage": 59.0,
            "current_usage": null
        },
        "rate_limits": {
            "five_hour": { "used_percentage": 24.0, "resets_at": 1_760_450_400 },
            "seven_day": { "used_percentage": 61.5, "resets_at": "2025-10-20T00:00:00Z" }
        }
    });
    let now = at(1_760_440_000);
    let usage = ContextUsage::from_statusline_json(&payload, now).expect("a reading");
    assert_eq!(usage.total_input_tokens, Some(82_000));
    assert_eq!(usage.context_window_size, Some(200_000));
    assert_eq!(usage.used_percentage, Some(41.0));
    assert_eq!(usage.updated_at, now);
    assert!(!usage.stale);
    let plan = usage.plan.expect("plan figures");
    assert_eq!(plan.updated_at, now);
    let five = plan.five_hour.expect("five-hour window");
    assert_eq!(five.used_percentage, Some(24.0));
    assert_eq!(five.resets_at, Some(at(1_760_450_400)));
    let seven = plan.seven_day.expect("seven-day window");
    assert_eq!(seven.used_percentage, Some(61.5));
    assert_eq!(
        seven.resets_at,
        Some(Utc.with_ymd_and_hms(2025, 10, 20, 0, 0, 0).unwrap())
    );
}

/// Why: a null before the first API call, or an absent `rate_limits` on a
/// non-subscriber account, must never become a `0`.
/// Test: itself.
#[test]
fn null_and_absent_figures_stay_none() {
    let payload = json!({
        "context_window": {
            "total_input_tokens": null,
            "context_window_size": 200_000,
            "used_percentage": null
        }
    });
    let usage = ContextUsage::from_statusline_json(&payload, at(0)).expect("a reading");
    assert_eq!(usage.total_input_tokens, None);
    assert_eq!(usage.context_window_size, Some(200_000));
    assert_eq!(usage.used_percentage, None);
    assert_eq!(usage.plan, None, "absent rate_limits is no plan");

    let windows_without_figures = json!({
        "rate_limits": { "five_hour": { "used_percentage": null }, "seven_day": "bogus" }
    });
    let usage =
        ContextUsage::from_statusline_json(&windows_without_figures, at(0)).expect("a reading");
    assert_eq!(usage.plan, None, "a window with no figure is no plan");
    assert_eq!(usage.used_percentage, None);
}

/// Why: an empty or unrelated payload is not a reading; recording one would
/// overwrite the last real figures with nothing.
/// Test: itself.
#[test]
fn a_payload_with_neither_object_records_nothing() {
    assert_eq!(
        ContextUsage::from_statusline_json(&json!({"session_id": "s"}), at(0)),
        None
    );
}

fn sample(updated_at: DateTime<Utc>) -> ContextUsage {
    ContextUsage {
        total_input_tokens: Some(50_000),
        context_window_size: Some(200_000),
        used_percentage: Some(25.0),
        plan: Some(PlanUsage {
            five_hour: Some(RateLimitWindow {
                used_percentage: Some(10.0),
                resets_at: None,
            }),
            seven_day: None,
            updated_at,
            stale: false,
        }),
        updated_at,
        stale: false,
    }
}

/// Why: the statusline writes in one process and the daemon reads in another;
/// the round trip under one root is the contract.
/// Test: itself.
#[test]
fn a_reading_round_trips_through_the_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let usage = sample(at(1_000));
    assert!(record_context_usage(dir.path(), "sess-1", &usage).expect("write"));
    let back = read_context_usage(dir.path(), "sess-1", at(1_010)).expect("read back");
    assert_eq!(back, usage);
    assert_eq!(read_context_usage(dir.path(), "sess-2", at(1_010)), None);
}

/// Why: a closed pane leaves its last reading on disk; a consumer must be able
/// to tell it is old.
/// Test: itself.
#[test]
fn an_old_reading_reads_back_stale() {
    let dir = tempfile::tempdir().expect("temp dir");
    let written = at(1_000);
    record_context_usage(dir.path(), "sess-1", &sample(written)).expect("write");

    let edge = written + Duration::seconds(STALE_AFTER_SECS);
    let fresh = read_context_usage(dir.path(), "sess-1", edge).expect("read");
    assert!(
        !fresh.stale,
        "exactly STALE_AFTER_SECS old is not yet stale"
    );

    let old = read_context_usage(dir.path(), "sess-1", edge + Duration::seconds(1)).expect("read");
    assert!(old.stale);
    assert!(
        old.plan.expect("plan").stale,
        "the plan figures age with it"
    );

    let mut skewed = sample(written + Duration::seconds(60));
    skewed.mark_staleness(written);
    assert!(!skewed.stale, "a reading dated in the future is not stale");
}

/// Why: the statusline must log a failed write, so the writer may not swallow
/// it the way `record_session_value` does.
/// Test: itself.
#[test]
fn a_failed_write_is_an_error_not_a_silent_skip() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A regular file where the root directory should be: `create_dir_all` fails.
    let root = dir.path().join("not-a-dir");
    std::fs::write(&root, b"x").expect("write blocker");
    let err = record_context_usage(&root, "sess-1", &sample(at(0)))
        .expect_err("a write under a file must fail");
    assert!(
        matches!(err, ContextUsageError::Io { .. }),
        "unexpected error: {err}"
    );
}

/// Why: `session_id` comes from the payload; an unsafe one must name no file.
/// Test: itself.
#[test]
fn an_unsafe_session_id_is_skipped() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(!record_context_usage(dir.path(), "../evil", &sample(at(0))).expect("skip"));
    assert!(!record_context_usage(dir.path(), "", &sample(at(0))).expect("skip"));
    assert!(!dir.path().join("usage").exists(), "nothing written");
}

/// Why: a reading written before a figure existed must still load.
/// Test: itself.
#[test]
fn a_record_lacking_the_new_fields_still_deserializes() {
    let usage: ContextUsage =
        serde_json::from_str(r#"{"updated_at":"2026-10-14T00:00:00Z"}"#).expect("parse");
    assert_eq!(usage.total_input_tokens, None);
    assert_eq!(usage.plan, None);
    assert!(!usage.stale);
}
