//! Tests for the `worktree_registry` doctor row (#8994, criterion 5).

use std::path::PathBuf;

use super::{CHECK_NAME, check_worktree_registry};
use crate::core::doctor::CheckStatus;
use crate::core::worktree_ledger::fold::{GIB, fold};
use crate::core::worktree_ledger::reconcile::reconcile_removed;
use crate::core::worktree_ledger::{EventKind, LedgerEvent, Origin, WorktreeLedger, ledger_key};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn created(path: &str, repo: &str) -> LedgerEvent {
    LedgerEvent::now(
        PathBuf::from(path),
        EventKind::Created {
            repo: PathBuf::from(repo),
            branch: None,
            origin: Origin::Backfill,
            session: None,
        },
    )
}

fn measured(path: &str, bytes: u64) -> LedgerEvent {
    LedgerEvent::now(PathBuf::from(path), EventKind::Measured { bytes })
}

/// Every path in the ledger names a repository and worktrees that do not
/// exist, so git has nothing to answer and a filesystem walk finds nothing.
/// The row still reports the recorded count and GiB, which it can only have
/// taken from the ledger.
#[test]
fn worktree_registry_row_reports_count_and_gib_from_the_ledger_alone() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorktreeLedger::under_home(home.path());
    let gib = GIB as u64;
    for e in [
        created(
            "/nonexistent-8994/alpha/.worktrees/a",
            "/nonexistent-8994/alpha",
        ),
        created(
            "/nonexistent-8994/alpha/.worktrees/b",
            "/nonexistent-8994/alpha",
        ),
        created(
            "/nonexistent-8994/beta/.worktrees/c",
            "/nonexistent-8994/beta",
        ),
        measured("/nonexistent-8994/alpha/.worktrees/a", gib),
        measured("/nonexistent-8994/alpha/.worktrees/b", gib / 2),
    ] {
        ledger.append(&e).unwrap();
    }
    let row = check_worktree_registry(home.path());
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message
            .starts_with("3 worktree(s) across 2 project(s), 1.50 GiB measured, 1 unmeasured"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("/nonexistent-8994/alpha 2 (1.50 GiB)"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("/nonexistent-8994/beta 1 (0.00 GiB)"),
        "{}",
        row.message
    );
}

#[test]
fn worktree_registry_row_with_no_ledger_is_ok_and_names_the_backfill() {
    let home = tempfile::tempdir().unwrap();
    let row = check_worktree_registry(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message.contains("no worktree ledger"),
        "{}",
        row.message
    );
    assert!(row.message.contains("`tm worktrees`"), "{}", row.message);
    assert!(
        !WorktreeLedger::under_home(home.path()).path().exists(),
        "the row must not create the ledger"
    );
}

#[test]
fn worktree_registry_row_warns_on_malformed_lines() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorktreeLedger::under_home(home.path());
    ledger
        .append(&created("/nonexistent-8994/a", "/nonexistent-8994"))
        .unwrap();
    let mut text = std::fs::read_to_string(ledger.path()).unwrap();
    text.push_str("not json\n");
    std::fs::write(ledger.path(), text).unwrap();
    let row = check_worktree_registry(home.path());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("1 malformed"), "{}", row.message);
}

/// #8994 finding 1: a tree removed from disk and from git leaves the row's
/// count AND its GiB once the reconcile `tm worktrees` runs has recorded it.
#[test]
fn worktree_registry_row_drops_a_tree_reconciled_as_removed() {
    let fixture = GitWorktreeFixture::new();
    let a = ledger_key(&fixture.add_worktree("a"));
    let b = ledger_key(&fixture.add_worktree("b"));
    let repo = ledger_key(&fixture.repo);
    let home = tempfile::tempdir().unwrap();
    let ledger = WorktreeLedger::under_home(home.path());
    let gib = GIB as u64;
    for (tree, bytes) in [(&a, gib), (&b, gib / 2)] {
        for kind in [
            EventKind::Created {
                repo: repo.clone(),
                branch: None,
                origin: Origin::TmDaemon,
                session: None,
            },
            EventKind::Measured { bytes },
        ] {
            ledger
                .append(&LedgerEvent::now(tree.clone(), kind))
                .unwrap();
        }
    }
    let before = check_worktree_registry(home.path());
    assert!(
        before
            .message
            .starts_with("2 worktree(s) across 1 project(s), 1.50 GiB measured"),
        "{}",
        before.message
    );

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["worktree", "remove", "--force"])
        .arg(&a)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = reconcile_removed(&ledger, &fold(&ledger.read().unwrap().events)).unwrap();
    assert_eq!(report.removed, 1, "{report:?}");

    let after = check_worktree_registry(home.path());
    assert_eq!(after.status, CheckStatus::Ok, "{}", after.message);
    assert!(
        after
            .message
            .starts_with("1 worktree(s) across 1 project(s), 0.50 GiB measured"),
        "{}",
        after.message
    );
    assert!(
        after
            .message
            .contains(&format!("{} 1 (0.50 GiB)", repo.display())),
        "{}",
        after.message
    );
}
