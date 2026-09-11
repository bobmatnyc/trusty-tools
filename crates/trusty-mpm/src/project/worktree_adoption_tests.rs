//! Unit tests for the #7357 pre-existing-worktree adoption store and backfill.
//!
//! Why: the bug is that registering a project changed NOTHING about the
//! daemon's worktree view, so the tests that carry weight are the ones asserting
//! a record now exists for a worktree that was on disk BEFORE registration — and
//! the refusals beside them: a second registration adds nothing, another
//! project's worktree is never taken, and an unreadable directory is skipped
//! rather than adopted.
//! What: exercises [`AdoptionStore`] directly and [`backfill_checkout`] over a
//! real git checkout built by `GitWorktreeFixture`.
//! Test: this file IS the test module.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// A fixed instant, so a record's timestamp is never a source of flake.
fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
}

fn entry(path: &Path, project: &str) -> AdoptedWorktree {
    AdoptedWorktree {
        path: path.to_path_buf(),
        project: project.to_string(),
        checkout: path.parent().unwrap_or(path).to_path_buf(),
        branch: None,
        adopted_at: at(0),
    }
}

/// The store file sits beside `projects.json`, under the registry data dir.
#[test]
fn adoption_store_path_is_beside_the_project_registry() {
    assert_eq!(ADOPTED_WORKTREES_FILE, "worktrees.json");
}

/// A missing store file is an empty store, not an error — first run must work
/// with no init step.
#[test]
fn load_of_a_missing_file_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(AdoptionStore::load(dir.path()).entries().is_empty());
}

/// A corrupt store must not stop registration, and the next save must republish
/// a valid document (fail-open, per the module docs).
#[test]
fn load_of_a_corrupt_file_is_empty_and_still_saveable() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(ADOPTED_WORKTREES_FILE), b"{ not json").expect("seed");

    let mut store = AdoptionStore::load(dir.path());
    assert!(store.entries().is_empty(), "a corrupt store reads as empty");
    assert_eq!(
        store.record(entry(Path::new("/tmp/wt-a"), "alpha")),
        RecordOutcome::Recorded
    );
    store.save().expect("save must republish a valid document");
    assert_eq!(AdoptionStore::load(dir.path()).entries().len(), 1);
}

/// One record per worktree path, round-tripped through the file.
#[test]
fn record_writes_one_entry_per_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = AdoptionStore::load(dir.path());
    assert_eq!(
        store.record(entry(Path::new("/tmp/wt-a"), "alpha")),
        RecordOutcome::Recorded
    );
    assert_eq!(
        store.record(entry(Path::new("/tmp/wt-b"), "alpha")),
        RecordOutcome::Recorded
    );
    store.save().expect("save");

    let reloaded = AdoptionStore::load(dir.path());
    assert_eq!(reloaded.entries().len(), 2);
    assert!(reloaded.entries().iter().all(|e| e.project == "alpha"));
}

/// Re-recording the same path for the same project changes nothing — this is
/// what makes a second `project_register` idempotent.
#[test]
fn record_is_idempotent_for_the_same_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = AdoptionStore::load(dir.path());
    store.record(entry(Path::new("/tmp/wt-a"), "alpha"));

    let mut second = entry(Path::new("/tmp/wt-a"), "alpha");
    second.adopted_at = at(600);
    assert_eq!(store.record(second), RecordOutcome::AlreadyRecorded);

    assert_eq!(store.entries().len(), 1);
    assert_eq!(
        store.entries()[0].adopted_at,
        at(0),
        "the record keeps naming when the path was FIRST seen"
    );
}

/// The refusal that matters: a worktree another project already holds is left
/// exactly as it was, and the caller is told whose it is.
#[test]
fn record_never_reattributes_another_projects_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = AdoptionStore::load(dir.path());
    store.record(entry(Path::new("/tmp/wt-a"), "alpha"));

    assert_eq!(
        store.record(entry(Path::new("/tmp/wt-a"), "beta")),
        RecordOutcome::ClaimedByAnother("alpha".to_string())
    );
    assert_eq!(store.entries().len(), 1);
    assert_eq!(store.entries()[0].project, "alpha");
}

/// Anchors are checkouts, not worktrees — the scan re-derives every worktree
/// fact from git, so what it needs is which checkouts to ask.
#[test]
fn anchors_are_the_distinct_checkouts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = AdoptionStore::load(dir.path());
    for name in ["wt-a", "wt-b"] {
        store.record(AdoptedWorktree {
            path: PathBuf::from("/tmp/repo/.worktrees").join(name),
            project: "alpha".to_string(),
            checkout: PathBuf::from("/tmp/repo"),
            branch: None,
            adopted_at: at(0),
        });
    }
    store.save().expect("save");

    assert_eq!(
        adopted_anchors(dir.path()),
        vec![PathBuf::from("/tmp/repo")]
    );
}

/// THE #7357 acceptance case: a checkout whose worktrees existed BEFORE
/// registration gets one record each, attributed to the project.
#[test]
fn backfill_records_every_pre_existing_worktree() {
    let fx = GitWorktreeFixture::new();
    let a = fx.add_worktree("alpha-one");
    let b = fx.add_worktree("alpha-two");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let report = backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(0));
    assert_eq!(report.recorded, 2, "both pre-existing worktrees adopted");
    assert_eq!(report.claimed_by_another, 0);

    let store = AdoptionStore::load(store_dir.path());
    let recorded: Vec<&PathBuf> = store.entries().iter().map(|e| &e.path).collect();
    for wt in [&a, &b] {
        let canonical = std::fs::canonicalize(wt).expect("canonical worktree");
        assert!(
            recorded.contains(&&canonical),
            "{} must have a record; got {recorded:?}",
            canonical.display()
        );
    }
    assert!(store.entries().iter().all(|e| e.project == "alpha"));
}

/// Registering twice must not double the records.
#[test]
fn backfill_is_idempotent() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("alpha-one");
    fx.add_worktree("alpha-two");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let first = backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(0));
    let second = backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(600));

    assert_eq!(first.recorded, 2);
    assert_eq!(second.recorded, 0, "a re-register writes nothing new");
    assert_eq!(second.already_recorded, 2);
    assert_eq!(AdoptionStore::load(store_dir.path()).entries().len(), 2);
}

/// A worktree another project already holds stays that project's.
#[test]
fn backfill_leaves_another_projects_worktree_alone() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("shared-one");
    let store_dir = tempfile::tempdir().expect("tempdir");

    backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(0));
    let report = backfill_checkout(store_dir.path(), "beta", &fx.repo, at(600));

    assert_eq!(report.recorded, 0);
    assert_eq!(report.claimed_by_another, 1);
    let store = AdoptionStore::load(store_dir.path());
    assert_eq!(store.entries().len(), 1);
    assert_eq!(store.entries()[0].project, "alpha");
}

/// FAIL-OPEN: a directory shaped like a worktree but carrying no gitdir pointer
/// is skipped with a warning, and the backfill still adopts everything else.
#[test]
fn backfill_skips_a_worktree_with_no_gitdir_pointer_and_still_succeeds() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("alpha-one");
    let broken = fx.repo.join(".claude").join("worktrees").join("broken");
    std::fs::create_dir_all(&broken).expect("broken worktree dir");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let report = backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(0));

    assert_eq!(report.skipped, 1, "the unreadable directory is skipped");
    assert_eq!(report.recorded, 1, "the readable worktree is still adopted");
    let store = AdoptionStore::load(store_dir.path());
    assert!(
        !store.entries().iter().any(|e| e.path.ends_with("broken")),
        "a directory with no gitdir pointer must never be adopted"
    );
}

/// A path that is not a repository root adopts nothing and writes no file — an
/// anchor derived from it would point git at an unrelated enclosing repository.
#[test]
fn backfill_ignores_a_path_that_is_not_a_repository_root() {
    let plain = tempfile::tempdir().expect("tempdir");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let report = backfill_checkout(store_dir.path(), "alpha", plain.path(), at(0));

    assert_eq!(report, BackfillReport::default());
    assert!(!store_dir.path().join(ADOPTED_WORKTREES_FILE).exists());
}
