//! Tests for unattended detection (#4652, epic #4646).
//!
//! Every test drives `now` explicitly and points the tracker at a temp
//! directory, so none of them sleeps and none of them reads a process-global
//! (the flakiness class of #4611).

use std::time::Duration;

use chrono::{TimeZone, Utc};
use tempfile::TempDir;

use super::*;
use crate::assistants::AssistantInstanceId;

/// A tracker over a fresh temp dir with the default threshold.
fn tracker() -> (TempDir, AttendanceTracker, AssistantInstanceId) {
    tracker_with(AttendanceConfig::default())
}

/// A tracker over a fresh temp dir with an explicit threshold.
fn tracker_with(config: AttendanceConfig) -> (TempDir, AttendanceTracker, AssistantInstanceId) {
    let dir = TempDir::new().expect("temp dir");
    let tracker = AttendanceTracker::new(attendance_root(dir.path()), config);
    let instance = AssistantInstanceId::new("izzie").expect("valid id");
    (dir, tracker, instance)
}

/// A fixed base instant, so every assertion reads as an offset from one point.
fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 3, 12, 0, 0).unwrap()
}

#[test]
fn default_threshold_is_fifteen_minutes() {
    assert_eq!(DEFAULT_UNATTENDED_AFTER, Duration::from_secs(15 * 60));
    assert_eq!(
        AttendanceConfig::default().unattended_after,
        DEFAULT_UNATTENDED_AFTER
    );
    let (_dir, tracker, _instance) = tracker();
    assert_eq!(tracker.config().unattended_after, DEFAULT_UNATTENDED_AFTER);
}

#[test]
fn fresh_human_turn_is_attended() {
    let (_dir, tracker, instance) = tracker();

    assert!(
        tracker
            .record_turn(&instance, TurnOrigin::Human, t0())
            .expect("record"),
        "a first human turn advances the clock"
    );

    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(t0())
    );

    let now = t0() + chrono::Duration::minutes(1);
    let attendance = tracker.attendance(&instance, now).expect("query");
    assert_eq!(
        attendance,
        Attendance::Attended {
            idle_for: Duration::from_secs(60)
        }
    );
    assert!(!attendance.is_unattended());
    assert_eq!(attendance.idle_for(), Some(Duration::from_secs(60)));
    assert!(!tracker.is_unattended(&instance, now).expect("query"));
}

#[test]
fn past_threshold_is_unattended() {
    let (_dir, tracker, instance) = tracker();
    tracker
        .record_turn(&instance, TurnOrigin::Human, t0())
        .expect("record");

    let now = t0() + chrono::Duration::minutes(16);
    let attendance = tracker.attendance(&instance, now).expect("query");
    assert_eq!(
        attendance,
        Attendance::Unattended {
            idle_for: Duration::from_secs(16 * 60)
        }
    );
    assert!(tracker.is_unattended(&instance, now).expect("query"));
}

/// The distinction the whole issue turns on: the assistant working alone must
/// never look like a human being present.
#[test]
fn assistant_turns_never_advance_the_clock() {
    let (_dir, tracker, instance) = tracker();
    tracker
        .record_turn(&instance, TurnOrigin::Human, t0())
        .expect("record");

    // Sixty minutes of the assistant's own work: replies, tool calls, hooks.
    // `last_activity_at` would advance on every one of these.
    for minute in 1..=60 {
        let advanced = tracker
            .record_turn(
                &instance,
                TurnOrigin::Assistant,
                t0() + chrono::Duration::minutes(minute),
            )
            .expect("record");
        assert!(!advanced, "assistant turn at +{minute}m advanced the clock");
    }

    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(t0()),
        "the clock still points at the human's turn"
    );
    let now = t0() + chrono::Duration::minutes(60);
    assert!(
        tracker.is_unattended(&instance, now).expect("query"),
        "an hour of assistant-only activity leaves the instance unattended"
    );
    assert!(!TurnOrigin::Assistant.is_human());
    assert!(TurnOrigin::Human.is_human());
}

/// Stronger than "does not advance": assistant activity does not write at all,
/// so an instance that has only ever worked alone has no attendance record.
#[test]
fn assistant_only_activity_leaves_no_record_at_all() {
    let (_dir, tracker, instance) = tracker();

    for minute in 0..30 {
        tracker
            .record_turn(
                &instance,
                TurnOrigin::Assistant,
                t0() + chrono::Duration::minutes(minute),
            )
            .expect("record");
    }

    assert!(
        !tracker.record_path(&instance).exists(),
        "assistant activity created an attendance record"
    );
    assert_eq!(tracker.last_human_turn(&instance).expect("read"), None);
    assert_eq!(
        tracker.attendance(&instance, t0()).expect("query"),
        Attendance::NeverAttended
    );
}

#[test]
fn threshold_boundary_is_unattended_at_exactly_the_threshold() {
    let (_dir, tracker, instance) = tracker_with(AttendanceConfig {
        unattended_after: Duration::from_secs(10 * 60),
    });
    tracker
        .record_turn(&instance, TurnOrigin::Human, t0())
        .expect("record");

    let one_second_short = t0() + chrono::Duration::seconds(10 * 60 - 1);
    assert!(
        !tracker
            .is_unattended(&instance, one_second_short)
            .expect("query"),
        "one second short of the threshold is still attended"
    );

    let exactly = t0() + chrono::Duration::seconds(10 * 60);
    assert!(
        tracker.is_unattended(&instance, exactly).expect("query"),
        "the boundary is inclusive: N minutes of silence is unattended"
    );
}

#[test]
fn never_attended_counts_as_unattended() {
    let (_dir, tracker, instance) = tracker();
    let attendance = tracker.attendance(&instance, t0()).expect("query");
    assert_eq!(attendance, Attendance::NeverAttended);
    assert_eq!(attendance.idle_for(), None);
    assert!(attendance.is_unattended());
    assert!(tracker.is_unattended(&instance, t0()).expect("query"));
}

#[test]
fn an_older_human_turn_never_rewinds_the_clock() {
    let (_dir, tracker, instance) = tracker();
    let latest = t0() + chrono::Duration::minutes(5);
    tracker
        .record_turn(&instance, TurnOrigin::Human, latest)
        .expect("record");

    assert!(
        !tracker
            .record_turn(&instance, TurnOrigin::Human, t0())
            .expect("record"),
        "an out-of-order turn must not be persisted"
    );
    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(latest)
    );
}

#[test]
fn a_future_timestamp_reads_as_attended() {
    let (_dir, tracker, instance) = tracker();
    tracker
        .record_turn(
            &instance,
            TurnOrigin::Human,
            t0() + chrono::Duration::hours(1),
        )
        .expect("record");

    let attendance = tracker.attendance(&instance, t0()).expect("query");
    assert_eq!(
        attendance,
        Attendance::Attended {
            idle_for: Duration::ZERO
        },
        "clock skew biases toward silence, not toward interrupting a human"
    );
}

#[test]
fn record_path_is_one_file_per_instance() {
    let (dir, tracker, izzie) = tracker();
    let cto = AssistantInstanceId::new("cto-assistant").expect("valid id");

    assert_eq!(
        tracker.record_path(&izzie),
        attendance_root(dir.path()).join("izzie.json")
    );
    assert_ne!(tracker.record_path(&izzie), tracker.record_path(&cto));

    tracker
        .record_turn(&izzie, TurnOrigin::Human, t0())
        .expect("record");
    assert_eq!(
        tracker.last_human_turn(&cto).expect("read"),
        None,
        "one instance's human turn must not attend another's"
    );
}

#[test]
fn unreadable_record_is_an_error_not_a_silent_never() {
    let (_dir, tracker, instance) = tracker();
    let path = tracker.record_path(&instance);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, b"{ not json").expect("write");

    assert!(
        tracker.last_human_turn(&instance).is_err(),
        "corruption must surface, not read as never-attended"
    );
    assert!(tracker.attendance(&instance, t0()).is_err());
}

#[test]
fn a_recorded_turn_survives_a_new_tracker() {
    let dir = TempDir::new().expect("temp dir");
    let instance = AssistantInstanceId::new("izzie").expect("valid id");
    let config = AttendanceConfig::default();

    AttendanceTracker::new(attendance_root(dir.path()), config)
        .record_turn(&instance, TurnOrigin::Human, t0())
        .expect("record");

    // A different process would resolve the same root and read the same value:
    // the signal is durable, so a restart cannot fabricate "nobody is here".
    let reopened = AttendanceTracker::new(attendance_root(dir.path()), config);
    assert_eq!(
        reopened.last_human_turn(&instance).expect("read"),
        Some(t0())
    );
    assert!(
        !reopened
            .is_unattended(&instance, t0() + chrono::Duration::minutes(1))
            .expect("query")
    );
}

#[test]
fn parse_threshold_accepts_whole_minutes() {
    assert_eq!(
        parse_threshold_minutes(Some("5")),
        Some(Duration::from_secs(300))
    );
    assert_eq!(
        parse_threshold_minutes(Some("  45 ")),
        Some(Duration::from_secs(45 * 60))
    );
}

#[test]
fn parse_threshold_rejects_junk_and_zero() {
    for raw in [
        None,
        Some(""),
        Some("   "),
        Some("abc"),
        Some("-3"),
        Some("1.5"),
    ] {
        assert_eq!(parse_threshold_minutes(raw), None, "accepted {raw:?}");
    }
    assert_eq!(
        parse_threshold_minutes(Some("0")),
        None,
        "a zero threshold would make every instance permanently unattended"
    );
}

/// The one-line hook the human-facing surfaces call.
#[test]
fn note_turn_records_a_turn_and_is_infallible() {
    let (dir, tracker, instance) = tracker();
    let root = attendance_root(dir.path());

    assert!(note_turn_in(&root, "izzie", TurnOrigin::Human, t0()));
    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(t0())
    );

    // Re-recording the same instant is not an advance, and is still not an error.
    assert!(!note_turn_in(&root, "izzie", TurnOrigin::Human, t0()));
}

#[test]
fn note_turn_swallows_an_unusable_instance_name() {
    let dir = TempDir::new().expect("temp dir");
    let root = attendance_root(dir.path());

    for bad in ["../escape", "", "Izzie", "a/b"] {
        assert!(
            !note_turn_in(&root, bad, TurnOrigin::Human, t0()),
            "`{bad}` must not record, and must not panic a live turn"
        );
    }
    assert!(!root.exists(), "a rejected name created state anyway");
}

/// #4683 regression: the outcome the two `handle_command` gaps produced. A
/// human who drives a chat bot with NOTHING but slash commands is present, so
/// the instance must read attended — before the fix `handle_command` recorded
/// nothing, and the same traffic aged past the threshold into `Unattended`.
#[test]
fn a_command_only_session_stays_attended() {
    let (dir, recorded, instance) = tracker();
    let root = attendance_root(dir.path());

    // Four slash commands, five minutes apart — e.g. `/status` polled while a
    // long task runs. Every one of them is a person typing.
    for step in 0..4 {
        note_command_turn_in(
            Some(&root),
            "izzie",
            TurnOrigin::Human,
            true,
            t0() + chrono::Duration::minutes(5 * step),
        );
    }

    // Twenty minutes after the FIRST command — well past the 15-minute
    // threshold — but only five after the last one.
    let now = t0() + chrono::Duration::minutes(20);
    assert_eq!(
        recorded.attendance(&instance, now).expect("query"),
        Attendance::Attended {
            idle_for: Duration::from_secs(5 * 60)
        },
        "slash commands are human turns; a command-only session is attended"
    );

    // The contrast that makes the assertion mean something: with the commands
    // NOT recorded (the pre-fix behaviour), the same twenty minutes reads as
    // never-attended, which `is_unattended` answers true to.
    let (_dir2, silent, silent_instance) = tracker();
    assert!(
        silent.is_unattended(&silent_instance, now).expect("query"),
        "the same window with no recorded turn is unattended"
    );
}

/// #4685: the property caller-declared origin buys. An automated caller
/// reaching for the CONVENIENT command helper — the exact mistake the old
/// `note_command_turn_in`, which hardcoded `TurnOrigin::Human` internally,
/// could not prevent — must record nothing even with every transport gate
/// satisfied.
///
/// The contrast in the second half is what makes the first half mean
/// something: the same call with `TurnOrigin::Human` DOES record, so the
/// assistant-origin no-op cannot be confused with a missing hook.
#[test]
fn an_assistant_origin_caller_records_nothing() {
    let (dir, tracker, instance) = tracker();
    let root = attendance_root(dir.path());

    for step in 0..4 {
        // Paired, authenticated, at a command path — every gate the transports
        // have is open. Origin is the only thing standing in the way.
        assert!(
            !note_command_turn_in(
                Some(&root),
                "izzie",
                TurnOrigin::Assistant,
                true,
                t0() + chrono::Duration::minutes(5 * step),
            ),
            "an assistant-origin command turn advanced the clock"
        );
    }
    assert!(
        !note_turn_in(&root, "izzie", TurnOrigin::Assistant, t0()),
        "an assistant-origin turn advanced the clock"
    );

    assert!(
        !tracker.record_path(&instance).exists(),
        "automation forged an attendance record"
    );
    assert_eq!(
        tracker
            .attendance(&instance, t0() + chrono::Duration::minutes(20))
            .expect("query"),
        Attendance::NeverAttended,
        "twenty minutes of automated commands must leave the owner absent"
    );

    // Same call, same gates, human origin — this one records.
    assert!(
        note_command_turn_in(Some(&root), "izzie", TurnOrigin::Human, true, t0()),
        "the human-origin path must still record, or the test above proves nothing"
    );
    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(t0())
    );
}

/// #4683 regression for the monotonic guard. Two processes recording human
/// turns for one instance interleave; the guard must be evaluated under the
/// same held lock as the write, or a stale reader's older timestamp lands last
/// and REWINDS the clock.
///
/// Fails against a read-then-write guard: the newest turn is written first and
/// every later writer carries an older instant, so any writer that read before
/// that write and published after it clobbers the newest value.
#[test]
fn concurrent_writers_never_rewind_the_clock() {
    let dir = TempDir::new().expect("temp dir");
    let root = attendance_root(dir.path());
    let instance = AssistantInstanceId::new("izzie").expect("valid id");
    let newest = t0() + chrono::Duration::hours(1);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for worker in 0..8 {
        let root = root.clone();
        let instance = instance.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let tracker = AttendanceTracker::new(root, AttendanceConfig::default());
            barrier.wait();
            for round in 0..25 {
                // Worker 0 publishes the newest instant immediately; everyone
                // else only ever offers strictly older ones, which the guard
                // must reject for the rest of the run.
                let at = if worker == 0 && round == 0 {
                    newest
                } else {
                    t0() + chrono::Duration::seconds(worker * 25 + round)
                };
                tracker
                    .record_turn(&instance, TurnOrigin::Human, at)
                    .expect("record");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker panicked");
    }

    let tracker = AttendanceTracker::new(root, AttendanceConfig::default());
    assert_eq!(
        tracker.last_human_turn(&instance).expect("read"),
        Some(newest),
        "a concurrent older turn rewound the clock — the monotonic guard is not \
         atomic with the write"
    );
}

#[test]
fn default_root_is_under_the_app_state_tree() {
    let root = default_attendance_root().expect("home dir");
    assert!(root.ends_with("attendance"));
    assert!(
        root.parent().expect("parent").ends_with(".trusty-agents"),
        "attendance is app-private machine state, not the user-browsable home"
    );
}
