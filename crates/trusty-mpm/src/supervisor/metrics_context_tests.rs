//! #8394: context figures on `SessionRecord` and `FleetMetrics`.
//!
//! A module of its own rather than `super::tests`, which parallel work edits.

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;
use crate::core::context_usage::{
    ContextUsage, PlanUsage, RateLimitWindow, STALE_AFTER_SECS, record_context_usage,
};
use crate::session_manager::tests::make_active_test_record;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .expect("valid timestamp")
}

fn reading(updated_at: DateTime<Utc>, pct: f64, plan_pct: Option<f64>) -> ContextUsage {
    ContextUsage {
        total_input_tokens: Some(1_000),
        context_window_size: Some(200_000),
        used_percentage: Some(pct),
        plan: plan_pct.map(|p| PlanUsage {
            five_hour: Some(RateLimitWindow {
                used_percentage: Some(p),
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

/// Why: the reading is keyed by the CLAUDE session id the statusline sees, not
/// the managed id; a record without one has nothing to look up.
/// Test: itself.
#[test]
fn attach_reads_the_reading_keyed_by_claude_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let written = at(10_000);
    record_context_usage(dir.path(), "claude-a", &reading(written, 41.0, None)).expect("write");

    let mut with_id = make_active_test_record("tm-a", "t", "/tmp/a");
    with_id.claude_session_id = Some("claude-a".into());
    let mut without_id = make_active_test_record("tm-b", "t", "/tmp/b");
    // A stale overlay from an earlier attach must be cleared, not kept.
    without_id.context_usage = Some(reading(written, 99.0, None));
    let mut records = vec![with_id, without_id];

    let late = written + Duration::seconds(STALE_AFTER_SECS + 1);
    attach_context_usage(&mut records, dir.path(), late);

    let attached = records[0].context_usage.as_ref().expect("attached");
    assert_eq!(attached.used_percentage, Some(41.0));
    assert!(attached.stale, "staleness is computed at attach time");
    assert_eq!(records[1].context_usage, None);
}

/// Why: the poller reads per-session figures and one account-wide plan value
/// from the fleet snapshot; a tombstone's figures are not fleet state.
/// Test: itself.
#[test]
fn fleet_metrics_report_each_live_sessions_context() {
    let mut older = make_active_test_record("tm-old", "t", "/tmp/o");
    older.context_usage = Some(reading(at(100), 10.0, Some(5.0)));
    let mut newer = make_active_test_record("tm-new", "t", "/tmp/n");
    newer.context_usage = Some(reading(at(200), 20.0, Some(7.0)));
    let mut newest_no_plan = make_active_test_record("tm-np", "t", "/tmp/p");
    newest_no_plan.context_usage = Some(reading(at(300), 30.0, None));
    let mut dead = make_active_test_record("tm-dead", "t", "/tmp/d");
    dead.state = ManagedSessionState::Decommissioned;
    dead.context_usage = Some(reading(at(400), 90.0, Some(99.0)));
    let bare = make_active_test_record("tm-bare", "t", "/tmp/b");

    let m = FleetMetrics::from_records(&[older, newer, newest_no_plan, dead, bare]);

    let names: Vec<&str> = m
        .context_usage
        .iter()
        .map(|c| c.tmux_name.as_str())
        .collect();
    assert_eq!(names, vec!["tm-old", "tm-new", "tm-np"]);
    assert_eq!(m.context_usage[1].context.used_percentage, Some(20.0));
    let plan = m.plan_usage.expect("an account-wide plan value");
    assert_eq!(
        plan.updated_at,
        at(200),
        "the newest reading that carried a plan"
    );
    assert_eq!(
        plan.five_hour.and_then(|w| w.used_percentage),
        Some(7.0),
        "a tombstone's plan figures are not used"
    );
}

/// Why: a session store and a published snapshot written before #8394 must
/// still load.
/// Test: itself.
#[test]
fn a_session_record_lacking_context_usage_still_deserializes() {
    let record = make_active_test_record("tm-x", "t", "/tmp/x");
    let mut json = serde_json::to_value(&record).expect("serialize");
    assert!(
        json.get("context_usage").is_none(),
        "a None overlay is never written to the store"
    );
    json.as_object_mut()
        .expect("object")
        .remove("context_usage");
    let back: SessionRecord = serde_json::from_value(json).expect("old record loads");
    assert_eq!(back.context_usage, None);

    let mut metrics = serde_json::to_value(FleetMetrics::default()).expect("serialize");
    let obj = metrics.as_object_mut().expect("object");
    obj.remove("context_usage");
    obj.remove("plan_usage");
    let back: FleetMetrics = serde_json::from_value(metrics).expect("old snapshot loads");
    assert!(back.context_usage.is_empty());
    assert_eq!(back.plan_usage, None);
}
