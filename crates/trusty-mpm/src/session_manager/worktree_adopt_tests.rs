//! Unit tests for the #6497 worktree-adoption gate.
//!
//! Why: the verb takes a directory away from whoever the sentinel says owns it,
//! so the refusal arms carry the whole safety argument and each one must fail
//! independently if its gate is deleted.
//! What: one test per gate, plus the two write-path cases.
//! Test: this file IS the test module.

use super::*;
use crate::core::agent::DelegationId;
use crate::core::session::SessionId;
use crate::session_manager::worktree_ownership::AgentWorktreeOwner;
use chrono::Utc;

/// An agent-owned sentinel value, as `read_sentinel_owner` would return it.
fn agent_owner(agent_id: &str) -> SentinelOwner {
    SentinelOwner::Agent(
        AgentWorktreeOwner {
            agent_id: agent_id.to_string(),
            delegation_id: DelegationId::new(),
            parent_session_id: SessionId::new(),
        },
        Utc::now(),
    )
}

/// FAIL-OPEN CHECK, gate 2: a LIVE owner keeps its tree. This is the harm the
/// verb must never cause — adoption is a transfer, and transferring a tree out
/// from under a working agent is the same loss as deleting it.
#[test]
fn adoption_refuses_a_live_owner() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = evaluate_adoption(
        dir.path(),
        &agent_owner("agent-still-working"),
        OwnerLiveness::Alive,
        &[],
    );
    let AdoptionVerdict::Refuse(reason) = verdict else {
        panic!("a live owner's tree must never be adoptable");
    };
    assert!(reason.contains("agent-still-working"), "{reason}");
    assert!(reason.contains("still running"), "{reason}");
}

/// Gate 2, the ADR-0045 half: an owner the registry has merely never heard of
/// is undeterminable, not absent. The delegation map is rebuilt empty at every
/// daemon boot, so its silence proves nothing.
#[test]
fn adoption_refuses_an_owner_the_registry_never_heard_of() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        OwnerLiveness::from_agent_state(AgentDelegationState::Unknown),
        OwnerLiveness::Undeterminable,
    );
    let verdict = evaluate_adoption(
        dir.path(),
        &agent_owner("agent-unheard-of"),
        OwnerLiveness::Undeterminable,
        &[],
    );
    assert!(
        !matches!(verdict, AdoptionVerdict::Adopt),
        "an unanswerable claim must refuse"
    );
}

/// Gate 1: no readable claim means nothing to transfer, and an unreadable
/// sentinel could be hiding a live claim.
#[test]
fn adoption_refuses_an_unreadable_claim() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = evaluate_adoption(
        dir.path(),
        &SentinelOwner::Unknown,
        OwnerLiveness::Dead,
        &[],
    );
    assert!(
        !matches!(verdict, AdoptionVerdict::Adopt),
        "an unreadable sentinel must refuse even when the caller says the owner is dead"
    );
}

/// Gate 3: the sentinel names who PROVISIONED the tree, never everyone who is
/// working in it. A live claimant refuses whatever the sentinel says.
#[test]
fn adoption_refuses_a_tree_something_still_works_in() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = evaluate_adoption(
        dir.path(),
        &agent_owner("agent-that-ended"),
        OwnerLiveness::Dead,
        &["rust-engineer".to_string()],
    );
    let AdoptionVerdict::Refuse(reason) = verdict else {
        panic!("a tree with a live claimant must not be adoptable");
    };
    assert!(reason.contains("rust-engineer"), "{reason}");
}

/// The permitting arm — the #6497 case. Every gate passed: the sentinel is
/// readable, the owner is positively ended, and nothing else claims the tree.
#[test]
fn adoption_takes_a_dead_owners_tree() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        OwnerLiveness::from_agent_state(AgentDelegationState::Ended),
        OwnerLiveness::Dead,
    );
    assert_eq!(
        evaluate_adoption(
            dir.path(),
            &agent_owner("agent-that-ended"),
            OwnerLiveness::Dead,
            &[],
        ),
        AdoptionVerdict::Adopt,
    );
}

/// A session-owned tree adopts by the same rule — the owner shape changes the
/// registry that answers gate 2, never the gate itself.
#[test]
fn adoption_handles_a_session_owned_tree_by_the_same_rule() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SentinelOwner::Known(ManagedSessionId::new(), Utc::now());
    assert_eq!(
        evaluate_adoption(dir.path(), &owner, OwnerLiveness::Dead, &[]),
        AdoptionVerdict::Adopt
    );
    assert!(!matches!(
        evaluate_adoption(dir.path(), &owner, OwnerLiveness::Alive, &[]),
        AdoptionVerdict::Adopt
    ));
}

/// #7974: the reported case — the registry cannot answer after a daemon
/// restart, and the worktree lock still names a pid the kernel does not have.
/// That is positive evidence the agent ended, so adoption proceeds and says
/// what made it proceed.
/// Test: this function IS the test.
#[test]
fn lock_fallback_takes_a_dead_pids_tree() {
    let (liveness, evidence) = liveness_with_lock_fallback(
        OwnerLiveness::Undeterminable,
        LockEvidence::HolderPidGone(17167),
    );
    assert_eq!(liveness, OwnerLiveness::Dead);
    let reason = evidence.expect("an adoption on this path must carry its evidence");
    assert!(reason.contains("17167"), "{reason}");
    assert!(reason.contains("#7974"), "{reason}");
}

/// #7974: a lock whose pid is RUNNING is not evidence of death, and pid reuse
/// means it is not proof of the agent's life either — the refusal stands
/// unchanged, with no evidence line to log.
/// Test: this function IS the test.
#[test]
fn lock_fallback_refuses_a_running_pid() {
    let (liveness, evidence) = liveness_with_lock_fallback(
        OwnerLiveness::Undeterminable,
        LockEvidence::HolderPidRunning(std::process::id()),
    );
    assert_eq!(liveness, OwnerLiveness::Undeterminable);
    assert_eq!(evidence, None);
}

/// #7974, FAIL-OPEN CHECK: a lock that says nothing leaves the ADR-0045
/// refusal exactly where it was. Every way the probe can decline — no git, no
/// lock, an operator lock, no pid, an unrecognised errno — arrives here.
/// Test: this function IS the test.
#[test]
fn lock_fallback_refuses_when_the_lock_says_nothing() {
    let (liveness, evidence) =
        liveness_with_lock_fallback(OwnerLiveness::Undeterminable, LockEvidence::Silent);
    assert_eq!(liveness, OwnerLiveness::Undeterminable);
    assert_eq!(evidence, None);
}

/// #7974, the non-weakening claim: a registry answer of `Alive` survives every
/// lock state, including a lock naming a pid that is gone. A live agent whose
/// dispatched process was replaced — or whose lock is stale — must not lose its
/// tree to this fallback.
/// Test: this function IS the test.
#[test]
fn lock_fallback_never_revives_a_live_owner() {
    for lock in [
        LockEvidence::HolderPidGone(17167),
        LockEvidence::HolderPidRunning(std::process::id()),
        LockEvidence::Silent,
    ] {
        assert_eq!(
            liveness_with_lock_fallback(OwnerLiveness::Alive, lock),
            (OwnerLiveness::Alive, None),
            "a live owner must survive {lock:?}"
        );
    }
}

/// #7974: a registry answer of `Dead` is already sufficient, so the fallback
/// adds nothing — and in particular does not attach an evidence line claiming
/// the lock is what permitted the transfer.
/// Test: this function IS the test.
#[test]
fn lock_fallback_leaves_a_dead_owner_dead() {
    assert_eq!(
        liveness_with_lock_fallback(OwnerLiveness::Dead, LockEvidence::HolderPidRunning(1)),
        (OwnerLiveness::Dead, None)
    );
}

/// The write: after adoption the sentinel names the ADOPTING session, so every
/// gate that reads it now protects the tree for its new owner.
#[test]
fn adopt_worktree_rewrites_the_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(WORKTREE_SENTINEL_FILE), b"{}").unwrap();
    let new_owner = ManagedSessionId::new();

    adopt_worktree(dir.path(), new_owner).expect("adoption must write the sentinel");

    match crate::session_manager::worktree_ownership::read_sentinel_owner(dir.path()) {
        SentinelOwner::Known(id, _) => assert_eq!(id, new_owner),
        other => panic!("the sentinel must name the adopting session; got {other:?}"),
    }
}

/// A path that is not a directory gets no sentinel: writing one would
/// manufacture a claim on a tree that does not exist.
#[test]
fn adopt_worktree_refuses_a_path_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-worktree");
    assert!(adopt_worktree(&missing, ManagedSessionId::new()).is_err());
    assert!(!missing.join(WORKTREE_SENTINEL_FILE).exists());
}
