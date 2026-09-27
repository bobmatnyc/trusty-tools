//! Tests for the daemon's automatic-resume relauncher (#8233).
//!
//! Why: the relauncher is what turns "the manager asked for a runtime" into
//! "the same adapter path an interactive resume takes". Its failure arm is the
//! one that must never be downgraded — a relaunch with no runtime behind it has
//! to come back as an `Err`, because the manager's only alternative is to leave
//! the record `Active`.
//! What: the dropped-state arm is driveable without a daemon; the adapter arm
//! is covered end to end by the `session_manager_mvp` resume tests, which run
//! the same `record_resume_outcome` this delegates to.
//! Test: this file.

use super::*;

/// FAIL-OPEN CHECK (#8233): a daemon state that has been dropped cannot start
/// anything. Answering `Ok` here — the shape a "best-effort" relaunch would
/// take — would let `resume_auto` mark the record `Active` behind a pane
/// nothing was ever put into.
#[tokio::test]
async fn a_dropped_daemon_state_fails_the_relaunch() {
    let root = tempfile::tempdir().expect("tempdir");
    let relauncher = {
        let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().into()).await);
        DaemonRelauncher::new(&state)
        // `state` drops here — the `Weak` can no longer upgrade.
    };
    let record = crate::session_manager::tests::make_active_test_record(
        "tmpm-gone",
        "task",
        "/tmp/relaunch-test",
    );

    let err = relauncher
        .relaunch(&record)
        .await
        .expect_err("a relaunch with no daemon behind it must not report success");

    assert!(
        err.contains("daemon state is gone"),
        "the failure must name why nothing was started: {err}"
    );
}
