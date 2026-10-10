//! Tests for #9101's second round: the callers outside `SessionManager`'s own
//! pane operations act on a record's pane only once it is proven on the live
//! tmux server.
//!
//! A child of `pane_ops_identity_tests`, whose `Restarted` fixture (a private
//! `-L` server restarted under a stale record) and scripted fake it reuses.
//! Each caller has one red-proof test on the restarted server and one
//! fail-open test on a fake whose pane-identity read exits non-zero.

use std::sync::Mutex;

use super::*;
use crate::daemon::api::claude_config_routes::{RestartRequest, restart_claude_code_op};
use crate::daemon::managed_routes::auto_relaunch::spawn_resume_into_owned_pane;
use crate::daemon::managed_routes::reactivate::{ReactivateQuery, reactivate_core};
use crate::daemon::state::DaemonState;
use crate::runtime::{RuntimeAdapter, RuntimeError};
use crate::session_manager::ManagedTmuxDriver;
use crate::session_manager::injection_status::InjectionStatus;

/// A daemon whose session manager runs every tmux call through `bin`.
async fn daemon_over(bin: &std::path::Path) -> (Arc<DaemonState>, tempfile::TempDir) {
    let root = crate::test_support::hermetic_temp_dir();
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let state = DaemonState::with_root_isolated_managed_and_driver(
        root.path().to_path_buf(),
        Arc::new(RealTmuxDriver::from_driver_for_test(driver)),
    )
    .await;
    (Arc::new(state), root)
}

/// The scripted fake whose pane-identity read exits non-zero, with an Active
/// record named [`LIVE`] on pane %9 of the live server.
async fn unreadable_identity() -> (Fixture, ManagedSessionId) {
    let f = Fixture::new(FakeTmux {
        identity: None,
        ..FakeTmux::default()
    })
    .await;
    let id = f.seed_on("active", Some("%9"), Some(LIVE_SERVER)).await;
    (f, id)
}

/// Assert the fake was reached and read the pane identity, and that nothing
/// typed into a pane, captured it, read its pid or renamed its session.
fn assert_identity_read_and_pane_untouched(f: &Fixture) {
    let calls = f.calls();
    assert!(
        calls.contains("session_id"),
        "the pane identity was read: {calls}"
    );
    for verb in ["send-keys", "capture-pane", "pane_pid", "rename-session"] {
        assert!(!calls.contains(verb), "a caller ran `{verb}`: {calls}");
    }
}

/// #9101 restart route, on a real tmux: `POST /claude-config/restart` for a
/// stale record's session name types nothing into the restarted server's
/// pane. Red on f91060103c, which typed Ctrl-C and `claude` into it.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_gets_a_claude_restart() {
    let Some(r) = Restarted::new("9101-restart").await else {
        return;
    };
    let (state, _root) = daemon_over(&r.f.bin).await;
    let mgr = state.session_manager().await;
    seed_named_into(&mgr, &r.name, "active", Some(&r.pane), Some(&r.old_server)).await;
    let request = RestartRequest {
        tmux_session: r.name.clone(),
    };
    let restarted = r.f.scoped(restart_claude_code_op(&state, request)).await;
    // tmux echoes typed keys through the pane's tty asynchronously.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let screen = r.screen();
    assert!(
        !screen.contains("claude"),
        "the restart typed into the live pane: {screen:?}"
    );
    assert!(restarted.is_err(), "the restart reported success");
}

/// Fail open: the restart route refuses when the pane identity is unreadable,
/// with a 409 as the reactivate route answers, not a 500.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_refuses_the_claude_restart() {
    let (f, _) = unreadable_identity().await;
    let (state, _root) = daemon_over(&f.bin).await;
    let mgr = state.session_manager().await;
    seed_named_into(&mgr, LIVE, "active", Some("%9"), Some(LIVE_SERVER)).await;
    let request = RestartRequest {
        tmux_session: LIVE.into(),
    };
    let restarted = f.scoped(restart_claude_code_op(&state, request)).await;
    let status = restarted.err().map(|e| e.status());
    assert_eq!(status, Some(axum::http::StatusCode::CONFLICT));
    assert_identity_read_and_pane_untouched(&f);
}

/// A runtime adapter that types [`MARK`] into the pane `spawn_resume` names,
/// so a resume that reached a pane shows on its screen.
struct TypingAdapter {
    tmux: Arc<dyn ManagedTmuxDriver>,
    panes: Mutex<Vec<Option<String>>>,
}

impl RuntimeAdapter for TypingAdapter {
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &std::path::Path,
        task: &str,
        session_id: &str,
        gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        self.spawn_resume(tmux_name, None, cwd, task, None, session_id, gh_env)
    }

    fn spawn_resume(
        &self,
        tmux_name: &str,
        pane_id: Option<&str>,
        _cwd: &std::path::Path,
        _task: &str,
        _claude_session_id: Option<&str>,
        _session_id: &str,
        _gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        self.panes.lock().unwrap().push(pane_id.map(str::to_owned));
        match pane_id {
            Some(pane) => self.tmux.send_line_to_pane(tmux_name, pane, MARK),
            None => self.tmux.send_line(tmux_name, MARK),
        }
        .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))
    }

    fn identify(&self) -> &str {
        "typing-9101"
    }
}

impl TypingAdapter {
    fn over(mgr: &SessionManager) -> Self {
        Self {
            tmux: mgr.tmux_driver(),
            panes: Mutex::new(Vec::new()),
        }
    }
}

/// #9101 resume spawn, on a real tmux: the spawn both resume paths share
/// (`resume_managed` and the auto-resume relauncher) types nothing into the
/// restarted server's pane a stale record names. Red on f91060103c, whose
/// two callers passed `record.pane_id` to `spawn_resume` unchecked.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_gets_a_resume_spawn() {
    let Some(r) = Restarted::new("9101-spawn").await else {
        return;
    };
    let id = r.stale("active").await;
    let record = r.f.mgr.get(&id).await.expect("record");
    let adapter = TypingAdapter::over(&r.f.mgr);
    let ws = r.f.dir.path();
    let spawned =
        r.f.scoped(async { spawn_resume_into_owned_pane(&r.f.mgr, &adapter, &record, ws, &[]) })
            .await;
    // tmux echoes typed keys through the pane's tty asynchronously.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let screen = r.screen();
    assert!(
        !screen.contains(MARK),
        "the spawn typed into the live pane: {screen:?}"
    );
    assert!(spawned.is_err(), "the spawn reported success");
    assert!(adapter.panes.lock().unwrap().is_empty());
}

/// Fail open: the resume spawn refuses when the pane identity is unreadable.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_refuses_the_resume_spawn() {
    let (f, id) = unreadable_identity().await;
    let record = f.mgr.get(&id).await.expect("record");
    let adapter = TypingAdapter::over(&f.mgr);
    let ws = f.dir.path();
    let spawned = f
        .scoped(async { spawn_resume_into_owned_pane(&f.mgr, &adapter, &record, ws, &[]) })
        .await;
    assert!(spawned.is_err(), "the spawn reported success");
    assert!(adapter.panes.lock().unwrap().is_empty());
    assert_identity_read_and_pane_untouched(&f);
}

/// #9101 task injection, on a real tmux: a stale record's `--task` is never
/// typed into the restarted server's pane, and the readiness poll stops at
/// once. Red on f91060103c, which polled the live pane by name for the whole
/// budget.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_has_its_task_injected() {
    let Some(r) = Restarted::new("9101-inject").await else {
        return;
    };
    let id = r.stale("active").await;
    let injected = r.f.scoped(r.f.mgr.inject_task_when_ready(&id, MARK)).await;
    let screen = r.screen();
    assert!(
        !screen.contains(MARK),
        "the task reached the live pane: {screen:?}"
    );
    assert!(
        matches!(&injected, Err(ManagedError::InvalidState(_, why)) if why.contains("stale")),
        "{injected:?}"
    );
    let record = r.f.mgr.get(&id).await.expect("record");
    assert_eq!(record.injection_status, InjectionStatus::FailedSessionDied);
}

/// Fail open: an unreadable pane identity stops task injection before any
/// readiness probe reads the pane.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_stops_task_injection_unread() {
    let (f, id) = unreadable_identity().await;
    let injected = f.scoped(f.mgr.inject_task_when_ready(&id, MARK)).await;
    assert!(
        matches!(&injected, Err(ManagedError::InvalidState(..))),
        "{injected:?}"
    );
    assert_identity_read_and_pane_untouched(&f);
}

/// #9101 rename, on a real tmux: renaming a stale record renames the record
/// only. The restarted server's session that reused its name and `%N` keeps
/// its name. Red on f91060103c, which renamed the live session.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_renames_the_live_session() {
    let Some(r) = Restarted::new("9101-rename").await else {
        return;
    };
    let id = r.stale("stopped").await;
    let new_name = reserved_session_name("9101-renamed");
    let renamed = r.f.scoped(r.f.mgr.rename(&id, &new_name)).await;
    let socket = Some(r.server.name());
    assert!(
        ScratchTmuxSession::exists_on_socket(TMUX, socket, &r.name),
        "the live session lost its name"
    );
    assert!(!ScratchTmuxSession::exists_on_socket(
        TMUX, socket, &new_name
    ));
    let record = renamed.expect("a record-only rename");
    assert_eq!(record.tmux_name, new_name);
}

/// Fail open: an unreadable pane identity refuses the rename, and neither
/// the session nor the record is renamed.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_refuses_the_rename() {
    let (f, id) = unreadable_identity().await;
    let renamed = f
        .scoped(f.mgr.rename(&id, &reserved_session_name("9101-unread")))
        .await;
    assert!(
        matches!(&renamed, Err(ManagedError::InvalidState(..))),
        "{renamed:?}"
    );
    assert_eq!(f.mgr.get(&id).await.expect("record").tmux_name, LIVE);
    assert_identity_read_and_pane_untouched(&f);
}

/// The reactivate query `bin/tm`'s in-place relaunch sends from pane `pane`.
fn from_pane(pane: &str) -> ReactivateQuery {
    ReactivateQuery {
        caller_pane_id: Some(pane.to_owned()),
        pane_confirmed_dead: false,
    }
}

/// #9101 in-place relaunch, on a real tmux: a caller in the restarted
/// server's pane that reused a stale record's `%N` cannot reactivate the
/// record, so `bin/tm` never execs `claude` there for it. Red on
/// f91060103c, which reactivated it.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_is_never_reactivated_in_place() {
    let Some(r) = Restarted::new("9101-react").await else {
        return;
    };
    let (state, _root) = daemon_over(&r.f.bin).await;
    let mgr = state.session_manager().await;
    let id = seed_named_into(&mgr, &r.name, "stopped", Some(&r.pane), Some(&r.old_server)).await;
    let outcome =
        r.f.scoped(reactivate_core(
            &state,
            &id.to_string(),
            &from_pane(&r.pane),
        ))
        .await;
    assert_eq!(outcome.status, 409, "{:?}", outcome.body);
    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Stopped);
}

/// Fail open: an unreadable pane identity refuses the in-place reactivate.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_refuses_the_in_place_reactivate() {
    let (f, _) = unreadable_identity().await;
    let (state, _root) = daemon_over(&f.bin).await;
    let mgr = state.session_manager().await;
    let id = seed_named_into(&mgr, LIVE, "stopped", Some("%9"), Some(LIVE_SERVER)).await;
    let outcome = f
        .scoped(reactivate_core(&state, &id.to_string(), &from_pane("%9")))
        .await;
    assert_eq!(outcome.status, 409, "{:?}", outcome.body);
    let record = mgr.get(&id).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Stopped);
    assert_identity_read_and_pane_untouched(&f);
}

/// Reactivate the Stopped record on the record's own pane %9, whose
/// foreground command the fake reports as `command` (#9566).
async fn reactivate_stopped_from_pane_running(
    command: &'static str,
) -> (u16, ManagedSessionState, String) {
    let f = Fixture::new(FakeTmux {
        pane_command: Some(command),
        ..FakeTmux::default()
    })
    .await;
    let (state, _root) = daemon_over(&f.bin).await;
    let mgr = state.session_manager().await;
    let id = seed_named_into(&mgr, LIVE, "stopped", Some("%9"), Some(LIVE_SERVER)).await;
    let outcome = f
        .scoped(reactivate_core(&state, &id.to_string(), &from_pane("%9")))
        .await;
    let record = mgr.get(&id).await.expect("record");
    (outcome.status, record.state, format!("{:?}", outcome.body))
}

/// #9566: a bare `tm` run by a live agent's Bash tool sits in that agent's
/// pane, so the pane's foreground is the agent, not `tm`. A Stopped record
/// whose claude is in fact live is never reactivated from there, so `bin/tm`
/// never execs a second `claude --resume` of the same session.
#[serial_test::serial]
#[tokio::test]
async fn a_stopped_record_whose_pane_runs_a_live_agent_is_never_reactivated_in_place() {
    for command in ["claude", "node", "2.1.3"] {
        let (status, state, body) = reactivate_stopped_from_pane_running(command).await;
        assert_eq!(status, 409, "foreground `{command}` reactivated: {body}");
        assert_eq!(
            state,
            ManagedSessionState::Stopped,
            "foreground `{command}`"
        );
    }
}

/// #9566 keeps #9565: the relaunch of a dead claude from its own pane, whose
/// foreground is the requesting `tm`, still reactivates the record.
#[serial_test::serial]
#[tokio::test]
async fn a_stopped_record_whose_pane_runs_tm_is_still_reactivated_in_place() {
    for command in ["tm", "zsh"] {
        let (status, state, body) = reactivate_stopped_from_pane_running(command).await;
        assert_eq!(status, 200, "foreground `{command}` refused: {body}");
        assert_eq!(state, ManagedSessionState::Active, "foreground `{command}`");
    }
}

/// #9101 runtime-exit reap, on a real tmux: reaping a stale Active record
/// stops the record but never publishes its id into the restarted server's
/// session that reused its name. Red on f91060103c, which set
/// `TM_MANAGED_SESSION_ID` there, so a bare `tm` in that pane would resolve
/// the stale record.
#[serial_test::serial]
#[tokio::test]
async fn a_stale_record_after_a_server_restart_never_publishes_its_id_on_reap() {
    let Some(r) = Restarted::new("9101-reap").await else {
        return;
    };
    let id = r.stale("active").await;
    let reaped = r.f.scoped(r.f.mgr.mark_runtime_exited_stopped(&id)).await;
    let target = trusty_common::tmux::exact_session_target(&r.name);
    let env = r
        .server
        .query(&["show-environment", "-t", &target, "TM_MANAGED_SESSION_ID"])
        .unwrap_or_default();
    assert!(
        !env.contains(&id.to_string()),
        "the stale id reached the live session: {env:?}"
    );
    let record = reaped.expect("the record-only transition still runs");
    assert_eq!(record.state, ManagedSessionState::Stopped);
}

/// Fail open: an unreadable pane identity still stops the record, and the
/// reap neither captures the pane nor publishes into its session.
#[serial_test::serial]
#[tokio::test]
async fn an_unreadable_pane_identity_reaps_the_record_without_reading_the_pane() {
    let (f, id) = unreadable_identity().await;
    let reaped = f.scoped(f.mgr.mark_runtime_exited_stopped(&id)).await;
    assert_eq!(reaped.expect("reaped").state, ManagedSessionState::Stopped);
    assert_identity_read_and_pane_untouched(&f);
    let calls = f.calls();
    assert!(!calls.contains("set-environment"), "{calls}");
}

/// What `pane` of session `name` on `server` shows.
fn screen_of(server: &PrivateTmuxServer, name: &str, pane: &str) -> String {
    let target = trusty_common::tmux::exact_pane_target(name, pane);
    server
        .query(&["capture-pane", "-p", "-t", &target])
        .unwrap_or_default()
}

/// #9101 reap backfill, on a real tmux: an Active record with no pane id,
/// whose name an idle live session holds, is reaped. The reap backfills the
/// pane id only, so the record stays unverifiable: the resume that follows
/// refuses, names the recovery, and nothing types into the session. Red on
/// bac1c1b4cb, whose by-name capture also stored that session's server, so
/// the resume re-attached and the resume spawn typed into it.
#[serial_test::serial]
#[tokio::test]
async fn a_pane_less_record_reaped_beside_a_live_name_is_never_resumed_into_it() {
    let Some((server, f)) = live_fixture("9101-backfill").await else {
        return;
    };
    let name = reserved_session_name("9101-backfill");
    let _idle = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
    let (pane, _, _) = live_pane(&server, &name).expect("the idle session is live");
    let id = f.seed_named(&name, "active", None, None).await;

    let reaped = f.scoped(f.mgr.mark_runtime_exited_stopped(&id)).await;
    let resumed = f.scoped(f.mgr.resume(&id)).await;
    let record = f.mgr.get(&id).await.expect("record");
    let adapter = TypingAdapter::over(&f.mgr);
    let ws = f.dir.path();
    let spawned = f
        .scoped(async { spawn_resume_into_owned_pane(&f.mgr, &adapter, &record, ws, &[]) })
        .await;
    // tmux echoes typed keys through the pane's tty asynchronously.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let screen = screen_of(&server, &name, &pane);
    assert!(
        !screen.contains(MARK),
        "the resume spawn typed into the live session: {screen:?}"
    );
    assert!(spawned.is_err(), "the resume spawn reported success");
    assert!(adapter.panes.lock().unwrap().is_empty());
    let recovery = format!("tmux kill-session -t '={name}'");
    assert!(
        matches!(&resumed, Err(ManagedError::InvalidState(_, why))
            if why.contains(&recovery) && why.contains(&format!("tm session resume {id}"))),
        "{resumed:?}"
    );
    assert_eq!(record.state, ManagedSessionState::Stopped);
    let reaped = reaped.expect("the record-only reap still runs");
    assert_eq!(reaped.pane_id.as_deref(), Some(pane.as_str()));
    assert_eq!(reaped.tmux_server, None, "a by-name read paired a server");
}

/// #9101 resume create, on a real tmux: the resume's recreate step never
/// attaches to a session that holds the record's name, which one can take
/// between the resume's probe and its create. Red on bac1c1b4cb, whose
/// `new-session -A` attached to it.
#[serial_test::serial]
#[tokio::test]
async fn the_resume_create_never_attaches_to_a_session_holding_the_name() {
    let Some((server, f)) = live_fixture("9101-create").await else {
        return;
    };
    let name = reserved_session_name("9101-create");
    let _taken = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, LIVE_PANE);
    let before = live_pane(&server, &name).expect("the session is live");
    // The taken session's own directory, so only exclusivity can refuse.
    let target = trusty_common::tmux::exact_window_target(&name);
    let cwd = server
        .query(&[
            "display-message",
            "-p",
            "-t",
            &target,
            "#{pane_current_path}",
        ])
        .expect("pane cwd");

    let created = f
        .scoped(async {
            crate::session_manager::resume_workdir::create_and_verify_pane(
                f.mgr.tmux.as_ref(),
                &name,
                cwd.trim(),
            )
        })
        .await;

    assert!(
        matches!(&created, Err(ManagedError::NameCollision(_))),
        "{created:?}"
    );
    assert_eq!(live_pane(&server, &name), Some(before));
}
