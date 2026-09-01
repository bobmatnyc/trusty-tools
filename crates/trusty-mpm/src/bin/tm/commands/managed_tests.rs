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
//! The pre-existing `truncate_*`/`short_timestamp_*` unit tests are carried over
//! unchanged from the inline module this file replaced;
//! `decommission_message_reflects_workspace_removed` was replaced by
//! `session_decommission_prints_daemon_verdict_over_http` (#5899).
//! Test: this file IS the test module for `commands::managed`.

use std::future::IntoFuture as _;

use super::super::managed_render::{
    format_ls_row, format_state_column, format_tombstone_row, short_timestamp, state_column_width,
    truncate,
};
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
        format_state_column("stopped", true, false, false, false),
        "stopped [dead]"
    );
    assert_eq!(
        format_state_column("errored", true, false, false, false),
        "errored [dead]"
    );
}

/// #2444: `tm sessions ls` must mark a session whose deployed assets drifted
/// from the catalog, and both markers must be able to appear together.
#[test]
fn format_state_column_appends_stale_assets_marker() {
    assert_eq!(
        format_state_column("active", false, true, false, false),
        "active [stale-assets]"
    );
    assert_eq!(
        format_state_column("stopped", true, true, false, false),
        "stopped [dead] [stale-assets]",
        "both markers must be able to appear together"
    );
}

/// #4322: a row the daemon did NOT probe must say so — `[assets ?]`, not
/// silence — so an operator cannot mistake a skipped check for a clean bill of
/// health. It composes with `[dead]`, and a real `stale` verdict always wins
/// over "undetermined" if both flags somehow arrive set.
#[test]
fn format_state_column_appends_unchecked_assets_marker() {
    assert_eq!(
        format_state_column("stopped", false, false, true, false),
        "stopped [assets ?]"
    );
    assert_eq!(
        format_state_column("stopped", true, false, true, false),
        "stopped [dead] [assets ?]",
        "the dead marker and the undetermined-assets marker must compose"
    );
    assert_eq!(
        format_state_column("stopped", false, true, true, false),
        "stopped [stale-assets]",
        "a real stale verdict must win over 'undetermined' — never both"
    );
}

/// #6568: a parked session must be distinguishable from an ordinary stopped
/// one, because nothing will ever auto-resume it again.
///
/// Test: this is the test. RED before the fix: the column had no such marker,
/// so a parked row rendered as a plain `stopped`.
#[test]
fn format_state_column_appends_resume_parked_marker() {
    assert_eq!(
        format_state_column("stopped", false, false, false, true),
        "stopped [resume-parked]"
    );
    // Markers coexist rather than shadowing each other.
    assert_eq!(
        format_state_column("stopped", true, true, false, true),
        "stopped [dead] [resume-parked] [stale-assets]"
    );
    // A session nothing parked is untouched.
    assert_eq!(
        format_state_column("stopped", false, false, false, false),
        "stopped"
    );
}

/// #2595: a healthy (resumable) session's STATE column must render byte-for-byte
/// unchanged — no regression for the common case.
#[test]
fn format_state_column_leaves_healthy_state_unchanged() {
    assert_eq!(
        format_state_column("active", false, false, false, false),
        "active"
    );
    assert_eq!(
        format_state_column("stopped", false, false, false, false),
        "stopped"
    );
    assert_eq!(
        format_state_column("provisioning", false, false, false, false),
        "provisioning"
    );
}

/// #3034: a deleted slot's row shows only its number and the placeholder —
/// never a blank-looking row that could be mistaken for "no session here".
#[test]
fn format_tombstone_row_shows_slot_and_placeholder() {
    assert_eq!(format_tombstone_row(1, false), "1      -- deleted --");
    assert_eq!(format_tombstone_row(42, false), "42     -- deleted --");
}

#[test]
fn format_state_column_renders_deleted_marker() {
    // A soft-deleted record renders the `--deleted--` marker (#2012) instead of
    // the raw `deleted` state, so the master list REFLECTS the deletion.
    assert_eq!(
        format_state_column("deleted", false, false, false, false),
        "--deleted--"
    );
    // Markers still compose on top of the deleted base.
    assert_eq!(
        format_state_column("deleted", true, false, false, false),
        "--deleted-- [dead]"
    );
}

#[test]
fn short_timestamp_formats_correctly() {
    assert_eq!(short_timestamp("2025-06-27T14:32:00Z"), "2025-06-27 14:32");
    assert_eq!(short_timestamp("short"), "short");
    assert_eq!(short_timestamp("2025-06-27T14:32"), "2025-06-27 14:32");
}

/// #5899: `session_decommission` must decode the real daemon response body and
/// leave an unowned workspace alone.
///
/// Replaces `decommission_message_reflects_workspace_removed`, which asserted only
/// that `serde_json` can read keys out of a literal it had just built — it never
/// touched the daemon or the handler, which is why the handler could go on printing
/// a hardcoded "workspace removed". This drives the handler against the real route:
/// the typed [`ManagedDecommissionOutcome`] decode is now part of the path, so a
/// daemon-side key rename surfaces as an `Err` here instead of a silent `None`.
/// The wording itself is asserted by `decommission_message_honours_every_verdict`.
#[tokio::test]
async fn session_decommission_prints_daemon_verdict_over_http() {
    use std::future::IntoFuture as _;
    use trusty_mpm::daemon::{api, state::DaemonState};
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::ManagedSessionId;

    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);
    let id = ManagedSessionId::new();
    let ws = root.join(format!("{id}-unowned-ws"));
    std::fs::create_dir_all(&ws).unwrap();
    state
        .session_manager()
        .await
        .create_with_id(
            id,
            "regression: #5899 decommission over HTTP".to_string(),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            None,
            None,
            RuntimeKind::default(),
            false,
            // Unowned: the daemon must report `workspace_removed: false`.
            false,
        )
        .await
        .expect("seed session");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, api::router(state)).into_future());

    session_decommission(
        &reqwest::Client::new(),
        &format!("http://{addr}"),
        id.to_string(),
    )
    .await
    .expect("decommissioning a seeded session must succeed");
    assert!(
        ws.exists(),
        "an unowned workspace must survive decommission: {}",
        ws.display()
    );
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
/// the filesystem itself (`ws` genuinely does not exist afterward). Before
/// #3715, `SessionManager::stop`'s non-fatal pre-kill snapshot capture would
/// silently `mkdir -p` `<workspace_path>/.trusty-mpm/` and recreate `ws` as a
/// side effect even though the workspace root never existed — that hazard is
/// now guarded (`capture_into` refuses to write when the workspace root is
/// missing), so `ws` genuinely stays absent through `stop()` with no extra
/// cleanup needed, giving a record that is genuinely `Stopped` AND genuinely
/// workspace-less at `/resume` time. Returns `(base_url, id)`.
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
    assert!(
        !ws.exists(),
        "#3715: stop()'s snapshot capture must NOT recreate a vanished workspace root"
    );

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
/// #3873: "already-live" now means a live RUNTIME, not merely a live tmux
/// session — see the fixture note below on why the pane must run a non-shell
/// command. The dead-runtime counterpart is
/// `session_resume_headless_dead_runtime_reconciles_and_restarts`.
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
/// always flips state to `active`. The real tmux session is owned by a
/// `ScratchTmuxSession` guard, so it is killed on every exit path — normal
/// return, assertion failure, and panic alike (#6116).
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
            // Process-unique so the derived `tmux_name` cannot collide in the
            // machine-global tmux namespace — see the fuller note on the
            // dead-runtime counterpart below. This test has always created a real
            // tmux session under a FIXED name (`tm-r-01`); with a second
            // real-tmux test now beside it, that is a live cross-binary hazard.
            Some(format!("https://example.com/r{}.git", std::process::id())),
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
    // #3873: the pane must run a NON-shell command. Since #3873, "live" means a
    // live RUNTIME, not merely a live tmux session — a bare `tmux new-session -d`
    // leaves the pane on a login shell, which is precisely the dead-runtime shape
    // this test is NOT trying to exercise (see the sibling test below, which is).
    // `sleep` is not in `orphan_gc::IDLE_SHELL_COMMANDS`, so it stands in for a
    // live agent process deterministically. Without it this fixture's verdict
    // depended on whether the login shell happened to still have a child mid-init
    // when the probe ran, making the test genuinely flaky under the new logic.
    //
    // #6116: owned by an RAII guard, so the session dies with this test whether
    // it returns, fails an assertion, or panics inside the wait below.
    let scratch = crate::test_support::tmux_session::ScratchTmuxSession::spawn(
        &tmux_bin,
        &record.tmux_name,
        "sleep 300",
    );
    // tmux runs the command through a shell, so there is a brief window after
    // `new-session` in which the pane still reports `sh`/`zsh` rather than
    // `sleep`. Probing inside that window reads the fixture as an idle shell and
    // sends this test down the #3873 dead-runtime branch. Wait for the pane to
    // settle into the live-runtime state the test is actually about — the exact
    // inverse of the wait in the dead-runtime counterpart below.
    wait_for_stable_live_runtime(&record.tmux_name);

    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    let result = session_resume(&client, &url, id.to_string()).await;

    // Killed here, before the assertions, as it always has been. Dropping the
    // guard is now only the EARLIEST it can happen — an assertion failure or
    // panic above already unwound through it.
    drop(scratch);

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

/// Block until `session_name`'s panes read as a settled LIVE runtime (#3873).
///
/// Why: the mirror of [`wait_for_stable_dead_runtime`] — see its doc. tmux
/// launches a pane command through a shell, so a pane told to run a long-lived
/// process still reports the shell for a moment; a test asserting about the
/// Attach branch must not start until the fixture actually expresses it.
/// What: polls until three consecutive probes report a live runtime, or panics
/// at a 10-second deadline. Any dead reading resets the streak.
fn wait_for_stable_live_runtime(session_name: &str) {
    const REQUIRED_AGREEING_READS: u32 = 3;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut streak = 0;
    while std::time::Instant::now() < deadline {
        if crate::commands::guided_resume::session_runtime_live(session_name) {
            streak += 1;
            if streak == REQUIRED_AGREEING_READS {
                return;
            }
        } else {
            streak = 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "fixture precondition: tmux session '{session_name}' never settled into a \
         stable live-runtime state within 10s; this test would silently exercise \
         the #3873 reconcile branch instead of the Attach branch it asserts about"
    );
}

/// Block until `session_name`'s panes read as a settled dead runtime (#3873).
///
/// Why: `session_runtime_live` consults the OS process tree, so a pane that has
/// only just been created can momentarily report a live child while it finishes
/// setting itself up. A single probe is therefore not a stable statement about
/// the fixture; a test that acts on one can silently exercise the opposite
/// branch from the one it asserts about. Requiring CONSECUTIVE agreeing reads
/// turns "dead right now" into "dead and settled".
/// What: polls until three consecutive probes report a dead runtime, or panics
/// at a 10-second deadline rather than letting the caller proceed on an
/// unsettled fixture (a silent proceed is what produced an intermittent
/// failure). Any live reading resets the streak.
fn wait_for_stable_dead_runtime(session_name: &str) {
    const REQUIRED_AGREEING_READS: u32 = 3;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut streak = 0;
    while std::time::Instant::now() < deadline {
        if crate::commands::guided_resume::session_runtime_live(session_name) {
            streak = 0;
        } else {
            streak += 1;
            if streak == REQUIRED_AGREEING_READS {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "fixture precondition: tmux session '{session_name}' never settled into a \
         stable dead-runtime state within 10s; this test cannot distinguish the \
         #3873 reconcile branch from Attach without it"
    );
}

/// #3873 end-to-end: a session whose tmux session is LIVE but whose runtime has
/// exited must reconcile-then-restart, not attach into the idle shell.
///
/// Why: the pure `plan_resume` seam proves the branch SELECTION, but not that
/// the selection survives the whole I/O driver — `resume_session` computes
/// `runtime_live` itself, and a wrong `pane_id`/session plumbing, a
/// short-circuit, or a daemon-side "re-attach to the live pane instead of
/// recreating" branch could each turn the fix into a silent no-op while every
/// unit test stayed green. This is the counterpart to
/// `session_resume_headless_active_live_tmux_skips_restart_and_attach`: same
/// seeding, same real-tmux liveness fixture, opposite pane command, opposite
/// expected outcome. Together they pin BOTH directions of the #3873 decision
/// against a real daemon.
/// What: seeds a `Provisioning` record with a workspace that EXISTS on disk (the
/// restart path validates it), spawns a real tmux session left on a bare login
/// shell — the exact dead-runtime shape — and asserts the daemon record flipped
/// to `active`, which only a `/runtime-stop` + `/resume` round-trip can do. The
/// daemon here is `FakeNoopTmuxDriver`-backed, so its `kill_session` is a no-op
/// and the real scratch session is torn down by this test. #6116: that teardown
/// is a `ScratchTmuxSession` guard rather than a trailing `kill-session`,
/// because `wait_for_stable_dead_runtime` below panics on timeout by design and
/// a trailing kill never runs on that path.
#[tokio::test]
async fn session_resume_headless_dead_runtime_reconciles_and_restarts() {
    use trusty_mpm::daemon::{api, state::DaemonState};
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::ManagedSessionId;

    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);
    let id = ManagedSessionId::new();
    let ws = root.join(format!("{id}-dead-runtime-ws"));
    // The restart path refuses a session whose workspace directory is gone, so
    // it must actually exist for this test to reach the /resume POST.
    std::fs::create_dir_all(&ws).expect("create workspace dir");
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            id,
            "regression: #3873 dead-runtime reconcile CLI test".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            // `tmux_name` is derived from the repo name, and this test creates a
            // REAL tmux session in the machine-global tmux namespace. The name
            // must therefore be unique BOTH from the sibling live-runtime test
            // (which would otherwise also derive `tm-r-01`) AND across test
            // BINARIES: this crate compiles the same sources into two bin targets
            // (`tm` and `trusty-mpm`) that cargo may run concurrently, so a fixed
            // name lets one process's cleanup destroy the other process's session
            // mid-test — observed as an intermittent failure of the final
            // assertion below.
            //
            // #6116: the slug also puts the DERIVED name in the reserved test
            // namespace, which the daemon's adoption sweep refuses — so a run
            // killed hard enough to skip the guard's `Drop` leaks a session
            // that never becomes a picker row.
            Some(format!(
                "https://example.com/{}.git",
                crate::test_support::tmux_session::reserved_project_slug(&format!(
                    "deadrt{}",
                    std::process::id()
                ))
            )),
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

    // #6116: the guard below covers a panic; nothing covers a SIGKILL. What
    // makes a session leaked that way harmless is the namespace it is in, so
    // assert the derived name actually landed there rather than assuming it.
    assert!(
        trusty_common::session_naming::is_reserved_test_session_name(&record.tmux_name),
        "this fixture's real tmux session must be one the daemon refuses to adopt, got {}",
        record.tmux_name
    );

    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    // A non-interactive shell, explicitly: the pane must be an idle shell with NO
    // live child — tmux session live, runtime dead, the #3873 defect shape.
    // Passing a command at all is what makes this deterministic. With no command
    // tmux starts the user's LOGIN shell, which runs the developer's rc files and
    // keeps spawning short-lived children for a while afterwards; the
    // `ChildLivenessProbe` gate correctly reads any of those as "still alive", so
    // the fixture's verdict depended on this machine's dotfiles.
    //
    // Note what actually runs: tmux executes the argument via its default-shell,
    // so `pane_current_command` here is that shell (`bash` on this machine,
    // verified with `tmux display-message -p '#{pane_current_command}'`), NOT the
    // literal `sh` spelled below. That is fine — every entry in
    // `orphan_gc::IDLE_SHELL_COMMANDS` is equally an idle shell, and the point of
    // the argument is only to skip login/rc processing so the pane settles
    // immediately and stays childless.
    //
    // #6116: owned by an RAII guard. `wait_for_stable_dead_runtime` below
    // panics on a 10s timeout by design, and the post-hoc `kill-session` this
    // replaces never ran on that path — every such run leaked a real
    // `tm-deadrt<pid>-01` session permanently. `Drop` still cannot run on a
    // SIGKILL; `spawn` also sweeps what an earlier hard-killed run left behind.
    let scratch = crate::test_support::tmux_session::ScratchTmuxSession::spawn(
        &tmux_bin,
        &record.tmux_name,
        "sh",
    );
    // Belt-and-braces on top of the deterministic fixture: require the "runtime
    // dead" reading to be STABLE (three consecutive probes) before proceeding,
    // so a single transient child during pane setup cannot send this test down
    // the Attach branch it is not testing.
    wait_for_stable_dead_runtime(&record.tmux_name);

    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    let result = session_resume(&client, &url, id.to_string()).await;

    // The daemon here is `FakeNoopTmuxDriver`-backed, so its `kill_session` is a
    // no-op and this guard is what actually tears the real session down.
    drop(scratch);

    // Deliberately NOT asserted as `Ok`. Whether the daemon can actually spawn a
    // runtime is environment-dependent (CI has no agent binary on PATH, so
    // `/resume` legitimately ends in `errored` there), and that is downstream of
    // what this test is about. The branch SELECTION is the claim, and the record
    // transition below proves it either way.
    let _ = result;

    let after: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let persisted = after
        .get("persisted_state")
        .and_then(|v| v.as_str())
        .expect("daemon must report persisted_state");
    // This is the exact discriminator between the two branches, and it holds in
    // any environment. Attach performs NO daemon round-trip, so the seeded
    // `provisioning` would survive untouched; reconcile-then-restart POSTs
    // `/runtime-stop` (-> `stopped`) and then `/resume` (-> `active`, or
    // `errored` if the runtime could not spawn). Any value other than
    // `provisioning` therefore proves the reconcile path ran.
    assert_ne!(
        persisted, "provisioning",
        "the record must have been reconciled and restarted — a /runtime-stop + \
         /resume round-trip moves it off 'provisioning' (to 'active', or \
         'errored' where no runtime binary exists). Reading 'provisioning' means \
         the CLI attached into the idle shell instead, i.e. the #3873 defect is back"
    );
}

/// #3531 core regression: reproduces the reported interactive-picker
/// dead-end against a REAL hermetic daemon (not a hand-rolled stub) — a
/// session whose tmux window is gone but whose PERSISTED record is still
/// `Active` (a zombie) must auto-reconcile via `session_resume`, never
/// surface the daemon's raw 409 ("cannot resume a session in state
/// 'active'; only Stopped or Errored sessions can be resumed").
///
/// Why: `spawn_test_daemon` (the isolated `FakeNoopTmuxDriver`-backed daemon)
/// always reports zero live tmux sessions (`list_sessions` → `Ok(vec![])`),
/// so ANY session summary the list/get endpoints return here is
/// display-reconciled to `state = "stopped"` (`reconcile_live_state`,
/// `summary.rs`) regardless of what is actually persisted — this is
/// EXACTLY the #3302 reconciliation that, pre-#3531, made the CLI's zombie
/// classification (`guided_resume::plan_resume`) misfire: it read the
/// display-reconciled `"stopped"` instead of the daemon's authoritative
/// persisted state, took the plain `Restart` branch, and the daemon's
/// `/resume` (which validates the REAL persisted state) rejected it with a
/// 409. Seeding the record `Active` (via `set_workspace`, mirroring a real
/// zombie — the runtime died/rebooted before the daemon's own reap tick
/// caught up) against this fake-tmux daemon reproduces that exact mismatch
/// deterministically, with no dependency on real tmux/TTY state.
///
/// This test deliberately does NOT assert `session_resume` returns `Ok(())`:
/// once the auto-reconcile resets the record to `Stopped`, the daemon's
/// `/resume` genuinely attempts to spawn a runtime (real `claude`/`tcode`
/// process) — an environment-dependent step this hermetic daemon does not
/// mock (mirrors the established convention in
/// `resume_managed_backfills_missing_status_line`/
/// `resume_managed_launches_despite_incomplete_deployment`,
/// `tests/session_manager_mvp.rs`: "the runtime adapter spawn itself is
/// allowed to fail in CI \[no real tmux/claude binary\]"). What this test DOES
/// assert is the actual #3531 fix: the specific 409 dead-end text must never
/// appear, whether the eventual spawn succeeds or fails for unrelated
/// environment reasons.
/// What: seeds a session `Active` with a workspace directory that genuinely
/// exists on disk (so the reconcile/restart attempt is not blocked by
/// `WorkspaceMissing`); asserts the GET response shows the #3531 mismatch
/// (`state: "stopped"`, `persisted_state: "active"`); calls the real
/// `session_resume` CLI function and asserts that IF it errors, the message
/// is NOT the daemon's raw "cannot resume a session in state" 409 — proving
/// the auto-reconcile (`/runtime-stop` then `/resume`) fired instead of
/// dead-ending; and confirms the daemon's final persisted state actually
/// changed away from the original untouched `active` zombie (proof a
/// daemon-side mutation genuinely occurred, not a silent no-op).
#[tokio::test]
async fn session_resume_zombie_active_tmux_absent_reconciles_and_restarts() {
    use trusty_mpm::daemon::{api, state::DaemonState};
    use trusty_mpm::runtime::RuntimeKind;
    use trusty_mpm::session_manager::{ManagedSessionId, ManagedSessionState};

    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root.clone()).await);
    let id = ManagedSessionId::new();
    let ws = root.join(format!("{id}-zombie-ws"));
    tokio::fs::create_dir_all(&ws)
        .await
        .expect("create a REAL workspace dir so the reconcile/restart attempt is not blocked");

    let mgr = state.session_manager().await;
    mgr.create_with_id(
        id,
        "regression: #3531 zombie-restart CLI test".to_string(),
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

    // Force the record to the zombie shape: PERSISTED Active, but this
    // isolated daemon's fake tmux driver reports no live sessions at all —
    // exactly "tmux window gone, daemon still says active".
    mgr.set_workspace(&id, ws, ManagedSessionState::Active)
        .await
        .expect("force record to Active (simulating a zombie)");

    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Sanity check: reproduce the #3531 mismatch at the wire level BEFORE
    // touching the resume path — the display `state` must already disagree
    // with `persisted_state`, exactly what made the pre-#3531 CLI misfire.
    let before: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        before.get("state").and_then(|v| v.as_str()),
        Some("stopped"),
        "sanity: the list/get endpoint's display reconciliation must show \
         'stopped' for an Active record with no live tmux (the #3302 behavior)"
    );
    assert_eq!(
        before.get("persisted_state").and_then(|v| v.as_str()),
        Some("active"),
        "sanity: persisted_state must still carry the TRUE record state"
    );

    // The actual #3531 fix under test: session_resume must auto-reconcile
    // this zombie instead of dead-ending on the daemon's raw 409. The
    // eventual runtime spawn is environment-dependent (see doc above), so
    // only the ABSENCE of the specific 409 dead-end text is asserted here.
    if let Err(e) = session_resume(&client, &url, id.to_string()).await {
        let msg = e.to_string();
        assert!(
            !msg.contains("cannot resume a session in state"),
            "session_resume must never dead-end on the daemon's raw 409 for a \
             zombie session — the #3531 regression: {msg}"
        );
    }

    let after: serde_json::Value = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(
        after.get("persisted_state").and_then(|v| v.as_str()),
        None,
        "persisted_state must still be present on the wire"
    );
    // A genuine reconcile+restart attempt always moves the record OFF its
    // original untouched-zombie shape: either all the way back to `active`
    // (the runtime spawn succeeded) or to `errored` (the daemon accepted and
    // attempted the resume, but the runtime spawn itself failed — the
    // environment-dependent case this test tolerates). If the pre-#3531 bug
    // were still present, the 409 would have been raised BEFORE any
    // daemon-side mutation, leaving the record silently stuck exactly as
    // seeded.
    let final_state = after.get("persisted_state").and_then(|v| v.as_str());
    assert!(
        matches!(final_state, Some("active") | Some("errored")),
        "a genuine reconcile+restart attempt must land the record in 'active' \
         (spawn succeeded) or 'errored' (spawn failed for environment reasons) \
         — never left untouched; got persisted_state={final_state:?}"
    );
}

/// #2457: a 404 from `decommission` on a nonexistent id must propagate as
/// `Err` — `prune.rs`'s bulk sweep records that `Err` as a failed row, and a
/// softened 404 would make a raced session read as a clean teardown.
///
/// #5913 moved the mapping into the shared transport
/// (`DaemonClient::decommission_managed_session`) when both entry points
/// converged onto one implementation; this test is what holds it there.
///
/// Two ids, because they take different daemon branches. `nonexistent-id` does
/// not parse as a UUID and comes back 400 — which is what this test asserted
/// before #5913, so it never once exercised a 404. A well-formed id absent from
/// the store is the real 404, and the friendly message proves the mapping (not
/// `error_for_status`'s generic status text) produced it.
#[tokio::test]
async fn session_decommission_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();

    let err = session_decommission(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a malformed id must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the rejected id: {err}"
    );

    let absent = "11111111-2222-3333-4444-555555555555";
    let err = session_decommission(&client, &url, absent.to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert_eq!(
        err.to_string(),
        format!("managed session '{absent}' not found"),
        "the 404 mapping must survive: prune's sweep counts this Err as a failed row"
    );
}

/// Serve a canned decommission response, plus the list route the routed entry
/// point resolves against.
///
/// Why: the `workspace_removed: None` arm means "a daemon old enough to send no
/// verdict at all", which the current daemon can never produce — a canned
/// response is the only way to drive it. Using the same stub for all three
/// verdicts keeps the two entry points reading byte-identical input.
/// What: `GET /api/v1/sessions/managed` returns one summary carrying
/// [`STUB_ID`]; `POST .../{id}/decommission` returns that summary flattened with
/// the requested verdict and no `workspace_path_was` (so neither path shells out
/// to git). Returns the base URL.
/// Test: `decommission_entry_points_agree_on_every_verdict`.
async fn spawn_decommission_stub(workspace_removed: Option<bool>) -> String {
    use axum::routing::{get, post};

    let list = serde_json::json!({
        "sessions": [{ "id": STUB_ID, "name": "tm-5913", "state": "active" }]
    });
    let outcome = serde_json::json!({
        "id": STUB_ID,
        "name": "tm-5913",
        "state": "decommissioned",
        "workspace_removed": workspace_removed,
        "workspace_path_was": serde_json::Value::Null,
    });

    let app = axum::Router::new()
        .route(
            "/api/v1/sessions/managed",
            get(move || std::future::ready(axum::Json(list.clone()))),
        )
        .route(
            "/api/v1/sessions/managed/{id}/decommission",
            post(move || std::future::ready(axum::Json(outcome.clone()))),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// The managed id both entry points target against [`spawn_decommission_stub`].
const STUB_ID: &str = "11111111-2222-3333-4444-555555555555";

/// #5913: both decommission entry points must report the same thing for the same
/// daemon response, across all three `workspace_removed` verdicts.
///
/// The bulk sweep used to reach the endpoint through its own hand-rolled POST,
/// which is how #5899's wording divergence survived undetected. They now share
/// one implementation, and this holds them to one observable result: the bulk
/// path's line (via [`super::session_decommission_line`], the string
/// `session_decommission` prints) compared against the routed path's line (via
/// `render_cli`). A future change to either path that the other does not get
/// fails here.
#[tokio::test]
async fn decommission_entry_points_agree_on_every_verdict() {
    use trusty_mpm::client::{CommandExecutor, TrustyCommand};

    for verdict in [Some(true), Some(false), None] {
        let url = spawn_decommission_stub(verdict).await;
        let client = reqwest::Client::new();

        let bulk = super::session_decommission_line(&client, &url, STUB_ID)
            .await
            .unwrap_or_else(|e| panic!("verdict {verdict:?}: bulk path failed: {e}"));
        let routed = super::super::managed_route::render_cli(
            &CommandExecutor::with_client(client.clone(), url.clone())
                .execute(TrustyCommand::ManagedDecommission {
                    target: STUB_ID.to_string(),
                })
                .await,
        );

        assert_eq!(
            bulk, routed,
            "verdict {verdict:?}: the two entry points disagree"
        );
        assert_eq!(
            bulk,
            super::super::managed_route::decommission_message(STUB_ID, verdict),
            "verdict {verdict:?}: neither path may invent its own wording"
        );
    }
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

// ── `tm ls` column color (NUM + NAME) ───────────────────────────────────────

/// Minimal summary fixture for the row-formatting cases.
fn ls_session(name: &str, slot: u32) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: "11111111-2222-3333-4444-555555555555".into(),
        name: name.to_string(),
        state: "active".into(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: Some("2026-08-05T12:34:56Z".into()),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: Some("do the thing".into()),
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot,
        deleted: false,
        auto_resume_parked: None,
    }
}

/// Strip every ANSI SGR escape, leaving the text a terminal actually draws.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Why: the `tm-NNNN` ↔ `NNNN` correspondence does not exist — a name's
/// trailing serial comes from `allocate_serial` (per-project, reuses gaps)
/// while `NUM` is a global slot. Painting both columns one hue would assert a
/// relationship that is false, and the more records retention evicts, the more
/// often the two numbers coincide by accident.
/// What: asserts both columns are wrapped in an SGR escape and that the two
/// escapes differ.
/// Test: this test.
#[test]
fn ls_row_colors_num_and_name_in_distinct_hues() {
    let row = format_ls_row(&ls_session("tm-trusty-tools-01", 7), true, 14);
    assert!(
        row.starts_with("\u{1b}[35m7\u{1b}[0m"),
        "NUM colored: {row:?}"
    );
    assert!(
        row.contains("\u{1b}[36mtm-trusty-tools-01\u{1b}[0m"),
        "NAME colored: {row:?}"
    );
    assert_ne!(
        "\u{1b}[35m", "\u{1b}[36m",
        "NUM and NAME must not share a hue"
    );
}

/// Why: `NO_COLOR`, a pipe, and every scripted reader must see exactly the
/// bytes the table produced before color existed.
/// What: asserts the plain row carries no escape at all, and that stripping the
/// escapes from the colored row reproduces it byte-for-byte.
/// Test: this test.
#[test]
fn ls_row_plain_when_color_disabled() {
    let s = ls_session("tm-trusty-tools-01", 7);
    let plain = format_ls_row(&s, false, 14);
    assert!(!plain.contains('\u{1b}'), "no escapes: {plain:?}");
    assert_eq!(
        strip_ansi(&format_ls_row(&s, true, 14)),
        plain,
        "color must change bytes only inside the escapes"
    );
    let tomb_plain = format_tombstone_row(7, false);
    assert!(!tomb_plain.contains('\u{1b}'));
    assert_eq!(strip_ansi(&format_tombstone_row(7, true)), tomb_plain);
}

/// Why: `{:<5}`/`{:<24}` measure the formatted value, so an ANSI-wrapped column
/// would come out ~9 chars narrow and stagger every row after it — including
/// the tombstone row, whose `NUM` field is padded by the same helper.
/// What: asserts the VISIBLE width of a colored row equals that of the plain
/// row, for a short name, a name at the truncation boundary, and a tombstone.
/// Test: this test.
#[test]
fn ls_row_alignment_matches_with_and_without_color() {
    for name in ["a", "tm-trusty-tools-01", &"x".repeat(60)] {
        for slot in [1u32, 107] {
            let s = ls_session(name, slot);
            assert_eq!(
                strip_ansi(&format_ls_row(&s, true, 14)).chars().count(),
                format_ls_row(&s, false, 14).chars().count(),
                "row width drifts for name={name:?} slot={slot}"
            );
        }
    }
    for slot in [1u32, 107] {
        assert_eq!(
            strip_ansi(&format_tombstone_row(slot, true))
                .chars()
                .count(),
            format_tombstone_row(slot, false).chars().count(),
            "tombstone row width drifts for slot={slot}"
        );
        // And the tombstone's NUM column is the same width as a live row's, so
        // the two row shapes line up under the same header.
        let plain = format_tombstone_row(slot, false);
        assert_eq!(
            &plain[..7],
            &format_ls_row(&ls_session("n", slot), false, 14)[..7],
            "tombstone NUM column matches the live row's"
        );
    }
}

/// Why: `ID` is the widest cell on the row and was the only uncolored one, so
/// the table read as a wall of undifferentiated UUID. It is dimmed rather than
/// hued because it exists to be copy-pasted, not scanned — and it must be a
/// THIRD escape, distinct from `NUM`'s and `NAME`'s, so no two columns blur
/// together.
/// What: asserts the id is wrapped in the dim SGR escape, and that the three
/// column escapes are pairwise distinct.
/// Test: this test.
#[test]
fn ls_row_colors_id_column_dimmed() {
    let s = ls_session("tm-trusty-tools-01", 7);
    let row = format_ls_row(&s, true, 14);
    assert!(
        row.contains(&format!("\u{1b}[2m{}\u{1b}[0m", s.id)),
        "ID colored dim: {row:?}"
    );
    let hues = ["\u{1b}[35m", "\u{1b}[2m", "\u{1b}[36m"];
    for (i, a) in hues.iter().enumerate() {
        for b in &hues[i + 1..] {
            assert_ne!(a, b, "NUM/ID/NAME must not share a hue");
        }
    }
}

/// Why: the `STATE` cell is the state PLUS any annotation, and the column was a
/// hardcoded `{:<14}`. `attached [stale-assets]` is 23 chars, so an annotated
/// row pushed `NAME`/`TASK`/`CREATED` nine columns right and staggered the
/// table — visible in any real `tm ls`.
/// What: asserts the computed width covers the longest annotated cell, is
/// floored at the historical 14 when nothing is annotated, and ignores
/// tombstone rows (which have no `STATE` cell).
/// Test: this test.
#[test]
fn state_column_width_absorbs_longest_annotation() {
    let plain = vec![ls_session("a", 1), ls_session("b", 2)];
    assert_eq!(
        state_column_width(&plain),
        14,
        "an all-plain listing keeps the historical width"
    );

    let mut stale = ls_session("c", 3);
    stale.attached = true;
    stale.stale_assets = true;
    let annotated = vec![ls_session("a", 1), stale];
    // "attached [stale-assets]" — the widest cell the table can produce.
    assert_eq!(
        state_column_width(&annotated),
        "attached [stale-assets]".len()
    );

    let mut tomb = ls_session("d", 4);
    tomb.deleted = true;
    tomb.state = "a-very-long-state-string-that-is-not-rendered".into();
    assert_eq!(
        state_column_width(&[tomb]),
        14,
        "a tombstone row has no STATE cell and must not widen the column"
    );
}

/// Why: this is the alignment bug itself. Before the fix, an annotated row's
/// `NAME` started nine columns right of every plain row's, because the
/// annotation overflowed a fixed-width `STATE`. The fix is only real if the
/// columns after `STATE` start at the SAME offset on both row shapes.
///
/// It also proves padding is computed on VISIBLE text: the assertion runs on
/// the colored rows with the escapes stripped, so padding measured on the
/// escaped string would put `NAME` ~9 chars early on every colored row and
/// fail here.
/// What: renders a plain row and an annotated row at the listing's shared
/// width, then asserts the `NAME` column begins at the same character offset in
/// both — in plain output and in stripped colored output.
/// Test: this test.
#[test]
fn ls_table_columns_align_when_a_row_carries_an_annotation() {
    let plain = ls_session("plain-name", 1);
    let mut annotated = ls_session("stale-name", 2);
    annotated.attached = true;
    annotated.stale_assets = true;
    let sessions = vec![plain.clone(), annotated.clone()];
    let width = state_column_width(&sessions);

    let name_offset = |row: &str, name: &str| row.find(name).expect("name present in row");
    for use_color in [false, true] {
        let a = strip_ansi(&format_ls_row(&plain, use_color, width));
        let b = strip_ansi(&format_ls_row(&annotated, use_color, width));
        assert_eq!(
            name_offset(&a, "plain-name"),
            name_offset(&b, "stale-name"),
            "NAME column drifts between a plain and an annotated row (use_color={use_color})"
        );
    }

    // And the annotation is actually present — otherwise the offsets above
    // would agree for the trivial reason that nothing was annotated.
    assert!(format_ls_row(&annotated, false, width).contains("attached [stale-assets]"));

    // Padding is computed on the VISIBLE text: stripping the escapes from the
    // colored row must reproduce the plain row byte-for-byte. Padding measured
    // on the escaped string would eat ~9 spaces per colored column here.
    for s in [&plain, &annotated] {
        assert_eq!(
            strip_ansi(&format_ls_row(s, true, width)),
            format_ls_row(s, false, width),
            "padding must be measured on visible text, not the ANSI-wrapped string"
        );
    }
}

/// 🔴 #6118 FAIL-OPEN GUARD: a `--dry-run` the daemon did not confirm is an
/// ERROR, never a reported preview.
///
/// Why: this daemon is long-lived by design — a CLI upgrade never bounces it —
/// and axum silently drops a query param the running handler does not declare.
/// A daemon predating the `?dry_run=` param therefore accepts the request, runs
/// the REAL sweep, and returns 200 with a count. Rendering that as "would
/// decommission 7" tells the operator nothing happened when seven sessions were
/// just torn down. The echoed `dry_run` field is the only signal that can tell
/// the two apart, so its absence has to fail the command.
/// What: three bodies against the same requested `dry_run: true` — a missing
/// echo, an explicit `false`, and a confirmed `true` — plus a real sweep, which
/// deliberately does NOT require the echo (an older daemon's real teardown is
/// what a real sweep asked for).
/// Test: this function IS the test.
#[test]
fn decommission_ephemeral_refuses_a_dry_run_a_stale_daemon_ignored() {
    use super::ephemeral_sweep_line;

    let missing = serde_json::json!({ "decommissioned": 7 });
    let err = ephemeral_sweep_line(&missing, true).expect_err("a silent daemon must fail the run");
    let msg = err.to_string();
    assert!(
        msg.contains("did not confirm --dry-run") && msg.contains('7'),
        "the refusal must say what may have been torn down: {msg}"
    );

    let denied = serde_json::json!({ "decommissioned": 2, "dry_run": false });
    assert!(
        ephemeral_sweep_line(&denied, true).is_err(),
        "an explicit `dry_run: false` is the same skew, stated outright"
    );

    // CONTROL: a real sweep never consults the echo, so the same silent body is
    // rendered without complaint — otherwise the guard above would have broken
    // the ordinary path rather than the skewed one.
    let real = ephemeral_sweep_line(&missing, false).expect("a real sweep needs no echo");
    assert_eq!(real, "decommissioned 7 ephemeral session(s)");
}

/// #6118: a confirmed dry run reports itself as one.
///
/// Why: the guard above must not be satisfiable by refusing everything — the
/// honoured path has to render, and has to say "would" rather than claiming a
/// teardown that never happened.
/// Test: this function IS the test.
#[test]
fn decommission_ephemeral_reports_a_confirmed_dry_run() {
    use super::ephemeral_sweep_line;

    let body = serde_json::json!({ "decommissioned": 3, "dry_run": true });
    assert_eq!(
        ephemeral_sweep_line(&body, true).expect("a confirmed preview renders"),
        "would decommission 3 ephemeral session(s) (dry run)"
    );
}
