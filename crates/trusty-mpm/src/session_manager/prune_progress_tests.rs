//! #7619: both destructive prune loops stop at the next candidate boundary
//! once cancelled, and never interrupt a removal in flight.
//!
//! Why: before #7619 neither loop read any stop signal, so a killed client
//! left the pass deleting to the end of its list. Each test here removes all of
//! its fixture trees when the loop checks are taken out.
//! What: real `git worktree add` fixtures driven through
//! `prune_orphaned_worktrees_in_with_cancel` and `reclaim_scoped_with_cancel`.
//! Cancel is either state-based (any fixture tree gone means a removal
//! completed) or fired from inside the first removal's last-moment guard.
//! Test: this file is the test module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::PruneCancel;
use crate::session_manager::SessionManager;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{KeepList, LiveClaims, PrIndex, ReclaimMode};
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_scoped_with_cancel};
use crate::session_manager::worktree_safety::DirtyWorktreePolicy;
use crate::session_manager::worktree_scope::WorktreeScope;

/// A cancel that fires once any of `paths` is gone: a removal has completed.
fn cancel_after_first_removal(paths: &[PathBuf]) -> PruneCancel {
    let paths = paths.to_vec();
    PruneCancel::from_probe(move || Some(paths.iter().any(|p| !p.exists())))
}

/// A cancel flag the test sets itself, with the signal that reads it.
fn flag_cancel() -> (Arc<AtomicBool>, PruneCancel) {
    let flag = Arc::new(AtomicBool::new(false));
    let read = Arc::clone(&flag);
    (
        flag,
        PruneCancel::from_probe(move || Some(read.load(Ordering::SeqCst))),
    )
}

async fn manager() -> (SessionManager, tempfile::TempDir) {
    let store = tempfile::tempdir().expect("store dir");
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    (mgr, store)
}

/// `n` orphans the sweep would remove: owner provably gone, clean.
fn orphans(fx: &GitWorktreeFixture, stem: &str, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let wt = fx.add_worktree(&format!("{stem}-{i}"));
            GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
            wt
        })
        .collect()
}

async fn sweep(
    mgr: &SessionManager,
    fx: &GitWorktreeFixture,
    cancel: &PruneCancel,
) -> crate::session_manager::prune::OrphanSweepOutcome {
    mgr.prune_orphaned_worktrees_in_with_cancel(
        &fx.repos_root,
        &[],
        false,
        DirtyWorktreePolicy::Skip,
        &[],
        &WorktreeScope::all(),
        cancel,
    )
    .await
    .expect("the sweep must not error")
}

fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    v.sort();
    v
}

/// 🔴 Plan 4(b), orphan loop: cancelled after the first removal, exactly one
/// tree is removed and four are reported unstarted and still on disk. Fails
/// with the removal-loop check taken out: all five are removed.
#[tokio::test]
async fn worktree_7619_a_cancelled_orphan_sweep_stops_after_the_first_removal() {
    let (mgr, _store) = manager().await;
    let fx = GitWorktreeFixture::new();
    let trees = orphans(&fx, "orphan-7619", 5);

    let out = sweep(&mgr, &fx, &cancel_after_first_removal(&trees)).await;

    assert_eq!(out.removed.len(), 1, "{out:?}");
    assert!(!out.removed[0].exists(), "the removed tree is gone");
    assert_eq!(out.cancelled_unstarted.len(), 4, "{out:?}");
    assert!(out.partially_removed.is_empty(), "{out:?}");
    for p in &out.cancelled_unstarted {
        assert!(p.exists(), "an unstarted tree was touched: {}", p.display());
        assert!(!out.removed.contains(p), "listed as both: {}", p.display());
    }
    let mut seen = out.removed.clone();
    seen.extend(out.cancelled_unstarted.iter().cloned());
    assert_eq!(
        sorted(seen),
        sorted(trees),
        "every candidate is accounted for"
    );
}

/// 🔴 Plan 4(d3), Fail-Open Check: a pass cancelled before it starts, or
/// whose cancel state cannot be read, removes nothing in either loop. Fails
/// with the checks taken out, or with `None` read as "keep going".
#[tokio::test]
async fn worktree_7619_an_unreadable_or_early_cancel_removes_nothing() {
    let rows: [(&str, PruneCancel); 2] = [
        ("already cancelled", PruneCancel::from_probe(|| Some(true))),
        ("unreadable", PruneCancel::from_probe(|| None)),
    ];
    for (label, cancel) in rows {
        assert!(cancel.stop_requested(), "{label}: must read as stop");
        let (mgr, _store) = manager().await;
        let fx = GitWorktreeFixture::new();
        let trees = orphans(&fx, "early-7619", 3);
        let out = sweep(&mgr, &fx, &cancel).await;
        assert!(out.removed.is_empty(), "{label}: orphan sweep {out:?}");
        assert_eq!(
            sorted(out.cancelled_unstarted.clone()),
            sorted(trees.clone())
        );

        let landed = merged_trees(&fx, "early-reclaim-7619", 3);
        let out = reclaim(&fx, &cancel, &|_| None);
        assert!(out.removed.is_empty(), "{label}: reclaim {out:?}");
        assert!(out.partially_removed.is_empty(), "{label}: {out:?}");
        for p in trees.iter().chain(&landed) {
            assert!(p.exists(), "{label}: {} was removed", p.display());
        }
    }
}

// ── the merged-PR loop through `reclaim_scoped`'s injected probes ─────────

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

fn no_keeps() -> KeepList {
    KeepList::default()
}

/// `n` worktrees in the state a merged PR leaves: one commit, pushed.
fn merged_trees(fx: &GitWorktreeFixture, stem: &str, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let wt = fx.add_worktree(&format!("{stem}-{i}"));
            std::fs::write(wt.join("landed.rs"), "// landed\n").expect("write");
            GitWorktreeFixture::commit_all_and_push(&wt, "landed");
            wt
        })
        .collect()
}

/// An index naming every `session/*` branch of the fixture as merged.
fn all_merged(root: &Path) -> PrIndex {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/session/",
        ])
        .output()
        .expect("git for-each-ref");
    let rows: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .enumerate()
        .map(|(i, b)| {
            format!(
                r#"{{"number": {}, "headRefName": "{b}", "state": "MERGED"}}"#,
                i + 1
            )
        })
        .collect();
    PrIndex::from_json(&format!("[{}]", rows.join(",")), 400)
}

fn reclaim(
    fx: &GitWorktreeFixture,
    cancel: &PruneCancel,
    cwd_holder: &dyn Fn(&Path) -> Option<String>,
) -> crate::session_manager::worktree_reclaim::ReclaimOutcome {
    reclaim_scoped_with_cancel(
        &fx.repos_root,
        &FreshProbes {
            prove: &crate::session_manager::worktree_reclaim_landed::reclaim_landed_proof,
            launched_from: &[],
            cwd_holder,
            keep_list: &no_keeps,
            agent_state: &no_agents,
            in_use_now: &|| Some(LiveClaims::default()),
            index_for: &all_merged,
        },
        ReclaimMode::Remove,
        &[],
        &WorktreeScope::all(),
        cancel,
    )
}

/// 🔴 Plan 4(b), merged-PR loop: cancelled after the first removal, exactly
/// one tree is reclaimed and four are reported unstarted and still on disk.
/// Fails with the delete-loop check taken out: all five are removed.
#[test]
fn worktree_7619_a_cancelled_reclaim_stops_after_the_first_removal() {
    let fx = GitWorktreeFixture::new();
    let trees = merged_trees(&fx, "reclaim-7619", 5);

    let out = reclaim(&fx, &cancel_after_first_removal(&trees), &|_| None);

    assert_eq!(
        out.survey.reclaimable, 5,
        "premise: all five approved: {out:?}"
    );
    assert_eq!(out.removed.len(), 1, "{out:?}");
    assert_eq!(out.cancelled_unstarted.len(), 4, "{out:?}");
    for p in &out.cancelled_unstarted {
        assert!(p.exists(), "an unstarted tree was touched: {}", p.display());
        assert!(!out.removed.contains(p), "listed as both: {}", p.display());
    }
}

/// 🔴 Plan 4(c): cancel fired inside the first removal's last-moment guard
/// lets that removal finish — the tree is gone and git no longer lists it,
/// not `PartiallyRemoved` — and starts no other. Fails with the delete-loop
/// check taken out (all three removed), and if cancel were read inside the
/// removal (the first tree kept or half-removed).
#[test]
fn worktree_7619_cancel_during_a_removal_lets_that_removal_finish() {
    let fx = GitWorktreeFixture::new();
    let trees = merged_trees(&fx, "inflight-7619", 3);
    let (flag, cancel) = flag_cancel();
    let fire = |_: &Path| {
        flag.store(true, Ordering::SeqCst);
        None
    };

    let out = reclaim(&fx, &cancel, &fire);

    assert_eq!(out.removed.len(), 1, "{out:?}");
    let gone = &out.removed[0];
    assert!(!gone.exists(), "the in-flight removal finished");
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git worktree list");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !listed.contains(gone.to_str().expect("utf8")),
        "git still lists the removed tree: {listed}"
    );
    assert!(out.partially_removed.is_empty(), "{out:?}");
    assert!(
        out.removal_failed.is_empty() && out.refused_at_recheck.is_empty(),
        "{out:?}"
    );
    assert_eq!(out.cancelled_unstarted.len(), 2, "{out:?}");
    assert!(out.cancelled_unstarted.iter().all(|p| p.exists()));
}

/// Restores a directory's mode on drop so `TempDir` can clean up.
struct Writable(PathBuf);

impl Drop for Writable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// 🔴 Plan 4(c) companion: a real `git worktree remove --force` that fails
/// mid-removal still reports `PartiallyRemoved`, and that tree is never also
/// listed as cancelled-unstarted. Each tree holds a tracked file under a
/// read-only directory, so git deletes the file and then fails on its parent.
/// Fails with the delete-loop check taken out: all three are partially removed
/// and none is unstarted.
#[test]
fn worktree_7619_a_partial_removal_is_never_also_cancelled_unstarted() {
    use std::os::unix::fs::PermissionsExt;
    let fx = GitWorktreeFixture::new();
    let mut restore = Vec::new();
    let trees: Vec<PathBuf> = (0..3)
        .map(|i| {
            let wt = fx.add_worktree(&format!("partial-7619-{i}"));
            let inner = wt.join("ro").join("inner");
            std::fs::create_dir_all(&inner).expect("mkdir");
            std::fs::write(inner.join("x.rs"), "// tracked\n").expect("write");
            GitWorktreeFixture::commit_all_and_push(&wt, "landed");
            let ro = wt.join("ro");
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).expect("chmod");
            restore.push(Writable(ro.clone()));
            assert!(
                std::fs::create_dir(ro.join("probe")).is_err(),
                "premise: a read-only directory refuses writes (not running as root)"
            );
            wt
        })
        .collect();
    let (flag, cancel) = flag_cancel();
    let fire = |_: &Path| {
        flag.store(true, Ordering::SeqCst);
        None
    };

    let out = reclaim(&fx, &cancel, &fire);

    assert_eq!(out.partially_removed.len(), 1, "{out:?}");
    assert!(out.removed.is_empty(), "{out:?}");
    assert_eq!(out.cancelled_unstarted.len(), 2, "{out:?}");
    for p in &out.cancelled_unstarted {
        let prefix = format!("{}: ", p.display());
        assert!(
            !out.partially_removed.iter().any(|r| r.starts_with(&prefix)),
            "listed as both partially removed and unstarted: {}",
            p.display()
        );
        assert!(
            p.join("ro/inner/x.rs").exists(),
            "unstarted tree was touched"
        );
    }
    assert!(trees.iter().all(|t| {
        out.cancelled_unstarted.contains(t)
            || out
                .partially_removed
                .iter()
                .any(|r| r.starts_with(&format!("{}: ", t.display())))
    }));
}
