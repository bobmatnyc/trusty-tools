//! Unit tests for `SessionManager::set_source_id`'s bounded retry (#2157 item 5),
//! and for the #12077 `SessionManager::set_claude_sandbox` beside it.
//!
//! Why: split out of `tests.rs` (which was already at the 1500-SLOC test cap)
//! rather than grown further, mirroring the established pattern of
//! `restart_tests.rs`/`reactivate_tests.rs`/`naming_tests.rs` etc.
//! What: the happy path (one clean write, no retry needed) and the
//! exhausted-retry path (an id that never resolves surfaces a typed error
//! rather than hanging or silently succeeding).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionId;
use super::tests::make_manager;

#[tokio::test]
async fn set_source_id_succeeds_first_try() {
    // #2157 item 5: the common case — one clean read-modify-write — must
    // succeed without needing any retry.
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    mgr.set_source_id(&record.id, "owner/repo")
        .await
        .expect("set_source_id");

    let reloaded = mgr.get(&record.id).await.expect("get after set");
    assert_eq!(reloaded.source_id.as_deref(), Some("owner/repo"));
}

#[tokio::test]
async fn set_source_id_returns_err_after_retries_for_missing_session() {
    // #2157 item 5: an id that never resolves must exhaust the bounded retry
    // loop and surface a typed error (not hang, not silently succeed) so the
    // caller's existing warn!-and-continue handling still degrades safely —
    // now backed by `tracing::error!` on exhaustion for a future
    // reconcile/doctor pass to find.
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let unknown_id = ManagedSessionId::new();

    let result = mgr.set_source_id(&unknown_id, "owner/repo").await;
    assert!(
        matches!(result, Err(ManagedError::SessionNotFound(_))),
        "expected SessionNotFound after exhausting retries, got {result:?}"
    );
}

/// #12077: an opt-in is stored on the record and survives a reload; the
/// default writes nothing, and a missing record still returns the opt-in for
/// this launch.
#[tokio::test]
async fn set_claude_sandbox_persists_only_an_opt_in_12077() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let off = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt-off")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let returned = mgr.set_claude_sandbox(off.clone(), false).await;
    assert!(!returned.claude_sandbox);
    assert!(!mgr.get(&off.id).await.expect("get").claude_sandbox);

    let on = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt-on")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let returned = mgr.set_claude_sandbox(on.clone(), true).await;
    assert!(returned.claude_sandbox, "the launch sees the opt-in");
    let reloaded = mgr.get(&on.id).await.expect("get after set");
    assert!(reloaded.claude_sandbox, "a resume reads the opt-in");

    let mut gone = on.clone();
    gone.id = ManagedSessionId::new();
    assert!(mgr.set_claude_sandbox(gone, true).await.claude_sandbox);
}
