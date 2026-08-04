//! Tests for merged-PR worktree reclamation (#2919).
//!
//! Why: this module deletes directories. Every gate in [`super::classify`]
//! therefore gets a test that FAILS if that gate is removed — a guard whose
//! absence no test notices is not a guard. The gates are exercised through the
//! real `classify`, against real git worktrees where the check reads git.
//! What: one refusal test per gate (non-admitted, live session, open PR,
//! closed-unmerged PR, no PR, indeterminate PR state, dirty tree, unpushed
//! commits), the single approval path, the [`super::PrIndex`]
//! truncation/availability rules that decide when "absent" is allowed to mean
//! "no PR", and the `Report`-mode and re-check behaviour of the removal path.

use std::path::{Path, PathBuf};

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_safety::inspect_dirt;

/// A dirt probe that always reports CLEAN — used only where the test's subject
/// is a gate ABOVE the dirt gate, so a real probe would add nothing.
fn clean(_: &Path) -> Option<DirtyWorktree> {
    None
}

/// A dirt probe that always reports dirty, standing in for any of the many
/// ways `inspect_dirt` fails toward dirty.
fn dirty(path: &Path) -> Option<DirtyWorktree> {
    Some(DirtyWorktree::new(path, "2 modified files", 2, 0))
}

/// Put the worktree in the state a merged PR leaves behind: one commit, pushed.
///
/// The file write is required rather than incidental — `commit_all_and_push`
/// runs a real `git commit`, which refuses an empty commit, and a fresh
/// `git worktree add` has nothing staged.
fn land(path: &Path) {
    std::fs::write(path.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(path, "landed");
}

fn merged(pr: u64) -> BranchPrState {
    BranchPrState::Merged { pr }
}

/// A synthetic path that satisfies the OWNERSHIP gate, so tests aimed at the
/// gates BELOW it are not short-circuited there.
///
/// `is_session_worktree` is a pure path predicate — the `.worktrees/<name>`
/// shape is what it matches, and nothing needs to exist on disk.
fn wt() -> PathBuf {
    PathBuf::from("/tmp/.worktrees/worktree-2919")
}

fn reason(v: &ReclaimVerdict) -> String {
    match v {
        ReclaimVerdict::Blocked { reason } => reason.clone(),
        ReclaimVerdict::Reclaimable { pr } => panic!("expected Blocked, got Reclaimable {pr}"),
    }
}

// ---------------------------------------------------------------------------
// Gate 1 — git's own admission verdict
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_non_admitted_worktree() {
    // Every non-Admitted verdict must refuse, including the operator's explicit
    // `git worktree lock` and the repository's own main checkout. Deleting the
    // main checkout is the 2026-07-21 incident in miniature.
    for admission in [
        Admission::MainCheckout,
        Admission::Bare,
        Admission::Locked,
        Admission::Prunable,
        Admission::Unresolvable,
        Admission::OutsideProject,
        Admission::OutsideReposRoot,
    ] {
        let v = classify(&wt(), admission, false, &merged(1), &clean);
        assert!(
            !v.is_reclaimable(),
            "{admission:?} must never be reclaimable, even with a merged PR"
        );
        assert_eq!(reason(&v), admission.reason());
    }
}

// ---------------------------------------------------------------------------
// Gate 2 — a live session's workspace
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_live_session_workspace() {
    // The strongest gate: a merged PR plus a clean tree still loses to a
    // session that claims the path. A live session can sit in a directory whose
    // record reads terminal — measured on this repo 2026-07-28.
    let v = classify(&wt(), Admission::Admitted, true, &merged(42), &clean);
    assert!(!v.is_reclaimable());
    assert!(
        reason(&v).contains("claims this workspace"),
        "{}",
        reason(&v)
    );
}

#[test]
fn live_check_matches_exact_ancestor_and_descendant_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    let candidate = root.join("wt");
    let inside = candidate.join("nested");
    std::fs::create_dir_all(&inside).expect("mkdir");

    assert!(
        is_live(&candidate, std::slice::from_ref(&candidate)),
        "exact match"
    );
    assert!(
        is_live(&candidate, std::slice::from_ref(&inside)),
        "a session sitting INSIDE the candidate must protect it"
    );
    assert!(
        is_live(&inside, std::slice::from_ref(&candidate)),
        "a candidate inside a claimed path must be protected"
    );
    assert!(
        !is_live(&candidate, &[root.join("unrelated")]),
        "an unrelated sibling must not protect it"
    );
    assert!(!is_live(&candidate, &[]), "nothing claimed means not live");
}

// ---------------------------------------------------------------------------
// Gate 3 — landing evidence
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_open_pr() {
    let v = classify(
        &wt(),
        Admission::Admitted,
        false,
        &BranchPrState::Open { pr: 9 },
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("#9"), "{}", reason(&v));
}

#[test]
fn classify_blocks_closed_unmerged_pr() {
    // Closed-without-merging is NOT landing evidence — the branch may hold the
    // only copy of abandoned-but-wanted work.
    let v = classify(
        &wt(),
        Admission::Admitted,
        false,
        &BranchPrState::ClosedUnmerged { pr: 11 },
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("without merging"), "{}", reason(&v));
}

#[test]
fn classify_blocks_no_pr() {
    let v = classify(
        &wt(),
        Admission::Admitted,
        false,
        &BranchPrState::NoPr,
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("no pull request"), "{}", reason(&v));
}

#[test]
fn classify_blocks_unknown_pr_state() {
    // THE indeterminate case. An unanswerable probe must be a skip, never a
    // delete — this is the fail-closed property the whole module rests on.
    let v = classify(
        &wt(),
        Admission::Admitted,
        false,
        &BranchPrState::Unknown,
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(
        reason(&v).contains("could not be determined"),
        "{}",
        reason(&v)
    );
}

// ---------------------------------------------------------------------------
// Gate 4 — unsaved work
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_dirty_worktree() {
    // A merged PR does not prove the directory holds nothing novel. The
    // 2026-07-21 salvage found merged-PR worktrees carrying real unpushed
    // source; this gate is why they survive.
    let v = classify(&wt(), Admission::Admitted, false, &merged(5), &dirty);
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("unsaved work"), "{}", reason(&v));
}

#[test]
fn classify_blocks_a_really_dirty_worktree() {
    // Same gate, but against REAL git rather than a stub probe: an uncommitted
    // file in a real worktree must block even with a merged PR.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("dirty-2919");
    std::fs::write(path.join("scratch.rs"), "fn main() {}\n").expect("write untracked file");
    let v = classify(&path, Admission::Admitted, false, &merged(5), &inspect_dirt);
    assert!(
        !v.is_reclaimable(),
        "a real untracked file must block: {v:?}"
    );
}

#[test]
fn classify_blocks_a_worktree_with_unpushed_commits() {
    // The case that LOOKS safe — the work IS committed — but is destroyed
    // anyway, because removal deletes the session branch that held it.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("unpushed-2919");
    GitWorktreeFixture::commit_unpushed(&path);
    let v = classify(&path, Admission::Admitted, false, &merged(6), &inspect_dirt);
    assert!(!v.is_reclaimable(), "an unpushed commit must block: {v:?}");
}

// ---------------------------------------------------------------------------
// The one path that says yes
// ---------------------------------------------------------------------------

#[test]
fn classify_allows_clean_pushed_merged_worktree() {
    // The approval path must actually be reachable, or every refusal test above
    // would pass against a function that refuses unconditionally.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("clean-2919");
    land(&path);
    let v = classify(
        &path,
        Admission::Admitted,
        false,
        &merged(77),
        &inspect_dirt,
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 77 });
}

// ---------------------------------------------------------------------------
// PrIndex — when "absent" is allowed to mean "no PR"
// ---------------------------------------------------------------------------

const ROWS: &str = r#"[
  {"number": 1, "headRefName": "feat/merged", "state": "MERGED"},
  {"number": 2, "headRefName": "feat/open", "state": "OPEN"},
  {"number": 3, "headRefName": "feat/closed", "state": "CLOSED"}
]"#;

#[test]
fn pr_index_reads_merged_open_and_closed_rows() {
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(
        idx.state_for(Some("feat/merged")),
        BranchPrState::Merged { pr: 1 }
    );
    assert_eq!(
        idx.state_for(Some("feat/open")),
        BranchPrState::Open { pr: 2 }
    );
    assert_eq!(
        idx.state_for(Some("feat/closed")),
        BranchPrState::ClosedUnmerged { pr: 3 }
    );
}

#[test]
fn pr_index_absent_branch_is_no_pr_when_complete() {
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(
        idx.state_for(Some("feat/never-had-one")),
        BranchPrState::NoPr
    );
}

#[test]
fn pr_index_truncated_reply_makes_absent_branches_unknown() {
    // A reply that filled the page may have dropped this branch's merged PR —
    // or its OPEN one. Absent must therefore mean "unknown", which blocks.
    let idx = PrIndex::from_json(ROWS, 3);
    assert_eq!(
        idx.state_for(Some("feat/never-had-one")),
        BranchPrState::Unknown
    );
    // Rows that DID come back are still trustworthy.
    assert_eq!(
        idx.state_for(Some("feat/merged")),
        BranchPrState::Merged { pr: 1 }
    );
}

#[test]
fn pr_index_unavailable_makes_every_branch_unknown() {
    let idx = PrIndex::unavailable();
    assert_eq!(idx.state_for(Some("anything")), BranchPrState::Unknown);
}

#[test]
fn pr_index_malformed_json_is_unavailable() {
    // A `gh` that printed a warning, an auth prompt, or nothing at all must not
    // parse into an empty-but-complete index (which would read as "no PRs
    // anywhere" and make every branch NoPr).
    for junk in ["", "not json", "{}", "gh: not authenticated"] {
        let idx = PrIndex::from_json(junk, 400);
        assert_eq!(
            idx.state_for(Some("feat/x")),
            BranchPrState::Unknown,
            "{junk:?} must not yield a usable index"
        );
    }
}

#[test]
fn pr_index_detached_worktree_is_unknown() {
    // No branch means no pull request can prove the work landed.
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(idx.state_for(None), BranchPrState::Unknown);
}

#[test]
fn pr_index_open_pr_beats_a_merged_one_on_the_same_branch() {
    // Branch reuse: PR #1 merged, then the branch was pushed again and PR #2
    // opened. Reclaiming on #1 would delete live work.
    let rows = r#"[
      {"number": 1, "headRefName": "feat/reused", "state": "MERGED"},
      {"number": 2, "headRefName": "feat/reused", "state": "OPEN"}
    ]"#;
    assert_eq!(
        PrIndex::from_json(rows, 400).state_for(Some("feat/reused")),
        BranchPrState::Open { pr: 2 }
    );
    // Order must not matter.
    let flipped = r#"[
      {"number": 2, "headRefName": "feat/reused", "state": "OPEN"},
      {"number": 1, "headRefName": "feat/reused", "state": "MERGED"}
    ]"#;
    assert_eq!(
        PrIndex::from_json(flipped, 400).state_for(Some("feat/reused")),
        BranchPrState::Open { pr: 2 }
    );
}

#[test]
fn pr_index_unrecognised_state_is_not_treated_as_merged() {
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "DRAFT_SOMETHING"}]"#;
    let idx = PrIndex::from_json(rows, 400);
    assert_ne!(
        idx.state_for(Some("feat/x")),
        BranchPrState::Merged { pr: 1 }
    );
}

#[test]
fn gh_command_strips_repository_redirecting_env() {
    // `gh` resolves the repository through git, so an inherited GIT_DIR would
    // aim the PR query at a different repository entirely.
    let cmd = gh_command(Path::new("/tmp"));
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GH_REPO"] {
        assert!(
            removed.contains(&key),
            "{key} must be stripped: {removed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Byte measurement
// ---------------------------------------------------------------------------

#[test]
fn measure_bytes_counts_file_contents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a"), vec![0u8; 100]).expect("write a");
    std::fs::create_dir(tmp.path().join("sub")).expect("mkdir");
    std::fs::write(tmp.path().join("sub").join("b"), vec![0u8; 50]).expect("write b");
    assert_eq!(measure_bytes_until(tmp.path(), None), Some(150));
}

#[test]
fn measure_bytes_of_missing_path_is_none() {
    assert_eq!(
        measure_bytes_until(Path::new("/nonexistent-2919"), None),
        None
    );
}

#[test]
fn measure_bytes_stops_at_an_expired_deadline() {
    // The walk must bound ITSELF. `tokio::time::timeout` cannot cancel a
    // `spawn_blocking` task, so an outer timeout leaves the walk running and
    // runtime shutdown waits for it — measured as a >20-minute hang against a
    // real ~1 TiB worktree store while building this change.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a"), vec![0u8; 100]).expect("write a");
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    assert_eq!(
        measure_bytes_until(tmp.path(), Some(expired)),
        None,
        "an expired deadline must abandon the walk, not measure it"
    );
    // A deadline comfortably in the future still measures normally.
    let ample = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(measure_bytes_until(tmp.path(), Some(ample)), Some(100));
}

#[test]
fn classify_blocks_a_worktree_trusty_mpm_does_not_own() {
    // #2919 HIGH: `remove_session_worktree` refuses any path with no ownership
    // sentinel that is not under `.worktrees/`, so classifying one as
    // `Reclaimable` made `tm doctor` advertise a command that then failed and
    // left the directory on disk. The classifier now applies exactly the
    // predicate the remover applies.
    let fx = GitWorktreeFixture::new();
    let parent = fx.repo.join(".claude").join("worktrees");
    let path = fx.add_worktree_at(&parent, "harness-owned-2919");
    let v = classify(&path, Admission::Admitted, false, &merged(9), &clean);
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(reason(&v).contains("out of scope"), "{}", reason(&v));
}

#[test]
fn the_remover_really_refuses_what_tm_provisioned_rejects() {
    // Ties the classifier's predicate to the REMOVER's actual behaviour rather
    // than to a copy of its rules. Without this the two can drift silently and
    // the classifier goes back to advertising worktrees that cannot be removed.
    let fx = GitWorktreeFixture::new();
    let parent = fx.repo.join(".claude").join("worktrees");
    let path = fx.add_worktree_at(&parent, "remover-refuses-2919");
    assert!(
        !tm_provisioned(&path),
        "precondition: the classifier rejects this shape"
    );
    let removed = crate::session_manager::decommission::remove_session_worktree(&path);
    assert!(!removed.removed(), "the remover must refuse it too");
    assert!(
        path.exists(),
        "and must leave the directory on disk — which is exactly why \
         classifying it Reclaimable produced a `removal_failed` report"
    );
}

#[test]
fn tm_provisioned_matches_the_removers_own_predicate() {
    // Both tiers `remove_session_worktree` accepts must be accepted here, or
    // the classifier and the remover disagree in the OTHER direction and real
    // reclaimable worktrees are silently skipped forever.
    let fx = GitWorktreeFixture::new();
    let under_worktrees = fx.add_worktree("convention-2919");
    assert!(
        tm_provisioned(&under_worktrees),
        "the `.worktrees/<name>` convention must be accepted"
    );

    let parked = fx.add_worktree_at(&fx.repo.join("elsewhere"), "sentinel-2919");
    assert!(
        !tm_provisioned(&parked),
        "no sentinel, not under .worktrees/"
    );
    GitWorktreeFixture::stamp_reclaimable_sentinel(&parked);
    assert!(
        tm_provisioned(&parked),
        "an ownership sentinel must be accepted wherever the worktree is parked"
    );
}

#[test]
fn pr_index_skips_fork_pull_requests() {
    // `headRefName` is not an identity. A fork's `fix/foo` is a different
    // branch from this repository's `fix/foo`; attributing the fork's merge to
    // a local worktree would authorise deleting work that never landed here.
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "MERGED",
                    "isCrossRepository": true}]"#;
    let idx = PrIndex::from_json(rows, 400);
    assert_eq!(
        idx.state_for(Some("feat/x")),
        BranchPrState::NoPr,
        "a fork PR must not make a local branch look merged"
    );
    assert!(
        !classify(
            &wt(),
            Admission::Admitted,
            false,
            &idx.state_for(Some("feat/x")),
            &clean
        )
        .is_reclaimable(),
        "and it must not authorise a delete"
    );
}

#[test]
fn pr_index_keeps_a_same_repo_pull_request() {
    // The complement: the skip must be scoped to fork rows only, or every PR
    // is discarded and nothing is ever reclaimable.
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "MERGED",
                    "isCrossRepository": false}]"#;
    assert_eq!(
        PrIndex::from_json(rows, 400).state_for(Some("feat/x")),
        BranchPrState::Merged { pr: 1 }
    );
}

#[test]
fn run_with_timeout_captures_output() {
    let mut cmd = std::process::Command::new("echo");
    cmd.arg("hello");
    let (ok, out) = run_with_timeout(cmd, std::time::Duration::from_secs(5))
        .expect("a fast command must complete");
    assert!(ok);
    assert_eq!(out.trim(), "hello");
}

#[test]
fn run_with_timeout_kills_a_hung_child() {
    // `Command::output()` has no timeout, so a wedged `gh` would hang the
    // blocking task forever — the same uncancellable-`spawn_blocking` shape
    // already fixed for the byte walk.
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let started = std::time::Instant::now();
    let result = run_with_timeout(cmd, std::time::Duration::from_millis(300));
    assert!(result.is_none(), "a timed-out child must report failure");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the timeout must actually fire; took {:?}",
        started.elapsed()
    );
}

#[test]
fn pr_state_for_branch_maps_a_failed_call_to_unknown() {
    // Any `gh` failure must block, not read as "this branch has no PR".
    //
    // NOTE on what this does and does not prove: an earlier version of this
    // comment claimed the failure came from `gh` being unable to resolve a
    // repository here. That was wrong, and the wrongness mattered — while the
    // module passed a bogus `-C` flag, `gh` died at FLAG PARSING and this test
    // was green for that reason instead, so it would have passed identically
    // against a perfectly valid repository. It pins the failure-to-`Unknown`
    // mapping only. `gh_command_passes_no_dash_c_flag` pins the argv, and
    // `pr_index_from_gh_reads_this_repository` proves a real call succeeds.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        pr_state_for_branch(tmp.path(), "feat/whatever"),
        BranchPrState::Unknown
    );
}

/// `gh` has no `-C` flag, and passing one made every call in this module fail
/// at flag parsing — silently, because a failed call blocks rather than errors.
///
/// Why this is an argv test and not a behaviour test: the behaviour of the bug
/// was "nothing is ever reclaimable", which is indistinguishable from a correct
/// run against a workspace with no merged-PR worktrees. Only the argv
/// distinguishes them.
#[test]
fn gh_command_passes_no_dash_c_flag() {
    let cmd = gh_command(Path::new("/tmp"));
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !args.iter().any(|a| a == "-C"),
        "`gh` has no -C flag (verified against gh 2.96.0: `unknown shorthand \
         flag: 'C' in -C`); argv was {args:?}"
    );
    assert!(
        args.is_empty(),
        "gh_command must contribute no positional args of its own; got {args:?}"
    );
}

#[test]
fn gh_command_runs_in_the_requested_directory() {
    // The repository is resolved from the working directory, so this IS the
    // repository selection — if it stops being set, every call silently
    // resolves whatever repository the daemon happens to be sitting in.
    let cmd = gh_command(Path::new("/tmp"));
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp")));
}

/// Prove a real `gh` call SUCCEEDS and parses — the coverage whose absence let
/// the `-C` bug ship.
///
/// Skips (rather than fails) unless `gh` resolves this exact upstream
/// repository. A contributor working on a FORK resolves their own repo, which
/// legitimately has zero pull requests, and an assertion there would go red for
/// a reason that has nothing to do with this module — and a test that is red on
/// a fork gets deleted by whoever hits it. Gating on the repository identity,
/// rather than on "did we get rows", keeps the assertion meaningful wherever it
/// DOES run.
#[test]
fn pr_index_from_gh_reads_this_repository() {
    const UPSTREAM: &str = "bobmatnyc/trusty-tools";
    if which::which("gh").is_err() {
        eprintln!("skipping: `gh` not on PATH");
        return;
    }
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let mut probe = std::process::Command::new("gh");
    probe.current_dir(repo_root).args([
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ]);
    let resolved = match run_with_timeout(probe, std::time::Duration::from_secs(10)) {
        Some((true, out)) => out.trim().to_string(),
        _ => {
            eprintln!("skipping: `gh` cannot resolve a repository here");
            return;
        }
    };
    if resolved != UPSTREAM {
        eprintln!("skipping: resolved {resolved:?}, not the upstream {UPSTREAM:?}");
        return;
    }
    let index = PrIndex::from_gh(repo_root);
    assert!(
        index.branch_count() > 0,
        "a successful `gh pr list` against {UPSTREAM} must yield branches; an empty \
         index here means the call failed and every branch will block — which is \
         exactly what the `gh -C` argv bug produced"
    );
}
