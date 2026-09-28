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
use crate::daemon::builder_slot_routes::{
    BuilderSlotRequest, BuilderSlotResponse, builder_slot_op_with_pool,
};

use super::core::DaemonState;

/// A pool under `root` with two seeded slots and a checkout naming a path
/// package, which a handover needs (#8794).
pub(super) fn seeded_pool(root: &Path) -> SlotPool {
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
pub(super) fn lease_path(pool: &SlotPool, index: u32) -> PathBuf {
    pool.slot_path(index)
        .with_file_name(format!(".slot-{index}.lease"))
}

/// Claim a slot through the route op for a builder `tool_use_id`, dispatched
/// by a session whose pid is this (live) test process. Returns the slot path
/// the hook would suggest.
pub(super) fn suggested_slot(
    state: &Arc<DaemonState>,
    pool: &SlotPool,
    tool_use_id: &str,
) -> Option<String> {
    let response = claim_via_route(state, pool, tool_use_id, 4);
    assert!(response.claimed, "{response:?}");
    response.slot_path
}

/// Claim through the route op against a measured capacity of `n_effective`,
/// from a session whose pid is this (live) test process.
fn claim_via_route(
    state: &Arc<DaemonState>,
    pool: &SlotPool,
    tool_use_id: &str,
    n_effective: u32,
) -> BuilderSlotResponse {
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
        n_effective,
        ceiling: 4,
        reason: CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
    };
    builder_slot_op_with_pool(
        state,
        &id.0.to_string(),
        BuilderSlotRequest { payload },
        &capacity,
        Some(pool),
        None,
    )
    .expect("a well-formed session id")
    .response
}

pub(super) fn slot(pool: &SlotPool, index: u32) -> Option<String> {
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
        // The marker a pre-#8819 handover left: a `served:` line and no lease.
        let marker = pool.slot_path(0).join(SEED_MARKER);
        let body = std::fs::read_to_string(&marker).expect("marker");
        std::fs::write(&marker, format!("{body}served: toolu_A from /co\n"))
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

/// #8819 cap follow-up: A holds slot 0; the daemon restarts; with room for one
/// builder, B is refused, and the refusal names A's restored lease. Before,
/// the restarted daemon counted 0 holders and admitted B over the host limit.
#[test]
fn a_restored_lease_counts_toward_the_cap_after_a_restart_8819() {
    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    let before = Arc::new(DaemonState::new());
    assert_eq!(suggested_slot(&before, &pool, "toolu_A"), slot(&pool, 0));
    drop(before);

    let after = Arc::new(DaemonState::new());
    let b = claim_via_route(&after, &pool, "toolu_B", 1);

    assert!(
        !b.claimed,
        "A's restored lease fills the one-builder cap: {b:?}"
    );
    let agents: Vec<&str> = b.holders.iter().map(|h| h.agent.as_str()).collect();
    assert_eq!(agents, ["restored-lease"]);
}

/// The delegation record carrying `tool_use_id`.
fn record_of(state: &DaemonState, tool_use_id: &str) -> crate::core::agent::Delegation {
    state
        .delegations
        .iter()
        .find(|e| e.value().tool_use_id.as_deref() == Some(tool_use_id))
        .map(|e| e.value().clone())
        .expect("a record for the dispatch")
}

/// #8819 critic item 2: A completes, then the daemon restarts; B is offered
/// A's slot. Before, A's lease outlived its build and, with A's dispatching
/// session still running, held the slot for the whole TTL after the restart.
#[test]
fn a_completed_builder_frees_its_slot_across_a_restart_8819() {
    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    let before = Arc::new(DaemonState::new());
    assert_eq!(suggested_slot(&before, &pool, "toolu_A"), slot(&pool, 0));
    let a = record_of(&before, "toolu_A");
    assert!(before.terminate_delegation(a.id, crate::core::agent::DelegationStatus::Completed));
    drop(before);

    let after = Arc::new(DaemonState::new());
    assert_eq!(
        suggested_slot(&after, &pool, "toolu_B"),
        slot(&pool, 0),
        "a finished builder's slot is free after the restart"
    );
}

/// #8819 critic item 3: an unreadable lease is bounded by its own mtime. Past
/// the TTL it frees its slot; inside the TTL it keeps it.
#[test]
fn an_unreadable_lease_is_bounded_by_the_ttl_8819() {
    for (age_secs, expected, why) in [
        (2 * 60 * 60, 0, "older than the TTL frees its slot"),
        (0, 1, "inside the TTL keeps its slot"),
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = seeded_pool(root.path());
        let lease = lease_path(&pool, 0);
        std::fs::write(&lease, "holder: toolu_A\ngranted_at: soon\n").expect("malformed");
        let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&lease)
            .and_then(|file| file.set_modified(mtime))
            .expect("set the lease's mtime");
        let state = Arc::new(DaemonState::new());
        assert_eq!(
            suggested_slot(&state, &pool, "toolu_B"),
            slot(&pool, expected),
            "an unreadable lease {why}"
        );
    }
}

/// #8819 critic item 5: a lease that cannot be written hands out no slot and
/// stamps no index. The pool's repo directory is read-only, so the lease
/// draft cannot be created while the lease path itself is free.
#[test]
fn a_lease_that_cannot_be_written_grants_no_slot_8819() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    let parent = pool.slot_path(0).parent().expect("repo dir").to_path_buf();
    let perms = |mode| std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode));
    perms(0o555).expect("read-only repo dir");

    let state = Arc::new(DaemonState::new());
    let b = claim_via_route(&state, &pool, "toolu_B", 4);
    perms(0o755).expect("restore the repo dir");

    assert!(b.claimed, "the admission stands: {b:?}");
    assert_eq!(b.slot_path, None, "no slot without a recorded lease");
    let record = record_of(&state, "toolu_B");
    assert_eq!(record.builder_slot, None, "no index was stamped");
    assert_eq!(record.builder_slot_dir, None);
    assert!(!lease_path(&pool, 0).exists() && !lease_path(&pool, 1).exists());
}
