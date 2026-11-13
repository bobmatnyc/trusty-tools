//! Tests for pending presence requests (#8878 PR2b-1a).

use super::*;
use crate::core::presence_test_support::{
    ARCHITECT, NOW, TTL, key, request, request_for, root, snapshot,
};
use crate::core::twin_identity::ClaudeProcess;

#[test]
fn a_request_round_trips_sealed_8878() {
    let (_d, root) = root();
    let made = request(&root, "write .claude/settings.json");
    assert_eq!(made.id.len(), 32);
    assert_eq!(made.expires, NOW + TTL.as_secs());
    assert_eq!(load(&root, &key(), &made.id, NOW + 1), Ok(made.clone()));
    assert_eq!(
        load(&root, &key(), "../../x", NOW),
        Err(RequestError::InvalidId)
    );
    // A request edited to name another PID no longer verifies.
    let path = pending_path(&root, &made.id).expect("path");
    let edited = Pending {
        pid: 1,
        ..made.clone()
    };
    std::fs::remove_file(&path).expect("rm");
    crate::core::architect_session::write_owner_only(
        &path,
        &serde_json::to_vec(&edited).expect("json"),
    )
    .expect("write");
    let before = snapshot(&root);
    assert_eq!(
        load(&root, &key(), &made.id, NOW),
        Err(RequestError::Forged)
    );
    assert_eq!(snapshot(&root), before, "a refused load consumes nothing");
}

/// Architect Q6: the caller's action text is always labelled.
#[test]
fn the_action_text_is_labelled_unverified_origin_8878() {
    let (_d, root) = root();
    let made = request(&root, "write .claude/settings.json");
    assert_eq!(
        made.action_label(),
        "write .claude/settings.json (unverified origin)"
    );
    assert_eq!(made.action_hash, action_hash("write .claude/settings.json"));
}

/// Architect Q8: one request per PID per two minutes.
#[test]
fn one_request_per_pid_per_two_minutes_8878() {
    let (_d, root) = root();
    request(&root, "a");
    let again = create(&root, &key(), new_for(ARCHITECT), TTL, NOW + 119);
    assert_eq!(again, Err(RequestError::RateLimited));
    request_for(&root, ARCHITECT, "b", NOW + 120);
}

/// Architect Q8: at most 16 live requests; expired ones are cleared.
#[test]
fn the_pending_cap_is_16_8878() {
    let (_d, root) = root();
    for pid in 0..PENDING_CAP as u32 {
        request_for(
            &root,
            ClaudeProcess {
                pid: 10 + pid,
                start_time: 1,
            },
            "a",
            NOW,
        );
    }
    let full = create(
        &root,
        &key(),
        new_for(ClaudeProcess {
            pid: 99,
            start_time: 1,
        }),
        TTL,
        NOW,
    );
    assert_eq!(full, Err(RequestError::Full));
    // Once they expire they are cleared, and a new request fits.
    let later = NOW + TTL.as_secs();
    request_for(
        &root,
        ClaudeProcess {
            pid: 99,
            start_time: 1,
        },
        "a",
        later,
    );
    assert_eq!(
        std::fs::read_dir(pending_dir(&root)).expect("dir").count(),
        1
    );
}

#[test]
fn an_expired_request_is_refused_8878() {
    let (_d, root) = root();
    let made = request(&root, "a");
    assert_eq!(
        load(&root, &key(), &made.id, made.expires),
        Err(RequestError::Expired)
    );
    let long = create(&root, &key(), new_for(ARCHITECT), MAX_TTL + TTL, NOW + 500);
    assert!(matches!(long, Err(RequestError::OutOfBounds(_))));
}

fn new_for(claude: ClaudeProcess) -> NewRequest {
    NewRequest {
        pid: claude.pid,
        start_time: claude.start_time,
        project: "p".to_owned(),
        session: "s".to_owned(),
        action: "a".to_owned(),
    }
}

/// #8878: at the 1-minute TTL floor, an expired request still holds its PID's
/// 2-minute window (Architect Q8).
#[test]
fn the_rate_window_holds_at_the_ttl_floor_8878() {
    let (_d, root) = root();
    let floor = crate::core::presence_config::PresenceConfig {
        request_ttl_minutes: 0,
        ..Default::default()
    }
    .request_ttl();
    create(&root, &key(), new_for(ARCHITECT), floor, NOW).expect("first");
    let expired_but_in_window = NOW + floor.as_secs() + 1;
    assert_eq!(
        create(
            &root,
            &key(),
            new_for(ARCHITECT),
            floor,
            expired_but_in_window
        ),
        Err(RequestError::RateLimited)
    );
    create(
        &root,
        &key(),
        new_for(ARCHITECT),
        floor,
        NOW + RATE_WINDOW.as_secs(),
    )
    .expect("after the window");
}
