//! Tests for hook-fed recording of agent-isolation trees (#8994 S2a).

use std::path::{Path, PathBuf};

use super::*;
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::{EventKind, LedgerEvent, WorktreeLedger};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn scratch_ledger() -> (tempfile::TempDir, WorktreeLedger) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = WorktreeLedger::under_home(dir.path());
    (dir, ledger)
}

/// A real `git worktree add` at `<repo>/.claude/worktrees/<name>`.
fn agent_tree(fixture: &GitWorktreeFixture, name: &str) -> PathBuf {
    fixture.add_worktree_at(&fixture.repo.join(".claude").join("worktrees"), name)
}

fn count(ledger: &WorktreeLedger, pred: impl Fn(&EventKind) -> bool) -> usize {
    let read = ledger.read().expect("read");
    read.events.iter().filter(|e| pred(&e.kind)).count()
}

#[test]
fn agent_tree_of_accepts_the_tree_and_its_subdirectories() {
    let tree = Path::new("/r/p/.claude/worktrees/agent-a1b2");
    assert_eq!(agent_tree_of(tree).as_deref(), Some(tree));
    assert_eq!(
        agent_tree_of(&tree.join("crates/x/src")).as_deref(),
        Some(tree)
    );
}

#[test]
fn agent_tree_of_refuses_odd_cwds() {
    let cases = [
        "",
        "/",
        "relative/.claude/worktrees/agent-a1",
        "/r/p",
        "/r/p/.claude/worktrees",
        "/r/p/.claude/worktrees/agent-",
        "/r/p/.claude/worktrees/feature-x",
        "/r/p/.worktrees/agent-a1",
        "/r/p/.claude/worktrees/agent-a1/../../../../etc",
        "/.claude/worktrees/agent-a1/..",
    ];
    for cwd in cases {
        assert_eq!(agent_tree_of(Path::new(cwd)), None, "{cwd:?}");
    }
}

#[test]
fn observe_records_a_registered_tree_once() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-once");
    let (_d, ledger) = scratch_ledger();
    let checkouts = [fixture.repo.clone()];
    let first = observe_agent_tree(&ledger, &tree, &checkouts, Some("sid")).unwrap();
    assert_eq!(
        first,
        ObserveOutcome::Recorded {
            branch: Some("wt/agent-once".into())
        }
    );
    for _ in 0..3 {
        let again = observe_agent_tree(&ledger, &tree, &checkouts, Some("other")).unwrap();
        assert_eq!(again, ObserveOutcome::AlreadyLive);
    }
    assert_eq!(
        count(&ledger, |k| matches!(k, EventKind::Observed { .. })),
        1
    );
    let state = fold(&ledger.read().unwrap().events);
    let wt = &state.live[&ledger_key(&tree)];
    assert_eq!(
        (wt.origin, wt.observed_only, wt.session.as_deref()),
        (Origin::AgentIsolation, true, Some("sid"))
    );
    assert_eq!(wt.repo, ledger_key(&fixture.repo));
}

#[test]
fn observe_refuses_an_unregistered_or_absent_tree() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-unreg");
    let (_d, ledger) = scratch_ledger();
    let other = tempfile::tempdir().unwrap();
    let outcome = observe_agent_tree(&ledger, &tree, &[other.path().to_path_buf()], None);
    assert_eq!(outcome.unwrap(), ObserveOutcome::Unregistered);
    let absent = fixture.repo.join(".claude/worktrees/agent-never");
    let outcome = observe_agent_tree(&ledger, &absent, std::slice::from_ref(&fixture.repo), None);
    assert_eq!(outcome.unwrap(), ObserveOutcome::NotADirectory);
    assert!(!ledger.read().unwrap().present, "nothing appended");
}

#[cfg(unix)]
#[test]
fn observe_refuses_a_symlinked_tree_that_resolves_elsewhere() {
    let fixture = GitWorktreeFixture::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let parent = fixture.repo.join(".claude/worktrees");
    std::fs::create_dir_all(&parent).unwrap();
    let link = parent.join("agent-link");
    std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();
    let (_d, ledger) = scratch_ledger();
    let outcome = observe_agent_tree(&ledger, &link, std::slice::from_ref(&fixture.repo), None);
    assert_eq!(outcome.unwrap(), ObserveOutcome::Unregistered);
}

#[test]
fn observe_after_removed_records_the_tree_again() {
    // Out of order relative to slice 1's reader: a `removed` followed by a new
    // `observed` for the same path folds to one live entry, never two.
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-again");
    let (_d, ledger) = scratch_ledger();
    let checkouts = [fixture.repo.clone()];
    observe_agent_tree(&ledger, &tree, &checkouts, None).unwrap();
    ledger
        .append(&LedgerEvent::now(ledger_key(&tree), EventKind::Removed))
        .unwrap();
    // A duplicate `removed` (two stop events racing) is a no-op in the fold.
    ledger
        .append(&LedgerEvent::now(ledger_key(&tree), EventKind::Removed))
        .unwrap();
    let outcome = observe_agent_tree(&ledger, &tree, &checkouts, None).unwrap();
    assert!(matches!(outcome, ObserveOutcome::Recorded { .. }));
    assert_eq!(fold(&ledger.read().unwrap().events).live.len(), 1);
}

#[test]
fn reconcile_leaves_a_present_tree_live() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-running");
    let (_d, ledger) = scratch_ledger();
    observe_agent_tree(&ledger, &tree, std::slice::from_ref(&fixture.repo), None).unwrap();
    let report = reconcile_agent_trees(&ledger).unwrap();
    assert_eq!(report, ReconcileReport::default());
    assert_eq!(count(&ledger, |k| *k == EventKind::Removed), 0);
}

#[test]
fn reconcile_marks_removed_only_when_gone_from_disk_and_git() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-done");
    let (_d, ledger) = scratch_ledger();
    observe_agent_tree(&ledger, &tree, std::slice::from_ref(&fixture.repo), None).unwrap();
    // Gone from disk, still in git's registry: stays live.
    std::fs::remove_dir_all(&tree).unwrap();
    let report = reconcile_agent_trees(&ledger).unwrap();
    assert_eq!((report.removed, report.still_listed), (0, 1), "{report:?}");
    // Pruned from git too: now, and only now, `removed`.
    let pruned = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(&fixture.repo)
        .status()
        .unwrap();
    assert!(pruned.success());
    let report = reconcile_agent_trees(&ledger).unwrap();
    assert_eq!(report.removed, 1, "{report:?}");
    assert!(fold(&ledger.read().unwrap().events).live.is_empty());
    // A second pass has nothing left to mark.
    assert_eq!(reconcile_agent_trees(&ledger).unwrap().removed, 0);
    assert_eq!(count(&ledger, |k| *k == EventKind::Removed), 1);
}

#[test]
fn reconcile_ignores_trees_of_other_origins() {
    let (dir, ledger) = scratch_ledger();
    let gone = dir.path().join("gone-created-tree");
    ledger
        .append(&LedgerEvent::now(
            gone,
            EventKind::Created {
                repo: dir.path().join("no-repo"),
                branch: None,
                origin: Origin::TmDaemon,
                session: None,
            },
        ))
        .unwrap();
    assert_eq!(
        reconcile_agent_trees(&ledger).unwrap(),
        ReconcileReport::default()
    );
}

#[test]
fn ingest_failure_sidecar_counts_and_clears() {
    let (_d, ledger) = scratch_ledger();
    std::fs::create_dir_all(ledger.path().parent().unwrap()).unwrap();
    assert_eq!(read_ingest_failure(&ledger), Ok(None));
    record_ingest_failure(&ledger, "first").unwrap();
    record_ingest_failure(&ledger, "second").unwrap();
    let failure = read_ingest_failure(&ledger).unwrap().unwrap();
    assert_eq!((failure.error.as_str(), failure.failures), ("second", 2));
    clear_ingest_failure(&ledger).unwrap();
    clear_ingest_failure(&ledger).unwrap();
    assert_eq!(read_ingest_failure(&ledger), Ok(None));
    std::fs::write(ingest_failure_path(&ledger), b"{torn").unwrap();
    assert!(read_ingest_failure(&ledger).is_err());
}
