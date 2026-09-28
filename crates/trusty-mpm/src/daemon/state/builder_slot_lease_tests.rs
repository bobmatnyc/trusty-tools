//! Tests that a held builder slot survives a daemon restart (#8819).
//!
//! Why: the restart is simulated the way it happens — a fresh [`DaemonState`]
//! with no delegation records, over the same on-disk slot pool — and the claim
//! goes through the route op whose `slot_path` the dispatch hook puts in the
//! brief. Every test uses only surfaces that predate #8819, so it compiles and
//! fails against the tree before it.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::builder_capacity::{Capacity, CapacityReason};
use crate::core::builder_slot_pool::{SEED_MARKER, SlotPool, test_support::write_checkout};
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::builder_slot_routes::{BuilderSlotRequest, builder_slot_op_with_pool};

use super::core::DaemonState;

/// A pool under `root` with two seeded slots and a checkout naming a path
/// package, which a handover needs (#8794).
fn seeded_pool(root: &Path) -> SlotPool {
    let checkout = write_checkout(&root.join("checkout"));
    let pool = SlotPool::new(
        root.join("pool"),
        trusty_common::github_path::GithubPath {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
        },
        4,
    )
    .with_checkout(checkout);
    for index in [0, 1] {
        pool.seed(index, None).expect("a seeded slot");
    }
    pool
}

/// Where slot `index`'s lease lives, spelled out so this file compiles on the
/// tree before #8819.
fn lease_path(pool: &SlotPool, index: u32) -> PathBuf {
    pool.slot_path(index)
        .with_file_name(format!(".slot-{index}.lease"))
}

/// Claim a slot through the route op for a builder `tool_use_id`, dispatched
/// by a session whose pid is this (live) test process. Returns the slot path
/// the hook would suggest.
fn suggested_slot(state: &Arc<DaemonState>, pool: &SlotPool, tool_use_id: &str) -> Option<String> {
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    session.pid = Some(std::process::id());
    let id = session.id;
    state.register_session(session);
    let payload = serde_json::json!({
        "cwd": "/repo",
        "tool": "Agent",
        "input": {"subagent_type": "rust-engineer", "description": "build"},
        "tool_use_id": tool_use_id,
    });
    let capacity = Capacity {
        n_effective: 4,
        ceiling: 4,
        reason: CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
    };
    let outcome = builder_slot_op_with_pool(
        state,
        &id.0.to_string(),
        BuilderSlotRequest { payload },
        &capacity,
        Some(pool),
        None,
    )
    .expect("a well-formed session id");
    assert!(outcome.response.claimed, "{:?}", outcome.response);
    outcome.response.slot_path
}

fn slot(pool: &SlotPool, index: u32) -> Option<String> {
    Some(pool.slot_path(index).to_string_lossy().into_owned())
}

/// #8819 items 1, 3 and 5: A holds slot 0; the daemon restarts; B's claim is
/// never offered slot 0 while A's owner runs. Before #8819 the restarted
/// daemon had no record of A and handed B slot 0 under A's build.
#[test]
fn a_held_slot_survives_a_daemon_restart_8819() {
    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    let before = Arc::new(DaemonState::new());
    assert_eq!(suggested_slot(&before, &pool, "toolu_A"), slot(&pool, 0));
    drop(before);

    let after = Arc::new(DaemonState::new());
    let b = suggested_slot(&after, &pool, "toolu_B");

    assert_eq!(b, slot(&pool, 1), "slot 0 is still A's after the restart");
    let marker = std::fs::read_to_string(pool.slot_path(0).join(SEED_MARKER)).expect("marker");
    assert!(marker.contains("served: toolu_A"), "{marker}");
    assert!(
        lease_path(&pool, 0).is_file(),
        "the handover recorded a lease"
    );
}

/// #8819 item 4, the error arm: held-slot evidence the restarted daemon cannot
/// read or verify keeps the slot — a lease it cannot read, a malformed lease,
/// and a pre-#8819 handover inside the TTL that records no owner at all.
#[test]
fn unverifiable_lease_evidence_keeps_its_slot_after_a_restart_8819() {
    fn unreadable(pool: &SlotPool) {
        std::fs::create_dir(lease_path(pool, 0)).expect("a directory where the lease goes");
    }
    fn malformed(pool: &SlotPool) {
        std::fs::write(lease_path(pool, 0), "holder: toolu_A\ngranted_at: soon\n")
            .expect("malformed lease");
    }
    fn legacy(pool: &SlotPool) {
        pool.hand_over(0, "toolu_A")
            .expect("a pre-#8819 handover to A");
    }
    type Plant = fn(&SlotPool);
    let cases: [(&str, Plant); 3] = [
        ("unreadable", unreadable),
        ("malformed", malformed),
        ("pre-#8819 served marker", legacy),
    ];
    for (why, plant) in cases {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = seeded_pool(root.path());
        plant(&pool);
        let state = Arc::new(DaemonState::new());
        assert_eq!(
            suggested_slot(&state, &pool, "toolu_B"),
            slot(&pool, 1),
            "{why}: unverifiable evidence must not read as a free slot"
        );
    }
}

/// #8819 item 2: a lease is never held on a pid alone. A dead owner, a reused
/// pid (live, other start time) and an expired lease each free the slot, so a
/// crashed builder's stale lease cannot hold its slot forever.
#[test]
fn a_stale_lease_frees_its_slot_after_a_restart_8819() {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    let now = chrono::Utc::now().timestamp();
    let me = std::process::id();
    let my_start =
        crate::session_manager::worktree_owner_gate::process_start_secs(me).expect("own start");
    let cases = [
        ("dead owner", dead, my_start, now),
        ("reused pid", me, my_start - 10_000, now),
        ("expired", me, my_start, now - 2 * 60 * 60),
    ];
    for (why, pid, start, granted_at) in cases {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = seeded_pool(root.path());
        let body = format!(
            "#8819 builder slot lease\nholder: toolu_A\ngranted_at: {granted_at}\n\
             owner_pid: {pid}\nowner_start: {start}\n"
        );
        std::fs::write(lease_path(&pool, 0), body).expect("lease");
        let state = Arc::new(DaemonState::new());
        assert_eq!(
            suggested_slot(&state, &pool, "toolu_B"),
            slot(&pool, 0),
            "{why}: a stale lease must free its slot"
        );
    }
}
