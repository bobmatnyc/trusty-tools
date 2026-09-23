//! Tests for #8301: `tm pr merge`'s post-merge choice outlives the process.
//!
//! Why: the supervisor sweep reads the cleanup registry minutes after the merge
//! exits, so a choice that is not written there is overridden by the WIDE
//! cleanup. Each test reads the registry back rather than the returned enum.
//! What: [`post_merge_step`] against a tempdir registry holding one entry.
//! Test: this file IS the test module.

use std::path::PathBuf;

use trusty_mpm::core::pr_cleanup::{CleanupRegistry, CleanupScope, OpenedPr};

use super::{PostMerge, post_merge_step};
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

fn slug() -> anyhow::Result<String> {
    Ok(REPO.to_string())
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
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = registry_with_entry(dir.path());
        let args = merge_args(no_cleanup, no_delete_branch, auto);
        let step = post_merge_step(&args, slug, &reg).expect("step");
        assert_eq!(
            step,
            PostMerge::Deferred,
            "no_cleanup={no_cleanup} no_delete_branch={no_delete_branch} auto={auto}"
        );
    }
}

/// 🔴 #8301: `--no-cleanup` takes the entry out of the sweep. Fails before the
/// fix, which left the entry `pending` for the sweep's wide cleanup.
#[test]
fn post_merge_no_cleanup_defers_the_registry_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());

    post_merge_step(&merge_args(true, false, false), slug, &reg).expect("step");

    assert!(
        reg.pending().is_empty(),
        "a deferred entry must not be pending for the sweep: {:?}",
        reg.entries()
    );
    assert_eq!(reg.entries()[0].scope, CleanupScope::Deferred);
}

/// 🔴 #8301: the merge-chained cleanup records its head-only scope before it
/// runs, so a blocked run is retried head-only, never wide.
#[test]
fn post_merge_cleanup_records_a_head_only_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());

    let step = post_merge_step(&merge_args(false, false, false), slug, &reg).expect("step");

    assert_eq!(
        step,
        PostMerge::Cleanup {
            repo: REPO.to_string()
        }
    );
    assert_eq!(reg.entries()[0].scope, CleanupScope::HeadOnly);
    assert_eq!(reg.pending().len(), 1, "it is still swept, head-only");
}

/// `--auto` alone records nothing and resolves no repo: nothing has merged.
#[test]
fn post_merge_auto_leaves_the_entry_to_the_sweep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_with_entry(dir.path());
    let no_slug = || -> anyhow::Result<String> { anyhow::bail!("must not be resolved") };

    let step = post_merge_step(&merge_args(false, false, true), no_slug, &reg).expect("step");

    assert_eq!(step, PostMerge::AwaitSweep);
    assert_eq!(reg.entries()[0].scope, CleanupScope::Wide);
}
