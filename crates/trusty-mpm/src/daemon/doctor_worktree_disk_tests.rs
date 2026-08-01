//! Tests for the `worktree_disk` doctor probe (#2919).
//!
//! Why: the probe's value is entirely in WHICH verdict it picks — an operator
//! who sees `Ok` next to a terabyte learns nothing, and one who sees `Unknown`
//! every run stops reading the report. Those branches are pinned here against
//! hand-built surveys so no repos root, `gh`, or terabyte of fixtures is
//! needed.
//! What: the pure `build_worktree_disk_check` verdicts, the byte formatter, and
//! the two "nothing to survey" short-circuits in `check_worktree_disk`.

use std::path::PathBuf;

use super::*;
use crate::session_manager::worktree_reclaim::{BranchPrState, ReclaimCandidate, ReclaimVerdict};

/// Build a survey holding `candidates`, with the totals derived the same way
/// the real survey derives them.
fn survey_of(candidates: Vec<ReclaimCandidate>) -> ReclaimSurvey {
    let mut s = ReclaimSurvey::default();
    for c in &candidates {
        match c.bytes {
            Some(b) => {
                s.total_bytes += b;
                if matches!(c.verdict, ReclaimVerdict::Reclaimable { .. }) {
                    s.reclaimable_bytes += b;
                }
            }
            None => s.unmeasured += 1,
        }
        if matches!(c.verdict, ReclaimVerdict::Reclaimable { .. }) {
            s.reclaimable += 1;
        } else {
            s.blocked += 1;
        }
        if c.pr == BranchPrState::Unknown {
            s.pr_state_unknown += 1;
        }
    }
    s.candidates = candidates;
    s
}

fn candidate(bytes: Option<u64>, pr: BranchPrState, verdict: ReclaimVerdict) -> ReclaimCandidate {
    ReclaimCandidate {
        path: PathBuf::from("/tmp/wt"),
        branch: Some("feat/x".into()),
        registry_root: PathBuf::from("/tmp"),
        bytes,
        pr,
        verdict,
    }
}

#[test]
fn human_bytes_renders_binary_units() {
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(1024 * 1024 * 3), "3.0 MiB");
    // The figure from the 2026-07-21 post-mortem must render as terabytes,
    // not as an unreadable integer.
    assert_eq!(human_bytes(1_209_462_790_553), "1.1 TiB");
}

#[test]
fn worktree_disk_check_is_ok_with_no_worktrees() {
    let check = build_worktree_disk_check(&survey_of(vec![]));
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "worktree_disk");
}

#[test]
fn worktree_disk_check_warns_when_bytes_are_reclaimable() {
    // Two worktrees, one reclaimable: the operator must see BOTH the total and
    // the reclaimable share, plus the command that acts on it.
    let s = survey_of(vec![
        candidate(
            Some(4 * 1024 * 1024 * 1024),
            BranchPrState::Merged { pr: 7 },
            ReclaimVerdict::Reclaimable { pr: 7 },
        ),
        candidate(
            Some(2 * 1024 * 1024 * 1024),
            BranchPrState::Open { pr: 9 },
            ReclaimVerdict::Blocked {
                reason: "PR #9 is still open".into(),
            },
        ),
    ]);
    let check = build_worktree_disk_check(&s);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("6.0 GiB"), "{}", check.message);
    assert!(check.message.contains("4.0 GiB"), "{}", check.message);
    assert!(
        check.message.contains("prune-worktrees --merged-prs"),
        "must name the remediation command: {}",
        check.message
    );
}

#[test]
fn worktree_disk_check_is_unknown_when_no_pr_state_resolved() {
    // A survey that resolved nothing has NOT established the workspace is
    // healthy. Reporting Ok here would be the fail-open shape #2919 is about:
    // a `gh` that is missing or unauthenticated would make every worktree
    // unclassifiable and the probe would read green forever.
    let s = survey_of(vec![
        candidate(
            Some(1024),
            BranchPrState::Unknown,
            ReclaimVerdict::Blocked {
                reason: "unknown".into(),
            },
        ),
        candidate(
            Some(2048),
            BranchPrState::Unknown,
            ReclaimVerdict::Blocked {
                reason: "unknown".into(),
            },
        ),
    ]);
    let check = build_worktree_disk_check(&s);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert_ne!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("gh"), "{}", check.message);
}

#[test]
fn worktree_disk_check_is_ok_when_nothing_is_reclaimable() {
    // Some PR state DID resolve, and nothing is reclaimable: healthy, but the
    // number is still reported.
    let s = survey_of(vec![candidate(
        Some(3 * 1024 * 1024),
        BranchPrState::Open { pr: 3 },
        ReclaimVerdict::Blocked {
            reason: "PR #3 is still open".into(),
        },
    )]);
    let check = build_worktree_disk_check(&s);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("3.0 MiB"), "{}", check.message);
}

#[test]
fn worktree_disk_check_is_unknown_when_the_walk_is_incomplete() {
    // #2919 HIGH: a 3-second budget against a >600s walk means the figure is a
    // floor. Reporting `Ok` from it inverts the check — the fuller the disk,
    // the less gets measured, and the more confidently it would read healthy.
    let s = survey_of(vec![
        candidate(
            Some(1024),
            BranchPrState::Open { pr: 1 },
            ReclaimVerdict::Blocked {
                reason: "open".into(),
            },
        ),
        candidate(
            None,
            BranchPrState::Open { pr: 2 },
            ReclaimVerdict::Blocked {
                reason: "open".into(),
            },
        ),
    ]);
    let check = build_worktree_disk_check(&s);
    assert_ne!(
        check.status,
        CheckStatus::Ok,
        "a partial walk must never read healthy: {}",
        check.message
    );
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("UNDETERMINED"), "{}", check.message);
}

#[test]
fn worktree_disk_check_flags_an_undercounted_total() {
    // A total assembled from partly-unmeasurable worktrees must say so —
    // reporting it bare would be the "cleanup reported as done when it had not
    // happened" failure the post-mortem's constraint 8 names.
    let s = survey_of(vec![
        candidate(
            Some(1024),
            BranchPrState::Open { pr: 1 },
            ReclaimVerdict::Blocked {
                reason: "open".into(),
            },
        ),
        candidate(
            None,
            BranchPrState::Open { pr: 2 },
            ReclaimVerdict::Blocked {
                reason: "open".into(),
            },
        ),
    ]);
    let check = build_worktree_disk_check(&s);
    assert!(
        check.message.contains("UNDERCOUNT"),
        "an unmeasured worktree must be disclosed: {}",
        check.message
    );
}

#[tokio::test]
async fn worktree_disk_check_is_ok_without_a_repos_root() {
    let check = check_worktree_disk(None, &[]).await;
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "worktree_disk");
}

#[tokio::test]
async fn worktree_disk_check_is_ok_for_a_missing_repos_root() {
    let missing = PathBuf::from("/nonexistent-repos-root-2919");
    let check = check_worktree_disk(Some(&missing), &[]).await;
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn worktree_disk_timeout_is_a_bounded_constant() {
    // The worst case — budget plus grace — must clear the client's 10-second
    // `DEFAULT_REQUEST_TIMEOUT`, or this probe breaks the very `/api/v1/doctor`
    // response it reports in. A 30-second budget did exactly that, failing
    // `execute_doctor_against_test_daemon` with "daemon unreachable".
    assert!(SURVEY_TIMEOUT.as_secs() > 0, "the probe must do some work");
    let worst_case = SURVEY_TIMEOUT + SURVEY_TIMEOUT_GRACE;
    assert!(
        worst_case < std::time::Duration::from_secs(10),
        "budget+grace ({worst_case:?}) must stay under the client request timeout"
    );
}
