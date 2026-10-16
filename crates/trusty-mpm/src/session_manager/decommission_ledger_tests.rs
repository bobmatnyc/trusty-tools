//! Tests for the decommission ledger record (#8994 S2c criterion 3).
//!
//! Why: decommission is one of the three removal routes; its removal must
//! reach the ledger, and a removal it refused must not.
//! What: a real in-project worktree, a real `SessionManager` with a recorder
//! over a scratch ledger, and `decommission_with_root`.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::super::decommission::WORKTREE_SENTINEL_FILE;
use super::super::manager::SessionManager;
use super::super::record::ManagedSessionId;
use super::super::worktree_git_fixture::GitWorktreeFixture;
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::removal_evidence::ledger_recorder;
use crate::core::worktree_ledger::{EventKind, LedgerEvent, Origin, WorktreeLedger, ledger_key};

/// A manager with a recorder over `<scratch>/worktrees.jsonl`, a session on
/// a fresh in-project worktree of `fx` recorded live in that ledger, and the
/// worktree path.
async fn session_on_worktree(
    fx: &GitWorktreeFixture,
    scratch: &Path,
    name: &str,
) -> (SessionManager, ManagedSessionId, WorktreeLedger, PathBuf) {
    let mgr = SessionManager::new(scratch, super::super::tests::fake_with_pane())
        .await
        .expect("manager");
    let ledger = WorktreeLedger::at(scratch.join("worktrees.jsonl"));
    assert!(
        mgr.install_removal_recorder(ledger_recorder(ledger.clone(), Arc::new(Mutex::new(()))))
    );
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    ledger
        .append(&LedgerEvent::now(
            ledger_key(&wt),
            EventKind::Created {
                repo: ledger_key(&fx.repo),
                branch: Some(format!("session/{name}")),
                origin: Origin::TmDaemon,
                session: None,
            },
        ))
        .expect("seed");
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(wt.clone()),
            None,
            Some(wt.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("create");
    (mgr, record.id, ledger, wt)
}

fn live(ledger: &WorktreeLedger, wt: &Path) -> bool {
    fold(&ledger.read().expect("read").events)
        .live
        .contains_key(&ledger_key(wt))
}

/// #8994 S2c criterion 3: a worktree decommission removed is recorded
/// `removed` once, after the re-check.
#[tokio::test]
async fn decommission_records_removed_for_the_worktree_it_removed() {
    let fx = GitWorktreeFixture::new();
    let scratch = tempfile::tempdir().expect("tempdir");
    let (mgr, id, ledger, wt) = session_on_worktree(&fx, scratch.path(), "s2c-decom").await;
    let key = ledger_key(&wt);

    let managed_root = tempfile::tempdir().expect("tempdir");
    let (_, removed) = mgr
        .decommission_with_root(&id, managed_root.path(), None)
        .await
        .expect("decommission");

    assert!(removed && !wt.exists(), "premise: decommission removed it");
    assert!(!ledger_live_key(&ledger, &key));
    let removed_lines = ledger
        .read()
        .expect("read")
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Removed && e.path == key)
        .count();
    assert_eq!(removed_lines, 1);
}

fn ledger_live_key(ledger: &WorktreeLedger, key: &Path) -> bool {
    fold(&ledger.read().expect("read").events)
        .live
        .contains_key(key)
}

/// #8994 S2c criterion 3: a worktree decommission kept (unsaved work)
/// appends nothing and stays live.
#[tokio::test]
async fn decommission_that_kept_the_worktree_records_nothing() {
    let fx = GitWorktreeFixture::new();
    let scratch = tempfile::tempdir().expect("tempdir");
    let (mgr, id, ledger, wt) = session_on_worktree(&fx, scratch.path(), "s2c-decom-dirty").await;
    std::fs::write(wt.join("unsaved.txt"), b"work\n").expect("dirty the tree");

    let managed_root = tempfile::tempdir().expect("tempdir");
    let (_, removed) = mgr
        .decommission_with_root(&id, managed_root.path(), None)
        .await
        .expect("decommission");

    assert!(!removed && wt.exists(), "premise: decommission kept it");
    assert!(live(&ledger, &wt));
    assert!(
        ledger
            .read()
            .expect("read")
            .events
            .iter()
            .all(|e| e.kind != EventKind::Removed)
    );
}
