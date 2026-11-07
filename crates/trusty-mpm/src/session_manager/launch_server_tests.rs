//! Tests for `launch_server` (#10101): a fresh PM launch creates its session
//! on its workspace's tmux server and records that server's socket.

use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::daemon::tmux::TmuxDriver;
use crate::session_manager::real_tmux::FakeNoopTmuxDriver;
use crate::session_manager::{RealTmuxDriver, SessionManager};

/// A scratch dir with a fake tmux that logs every argv and answers nothing,
/// and a real driver over it whose socket directory is that scratch dir.
fn real_driver() -> (tempfile::TempDir, PathBuf, RealTmuxDriver) {
    let dir = crate::test_support::hermetic_temp_dir();
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let driver =
        RealTmuxDriver::from_driver_for_test(driver).with_socket_dir_for_test(dir.path().into());
    (dir, log, driver)
}

/// The manager's data dir under a scratch framework root (`<dir>/.trusty-mpm`),
/// so nothing the manager deploys reaches the operator's home.
fn scratch_data_dir(dir: &std::path::Path) -> PathBuf {
    let data = dir.join(".trusty-mpm").join("session-manager");
    std::fs::create_dir_all(&data).expect("data dir");
    data
}

#[serial_test::serial]
#[test]
fn the_opt_out_keeps_the_default_server() {
    let (dir, _log, tmux) = real_driver();
    assert!(opted_out(Some("0")));
    assert!(opted_out(Some(" 0 ")));
    assert!(!opted_out(Some("1")));
    assert!(!opted_out(None));
    assert!(launch_target_with(&tmux, dir.path(), Some("0")).is_none());
    let (_, socket) = launch_target_with(&tmux, dir.path(), None).expect("workspace server");
    assert_eq!(
        socket,
        dir.path()
            .join(crate::core::tmux_server::workspace_server_name(dir.path()))
    );
    // A driver that reaches no named server launches on the default server.
    assert!(launch_target_with(&FakeNoopTmuxDriver, dir.path(), None).is_none());
}

/// The create path starts the workspace server, creates the session there and
/// nowhere else, and stores the server's socket on the record; the record
/// then routes to that server. Red on 420b3717bf, which created every session
/// on the default server and stored no socket.
#[serial_test::serial]
#[tokio::test]
async fn a_fresh_launch_creates_its_session_on_the_workspace_server() {
    let (dir, log, tmux) = real_driver();
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let server = crate::core::tmux_server::workspace_server_name(&workspace);
    let data = scratch_data_dir(dir.path());
    let mgr = SessionManager::new(&data, Arc::new(tmux))
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(workspace.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert_eq!(record.tmux_socket, Some(dir.path().join(&server)));
    let calls = std::fs::read_to_string(&log).expect("log");
    let on_server = format!("-L {server} ");
    let creates: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("new-session"))
        .collect();
    assert!(!creates.is_empty(), "{calls}");
    assert!(
        creates.iter().all(|l| l.starts_with(&on_server)),
        "a session was created off the workspace server: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l == format!("-L {server} start-server")),
        "the workspace server was not started: {calls}"
    );
    assert!(
        mgr.tmux_driver_for(&record)
            .is_ok_and(|_| crate::session_manager::tmux_route::route(
                mgr.tmux_driver().as_ref(),
                &record
            )
            .is_ok_and(|t| t.is_named())),
        "the record routes to its server"
    );
}

/// Error arm (Fail-Open Check): a workspace server that crashed leaves a
/// socket that refuses (ECONNREFUSED). Resume starts that server again, reads
/// it as empty and recreates the pane on it. Red on 420b3717bf, where the
/// refused listing failed the resume.
#[serial_test::serial]
#[tokio::test]
async fn resuming_on_a_crashed_workspace_server_restarts_it_and_recreates_the_pane() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = std::fs::canonicalize(dir.path()).expect("canonical");
    let log = dir.path().join("calls.log");
    let up = dir.path().join("server-up");
    let bin = dir.path().join("fake-tmux");
    let server = "tm-ws-10101-crashed";
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\n\
         if [ \"$1\" = -L ] && [ \"$3\" = start-server ]; then touch '{up}'; exit 0; fi\n\
         if [ \"$1\" = -L ] && [ \"$3\" = list-sessions ] && [ ! -f '{up}' ]; then \
         echo 'error connecting to /tmp/{server} (Connection refused)' >&2; exit 1; fi\n\
         case \"$*\" in *pane_current_path*) echo '{wd}';; esac\nexit 0\n",
        log = log.display(),
        up = up.display(),
        wd = workdir.display(),
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let driver =
        RealTmuxDriver::from_driver_for_test(driver).with_socket_dir_for_test(dir.path().into());
    let mgr = SessionManager::new(&scratch_data_dir(dir.path()), Arc::new(driver))
        .await
        .expect("manager");
    let id = crate::session_manager::ManagedSessionId::new();
    let record: crate::session_manager::SessionRecord = serde_json::from_value(serde_json::json!({
        "id": id.to_string(), "task": "t", "tmux_name": "tm-crashed",
        "cwd": workdir, "state": "stopped", "created_at": "2026-11-07T00:00:00Z",
        "tmux_socket": dir.path().join(server),
    }))
    .expect("record");
    mgr.store.write().await.upsert(record).await.expect("seed");

    let resumed = mgr.resume(&id).await.expect("resume after a server crash");

    assert_eq!(
        resumed.state,
        crate::session_manager::ManagedSessionState::Active
    );
    let calls = std::fs::read_to_string(&log).expect("log");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with(&format!("-L {server} ")) && l.contains("new-session")),
        "the pane was not recreated on its server: {calls}"
    );
}
