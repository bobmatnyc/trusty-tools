//! Unit tests for the per-turn file-change budget (`super`), relocated out of
//! `mod.rs` (issue #2918) so the production file stays under the 500-SLOC cap.
//! `tests.rs` is classified as a test file (1500-SLOC cap).

use super::*;

#[test]
fn advance_budget_starts_fresh_window() {
    let (state, decision) = advance_budget(None, 1000, 3);
    assert_eq!(
        state,
        BudgetState {
            count: 1,
            window_started_at: 1000
        }
    );
    assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
}

#[test]
fn advance_budget_allows_up_to_budget() {
    let mut prev = None;
    for expected_used in 1..=3 {
        let (state, decision) = advance_budget(prev, 1000, 3);
        assert_eq!(
            decision,
            BudgetDecision::Allowed {
                used: expected_used,
                budget: 3
            }
        );
        prev = Some(state);
    }
}

#[test]
fn advance_budget_denies_past_budget() {
    let mut prev = None;
    for _ in 1..=3 {
        let (state, _) = advance_budget(prev, 1000, 3);
        prev = Some(state);
    }
    let (state, decision) = advance_budget(prev, 1000, 3);
    assert_eq!(decision, BudgetDecision::Exhausted { budget: 3 });
    // Saturates at budget, doesn't keep growing.
    assert_eq!(state.count, 3);
}

#[test]
fn advance_budget_resets_after_window_expiry() {
    let prev = Some(BudgetState {
        count: 3,
        window_started_at: 1000,
    });
    // Exactly at the boundary is still "expired" (>= window, not <).
    let (state, decision) = advance_budget(prev, 1000 + TURN_WINDOW_SECS, 3);
    assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
    assert_eq!(state.count, 1);
}

#[test]
fn advance_budget_saturates_count_on_repeated_denial() {
    let prev = Some(BudgetState {
        count: 3,
        window_started_at: 1000,
    });
    let (state1, d1) = advance_budget(prev, 1005, 3);
    let (state2, d2) = advance_budget(Some(state1), 1010, 3);
    assert_eq!(d1, BudgetDecision::Exhausted { budget: 3 });
    assert_eq!(d2, BudgetDecision::Exhausted { budget: 3 });
    assert_eq!(state1.count, 3);
    assert_eq!(state2.count, 3);
}

#[test]
fn sanitize_session_id_replaces_unsafe_bytes() {
    assert_eq!(sanitize_session_id("abc-123_XYZ"), "abc-123_XYZ");
    assert_eq!(sanitize_session_id("../../etc/passwd"), "______etc_passwd");
    assert_eq!(sanitize_session_id(""), "_default");
}

#[test]
fn record_file_change_allows_first_three_then_exhausts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = 5000;
    for expected_used in 1..=3 {
        let decision = record_file_change_at(dir.path(), "sess-a", 3, now);
        assert_eq!(
            decision,
            BudgetDecision::Allowed {
                used: expected_used,
                budget: 3
            }
        );
    }
    let decision = record_file_change_at(dir.path(), "sess-a", 3, now);
    assert_eq!(decision, BudgetDecision::Exhausted { budget: 3 });
}

#[test]
fn record_file_change_isolates_by_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = 5000;
    for _ in 1..=3 {
        record_file_change_at(dir.path(), "sess-a", 3, now);
    }
    // A different session has its own independent budget.
    let decision = record_file_change_at(dir.path(), "sess-b", 3, now);
    assert_eq!(decision, BudgetDecision::Allowed { used: 1, budget: 3 });
}

// ---- code-critic PR #2928 HIGH: storage failure fails toward DENY --------

#[test]
fn record_file_change_denies_on_persist_failure() {
    // Force a persist failure by pre-creating a DIRECTORY at the exact path
    // record_file_change_at will try to std::fs::write the state JSON to —
    // the write then fails ("Is a directory"). Before the fix, this scenario
    // silently returned Allowed{used:1,..} forever (unlimited fail-open).
    let dir = tempfile::tempdir().expect("tempdir");
    let now = 5000;
    let sanitized = sanitize_session_id("sess-fail");
    std::fs::create_dir_all(dir.path().join(format!("{sanitized}.json")))
        .expect("pre-create collision dir");

    let decision = record_file_change_at(dir.path(), "sess-fail", 3, now);
    assert_eq!(
        decision,
        BudgetDecision::Exhausted { budget: 3 },
        "a persist failure must fail toward deny, not silently allow"
    );
    // Repeating the call must keep denying — it must never "recover" into an
    // unlimited-allow state just because the write keeps failing the same way.
    let decision2 = record_file_change_at(dir.path(), "sess-fail", 3, now);
    assert_eq!(decision2, BudgetDecision::Exhausted { budget: 3 });
}

// ---- persist_and_verify / verify_roundtrip --------------------------------

#[test]
fn persist_and_verify_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("state.json");
    let state = BudgetState {
        count: 2,
        window_started_at: 1234,
    };
    assert!(persist_and_verify(dir.path(), &file, &state).is_ok());
    let on_disk = std::fs::read_to_string(&file).expect("read back");
    let parsed: BudgetState = serde_json::from_str(&on_disk).expect("parse");
    assert_eq!(parsed, state);
}

#[test]
fn verify_roundtrip_ok_on_match() {
    let state = BudgetState {
        count: 1,
        window_started_at: 100,
    };
    let json = serde_json::to_string(&state).expect("serialize");
    assert!(verify_roundtrip(&state, &json).is_ok());
}

#[test]
fn verify_roundtrip_detects_mismatch() {
    let written = BudgetState {
        count: 3,
        window_started_at: 100,
    };
    let different = BudgetState {
        count: 1,
        window_started_at: 100,
    };
    let readback_json = serde_json::to_string(&different).expect("serialize");
    let err = verify_roundtrip(&written, &readback_json).expect_err("must detect mismatch");
    assert!(err.contains("mismatch"), "error should say mismatch: {err}");
}

#[test]
fn verify_roundtrip_detects_parse_failure() {
    let written = BudgetState {
        count: 1,
        window_started_at: 100,
    };
    let err = verify_roundtrip(&written, "not json").expect_err("must detect parse failure");
    assert!(err.contains("parse"), "error should say parse: {err}");
}

// ---- code-critic PR #2928 MEDIUM: session-lock serialization -------------

#[test]
fn with_session_lock_acquires_when_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_path = dir.path().join("sess.lock");
    assert!(!lock_path.exists());
    let guard = with_session_lock(&lock_path);
    assert!(guard.is_some(), "an unheld lock must be acquired");
    assert!(lock_path.exists(), "the lockfile must exist while held");
    drop(guard);
    assert!(
        !lock_path.exists(),
        "dropping the guard must release (remove) the lockfile"
    );
}

#[test]
fn with_session_lock_serializes_against_a_held_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_path = dir.path().join("sess.lock");
    let held = with_session_lock(&lock_path).expect("first acquire must succeed");

    // A second attempt, while the first guard is still alive, must fail
    // (bounded retry, then give up) rather than double-acquiring.
    let second = with_session_lock(&lock_path);
    assert!(
        second.is_none(),
        "a lock already held by another guard must not be re-acquired"
    );

    drop(held);
    // Once released, acquisition succeeds again.
    assert!(with_session_lock(&lock_path).is_some());
}

#[test]
fn with_session_lock_clears_a_stale_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_path = dir.path().join("sess.lock");
    // Simulate an abandoned lock left behind by a crashed/killed
    // `tm hook --pm-guard` process: create it, then backdate its mtime past
    // the staleness threshold.
    let file = std::fs::File::create(&lock_path).expect("create lock");
    let stale_time = SystemTime::now() - Duration::from_secs(STALE_LOCK_SECS + 1);
    file.set_modified(stale_time).expect("set_modified");
    drop(file);

    let guard = with_session_lock(&lock_path);
    assert!(
        guard.is_some(),
        "a stale lock must be cleared and re-acquired, not treated as held forever"
    );
}
