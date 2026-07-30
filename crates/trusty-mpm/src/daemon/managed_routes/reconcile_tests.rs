//! Tests for the report-only reconcile-worktrees route (#4207 slice 3, #4288).
//!
//! Why: the route's two claims are that it SURFACES the excluded set (invisible
//! on every other worktree surface) and that it MUTATES NOTHING. Both are
//! asserted against a real git fixture — a mocked git could not tell an
//! admitted worktree from an excluded one, which is the whole distinction.
//! What: the manager method in isolation, then the HTTP route end-to-end.
//! Test: this file IS the test module.

use super::*;

use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The inventory reports admitted AND excluded worktrees, and touches nothing.
///
/// Why: `enumerate_registered_worktrees` returns only admitted paths, so the
/// locked worktree below — the operator's explicit "do not remove this" — is
/// invisible to every existing surface. An operator who cannot see it cannot
/// reason about it. And because this is the slice that must not act, the
/// on-disk state is asserted unchanged rather than assumed.
#[tokio::test]
async fn reconcile_inventory_reports_without_mutating_anything() {
    let fx = GitWorktreeFixture::new();
    let admitted = fx.add_worktree("plain-candidate");
    let locked = fx.add_worktree("operator-locked");
    fx.lock_worktree(&locked);
    let sentinel = admitted.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);

    let root = crate::test_support::hermetic_temp_dir();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let report = mgr
        .reconcile_worktree_inventory(&fx.repos_root)
        .await
        .expect("inventory must not error");

    let canonical = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
    let entry = |p: &std::path::Path| {
        let want = canonical(p);
        report
            .entries
            .iter()
            .find(|e| e.path == want)
            .unwrap_or_else(|| panic!("{} must be reported", want.display()))
    };

    assert_eq!(report.counts.admitted, 1, "got {:?}", report.entries);
    assert_eq!(
        entry(&admitted).admission,
        Some(crate::session_manager::worktree_registry::Admission::Admitted)
    );
    assert_eq!(
        entry(&locked).admission,
        Some(crate::session_manager::worktree_registry::Admission::Locked),
        "the excluded set must be REPORTED — it is invisible everywhere else"
    );
    assert!(
        entry(&locked).reason.contains("locked"),
        "every row carries its reason; got {}",
        entry(&locked).reason
    );

    // Report-only: nothing removed, nothing written.
    assert!(admitted.is_dir() && locked.is_dir());
    assert!(
        !sentinel.exists(),
        "reconciliation must never WRITE an ownership sentinel"
    );
}

/// The HTTP route returns the same inventory as JSON and deletes nothing.
///
/// `await_holding_lock` is the point, not an oversight: the route reads the
/// workspace root from the process environment, so the crate-wide
/// `env_test_lock` must span the awaited route call or a sibling env test can
/// clobber the override mid-request.
#[allow(clippy::await_holding_lock)]
#[serial_test::serial]
#[tokio::test]
async fn reconcile_worktrees_route_reports_without_mutating() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("surfaced-by-the-route");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let root = crate::test_support::hermetic_temp_dir();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

    let _env = crate::core::trusty_tools_config::env_test_lock();
    // SAFETY: guarded by env_test_lock; removed immediately after the call.
    unsafe {
        std::env::set_var(
            crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV,
            &fx.repos_root,
        )
    };
    let resp = reconcile_worktrees_route(axum::extract::State(state))
        .await
        .into_response();
    // SAFETY: guarded by env_test_lock, still held.
    unsafe { std::env::remove_var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV) };

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("route body must be readable");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("route body must be JSON");

    let canonical = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    let entries = body
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("body must carry `entries`: {body}"));
    let row = entries
        .iter()
        .find(|e| e.get("path").and_then(serde_json::Value::as_str) == canonical.to_str())
        .unwrap_or_else(|| panic!("{} must be reported: {body}", canonical.display()));
    assert_eq!(
        row.get("state").and_then(serde_json::Value::as_str),
        Some("orphaned"),
        "row was {row}"
    );
    assert!(
        !row.get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .is_empty(),
        "every row must carry a reason: {row}"
    );
    assert_eq!(
        body.pointer("/counts/orphaned")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "body was {body}"
    );
    assert!(
        wt.is_dir(),
        "a reporting route must never remove a worktree"
    );
}
