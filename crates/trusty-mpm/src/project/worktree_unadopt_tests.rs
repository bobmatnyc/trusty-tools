//! Unit tests for the #10592 ledger-only unadopt.
//!
//! Why: the contract is "ledger only": a record leaves the store, the folder
//! and every other record stay, a dry run writes nothing, and a failed write
//! never reports a removal.
//! What: exercises [`unadopt_paths`] against a real store file.
//! Test: this file IS the test module.

use super::*;
use chrono::DateTime;

fn entry(path: &Path, project: &str) -> AdoptedWorktree {
    AdoptedWorktree {
        path: path.to_path_buf(),
        project: project.to_string(),
        checkout: path.parent().unwrap_or(path).to_path_buf(),
        branch: Some("feature".to_string()),
        adopted_at: DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp"),
    }
}

/// Seed a store with one record per path, returning the store bytes.
fn seed(store_dir: &Path, paths: &[&Path]) -> Vec<u8> {
    AdoptionStore::update(store_dir, |store| {
        for p in paths {
            store.record(entry(p, "legacy"));
        }
    })
    .expect("seed store");
    std::fs::read(store_dir.join(ADOPTED_WORKTREES_FILE)).expect("read store")
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).expect("canonicalize")
}

#[test]
fn unadopt_removes_only_the_named_record_and_keeps_the_folder() {
    let store = tempfile::tempdir().expect("store");
    let trees = tempfile::tempdir().expect("trees");
    let a = trees.path().join("a");
    let b = trees.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("work.txt"), b"keep me").expect("write");
    let (a, b) = (canon(&a), canon(&b));
    seed(store.path(), &[&a, &b]);

    let report = unadopt_paths(store.path(), std::slice::from_ref(&a), false).expect("unadopt");

    assert!(!report.dry_run);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].path, a);
    assert!(report.not_adopted.is_empty());
    let left: Vec<PathBuf> = AdoptionStore::load(store.path())
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(left, vec![b], "only the named record leaves the ledger");
    assert_eq!(
        std::fs::read(a.join("work.txt")).expect("folder kept"),
        b"keep me",
        "the worktree folder and its contents are never touched"
    );
}

#[test]
fn unadopt_dry_run_reports_and_writes_nothing() {
    let store = tempfile::tempdir().expect("store");
    let trees = tempfile::tempdir().expect("trees");
    let a = canon(trees.path());
    let before = seed(store.path(), &[&a]);
    let missing = trees.path().join("never-adopted");

    let report = unadopt_paths(store.path(), &[a.clone(), missing.clone()], true).expect("dry run");

    assert!(report.dry_run);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.not_adopted, vec![missing]);
    let after = std::fs::read(store.path().join(ADOPTED_WORKTREES_FILE)).expect("read");
    assert_eq!(before, after, "a dry run leaves the store byte-for-byte");
}

#[test]
fn unadopt_matches_a_non_canonical_path() {
    let store = tempfile::tempdir().expect("store");
    let trees = tempfile::tempdir().expect("trees");
    let a = trees.path().join("a");
    std::fs::create_dir_all(&a).expect("mkdir");
    seed(store.path(), &[&canon(&a)]);
    let dotted = trees.path().join("a").join(".").join("..").join("a");

    let report = unadopt_paths(store.path(), &[dotted], false).expect("unadopt");

    assert_eq!(report.removed.len(), 1);
    assert!(AdoptionStore::load(store.path()).entries().is_empty());
}

/// Fail-Open Check: a store that will not parse is an error in both modes,
/// stays byte-for-byte, and no removal is reported.
#[test]
fn unadopt_refuses_a_corrupt_store_and_reports_no_removal() {
    let store = tempfile::tempdir().expect("store");
    let file = store.path().join(ADOPTED_WORKTREES_FILE);
    std::fs::write(&file, b"{ not json").expect("seed corrupt");
    let path = PathBuf::from("/nowhere/wt");

    for dry_run in [true, false] {
        let err = unadopt_paths(store.path(), std::slice::from_ref(&path), dry_run)
            .expect_err("a corrupt store must refuse");
        assert!(
            matches!(err, JsonRmwError::Serialize { .. }),
            "dry_run={dry_run}: {err}"
        );
        assert_eq!(std::fs::read(&file).expect("read"), b"{ not json");
    }
}
