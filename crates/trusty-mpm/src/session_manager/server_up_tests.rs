//! Tests for the tmux-server-up guard (#3823).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! regression coverage lives here (mirroring `resume_reattach_tests.rs` /
//! `naming_tests.rs`) so that file does not grow past its limit.
//! What: proves `SessionManager::create_with_id`, `create_with_reserved_name`,
//! and `resume` all confirm the tmux server is up via
//! `ManagedTmuxDriver::ensure_server_up` BEFORE issuing any other tmux call
//! (`list-sessions` via name resolution/dedup, `session_exists`) — on a
//! machine where tmux has never run, that FIRST `list-sessions` call used to
//! 500 the whole flow before the pre-existing #3386/#3722
//! `create_managed_session` choke point was ever reached. Also proves the
//! guard's failure short-circuits BEFORE `create_session` is ever attempted
//! (never a wasted/partial session creation).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::make_manager;

/// `create` must confirm the tmux server is up BEFORE issuing any other
/// tmux call (`list-sessions` via name resolution, then `create_session`).
#[tokio::test]
async fn create_ensures_server_up_before_creating_session() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt-server-up")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create must succeed when the server can be started");

    assert_eq!(
        *fake.ensure_server_up_calls.lock().unwrap(),
        1,
        "create must confirm the tmux server is up exactly once"
    );
    assert_eq!(
        fake.create_cwd_calls.lock().unwrap().len(),
        1,
        "create_session must still run once the server-up guard succeeds"
    );
    assert_eq!(record.state, ManagedSessionState::Provisioning);
}

/// When the tmux server cannot be started (e.g. a machine where tmux has
/// never run and `start-server` keeps failing), `create` must fail loudly
/// with the server-up error and must NEVER attempt `create_session` — the
/// "server-absent → StartServer issued before create" half of the
/// regression: the guard runs, and its failure short-circuits before any
/// session is spawned.
#[tokio::test]
async fn create_fails_loudly_when_server_cannot_start() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;
    *fake.ensure_server_up_should_fail.lock().unwrap() = true;

    let err = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt-server-down")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("create must fail when the tmux server cannot be started");

    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "must surface as TmuxUnavailable, not a generic error: {err:?}"
    );
    assert_eq!(
        *fake.ensure_server_up_calls.lock().unwrap(),
        1,
        "the server-up guard must have run (and been the thing that failed)"
    );
    assert!(
        fake.create_cwd_calls.lock().unwrap().is_empty(),
        "create_session must NEVER run when the server-up guard fails first"
    );
}

/// `create_with_reserved_name` (the in-project spawn path) must also
/// confirm the server is up before its own first tmux call
/// (`dedupe_session_name`'s `list-sessions`), independently of
/// `create_with_id`'s guard.
#[tokio::test]
async fn create_with_reserved_name_ensures_server_up_before_creating_session() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    mgr.create_with_reserved_name(
        ManagedSessionId::new(),
        "tm-reserved-server-up-01".into(),
        "task".into(),
        Some(PathBuf::from("/tmp/wt-reserved")),
        None,
        None,
        None,
        crate::runtime::RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("create_with_reserved_name must succeed when the server can be started");

    assert_eq!(
        *fake.ensure_server_up_calls.lock().unwrap(),
        1,
        "create_with_reserved_name must confirm the tmux server is up exactly once"
    );
    assert_eq!(fake.create_cwd_calls.lock().unwrap().len(), 1);
}

/// `resume` must also confirm the tmux server is up before its first tmux
/// call, and must fail loudly (never attempt to recreate the session) when
/// the server cannot be started.
#[tokio::test]
async fn resume_fails_loudly_when_server_cannot_start() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workspace_dir = crate::test_support::hermetic_temp_dir();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_dir.path().to_owned()),
            Some("resume-server-down".into()),
            Some(workspace_dir.path().to_owned()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.stop(&record.id).await.expect("stop");

    // Reset the counter recorded during create/stop so this test observes
    // only the resume call's own guard.
    *fake.ensure_server_up_calls.lock().unwrap() = 0;
    *fake.ensure_server_up_should_fail.lock().unwrap() = true;
    let creates_before = fake.create_cwd_calls.lock().unwrap().len();

    let err = mgr
        .resume(&record.id)
        .await
        .expect_err("resume must fail when the tmux server cannot be started");

    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "must surface as TmuxUnavailable: {err:?}"
    );
    assert_eq!(*fake.ensure_server_up_calls.lock().unwrap(), 1);
    assert_eq!(
        fake.create_cwd_calls.lock().unwrap().len(),
        creates_before,
        "resume must never attempt to recreate the session when the \
         server-up guard fails first"
    );
}
