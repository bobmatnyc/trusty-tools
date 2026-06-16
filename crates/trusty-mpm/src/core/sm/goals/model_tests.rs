//! Tests for the SM goal-tracking data model (SM-6, DOC-14 §9.1).
//!
//! Why: prove the pure model contracts the store relies on — default states,
//! derived progress (the 1-of-3 → 33% case the acceptance calls out), the
//! all-verified gate predicate, and serde round-trips for the palace/cache
//! payload. No I/O, no clock reads: timestamps are passed in, so these are fully
//! deterministic and need no ONNX.

use super::model::{Goal, GoalStatus, SessionLink, SessionTaskState};
use chrono::{TimeZone, Utc};

/// A fixed timestamp for deterministic construction.
fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
}

/// Build a goal with `total` links, the first `verified` of which are `Verified`.
fn goal_with_links(total: usize, verified: usize) -> Goal {
    let mut g = Goal::new("g-test1234", "ship the thing", vec![], ts());
    for i in 0..total {
        let mut link = SessionLink::launched(format!("s-{i}"), format!("task {i}"));
        if i < verified {
            link.state = SessionTaskState::Verified;
        }
        g.sessions.push(link);
    }
    g.recompute_progress();
    g
}

/// Why: a freshly created goal must start `Pending` with `0%` progress and no
/// links — the intake baseline (§9.2).
/// What: constructs a goal and asserts the initial field values.
/// Test: this is the test.
#[test]
fn new_goal_is_pending_zero_progress() {
    let g = Goal::new("g-abc", "do work", vec!["passes".into()], ts());
    assert_eq!(g.status, GoalStatus::Pending);
    assert_eq!(g.progress, 0);
    assert!(g.sessions.is_empty());
    assert_eq!(g.created, g.updated);
    assert_eq!(g.acceptance, vec!["passes".to_string()]);
}

/// Why: `GoalStatus` and `SessionTaskState` defaults anchor the lifecycle —
/// `Pending` goal, `Launched` link.
/// What: asserts both `Default`s.
/// Test: this is the test.
#[test]
fn status_and_state_defaults() {
    assert_eq!(GoalStatus::default(), GoalStatus::Pending);
    assert_eq!(SessionTaskState::default(), SessionTaskState::Launched);
}

/// Why: the delegation wire response carries `goal_status` from
/// [`GoalStatus::label`] so callers can distinguish "in progress" from
/// "blocked"/"done"; the label must be the canonical PascalCase per variant.
/// What: asserts the label string for every variant.
/// Test: this is the test.
#[test]
fn goal_status_label_matches_variants() {
    assert_eq!(GoalStatus::Pending.label(), "Pending");
    assert_eq!(GoalStatus::InProgress.label(), "InProgress");
    assert_eq!(GoalStatus::Blocked.label(), "Blocked");
    assert_eq!(GoalStatus::Done.label(), "Done");
    assert_eq!(GoalStatus::Abandoned.label(), "Abandoned");
}

/// Why: only `Verified` satisfies the gate / progress numerator (§3.5).
/// What: asserts `is_verified()` for every variant.
/// Test: this is the test.
#[test]
fn verified_is_the_only_gate_satisfying_state() {
    assert!(SessionTaskState::Verified.is_verified());
    assert!(!SessionTaskState::Launched.is_verified());
    assert!(!SessionTaskState::Running.is_verified());
    assert!(!SessionTaskState::Failed.is_verified());
}

/// Why: the acceptance criterion — linking sessions, 1 of 3 verified, yields ~33%
/// progress (derived, integer-rounded).
/// What: builds a 3-link goal with 1 verified and asserts `progress == 33`.
/// Test: this is the test.
#[test]
fn progress_one_of_three_verified_is_33() {
    let g = goal_with_links(3, 1);
    assert_eq!(g.progress, 33, "1/3 verified must round to 33%");
}

/// Why: every link verified → 100%; the close-gate precondition is reflected in
/// progress too.
/// What: 3/3 verified asserts `progress == 100`.
/// Test: this is the test.
#[test]
fn progress_all_verified_is_100() {
    let g = goal_with_links(3, 3);
    assert_eq!(g.progress, 100);
}

/// Why: a goal with no links has nothing verified → 0% (and cannot be done).
/// What: 0 links asserts `progress == 0` and `!all_verified`.
/// Test: this is the test.
#[test]
fn progress_no_links_is_zero() {
    let g = goal_with_links(0, 0);
    assert_eq!(g.progress, 0);
    assert!(!g.all_verified());
}

/// Why: 2 of 4 verified must round to 50% (exercises the integer rounding path).
/// What: asserts `progress == 50`.
/// Test: this is the test.
#[test]
fn progress_two_of_four_is_50() {
    let g = goal_with_links(4, 2);
    assert_eq!(g.progress, 50);
}

/// Why: the verification-gate predicate must be `false` if ANY link is
/// unverified, and `false` with no links (no observed evidence of anything).
/// What: asserts `all_verified` across mixed/empty/full link sets.
/// Test: this is the test.
#[test]
fn all_verified_predicate() {
    assert!(
        !goal_with_links(3, 2).all_verified(),
        "any unverified → false"
    );
    assert!(!goal_with_links(0, 0).all_verified(), "no links → false");
    assert!(
        goal_with_links(2, 2).all_verified(),
        "every link verified → true"
    );
}

/// Why: a launched link starts with no evidence (evidence is captured later on
/// observation, §3.5).
/// What: asserts the constructor's defaults.
/// Test: this is the test.
#[test]
fn launched_link_has_no_evidence() {
    let link = SessionLink::launched("s-1", "do x");
    assert_eq!(link.state, SessionTaskState::Launched);
    assert!(link.evidence.is_none());
}

/// Why: goals (and their links) are the palace/cache payload — they must survive a
/// JSON round-trip byte-for-byte (the dual-persistence contract).
/// What: builds a fully-populated goal, serialises + deserialises it, asserts
/// equality.
/// Test: this is the test.
#[test]
fn goal_serde_roundtrip() {
    let mut g = goal_with_links(2, 1);
    g.status = GoalStatus::InProgress;
    g.sessions[0].evidence = Some("https://example.com/pr/1".into());
    g.notes.push("blocked on review".into());

    let json = serde_json::to_string(&g).expect("serialise");
    let back: Goal = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(g, back, "goal must round-trip through JSON unchanged");
}

/// Why: the camelCase serde renames keep the on-disk JSON stable and readable; a
/// drift would break previously-written palace/cache entries.
/// What: asserts the wire forms of the enums.
/// Test: this is the test.
#[test]
fn enum_wire_forms_are_camel_case() {
    assert_eq!(
        serde_json::to_string(&GoalStatus::InProgress).unwrap(),
        "\"inProgress\""
    );
    assert_eq!(
        serde_json::to_string(&SessionTaskState::Verified).unwrap(),
        "\"verified\""
    );
}
