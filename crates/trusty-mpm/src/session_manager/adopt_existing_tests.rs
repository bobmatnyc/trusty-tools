//! Tests for `SessionManager::adopt_existing` (#1433).
//!
//! Why: extracted from `tests.rs` (issue #2468) to keep that file under its
//! own 1500-SLOC test cap after the pane-scoped inject/observe `FakeTmuxDriver`
//! additions — mirrors why `reload_error_tests.rs` was split out earlier.
//! What: the explicit-adopt happy path (registers `Active`, never calls
//! `create_session`), the `TmuxSessionMissing`/`AlreadyAdopted` typed-error
//! guards, the non-`tmpm-`-prefix allowance (unlike reconcile's automatic
//! adoption), and the persisted `ephemeral` flag.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionState;
use super::tests::make_manager;

#[tokio::test]
async fn manager_adopt_existing_registers_active() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    // The pane already exists (operator started it outside trusty-mpm).
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-hand-started".into());

    let record = mgr
        .adopt_existing(
            "tmpm-hand-started",
            PathBuf::from("/Users/op/work/proj"),
            "drive my hand-started session".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("adopt existing");

    assert_eq!(record.tmux_name, "tmpm-hand-started");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert_eq!(record.cwd, PathBuf::from("/Users/op/work/proj"));
    assert_eq!(record.task, "drive my hand-started session");

    // Adoption must NOT spawn a new tmux session — the pane already exists.
    assert!(
        fake.create_cwd_calls.lock().unwrap().is_empty(),
        "adopt_existing must NOT call create_session; calls: {:?}",
        fake.create_cwd_calls.lock().unwrap()
    );

    // The record is durably queryable.
    let got = mgr.get(&record.id).await.expect("get adopted");
    assert_eq!(got.id, record.id);
    assert_eq!(got.state, ManagedSessionState::Active);
}

/// Adopting a tmux name that does NOT exist on the host must error — you cannot
/// adopt a pane that is not there (#1433).
///
/// Why: this is the inverse of `create`'s NameCollision guard. The error must be
/// the dedicated `TmuxSessionMissing` variant so the HTTP layer maps it to a 404.
/// What: adopts a name the driver does not report and asserts the typed error.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_missing_tmux_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let err = mgr
        .adopt_existing(
            "tmpm-not-here",
            PathBuf::from("/tmp/x"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("adopting a nonexistent pane must fail");

    assert!(
        matches!(err, ManagedError::TmuxSessionMissing(ref n) if n == "tmpm-not-here"),
        "expected TmuxSessionMissing, got {err:?}"
    );
}

/// Adopting a tmux name the store ALREADY tracks must error — no double records
/// for one pane (#1433).
///
/// Why: a second record for the same pane would split ownership and confuse every
/// downstream verb. The dedicated `AlreadyAdopted` variant lets the HTTP layer map
/// it to a 409 Conflict.
/// What: adopts once (succeeds), then adopts the same live name again and asserts
/// the second call returns `AlreadyAdopted`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_double_adopt_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names.lock().unwrap().push("tmpm-once".into());

    mgr.adopt_existing(
        "tmpm-once",
        PathBuf::from("/tmp/once"),
        String::new(),
        crate::runtime::RuntimeKind::default(),
        false,
    )
    .await
    .expect("first adopt succeeds");

    let err = mgr
        .adopt_existing(
            "tmpm-once",
            PathBuf::from("/tmp/once"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("second adopt of the same pane must fail");

    assert!(
        matches!(err, ManagedError::AlreadyAdopted(ref n) if n == "tmpm-once"),
        "expected AlreadyAdopted, got {err:?}"
    );
}

/// The explicit adopt path must allow NON-`tmpm-` names (unlike reconcile, which
/// filters to the `tmpm-` prefix for safe automatic adoption) (#1433).
///
/// Why: an operator naming a pane explicitly knows what they are adopting; the
/// `tmpm-` prefix filter exists only to make AUTOMATIC boot adoption safe. The
/// explicit path must not reject a session just because it lacks the prefix.
/// What: seeds a non-`tmpm-` live name, adopts it, and asserts success.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_allows_non_tmpm_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names
        .lock()
        .unwrap()
        .push("my-cli-session".into());

    let record = mgr
        .adopt_existing(
            "my-cli-session",
            PathBuf::from("/Users/op/repo"),
            "adopt non-prefixed".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("non-tmpm names are adoptable on the explicit path");

    assert_eq!(record.tmux_name, "my-cli-session");
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// `adopt_existing` persists the caller-supplied `ephemeral` flag (#1508).
///
/// Why: the e2e harness adopts panes as ephemeral; the flag must reach the record.
/// What: seeds a live pane, adopts it with `ephemeral=true`, asserts it persisted.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_persists_ephemeral_flag() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-eph-adopt".into());

    let record = mgr
        .adopt_existing(
            "tmpm-eph-adopt",
            PathBuf::from("/tmp/adopt"),
            "throwaway adopt".into(),
            crate::runtime::RuntimeKind::default(),
            true,
        )
        .await
        .expect("adopt ephemeral");

    assert!(
        mgr.get(&record.id).await.unwrap().ephemeral,
        "adopted ephemeral flag persisted"
    );
}
