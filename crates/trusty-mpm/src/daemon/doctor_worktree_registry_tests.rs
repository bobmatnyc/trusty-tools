//! Tests for the `worktree_registry` doctor row (#8994, criterion 5).

use std::path::PathBuf;

use super::{CHECK_NAME, check_worktree_registry};
use crate::core::doctor::CheckStatus;
use crate::core::worktree_ledger::fold::GIB;
use crate::core::worktree_ledger::{EventKind, LedgerEvent, Origin, WorktreeLedger};

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
