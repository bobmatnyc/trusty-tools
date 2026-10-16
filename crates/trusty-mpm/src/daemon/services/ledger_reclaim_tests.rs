//! Tests for the daemon's ledger reclaim pass (#8994 S3a criteria 1 and 8).
//!
//! Why: the config flag is the only thing between the backlog and its
//! deletion, so it must fail closed; a pass that cannot read the ledger must
//! remove nothing and say so.
//! What: scratch config homes for [`mode_at`]; a scratch-rooted daemon for
//! [`run`].
//! Test: this file.

use std::path::PathBuf;
use std::sync::Arc;

use super::{LedgerReclaimOutcome, log_outcome, mode_at, run};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_reclaim::ReclaimMode;

fn home_with(config: Option<&str>) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    if let Some(yaml) = config {
        let path = trusty_common::crate_config::crate_config_path_at(
            home.path(),
            crate::core::trusty_tools_config::CRATE_NAME,
        );
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, yaml).expect("write config");
    }
    home
}

/// #8994 S3a criterion 1: removal is off by default and on only when the
/// config parses and says so; an unreadable config reports, with its error.
#[test]
fn the_mode_is_report_unless_the_config_says_remove() {
    let cases = [
        (None, ReclaimMode::Report, false),
        (
            Some("disk:\n  max_usage_pct: 90\n"),
            ReclaimMode::Report,
            false,
        ),
        (
            Some("disk:\n  ledger_reclaim_remove: false\n"),
            ReclaimMode::Report,
            false,
        ),
        (
            Some("disk:\n  ledger_reclaim_remove: true\n"),
            ReclaimMode::Remove,
            false,
        ),
        (
            Some("disk:\n  ledger_reclaim_remove: true\nauto_resume: not-a-bool\n"),
            ReclaimMode::Report,
            true,
        ),
    ];
    for (config, want, errs) in cases {
        let home = home_with(config);
        let (mode, error) = mode_at(home.path());
        assert_eq!(mode, want, "{config:?}");
        assert_eq!(error.is_some(), errs, "{config:?}: {error:?}");
    }
}

fn scratch_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (Arc::new(DaemonState::with_root(root)), dir)
}

/// #8994 S3a: an empty ledger is a pass that removes nothing, even in Remove
/// mode.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_ledger_removes_nothing() {
    let (state, _dir) = scratch_state();
    let outcome = run(&state, ReclaimMode::Remove).await.expect("pass runs");
    assert_eq!(outcome, LedgerReclaimOutcome::default());
}

/// #8994 S3a criterion 8: a ledger that cannot be read is an error, never an
/// empty success.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_ledger_runs_no_pass() {
    let (state, dir) = scratch_state();
    let ledger = std::fs::canonicalize(dir.path())
        .expect("canonicalize")
        .join(crate::core::worktree_ledger::LEDGER_FILE);
    std::fs::create_dir_all(&ledger).expect("a directory where the file goes");
    let err = run(&state, ReclaimMode::Remove)
        .await
        .expect_err("an unreadable ledger is an error");
    assert!(err.contains("worktree ledger"), "{err}");
}

/// #8994 S3a criterion 8: a removal that did not complete is reported as a
/// failure; a clean report is not.
#[test]
fn a_failed_removal_is_logged_as_a_failure() {
    let mut outcome = LedgerReclaimOutcome {
        would_remove: vec![(PathBuf::from("/r/a"), "PR #1 merged".into())],
        kept: vec![(PathBuf::from("/r/b"), "F1: HEAD is detached".into())],
        ..LedgerReclaimOutcome::default()
    };
    assert!(!log_outcome(ReclaimMode::Report, &outcome));
    outcome
        .failed
        .push((PathBuf::from("/r/c"), "git refused".into()));
    assert!(log_outcome(ReclaimMode::Remove, &outcome));
}
