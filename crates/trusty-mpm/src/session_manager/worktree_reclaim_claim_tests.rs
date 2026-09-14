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

    // #7652: a candidate strictly INSIDE a foreign claim is no longer refused —
    // that claim is about the project, not about this worktree. The direction
    // that still refuses is the one above (the claim inside the candidate), and
    // `worktree_7652_a_foreign_project_root_claim_no_longer_blocks_a_nested_worktree`
    // owns the permitted direction's full assertion.
    let claim_outer = LiveClaims::foreign(vec![WorkspaceClaim::new("s1", &candidate)]);
    assert!(
        claim_outer.claim_state(&inside).refusal(false).is_none(),
        "a project-level claim must not veto a worktree nested under it (#7652)"
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

/// The pre-#6806 protection, kept: another live session's claim ON THIS
/// WORKTREE still blocks.
///
/// #7652 narrowed which foreign claim that is — one that COVERS the candidate,
/// not one that merely contains it — so this fixture claims the worktree
/// itself, which is the shape the protection is actually about.
#[test]
fn a_foreign_sessions_claim_still_blocks() {
    let (_tmp, root) = tree();
    let worktree = root.join("client").join(".worktrees").join("rb-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new("tm-other-01", &worktree)],
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

/// 🔴 #7232: a claim whose session is gone must not decide anything.
///
/// Why: the store tombstones records, so a `deleted` record for the adopted
/// pane `tm-bobmatnyc` — holding the org-level path
/// `~/trusty-mpm-projects/bobmatnyc` — refused every worktree in every project
/// beneath it. The reclaim sweep reported "would remove 0" across seven
/// repositories and named that one session on every candidate. Fails on
/// `ad64460e8`, where `WorkspaceClaim` has no liveness and this is `Foreign`.
#[test]
fn a_dead_sessions_claim_no_longer_blocks() {
    let (_tmp, root) = tree();
    let worktree = root.join("client").join(".worktrees").join("agent-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims::foreign(vec![WorkspaceClaim::with_liveness(
        "51786c9c-478f-5b3f-83a7-cbe63e3d5958",
        &root,
        ClaimLiveness::SessionGone,
    )]);
    let state = claims.claim_state(&worktree);
    assert!(
        !matches!(state, ClaimState::Foreign { .. }),
        "a dead session's claim must not be foreign: {state:?}"
    );
    assert_eq!(state.refusal(false), None, "and must not refuse: {state:?}");
    let note = state
        .note()
        .expect("the decision must name what it discarded");
    assert!(
        note.contains("51786c9c-478f-5b3f-83a7-cbe63e3d5958"),
        "{note}"
    );
    assert!(note.contains("#7232"), "{note}");
}

/// 🔴 The other half of #7232: liveness narrows nothing else. A claim by a
/// session that IS live still refuses exactly as #6806 made it.
///
/// #7652: the claim is on the worktree itself rather than on the project root
/// above it, because a project-root claim is now decided by overlap — see
/// `worktree_7652_a_foreign_project_root_claim_no_longer_blocks_a_nested_worktree`.
/// Liveness is still what this test varies.
#[test]
fn a_live_foreign_sessions_claim_still_blocks() {
    let (_tmp, root) = tree();
    let worktree = root.join("client").join(".worktrees").join("agent-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims::foreign(vec![WorkspaceClaim::with_liveness(
        "tm-other-01",
        &worktree,
        ClaimLiveness::Live,
    )]);
    let state = claims.claim_state(&worktree);
    assert!(
        matches!(state, ClaimState::Foreign { .. }),
        "a live foreign claim must still refuse: {state:?}"
    );
    assert!(state.refusal(false).is_some());
    assert_eq!(state.note(), None, "nothing was discarded");
}

/// A live claim beside a dead one still decides. The discard must not become a
/// way for one tombstone to launder a genuine owner's refusal.
#[test]
fn a_live_claim_beside_a_dead_one_still_blocks() {
    let (_tmp, root) = tree();
    let worktree = root.join("client").join(".worktrees").join("agent-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims::foreign(vec![
        WorkspaceClaim::with_liveness("tm-dead-01", &root, ClaimLiveness::SessionGone),
        WorkspaceClaim::with_liveness("tm-alive-02", &worktree, ClaimLiveness::Live),
    ]);
    let reason = claims
        .claim_state(&worktree)
        .refusal(false)
        .expect("the live claim must still refuse");
    assert!(reason.contains("tm-alive-02"), "{reason}");
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

/// 🔴 #7652: a live foreign session's PROJECT-ROOT claim must not veto a
/// worktree nested under it.
///
/// Why: a session's `workspace_path` is the whole project checkout, so its mere
/// liveness refused every worktree any other session had created under that
/// project — `.claude/worktrees/agent-ae594ecd19bcd72bb`, sitting exactly at the
/// merged head, was refused because unrelated session `0b318c84-…` had the same
/// project registered. A 2026-09-14 reclaim pass then lost all four candidates
/// to the same refusal, and `--force` never reached past it. Fails on
/// `0f2bd5134`, where every `Overlap::Nested` foreign claim returns
/// `ClaimState::Foreign`.
#[test]
fn worktree_7652_a_foreign_project_root_claim_no_longer_blocks_a_nested_worktree() {
    let (_tmp, root) = tree();
    let project = root.join("bobmatnyc").join("trusty-tools");
    let worktree = project.join(".claude").join("worktrees").join("agent-ae59");
    std::fs::create_dir_all(&worktree).expect("mkdir");

    let claims = LiveClaims {
        claims: vec![WorkspaceClaim::new(
            "0b318c84-bae9-4a50-8832-65ed61f8ab22",
            &project,
        )],
        caller: Some("b175bb88-af7f-5ba5-b75d-85795d60b234".to_string()),
    };
    let state = claims.claim_state(&worktree);
    assert_eq!(
        state,
        ClaimState::ForeignNested {
            session: "0b318c84-bae9-4a50-8832-65ed61f8ab22".to_string(),
            caller: Some("b175bb88-af7f-5ba5-b75d-85795d60b234".to_string()),
        },
        "a project-level foreign claim is not an attribution of this worktree"
    );
    assert_eq!(
        state.refusal(false),
        None,
        "gate 2 must let the later gates decide it"
    );
    assert_eq!(
        state.refusal(true),
        None,
        "and the pre-delete re-check must agree, or the two gates disagree"
    );
    let note = state
        .note()
        .expect("a claim that stopped vetoing must say so");
    assert!(
        note.contains("0b318c84-bae9-4a50-8832-65ed61f8ab22"),
        "{note}"
    );
    assert!(note.contains("#7652"), "{note}");

    // The guard this narrowing must NOT remove: the same foreign session
    // claiming THE WORKTREE ITSELF still refuses, and names it.
    let on_the_worktree = LiveClaims {
        claims: vec![WorkspaceClaim::new(
            "0b318c84-bae9-4a50-8832-65ed61f8ab22",
            &worktree,
        )],
        caller: Some("b175bb88-af7f-5ba5-b75d-85795d60b234".to_string()),
    };
    let reason = on_the_worktree
        .claim_state(&worktree)
        .refusal(false)
        .expect("a foreign claim ON this worktree must still refuse");
    assert!(
        reason.contains("0b318c84-bae9-4a50-8832-65ed61f8ab22"),
        "{reason}"
    );
}
