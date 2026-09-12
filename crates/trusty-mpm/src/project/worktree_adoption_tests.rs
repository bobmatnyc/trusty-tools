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

/// A corrupt store reads as empty, so a READER never fails on one.
#[test]
fn load_of_a_corrupt_file_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(ADOPTED_WORKTREES_FILE), b"{ not json").expect("seed");

    assert!(
        AdoptionStore::load(dir.path()).entries().is_empty(),
        "a corrupt store reads as empty"
    );
}

/// The WRITER is the opposite of the reader: a store that is present but will
/// not parse is left byte-for-byte alone rather than replaced with this
/// caller's view of it (#7357 review finding 2).
#[test]
fn update_refuses_to_clobber_a_corrupt_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join(ADOPTED_WORKTREES_FILE);
    std::fs::write(&file, b"{ not json").expect("seed");

    let outcome = AdoptionStore::update(dir.path(), |store| {
        store.record(entry(Path::new("/tmp/wt-a"), "alpha"))
    });

    assert!(
        outcome.is_err(),
        "an unparsable store must not be published over"
    );
    assert_eq!(
        std::fs::read(&file).expect("read back"),
        b"{ not json",
        "the corrupt document is left exactly as it was"
    );
}

/// One record per worktree path, round-tripped through the file.
#[test]
fn record_writes_one_entry_per_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    AdoptionStore::update(dir.path(), |store| {
        assert_eq!(
            store.record(entry(Path::new("/tmp/wt-a"), "alpha")),
            RecordOutcome::Recorded
        );
        assert_eq!(
            store.record(entry(Path::new("/tmp/wt-b"), "alpha")),
            RecordOutcome::Recorded
        );
    })
    .expect("publish");

    let reloaded = AdoptionStore::load(dir.path());
    assert_eq!(reloaded.entries().len(), 2);
    assert!(reloaded.entries().iter().all(|e| e.project == "alpha"));
}

/// Re-recording the same path for the same project changes nothing — this is
/// what makes a second `project_register` idempotent.
#[test]
fn record_is_idempotent_for_the_same_project() {
    let mut store = AdoptionStore::default();
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
    let mut store = AdoptionStore::default();
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
    AdoptionStore::update(dir.path(), |store| {
        for name in ["wt-a", "wt-b"] {
            store.record(AdoptedWorktree {
                path: PathBuf::from("/tmp/repo/.worktrees").join(name),
                project: "alpha".to_string(),
                checkout: PathBuf::from("/tmp/repo"),
                branch: None,
                adopted_at: at(0),
            });
        }
    })
    .expect("publish");

    assert_eq!(
        adopted_anchors(dir.path()),
        vec![PathBuf::from("/tmp/repo")]
    );
}

/// #7588: the second registration surface must adopt under the name that
/// already owns the checkout, or every worktree comes back `ClaimedByAnother`.
#[test]
fn owning_project_reports_the_name_that_already_claimed_the_checkout_7588() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("pre-one");
    let store_dir = tempfile::tempdir().expect("tempdir");
    backfill_checkout(store_dir.path(), "gnomish", &fx.repo, at(0));

    assert_eq!(
        project_owning_checkout(store_dir.path(), &fx.repo),
        Some("gnomish".to_string())
    );
}

/// #7588: an unrecorded checkout has no owner, so the caller falls back to its
/// own derived name rather than inventing an attribution.
#[test]
fn owning_project_is_none_for_an_unrecorded_checkout_7588() {
    let fx = GitWorktreeFixture::new();
    let store_dir = tempfile::tempdir().expect("tempdir");

    assert_eq!(project_owning_checkout(store_dir.path(), &fx.repo), None);
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
/// It is REPORTED rather than silent, so a caller can tell it apart from a
/// checkout that simply has no worktrees (#7357 review finding 4).
#[test]
fn backfill_ignores_a_path_that_is_not_a_repository_root() {
    let plain = tempfile::tempdir().expect("tempdir");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let report = backfill_checkout(store_dir.path(), "alpha", plain.path(), at(0));

    assert_eq!(report.recorded, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(
        report.unrecorded,
        vec![UnrecordedWorktree {
            path: plain.path().to_path_buf(),
            reason: SkipReason::NotARepositoryRoot,
        }]
    );
    assert!(!store_dir.path().join(ADOPTED_WORKTREES_FILE).exists());
}

/// The signal finding 4 asks for: a skipped worktree reaches the caller by
/// PATH and REASON, not as a bare count and a `tracing` line.
#[test]
fn backfill_names_the_skipped_worktree_and_why() {
    let fx = GitWorktreeFixture::new();
    fx.add_worktree("alpha-one");
    let broken = fx.repo.join(".claude").join("worktrees").join("broken");
    std::fs::create_dir_all(&broken).expect("broken worktree dir");
    let store_dir = tempfile::tempdir().expect("tempdir");

    let report = backfill_checkout(store_dir.path(), "alpha", &fx.repo, at(0));

    let named: Vec<&UnrecordedWorktree> = report
        .unrecorded
        .iter()
        .filter(|u| u.reason == SkipReason::NoGitdirPointer)
        .collect();
    assert_eq!(named.len(), 1, "got {:?}", report.unrecorded);
    assert!(
        named[0].path.ends_with("broken"),
        "the skipped path must be named: {:?}",
        named[0].path
    );

    // And a foreign claim is reported the same way, naming the holder.
    let second = backfill_checkout(store_dir.path(), "beta", &fx.repo, at(600));
    assert!(
        second.unrecorded.iter().any(|u| u.reason
            == SkipReason::ClaimedByAnother {
                project: "alpha".to_string()
            }),
        "a foreign claim must name the project holding it: {:?}",
        second.unrecorded
    );
}

/// Two registrations racing on one store must both survive (#7357 review
/// finding 2).
///
/// Why: the pre-fix write path was load → mutate in memory → `fs::write` the
/// whole file, with no cross-process serialisation; two backfills that
/// interleaved read/read/write/write dropped one project's records while both
/// callers saw success. Ten rounds is what makes the old shape lose reliably
/// rather than occasionally — the locked [`AdoptionStore::update`] passes every
/// round by construction.
/// Test: this function IS the test.
#[test]
fn concurrent_backfills_never_lose_a_projects_records() {
    let alpha = GitWorktreeFixture::new();
    alpha.add_worktree("alpha-one");
    let beta = GitWorktreeFixture::new();
    beta.add_worktree("beta-one");

    for round in 0..10 {
        let store_dir = tempfile::tempdir().expect("tempdir");
        let store_path = store_dir.path().to_path_buf();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<std::thread::JoinHandle<()>> = [("alpha", &alpha), ("beta", &beta)]
            .into_iter()
            .map(|(name, fx)| {
                let store_path = store_path.clone();
                let checkout = fx.repo.clone();
                let gate = std::sync::Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    backfill_checkout(&store_path, name, &checkout, at(0));
                })
            })
            .collect();
        for h in handles {
            h.join().expect("backfill thread");
        }

        let store = AdoptionStore::load(store_dir.path());
        let mut projects: Vec<&str> = store.entries().iter().map(|e| e.project.as_str()).collect();
        projects.sort_unstable();
        assert_eq!(
            projects,
            vec!["alpha", "beta"],
            "round {round}: both projects' records must survive the race"
        );
    }
}
