//! Tests for #8301: `tm pr merge`'s post-merge choice outlives the process.
//!
//! Why: the supervisor sweep reads the cleanup registry minutes after the merge
//! exits, so a choice that is not written there — before the merge — is
//! overridden by the WIDE cleanup. Each test reads the registry back rather
//! than trusting a returned enum.
//! What: [`record_merge_scope`], [`post_merge_step`] and
//! [`merge_with_recorded_scope`] against a tempdir registry.
//! Test: this file IS the test module.

use std::cell::RefCell;
use std::path::PathBuf;

use trusty_mpm::core::pr_cleanup::{CleanupRegistry, CleanupScope, OpenedPr};

use super::super::{GhRun, GhRunner};
use super::{PostMerge, merge_with_recorded_scope, post_merge_step, record_merge_scope};
use crate::cli::PrMergeArgs;

const REPO: &str = "bobmatnyc/trusty-tools";

fn merge_args(no_cleanup: bool, no_delete_branch: bool, auto: bool) -> PrMergeArgs {
    PrMergeArgs {
        pr: 8301,
        auto,
        no_delete_branch,
        no_cleanup,
        repo: None,
    }
}

/// A registry holding one pending entry for PR 8301.
fn registry_with_entry(dir: &std::path::Path) -> CleanupRegistry {
    let reg = CleanupRegistry::under_root(dir);
    reg.record_open(OpenedPr {
        pr: 8301,
        repo: REPO.to_string(),
        repo_root: PathBuf::from("/repo"),
        opened_at: chrono::Utc::now(),
        cleaned_at: None,
        scope: CleanupScope::Wide,
    })
    .expect("record");
    reg
}

/// #8301: either opt-out flag, with or without `--auto`, never yields the
/// cleanup step.
#[test]
fn post_merge_no_cleanup_never_reaches_after_merge() {
    for (no_cleanup, no_delete_branch, auto) in [
        (true, false, false),
        (false, true, false),
        (true, false, true),
    ] {
        let args = merge_args(no_cleanup, no_delete_branch, auto);
        assert_eq!(
            post_merge_step(&args, REPO.to_string()),
            PostMerge::Deferred,
            "no_cleanup={no_cleanup} no_delete_branch={no_delete_branch} auto={auto}"
        );
    }
}

/// 🔴 #8301: `--no-cleanup` takes the entry out of the sweep, and the repo slug
/// matches whatever case `gh` spelled it in.
#[test]
fn post_merge_no_cleanup_defers_the_registry_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());

    record_merge_scope(
        &merge_args(true, false, false),
        "BobMatNYC/Trusty-Tools",
        &reg,
    )
    .expect("record");

    assert!(
        reg.pending().is_empty(),
        "a deferred entry must not be pending for the sweep: {:?}",
        reg.entries()
    );
    assert_eq!(reg.entries()[0].scope, CleanupScope::Deferred);
}

/// 🔴 #8301: the merge-chained cleanup records its head-only scope, so a
/// blocked run is retried head-only, never wide.
#[test]
fn post_merge_cleanup_records_a_head_only_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());
    let args = merge_args(false, false, false);

    record_merge_scope(&args, REPO, &reg).expect("record");

    assert_eq!(reg.entries()[0].scope, CleanupScope::HeadOnly);
    assert_eq!(reg.pending().len(), 1, "it is still swept, head-only");
    assert_eq!(
        post_merge_step(&args, REPO.to_string()),
        PostMerge::Cleanup {
            repo: REPO.to_string()
        }
    );
}

/// 🔴 #8301 round 2: `--auto` leaves the entry to the sweep, but bound to the
/// head-only scope. Fails before the fix, which recorded nothing under
/// `--auto` and let the sweep run wide.
#[test]
fn post_merge_auto_leaves_the_entry_to_the_sweep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());
    let args = merge_args(false, false, true);

    record_merge_scope(&args, REPO, &reg).expect("record");

    assert_eq!(
        post_merge_step(&args, REPO.to_string()),
        PostMerge::AwaitSweep
    );
    assert_eq!(reg.entries()[0].scope, CleanupScope::HeadOnly);
}

/// A `gh` that records every call and answers each one with a failure.
struct RecordingGh {
    calls: RefCell<Vec<String>>,
}

impl GhRunner for RecordingGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        self.calls.borrow_mut().push(args.join(" "));
        Ok(GhRun {
            success: false,
            stdout: String::new(),
            stderr: "RecordingGh answers nothing".to_string(),
        })
    }
}

/// 🔴 #8301 round 2: a registry that cannot be written stops the merge before
/// `gh` is asked anything, with an error (a non-zero exit). Fails before the
/// fix, which merged first and recorded the scope afterwards.
#[test]
fn merge_aborts_before_merging_when_the_scope_cannot_be_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"a regular file").expect("write blocker");
    // The registry's parent is a regular file, so neither lock nor write works.
    let reg = CleanupRegistry::under_root(blocker.join("root"));
    let gh = RecordingGh {
        calls: RefCell::new(Vec::new()),
    };

    let outcome = merge_with_recorded_scope(
        &gh,
        &merge_args(false, false, false),
        || Ok(REPO.to_string()),
        &reg,
    );

    let err = outcome.expect_err("an unrecordable scope must fail the command");
    assert!(err.to_string().contains("not merging #8301"), "{err:#}");
    assert!(
        err.to_string().contains("aside to proceed"),
        "the error names the recovery: {err:#}"
    );
    assert!(
        gh.calls.borrow().is_empty(),
        "nothing may reach `gh` — no merge: {:?}",
        gh.calls.borrow()
    );
}

/// 🔴 #8301 round 3: the scope-aware marker cannot be written (a directory
/// occupies its path), so the write fails closed and nothing reaches `gh`.
/// Fails if the marker write's error is dropped.
#[test]
fn merge_aborts_when_the_scope_marker_cannot_be_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());
    let marker = dir.path().join("pr-cleanup.json.scoped");
    std::fs::remove_file(&marker).expect("premise: the seed write left a marker");
    std::fs::create_dir(&marker).expect("occupy the marker path");
    let gh = RecordingGh {
        calls: RefCell::new(Vec::new()),
    };

    let outcome = merge_with_recorded_scope(
        &gh,
        &merge_args(false, false, false),
        || Ok(REPO.to_string()),
        &reg,
    );

    let err = outcome.expect_err("a failed marker write must fail the command");
    assert!(err.to_string().contains("not merging #8301"), "{err:#}");
    assert!(gh.calls.borrow().is_empty(), "{:?}", gh.calls.borrow());
}

/// 🔴 #8301 round 3: a registry an older writer rewrote makes
/// `record_merge_scope` warn with the `tm restart` fix, and the scope is
/// still recorded. Fails before the fix, which never looked.
#[test]
fn record_merge_scope_warns_on_a_registry_an_older_writer_rewrote() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());
    // What an older daemon's `mark_cleaned` writes: no `format` stamp.
    std::fs::write(
        reg.path(),
        format!(
            "{{\"entries\":[{{\"pr\":8301,\"repo\":\"{REPO}\",\"repo_root\":\"/repo\",\
             \"opened_at\":\"2026-09-23T00:00:00Z\"}}]}}"
        ),
    )
    .expect("older write");

    let warnings = record_merge_scope(&merge_args(true, false, false), REPO, &reg).expect("record");

    assert!(
        warnings.iter().any(|w| w.contains("tm restart")),
        "{warnings:?}"
    );
    assert_eq!(reg.entries()[0].scope, CleanupScope::Deferred);
}
