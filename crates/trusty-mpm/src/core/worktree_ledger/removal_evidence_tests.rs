//! Tests for `removed` on evidence (#8994 S2c criteria 3 and 4).
//!
//! Why: a route that reports a removal must not move the ledger unless the
//! tree is gone from disk AND git; every other answer keeps the tree live.
//! What: real git trees from [`GitWorktreeFixture`], a scratch ledger, and
//! [`record_confirmed_removals`] / [`ledger_recorder`] on each answer.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use super::super::fold::fold;
use super::super::{EventKind, LedgerEvent, Origin, WorktreeLedger, ledger_key};
use super::{RemovalVerdict, ledger_recorder, record_confirmed_removals};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A scratch ledger holding one live `created` entry for `tree` under `repo`.
fn ledger_with(dir: &Path, tree: &Path, repo: &Path) -> WorktreeLedger {
    let ledger = WorktreeLedger::at(dir.join("worktrees.jsonl"));
    ledger
        .append(&LedgerEvent::now(
            ledger_key(tree),
            EventKind::Created {
                repo: ledger_key(repo),
                branch: Some("session/x".into()),
                origin: Origin::TmDaemon,
                session: None,
            },
        ))
        .expect("seed the ledger");
    ledger
}

fn removed_events(ledger: &WorktreeLedger) -> usize {
    ledger
        .read()
        .expect("read")
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Removed)
        .count()
}

fn live(ledger: &WorktreeLedger, tree: &Path) -> bool {
    fold(&ledger.read().expect("read").events)
        .live
        .contains_key(&ledger_key(tree))
}

/// Capture everything `body` logs, as rendered lines.
fn captured<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let out = tracing::subscriber::with_default(subscriber, body);
    (out, buffer.tail(64))
}

/// #8994 S2c criterion 3: a tree gone from disk and from git earns exactly
/// one `removed`; a second report of the same removal appends nothing.
#[test]
fn a_removal_gone_from_disk_and_git_appends_removed_once() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("s2c-gone");
    let scratch = tempfile::tempdir().expect("tempdir");
    let ledger = ledger_with(scratch.path(), &wt, &fx.repo);
    let key = ledger_key(&wt);
    git(
        &fx.repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );

    let first = record_confirmed_removals(&ledger, std::slice::from_ref(&key)).expect("record");
    assert_eq!(first[0].verdict, RemovalVerdict::Recorded);
    assert!(!live(&ledger, &key));
    let again = record_confirmed_removals(&ledger, &[key]).expect("record again");
    assert_eq!(again[0].verdict, RemovalVerdict::NotLive);
    assert_eq!(removed_events(&ledger), 1, "no double write");
}

/// #8994 S2c criterion 3: a removal that left the directory, or one git still
/// lists, appends nothing and the tree stays live.
#[test]
fn a_removal_that_left_the_tree_behind_appends_nothing() {
    let fx = GitWorktreeFixture::new();
    let present = fx.add_worktree("s2c-present");
    let listed = fx.add_worktree("s2c-listed");
    let scratch = tempfile::tempdir().expect("tempdir");
    let ledger = ledger_with(scratch.path(), &present, &fx.repo);
    let listed_key = ledger_key(&listed);
    ledger
        .append(&LedgerEvent::now(
            listed_key.clone(),
            EventKind::Observed {
                repo: ledger_key(&fx.repo),
                branch: None,
                origin: Origin::AgentIsolation,
                session: None,
            },
        ))
        .expect("seed");
    // Deleted behind git's back: the registry still names it (prunable).
    std::fs::remove_dir_all(&listed).expect("rm the tree");

    let evidence = record_confirmed_removals(&ledger, &[ledger_key(&present), listed_key.clone()])
        .expect("record");
    assert_eq!(evidence[0].verdict, RemovalVerdict::StillOnDisk);
    assert_eq!(evidence[1].verdict, RemovalVerdict::StillListed);
    assert_eq!(removed_events(&ledger), 0);
    assert!(live(&ledger, &present) && live(&ledger, &listed_key));
}

/// #8994 S2c criterion 4: a re-check that cannot be answered appends nothing,
/// keeps the tree live and is logged at warn with its cause — a stat error
/// that is not `NotFound`, a repository git cannot list, and a repository
/// directory that is gone.
#[test]
#[serial_test::serial]
fn an_unanswerable_recheck_appends_nothing_and_warns_with_its_cause() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(scratch.path()).expect("canonicalize");
    let file = root.join("a-file");
    std::fs::write(&file, "not a directory").expect("write");
    let not_a_repo = root.join("not-a-repo");
    std::fs::create_dir(&not_a_repo).expect("mkdir");
    let cases: [(PathBuf, PathBuf, &str); 3] = [
        // ENOTDIR: a stat error, not NotFound.
        (file.join("tree"), root.clone(), "stat"),
        // Git fails here, or walks up into an enclosing repository that does
        // not list this one: both are a listing that cannot be trusted.
        (
            not_a_repo.join(".worktrees/t"),
            not_a_repo.clone(),
            "`git worktree list`",
        ),
        (
            root.join("gone-repo/.worktrees/t"),
            root.join("gone-repo"),
            "repository directory is gone",
        ),
    ];
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let ledger = WorktreeLedger::at(ledger_dir.path().join("worktrees.jsonl"));
    for (tree, repo, _) in &cases {
        ledger
            .append(&LedgerEvent::now(
                tree.clone(),
                EventKind::Created {
                    repo: repo.clone(),
                    branch: None,
                    origin: Origin::Backfill,
                    session: None,
                },
            ))
            .expect("seed");
    }
    let trees: Vec<PathBuf> = cases.iter().map(|c| c.0.clone()).collect();
    let recorder = ledger_recorder(ledger.clone(), Arc::new(Mutex::new(())));

    let (evidence, lines) = captured(|| recorder("test route", &trees));

    for ((tree, _, cause), e) in cases.iter().zip(&evidence) {
        let RemovalVerdict::Unanswerable(why) = &e.verdict else {
            panic!("{}: {:?}", tree.display(), e.verdict);
        };
        assert!(why.contains(cause), "{}: {why}", tree.display());
        assert!(
            lines.iter().any(|l| l.contains("WARN")
                && l.contains(&tree.display().to_string())
                && l.contains(cause)),
            "{} must be warned with its cause. Captured: {lines:#?}",
            tree.display()
        );
        assert!(live(&ledger, tree), "{} stays live", tree.display());
    }
    assert_eq!(removed_events(&ledger), 0);
}

/// #8994 S2c error arm: a ledger the recorder cannot read appends nothing,
/// warns, and records the failure in the sidecar the doctor row reads.
#[test]
#[serial_test::serial]
fn an_unreadable_ledger_is_warned_and_reaches_the_sidecar() {
    let scratch = tempfile::tempdir().expect("tempdir");
    // A directory where the ledger file should be: every read fails.
    let path = scratch.path().join("worktrees.jsonl");
    std::fs::create_dir(&path).expect("mkdir");
    let ledger = WorktreeLedger::at(&path);
    let recorder = ledger_recorder(ledger.clone(), Arc::new(Mutex::new(())));

    let (evidence, lines) = captured(|| recorder("test route", &[scratch.path().join("t")]));

    assert!(evidence.is_empty());
    assert!(
        lines
            .iter()
            .any(|l| l.contains("WARN") && l.contains("test route")),
        "Captured: {lines:#?}"
    );
    let sidecar = super::super::hook_ingest::read_ingest_failure(&ledger)
        .expect("readable sidecar")
        .expect("the failure is recorded");
    assert!(sidecar.error.contains("test route"), "{}", sidecar.error);
}
