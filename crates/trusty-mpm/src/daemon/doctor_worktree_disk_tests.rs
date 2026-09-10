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

use tempfile::TempDir;

use super::*;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState};
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_reclaim::{
    BranchPrState, ReclaimCandidate, ReclaimGate, ReclaimVerdict,
};
use crate::session_manager::{SessionManager, tests::make_active_test_record};

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
                gate: ReclaimGate::PrState,
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
                gate: ReclaimGate::PrState,
                reason: "unknown".into(),
            },
        ),
        candidate(
            Some(2048),
            BranchPrState::Unknown,
            ReclaimVerdict::Blocked {
                gate: ReclaimGate::PrState,
                reason: "unknown".into(),
            },
        ),
    ]);
    let check = build_worktree_disk_check(&s);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert_ne!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("gh"), "{}", check.message);
}

/// 🔴 #6867: a SUSPENDED poll reaches the operator through the same clause a
/// broken one does, naming the strike count and when polling resumes.
///
/// Why: the closure condition asks for the backoff to be "surfaced in
/// `tm doctor`". It is surfaced by riding the `lookup_failure` channel #6561
/// already built rather than by adding a second one — this test is what pins
/// that the reason survives the trip verbatim instead of being summarised
/// away.
#[test]
fn worktree_disk_check_reports_a_suspended_gh_poll() {
    let suspended =
        "gh polling suspended: 3 consecutive timeouts, next retry at 14:32:10 EDT".to_string();
    let mut s = survey_of(vec![candidate(
        Some(4096),
        BranchPrState::LookupFailed {
            reason: suspended.clone(),
        },
        ReclaimVerdict::Blocked {
            gate: ReclaimGate::PrState,
            reason: suspended.clone(),
        },
    )]);
    s.lookup_failed = 1;
    s.lookup_failure = Some(suspended.clone());

    let check = build_worktree_disk_check(&s);
    assert!(check.message.contains(&suspended), "{}", check.message);
    assert_ne!(
        check.status,
        CheckStatus::Ok,
        "a survey that could not poll at all must not read healthy"
    );
}

#[test]
fn worktree_disk_check_is_ok_when_nothing_is_reclaimable() {
    // Some PR state DID resolve, and nothing is reclaimable: healthy, but the
    // number is still reported.
    let s = survey_of(vec![candidate(
        Some(3 * 1024 * 1024),
        BranchPrState::Open { pr: 3 },
        ReclaimVerdict::Blocked {
            gate: ReclaimGate::PrState,
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
                gate: ReclaimGate::PrState,
                reason: "open".into(),
            },
        ),
        candidate(
            None,
            BranchPrState::Open { pr: 2 },
            ReclaimVerdict::Blocked {
                gate: ReclaimGate::PrState,
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
                gate: ReclaimGate::PrState,
                reason: "open".into(),
            },
        ),
        candidate(
            None,
            BranchPrState::Open { pr: 2 },
            ReclaimVerdict::Blocked {
                gate: ReclaimGate::PrState,
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
    let check = check_worktree_disk(None, &LiveClaims::default()).await;
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "worktree_disk");
}

#[tokio::test]
async fn worktree_disk_check_is_ok_for_a_missing_repos_root() {
    let missing = PathBuf::from("/nonexistent-repos-root-2919");
    let check = check_worktree_disk(Some(&missing), &LiveClaims::default()).await;
    assert_eq!(check.status, CheckStatus::Ok);
}

// ---------------------------------------------------------------------------
// #7259 — the claim set the probe surveys with
// ---------------------------------------------------------------------------

/// The adopted pane from the #7232 incident: a `deleted` record holding an
/// ORG-level `workspace_path`, one level above every repository under it.
///
/// Why: the record's `state` must never be what decides anything (#2919
/// measured a live session in a terminal-looking record), so the fixture is
/// deliberately terminal — an assertion that passed by reading `state` would
/// pass here for the wrong reason.
/// What: seeds one record under `tmux_name` into a manager backed by
/// `FakeTmuxDriver`, so the test controls exactly which names a
/// `tmux list-sessions` reports.
async fn manager_claiming(
    dir: &TempDir,
    fake: std::sync::Arc<FakeTmuxDriver>,
    tmux_name: &str,
    org_path: &Path,
) -> SessionManager {
    let mgr = SessionManager::new(dir.path(), fake)
        .await
        .expect("manager");
    let mut record = make_active_test_record(
        tmux_name,
        "adopted pane",
        org_path.to_str().expect("utf8 org path"),
    );
    record.id = ManagedSessionId::for_adopted_tmux_name(tmux_name);
    record.state = ManagedSessionState::Deleted;
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert");
    mgr
}

/// Put a worktree in the state a merged pull request leaves behind: one
/// commit, pushed. `commit_all_and_push` runs a real `git commit`, which
/// refuses an empty one, so the file write is required rather than incidental.
fn land(path: &Path) {
    std::fs::write(path.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(path, "landed");
}

/// A pull-request index naming one branch as merged.
fn merged_index(branch: &str, pr: u64) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": {pr}, "headRefName": "{branch}", "state": "MERGED"}}]"#),
        400,
    )
}

/// 🔴 #7259, the reported defect: a tombstoned adopted session claiming an
/// ORG-level path must not hide the orphaned disk beneath it.
///
/// Why: `run_doctor_for_manager` built this probe's claim set from
/// `mgr.list()`, which has no liveness in it — so one `deleted` record for the
/// pane `tm-bobmatnyc`, holding `<repos_root>/<owner>`, covered every worktree
/// in every repository below it and the disk figures counted the whole subtree
/// as in use. On `65d31525b` this worktree comes back blocked at gate 2 and the
/// check reports nothing reclaimable.
/// What: one real merged, clean, pushed worktree under an org-level path that
/// only a dead session claims; tmux answers, and does not list that name.
#[tokio::test]
async fn a_dead_sessions_org_level_claim_no_longer_hides_orphaned_disk() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("doctor-7259");
    land(&wt);
    let org = fx.repo.parent().expect("owner dir above the checkout");

    let dir = TempDir::new().expect("tempdir");
    // Seeded with a DIFFERENT live name: the probe answered, and `tm-bobmatnyc`
    // was not in the answer.
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-someone-else".into());
    let mgr = manager_claiming(&dir, fake, "tm-bobmatnyc", org).await;

    let check = check_worktree_disk_with_index(
        Some(&fx.repos_root),
        &mgr.workspace_claims(None).await,
        |_: &Path| merged_index("session/doctor-7259", 7259),
    )
    .await;

    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "a merged worktree claimed only by a dead session is reclaimable disk: {}",
        check.message
    );
    assert!(
        check.message.contains("prune-worktrees --merged-prs"),
        "the reclaim command must be advertised: {}",
        check.message
    );
}

/// 🔴 #5856 / #7232's fail direction, restated at this probe: an unobservable
/// tmux is not an empty tmux, so the same claim still hides the same worktree.
///
/// Why: the probe reports a number an operator acts on with a DELETE, so a
/// liveness question nobody could answer must resolve toward "in use". This is
/// the assertion that stops the fix above from being implemented as "ignore
/// tombstones", which would discard live sessions' claims during a tmux outage.
#[tokio::test]
async fn an_unobservable_tmux_still_hides_the_same_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("doctor-7259-failclosed");
    land(&wt);
    let org = fx.repo.parent().expect("owner dir above the checkout");

    let dir = TempDir::new().expect("tempdir");
    let fake = FakeTmuxDriver::new();
    let mgr = manager_claiming(&dir, fake.clone(), "tm-bobmatnyc", org).await;
    *fake.list_sessions_should_fail.lock().unwrap() = true;

    let check = check_worktree_disk_with_index(
        Some(&fx.repos_root),
        &mgr.workspace_claims(None).await,
        |_: &Path| merged_index("session/doctor-7259-failclosed", 7259),
    )
    .await;

    assert_ne!(
        check.status,
        CheckStatus::Warn,
        "a probe that could not read tmux must keep the claim, not reclaim under it: {}",
        check.message
    );
    assert!(
        !check.message.contains("prune-worktrees --merged-prs"),
        "nothing may be advertised as reclaimable here: {}",
        check.message
    );
}

#[test]
fn worktree_disk_timeout_is_a_bounded_constant() {
    // The worst case — budget plus grace — must clear the client's 10-second
    // `DEFAULT_REQUEST_TIMEOUT`, or this probe breaks the very `/api/v1/doctor`
    // response it reports in. A 30-second budget did exactly that, failing
    // `execute_doctor_against_test_daemon` with "daemon unreachable".
    assert!(SURVEY_TIMEOUT.as_secs() > 0, "the probe must do some work");
    // Two phases now — classify, then measure — each bounded by SURVEY_TIMEOUT,
    // plus the subprocess grace. That is the real ceiling the client must clear.
    let worst_case = SURVEY_TIMEOUT + SURVEY_TIMEOUT + SURVEY_TIMEOUT_GRACE;
    assert!(
        worst_case < std::time::Duration::from_secs(10),
        "budget+grace ({worst_case:?}) must stay under the client request timeout"
    );
}
