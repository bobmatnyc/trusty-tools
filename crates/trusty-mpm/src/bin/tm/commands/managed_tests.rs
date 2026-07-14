//! Unit tests for `commands::managed` — split out of `managed.rs` (test-file
//! budget: 1500 SLOC) so the #2457 HTTP-round-trip coverage below doesn't push
//! the production file toward the 500-SLOC cap.
//!
//! Why: `session_stop`/`session_resume`/`session_decommission`/`session_activity`
//! previously printed "not found" (or, for resume, a conflict message) and
//! returned `Ok(())` on a 404/409 from the daemon — a genuine failure reported
//! as a successful exit code (#2457). The tests below drive each function
//! against a real hermetic daemon (mirroring `client::executor::tests`'s
//! `spawn_test_daemon` pattern) and assert `Err` is returned instead.
//! What: the `session_*_not_found_errors` tests cover the 404 exit-code fix.
//! `session_resume` no longer POSTs `/resume` directly (#2649) — it now
//! fetches the record and delegates to the shared
//! `guided_resume::resume_guided_session` helper (also used by the bare-`tm`
//! picker), so its restart/attach decision and error propagation are
//! primarily covered by that module's own `plan_resume`/`needs_restart`/
//! `is_zombie` unit tests plus the e2e suite. `session_resume_restart_failure_errors`
//! covers the ONE thing specific to this CLI-layer wrapper: a daemon-rejected
//! restart still surfaces as `Err`, not a swallowed `Ok(())` — the same
//! guarantee #2457/#2521's `session_resume_conflict_state_errors` (now
//! superseded — see `spawn_test_daemon_with_unrestartable_stopped_session`'s
//! doc for why the old `Provisioning`-seed/409 scenario no longer applies
//! cleanly at this layer) established for the prior direct-POST
//! implementation.
//! The pre-existing `truncate_*`/`short_timestamp_*`/
//! `decommission_message_reflects_workspace_removed` unit tests are carried
//! over unchanged from the inline module this file replaced.
//! Test: this file IS the test module for `commands::managed`.

use std::future::IntoFuture as _;

use super::{session_activity, session_decommission, session_resume, session_stop};
use super::{short_timestamp, truncate};

#[test]
fn truncate_clips_and_appends_ellipsis() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello world", 5), "hell\u{2026}");
    assert_eq!(truncate("", 5), "");
    assert_eq!(truncate("abcde", 5), "abcde");
}

#[test]
fn short_timestamp_formats_correctly() {
    assert_eq!(short_timestamp("2025-06-27T14:32:00Z"), "2025-06-27 14:32");
    assert_eq!(short_timestamp("short"), "short");
    assert_eq!(short_timestamp("2025-06-27T14:32"), "2025-06-27 14:32");
}

#[test]
fn decommission_message_reflects_workspace_removed() {
    // Guard that the key field names used in session_decommission match the
    // daemon's DecommissionResponse serde output. If the daemon renames those
    // keys this test catches the drift before the JSON decodes silently to None.
    let owned_removed = serde_json::json!({
        "id": "abc-123",
        "workspace_removed": true,
        "workspace_path_was": "/some/workspace/path"
    });
    assert_eq!(
        owned_removed
            .get("workspace_removed")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        owned_removed
            .get("workspace_path_was")
            .and_then(|v| v.as_str()),
        Some("/some/workspace/path")
    );
    let adopted_not_removed = serde_json::json!({
        "id": "xyz-456",
        "workspace_removed": false
    });
    assert_eq!(
        adopted_not_removed
            .get("workspace_removed")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(adopted_not_removed.get("workspace_path_was").is_none());
}

/// Spawn the daemon's real HTTP API on a random loopback port, rooted in a
/// throwaway temp directory.
///
/// Why: mirrors `client::executor::tests::spawn_test_daemon` — an empty
/// isolated managed-session store means ANY id is a genuine 404, so these
/// tests exercise the real daemon route (not a hand-rolled mock response).
/// What: builds `daemon::api::router(DaemonState::with_root_isolated_managed(...))`,
/// binds an ephemeral port, serves it on a background task, and returns the
/// base URL.
async fn spawn_test_daemon() -> String {
    use trusty_mpm::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// #2457: a 404 from `runtime-stop` on a nonexistent id must propagate as
/// `Err`, not a printed "not found" with `Ok(())`.
#[tokio::test]
async fn session_stop_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_stop(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// #2457: a 404 from `resume` on a nonexistent id must propagate as `Err`.
#[tokio::test]
async fn session_resume_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_resume(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// Spawn the daemon's real HTTP API with ONE managed session seeded `Stopped`
/// whose workspace/cwd directory was never created on disk, so a `resume`
/// call against it is a genuine, deterministic daemon-side restart failure
/// (`ManagedError::WorkspaceMissing` -> HTTP 422) — not a hand-rolled stub
/// response, and not dependent on this test process's own tmux/TTY
/// environment.
///
/// Why (#2649 — supersedes the old #2521 `spawn_test_daemon_with_unresumable_session`
/// helper): `session_resume` no longer POSTs `/resume` directly — it now
/// fetches the record and delegates to
/// `guided_resume::resume_guided_session`, the SAME helper the bare-`tm`
/// picker uses, so both entry points share one "restart, then hand the
/// terminal to the tmux window" implementation instead of maintaining two
/// (the exact duplication that let this bug survive five rounds of fixes to
/// the picker's sibling path). That helper picks its action purely from
/// state + live tmux liveness (`plan_resume`): seeding a record in a
/// non-`Stopped`/`Errored` state (the old `Provisioning` seed) no longer
/// reaches a raw 409 here — it is classified a "zombie" (record not
/// stopped/errored, tmux pane gone) and AUTO-RECONCILED via `/runtime-stop`
/// then `/resume`, exactly the #2001 behavior this fix ports to the
/// explicit-id verb. Driving a test through that path would, on success,
/// reach a REAL tmux `switch-client`/`attach-session` call whose outcome
/// depends on whether this test process has a real attached tmux client —
/// which it never does under `cargo test` — making the assertion flaky by
/// construction. Seeding directly into `Stopped` sidesteps the zombie branch
/// entirely (`needs_restart` is `true` regardless of tmux liveness), so the
/// only variable left is the daemon's `/resume` response.
/// What: `with_root_isolated_managed` seeds the `SessionManager` with
/// [`trusty_mpm::session_manager::real_tmux::FakeNoopTmuxDriver`] — no real
/// tmux process is ever spawned server-side, so nothing here depends on this
/// test process's own tmux/TTY environment. `create_with_id` does NOT touch
/// the filesystem itself (`ws` genuinely does not exist afterward), but
/// `SessionManager::stop`'s non-fatal pre-kill snapshot capture DOES:
/// `capture_into` unconditionally `mkdir -p`'s `<workspace_path>/.trusty-mpm/`
/// to write a (here, empty — the fake driver's `capture` returns `""`)
/// scrollback file, which recreates `ws` as a side effect (confirmed
/// empirically). So the directory is removed a SECOND time, after `stop`,
/// immediately before the router starts serving, to end up with a record
/// that is genuinely `Stopped` AND genuinely workspace-less at `/resume`
/// time. Returns `(base_url, id)`.
async fn spawn_test_daemon_with_unrestartable_stopped_session() -> (String, String) {
    use trusty_mpm::daemon::{api, state::DaemonState};
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::ManagedSessionId;

    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);

    let id = ManagedSessionId::new();
    let ws = root.join(format!("{id}-resume-missing-ws"));
    let mgr = state.session_manager().await;
    mgr.create_with_id(
        id,
        "regression: #2649 resume-restart-failure CLI test".to_string(),
        Some(ws.clone()),
        None,
        Some(ws.clone()),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session");
    mgr.stop(&id).await.expect("mark seeded session Stopped");
    tokio::fs::remove_dir_all(&ws)
        .await
        .expect("remove the dir stop()'s snapshot capture recreated, so it is genuinely absent");

    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (format!("http://{addr}"), id.to_string())
}

/// #2649: a daemon-rejected restart (422 — the seeded workspace directory was
/// never created on disk) must still propagate as `Err` at the CLI layer now
/// that `session_resume` delegates to `resume_guided_session` — the same
/// non-swallowing guarantee #2457/#2521 established for the old direct-POST
/// implementation. Deterministic and tmux/TTY-independent: the failure fires
/// inside `restart_via_daemon`, BEFORE `resume_guided_session` would ever
/// reach the terminal hand-off (`tmux_attach`).
#[tokio::test]
async fn session_resume_restart_failure_errors() {
    let (url, id) = spawn_test_daemon_with_unrestartable_stopped_session().await;
    let client = reqwest::Client::new();
    let err = session_resume(&client, &url, id)
        .await
        .expect_err("a daemon-rejected restart must be a hard failure, not Ok(())");
    assert!(
        err.to_string().contains("cannot restart"),
        "error should surface the daemon's rejection: {err}"
    );
}

/// #2457: a 404 from `decommission` on a nonexistent id must propagate as
/// `Err` — `prune.rs`'s bulk loop relies on this via `?`.
#[tokio::test]
async fn session_decommission_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_decommission(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// #2457: a 404 from `activity` on a nonexistent id must propagate as `Err`.
#[tokio::test]
async fn session_activity_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_activity(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}
