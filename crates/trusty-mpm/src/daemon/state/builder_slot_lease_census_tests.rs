//! Tests that restored builder-slot leases count toward the census and the
//! cap's holder set, once each, after a daemon restart (#8819).
//!
//! Why: the fail-first proof lives in `builder_slot_lease_tests`, which
//! compiles against the tree before the cap follow-up; this file exercises
//! the `_with_pool_root` surfaces that follow-up added.
//! Test: this file IS the test module.

use std::sync::Arc;

use super::builder_slot_lease::RESTORED_LEASE_AGENT;
use super::builder_slot_lease_tests::{lease_path, seeded_pool, slot, suggested_slot};
use super::core::DaemonState;

/// The agents the census names under `root`, restored leases included.
fn census_agents(state: &DaemonState, root: &std::path::Path) -> Vec<String> {
    let mut agents: Vec<String> = state
        .builder_slot_census_with_pool_root(4, Some(root))
        .holders
        .into_iter()
        .map(|h| h.agent)
        .collect();
    agents.sort();
    agents
}

/// #8819 cap follow-up items 1 to 4: the granting daemon counts A once, by its
/// record; after a restart A's live lease and an unverifiable lease each count
/// as a restored lease, and a dead owner's lease does not count.
#[test]
fn the_census_counts_restored_leases_once_and_skips_stale_ones_8819() {
    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    pool.seed(2, None).expect("a seeded slot 2");
    let pool_root = pool.root().to_path_buf();
    let before = Arc::new(DaemonState::new());
    assert_eq!(suggested_slot(&before, &pool, "toolu_A"), slot(&pool, 0));
    assert_eq!(
        census_agents(&before, &pool_root),
        ["rust-engineer"],
        "a lease whose holder has a record is counted once"
    );
    drop(before);

    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    let now = chrono::Utc::now().timestamp();
    std::fs::write(
        lease_path(&pool, 1),
        format!("holder: toolu_DEAD\ngranted_at: {now}\nowner_pid: {dead}\nowner_start: 1\n"),
    )
    .expect("dead owner's lease");
    std::fs::write(lease_path(&pool, 2), "holder: toolu_X\ngranted_at: soon\n")
        .expect("malformed lease");

    let after = Arc::new(DaemonState::new());
    assert_eq!(
        census_agents(&after, &pool_root),
        [RESTORED_LEASE_AGENT, RESTORED_LEASE_AGENT],
        "A's live lease and the unverifiable one count; the dead owner's does not"
    );
    let holders = after.builder_slot_holders_with_pool_root(None, Some(&pool_root));
    assert_eq!(holders.len(), 2, "the cap counts what the census does");
    let nil = holders.iter().filter(|h| h.session.0.is_nil()).count();
    assert_eq!(
        nil, 1,
        "A's lease names A's session; the unreadable one cannot"
    );
}

/// #8819 critic item 1: after a restart, A's restored lease fills a
/// one-builder cap, and a dispatch with NO pool (a checkout with no GitHub
/// origin) is refused. Before, a pool-less claim counted no restored lease.
#[test]
fn a_claim_with_no_pool_counts_restored_leases_8819() {
    use crate::core::builder_capacity::{Capacity, CapacityReason};
    use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
    use crate::daemon::builder_slot_routes::{BuilderSlotRequest, builder_slot_op_with_pool_root};

    let root = tempfile::tempdir().expect("tempdir");
    let pool = seeded_pool(root.path());
    let before = Arc::new(DaemonState::new());
    assert_eq!(suggested_slot(&before, &pool, "toolu_A"), slot(&pool, 0));
    drop(before);

    let after = Arc::new(DaemonState::new());
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    session.pid = Some(std::process::id());
    let id = session.id;
    after.register_session(session);
    let payload = serde_json::json!({
        "cwd": "/no-origin",
        "tool": "Agent",
        "input": {"subagent_type": "rust-engineer", "description": "build"},
        "tool_use_id": "toolu_B",
    });
    let capacity = Capacity {
        n_effective: 1,
        ceiling: 4,
        reason: CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
    };
    let b = builder_slot_op_with_pool_root(
        &after,
        &id.0.to_string(),
        BuilderSlotRequest { payload },
        &capacity,
        None,
        Some("no pool: the checkout has no git origin identity"),
        Some(pool.root()),
    )
    .expect("a well-formed session id")
    .response;

    assert!(!b.claimed, "the restored lease fills the cap: {b:?}");
    let agents: Vec<&str> = b.holders.iter().map(|h| h.agent.as_str()).collect();
    assert_eq!(agents, [RESTORED_LEASE_AGENT]);
}
