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

use super::{format_state_column, short_timestamp, truncate};
use super::{session_activity, session_decommission, session_resume, session_stop};

#[test]
fn truncate_clips_and_appends_ellipsis() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello world", 5), "hell\u{2026}");
    assert_eq!(truncate("", 5), "");
    assert_eq!(truncate("abcde", 5), "abcde");
}

/// #2595: `tm sessions ls` must mark a dead pick right in the STATE column so
/// the operator sees it without selecting the session and hitting a 422 first.
#[test]
fn format_state_column_appends_dead_marker() {
    assert_eq!(
        format_state_column("stopped", true, false),
        "stopped [dead]"
    );
    assert_eq!(
        format_state_column("errored", true, false),
        "errored [dead]"
    );
}

/// #2444: `tm sessions ls` must mark a session whose deployed assets drifted
/// from the catalog, and both markers must be able to appear together.
#[test]
fn format_state_column_appends_stale_assets_marker() {
    assert_eq!(
        format_state_column("active", false, true),
        "active [stale-assets]"
    );
    assert_eq!(
        format_state_column("stopped", true, true),
        "stopped [dead] [stale-assets]",
        "both markers must be able to appear together"
    );
}

/// #2595: a healthy (resumable) session's STATE column must render byte-for-byte
/// unchanged — no regression for the common case.
#[test]
fn format_state_column_leaves_healthy_state_unchanged() {
    assert_eq!(format_state_column("active", false, false), "active");
    assert_eq!(format_state_column("stopped", false, false), "stopped");
    assert_eq!(
        format_state_column("provisioning", false, false),
        "provisioning"
    );
}

#[test]
fn format_state_column_renders_deleted_marker() {
    // A soft-deleted record renders the `--deleted--` marker (#2012) instead of
    // the raw `deleted` state, so the master list REFLECTS the deletion.
    assert_eq!(format_state_column("deleted", false, false), "--deleted--");
    // Markers still compose on top of the deleted base.
    assert_eq!(
        format_state_column("deleted", true, false),
        "--deleted-- [dead]"
    );
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
/// that `session_resume` delegates to `resume_session` — the same
/// non-swallowing guarantee #2457/#2521 established for the old direct-POST
/// implementation. Deterministic and tmux/TTY-independent: the failure fires
/// inside `restart_via_daemon`, BEFORE `resume_session` would ever reach the
/// terminal hand-off (`tmux_attach`).
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

/// #2649 code-critic review, HIGH #1 + HIGH #2: a session that is NOT
/// stopped/errored (e.g. `active`/`provisioning`) WITH a live tmux pane must
/// resolve to `ResumeAction::Attach` — no `/resume` POST, no daemon-side
/// mutation at all — the PM-accepted idempotent "resume = get me into this
/// session" UX (see `session_resume`'s doc for the full rationale; this
/// SUPERSEDES the pre-#2649 behavior of bailing with the daemon's raw 409
/// "cannot resume a session in state 'active'"). It also proves the HIGH #1
/// `no_attach` gate: under `cargo test`, stdin is never a TTY, so
/// `session_resume` must still return `Ok(())` (headless success) rather
/// than attempting (and presumably failing/hanging on) a real tmux attach
/// with no controlling terminal to move.
///
/// Why `provisioning` and not the literal `active` string: branch selection
/// here is state-string-independent — both `active` and `provisioning` fail
/// `needs_restart` identically, so the `Provisioning` state `create_with_id`
/// naturally leaves a freshly-seeded record in is an equally valid stand-in.
/// Getting a record into `Active` through public API surface alone would
/// require the separate `/reactivate` machinery (#2453), adding seeding
/// complexity with no additional coverage — the literal `"active"` string is
/// already exhaustively covered at the pure-function layer by
/// `guided_resume_plan_active_live_tmux_attaches` in `tests_behavior_c_tests.rs`.
///
/// What: seeds a session via `create_with_id` (left in its default
/// `Provisioning` state), then spins up a REAL (not the daemon's fake) tmux
/// session with the record's EXACT `tmux_name` — the CLI's own client-side
/// `tmux_has_session` liveness probe always shells to the real system tmux
/// regardless of what driver the (isolated, fake-driven) daemon uses
/// server-side, so this is the one deterministic way to make that probe
/// report "live" without depending on this test process's own tmux/TTY
/// environment for the OUTCOME (only tmux's presence on `PATH`, already a
/// hard dependency of this workspace's test suite — see
/// `session_manager/tests.rs`). Calls the real `session_resume` CLI function
/// and asserts: (1) `Ok(())` — headless success; (2) the daemon's own record
/// of the session state is UNCHANGED afterward (still `provisioning`) — the
/// only way to prove NO `/resume` POST landed, since a successful restart
/// always flips state to `active`. The real tmux session is killed
/// unconditionally (even on assertion failure) before returning.
#[tokio::test]
async fn session_resume_headless_active_live_tmux_skips_restart_and_attach() {
    use trusty_mpm::daemon::{api, state::DaemonState};
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::ManagedSessionId;

    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);
    let id = ManagedSessionId::new();
    let ws = root.join(format!("{id}-headless-attach-ws"));
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            id,
            "regression: #2649 headless active-live-tmux CLI test".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
    assert_eq!(
        record.state.to_string(),
        "provisioning",
        "sanity: create_with_id must leave a fresh record un-stopped/un-errored"
    );

    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let create_status = std::process::Command::new(&tmux_bin)
        .args(["new-session", "-d", "-s", &record.tmux_name])
        .status()
        .expect("spawn real tmux session for the liveness probe");
    assert!(
        create_status.success(),
        "failed to create the real scratch tmux session"
    );

    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    let result = session_resume(&client, &url, id.to_string()).await;

    // Unconditional cleanup of the real tmux session, even if an assertion
    // below panics.
    let _ = std::process::Command::new(&tmux_bin)
        .args(["kill-session", "-t", &record.tmux_name])
        .output();

    result.expect(
        "headless resume of an already-live, non-stopped/errored session must succeed (Ok), \
         not error",
    );

    let after: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after.get("state").and_then(|v| v.as_str()),
        Some("provisioning"),
        "state must be UNCHANGED — a /resume POST would have flipped it to 'active'; this \
         proves the Attach branch never issued a daemon-side restart"
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
