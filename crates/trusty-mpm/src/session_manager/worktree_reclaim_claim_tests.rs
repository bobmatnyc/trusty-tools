//! Tests for gate 2's claim resolution (#2919, #6806).
//!
//! Why: this is the gate that refused 31 of a session's own clean, pushed,
//! merged worktrees. Each variant of [`ClaimState`] therefore gets a test that
//! fails if its precedence is dropped — foreign over caller, caller-workspace
//! over caller-nested — plus the containment matching inherited from the
//! `is_live` predicate this module replaces.

use super::*;

/// A real directory tree, so `canonicalize` succeeds on both spellings.
fn tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    (tmp, root)
}

#[test]
fn claim_state_matches_exact_ancestor_and_descendant_paths() {
    // The #2919 containment rule, carried over unchanged: matching happens in
    // BOTH directions. Only the ANSWER's shape changed in #6806.
    let (_tmp, root) = tree();
    let candidate = root.join("wt");
    let inside = candidate.join("nested");
    std::fs::create_dir_all(&inside).expect("mkdir");

    let exact = LiveClaims::foreign(vec![WorkspaceClaim::new("s1", &candidate)]);
    assert!(
        exact.claim_state(&candidate).refusal(false).is_some(),
        "exact match must block"
    );

    let claim_inside = LiveClaims::foreign(vec![WorkspaceClaim::new("s1", &inside)]);
    assert!(
        claim_inside
            .claim_state(&candidate)
            .refusal(false)
            .is_some(),
        "a session sitting INSIDE the candidate must protect it"
    );

    let claim_outer = LiveClaims::foreign(vec![WorkspaceClaim::new("s1", &candidate)]);
    assert!(
        claim_outer.claim_state(&inside).refusal(false).is_some(),
        "a candidate inside a claimed path must be protected"
    );

    let unrelated = LiveClaims::foreign(vec![WorkspaceClaim::new("s1", root.join("unrelated"))]);
    assert_eq!(
        unrelated.claim_state(&candidate),
        ClaimState::Unclaimed,
        "an unrelated sibling must not protect it"
    );
    assert_eq!(
        LiveClaims::default().claim_state(&candidate),
        ClaimState::Unclaimed,
        "nothing claimed means not live"
    );
}

/// #6806: the whole point. A worktree nested inside the CALLER's workspace is
/// claimed only by the caller, and must fall through gate 2.
#[test]
fn claims_from_the_caller_alone_do_not_block() {
    let (_tmp, root) = tree();
    let workspace = root.join("client");
    let worktree = workspace.join(".worktrees").join("rb-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new("tm-client-03", &workspace)],
        caller: Some("tm-client-03".to_string()),
    };
    assert_eq!(
        claims.claim_state(&worktree),
        ClaimState::CallerNested {
            session: "tm-client-03".to_string()
        }
    );
    assert!(
        claims.claim_state(&worktree).refusal(false).is_none(),
        "the caller's own claim must not block its own nested worktree"
    );
}

/// The guard that keeps #6806 from becoming a self-deletion: a caller may
/// prune worktrees INSIDE its workspace, never the workspace itself.
#[test]
fn a_caller_may_not_reclaim_its_own_workspace() {
    let (_tmp, root) = tree();
    let workspace = root.join("client");
    std::fs::create_dir_all(&workspace).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new("tm-client-03", &workspace)],
        caller: Some("tm-client-03".to_string()),
    };
    let reason = claims
        .claim_state(&workspace)
        .refusal(false)
        .expect("a caller's own workspace must still refuse");
    assert!(reason.contains("tm-client-03"), "{reason}");
    assert!(reason.contains("IS the caller"), "{reason}");

    // Same refusal when the candidate CONTAINS the caller's workspace.
    let reason = claims
        .claim_state(&root)
        .refusal(false)
        .expect("a candidate containing the caller's workspace must refuse");
    assert!(reason.contains("IS the caller"), "{reason}");
}

/// The pre-#6806 protection, kept: another live session's claim still blocks.
#[test]
fn a_foreign_sessions_claim_still_blocks() {
    let (_tmp, root) = tree();
    let worktree = root.join("client").join(".worktrees").join("rb-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new("tm-other-01", root.join("client"))],
        caller: Some("tm-client-03".to_string()),
    };
    assert!(
        claims.claim_state(&worktree).refusal(false).is_some(),
        "a live session that is NOT the caller must still block"
    );
}

/// #6806 closure criterion 2: name the claimant, and say it is not the caller.
#[test]
fn a_foreign_refusal_names_the_claimant_and_denies_it_is_the_caller() {
    let (_tmp, root) = tree();
    let worktree = root.join("wt");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new("tm-other-01", &worktree)],
        caller: Some("tm-client-03".to_string()),
    };
    let reason = claims
        .claim_state(&worktree)
        .refusal(false)
        .expect("a foreign claim must refuse");
    assert!(
        reason.contains("tm-other-01"),
        "names the claimant: {reason}"
    );
    assert!(
        reason.contains("tm-client-03"),
        "names the caller: {reason}"
    );
    assert!(
        reason.contains("not the calling session"),
        "says it is not the caller: {reason}"
    );
}

/// A caller with no identity is told so, rather than being told the claim is
/// "not yours" — nobody supplied an identity to compare against.
#[test]
fn a_foreign_refusal_says_when_the_caller_named_no_session() {
    let (_tmp, root) = tree();
    let worktree = root.join("wt");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims::foreign(vec![WorkspaceClaim::new("tm-other-01", &worktree)]);
    let reason = claims
        .claim_state(&worktree)
        .refusal(false)
        .expect("a foreign claim must refuse");
    assert!(reason.contains("tm-other-01"), "{reason}");
    assert!(reason.contains("named no session of its own"), "{reason}");
}

/// Precedence: a foreign claim outranks the caller's own, whatever the order
/// the store returns them in.
#[test]
fn a_foreign_claim_outranks_the_callers_own() {
    let (_tmp, root) = tree();
    let workspace = root.join("client");
    let worktree = workspace.join(".worktrees").join("rb-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    for order in [0usize, 1] {
        let mut claims = vec![
            WorkspaceClaim::new("tm-client-03", &workspace),
            WorkspaceClaim::new("tm-other-01", &worktree),
        ];
        if order == 1 {
            claims.reverse();
        }
        let live = LiveClaims {
            claims,
            caller: Some("tm-client-03".to_string()),
        };
        let state = live.claim_state(&worktree);
        assert!(
            matches!(state, ClaimState::Foreign { .. }),
            "order {order}: foreign must win, got {state:?}"
        );
    }
}

/// The re-check wording keeps its present tense, so the operator can tell a
/// survey refusal from one taken immediately before a deletion.
#[test]
fn the_recheck_wording_is_present_tense() {
    let (_tmp, root) = tree();
    let worktree = root.join("wt");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims::foreign(vec![WorkspaceClaim::new("tm-other-01", &worktree)]);
    let state = claims.claim_state(&worktree);
    assert!(
        state
            .refusal(true)
            .expect("refuses")
            .contains("claims this workspace now"),
        "{:?}",
        state.refusal(true)
    );
    assert!(
        state
            .refusal(false)
            .expect("refuses")
            .contains("still claims this workspace"),
        "{:?}",
        state.refusal(false)
    );
}
