//! Tests for `resume_server` (#10101, owner ruling 2026-11-18): a resumed
//! stopped record leaves a dead workspace server for the default server.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::daemon::tmux::TmuxDriver;
use crate::secret_source::test_env::EnvVarGuard;
use crate::session_manager::launch_server::WORKSPACE_TMUX_ENV;
use crate::session_manager::{ManagedSessionId, RealTmuxDriver, SessionManager};

const SERVER: &str = "tm-ws-10101-rehome";

/// A real driver over a fake tmux in `dir` that logs every argv, answers
/// `pane_current_path` with `workdir`, and runs `extra` (shell) first.
fn driver(dir: &Path, workdir: &Path, extra: &str) -> (PathBuf, RealTmuxDriver) {
    let log = dir.join("calls.log");
    let bin = dir.join("fake-tmux");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\n{extra}\n\
         case \"$*\" in *pane_current_path*) echo '{wd}';; esac\nexit 0\n",
        log = log.display(),
        wd = workdir.display(),
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let driver = RealTmuxDriver::from_driver_for_test(driver).with_socket_dir_for_test(dir.into());
    (log, driver)
}

/// A stopped record named `tm-rehome` at `workdir` on [`SERVER`] in `dir`.
fn stopped_record(id: &ManagedSessionId, dir: &Path, workdir: &Path) -> SessionRecord {
    serde_json::from_value(serde_json::json!({
        "id": id.to_string(), "task": "t", "tmux_name": "tm-rehome",
        "cwd": workdir, "state": "stopped", "created_at": "2026-11-07T00:00:00Z",
        "tmux_socket": dir.join(SERVER), "claude_session_id": "claude-10101",
        "pane_id": "%9", "tmux_server": "1:1",
    }))
    .expect("record")
}

/// The end-to-end move: resume recreates the pane on the default server,
/// never starts the workspace server, and keeps the conversation id. Red on
/// 45d9b68961, which recreated the pane on the stored server.
#[serial_test::serial]
#[tokio::test]
async fn resuming_a_stopped_workspace_record_recreates_it_on_the_default_server() {
    let _env = EnvVarGuard::unset(WORKSPACE_TMUX_ENV);
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = std::fs::canonicalize(dir.path()).expect("canonical");
    let (log, tmux) = driver(dir.path(), &workdir, "");
    let data = dir.path().join(".trusty-mpm").join("session-manager");
    std::fs::create_dir_all(&data).expect("data dir");
    let mgr = SessionManager::new(&data, Arc::new(tmux))
        .await
        .expect("manager");
    let id = ManagedSessionId::new();
    let seeded = stopped_record(&id, dir.path(), &workdir);
    mgr.store.write().await.upsert(seeded).await.expect("seed");

    let resumed = mgr.resume(&id).await.expect("resume");

    assert_eq!(
        resumed.tmux_socket, None,
        "the record left the workspace server"
    );
    assert_eq!(resumed.claude_session_id.as_deref(), Some("claude-10101"));
    assert_eq!(resumed.cwd, workdir);
    let calls = std::fs::read_to_string(&log).expect("log");
    let creates: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("new-session"))
        .collect();
    assert!(!creates.is_empty(), "{calls}");
    assert!(
        creates.iter().all(|l| !l.starts_with("-L ")),
        "a pane was recreated off the default server: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l == format!("-L {SERVER} start-server")),
        "the workspace server was started: {calls}"
    );
}

/// A pane still live on its workspace server is never re-routed.
#[serial_test::serial]
#[test]
fn a_live_pane_on_a_workspace_server_is_never_rerouted() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = dir.path().to_path_buf();
    let live = "case \"$*\" in *list-sessions*) echo tm-rehome:1700000000:1;; esac";
    let (_log, tmux) = driver(dir.path(), &workdir, live);
    let mut record = stopped_record(&ManagedSessionId::new(), dir.path(), &workdir);
    assert!(!rehome_with(&tmux, &mut record, None).expect("probe"));
    assert_eq!(record.tmux_socket, Some(dir.path().join(SERVER)));
    assert_eq!(record.pane_id.as_deref(), Some("%9"));
}

/// `TM_WORKSPACE_TMUX=1` keeps the record on its server, so the existing
/// crashed-server restart path still runs for it.
#[serial_test::serial]
#[test]
fn the_opt_in_keeps_a_resumed_record_on_its_workspace_server() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = dir.path().to_path_buf();
    let (log, tmux) = driver(dir.path(), &workdir, "");
    let mut record = stopped_record(&ManagedSessionId::new(), dir.path(), &workdir);
    assert!(!rehome_with(&tmux, &mut record, Some("1")).expect("opt-in"));
    assert_eq!(record.tmux_socket, Some(dir.path().join(SERVER)));
    assert!(!log.exists(), "an opted-in record is not probed");
}

/// A crashed workspace server (its socket refuses) moves the record; it is
/// not restarted.
#[serial_test::serial]
#[test]
fn a_crashed_workspace_server_moves_the_record_to_the_default_server() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = dir.path().to_path_buf();
    // A socket file nothing listens on: a crashed server.
    drop(std::os::unix::net::UnixListener::bind(dir.path().join(SERVER)).expect("bind"));
    let refuse =
        "if [ \"$1\" = -L ]; then echo 'error connecting (Connection refused)' >&2; exit 1; fi";
    let (_log, tmux) = driver(dir.path(), &workdir, refuse);
    let mut record = stopped_record(&ManagedSessionId::new(), dir.path(), &workdir);
    assert!(rehome_with(&tmux, &mut record, None).expect("probe"));
    assert_eq!(record.tmux_socket, None);
    assert_eq!(record.pane_id, None);
    assert_eq!(record.claude_session_id.as_deref(), Some("claude-10101"));
}

/// Fail-closed arm: a failed listing whose socket still answers keeps the
/// record on its server, untouched. A record with no socket never moves.
#[serial_test::serial]
#[test]
fn a_failed_listing_on_an_answering_socket_keeps_the_record() {
    let dir = crate::test_support::hermetic_temp_dir();
    let workdir = dir.path().to_path_buf();
    // A live listener, held to the end: the server's socket answers.
    let _listener = std::os::unix::net::UnixListener::bind(dir.path().join(SERVER)).expect("bind");
    let refuse = "if [ \"$1\" = -L ]; then echo 'list failed' >&2; exit 1; fi";
    let (_log, tmux) = driver(dir.path(), &workdir, refuse);
    let mut record = stopped_record(&ManagedSessionId::new(), dir.path(), &workdir);
    assert!(!rehome_with(&tmux, &mut record, None).expect("probe"));
    assert_eq!(record.tmux_socket, Some(dir.path().join(SERVER)));
    assert_eq!(record.tmux_server.as_deref(), Some("1:1"));
    assert_eq!(record.pane_id.as_deref(), Some("%9"));

    // A probe of this socket still in flight: `socket_answers` reads `None`
    // (unknown), which keeps the record too.
    let socket = dir.path().join(SERVER);
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let held = crate::core::tmux_server::bounded_probe(
        &socket,
        std::time::Duration::from_millis(10),
        move || wait.recv().ok(),
    );
    assert!(held.is_none(), "the holding probe outlives its budget");
    assert_eq!(
        crate::core::tmux_server::socket_answers(&socket, std::time::Duration::from_millis(10)),
        None
    );
    assert!(!rehome_with(&tmux, &mut record, None).expect("unknown probe"));
    assert_eq!(record.tmux_socket, Some(socket));
    assert_eq!(record.tmux_server.as_deref(), Some("1:1"));
    assert_eq!(record.pane_id.as_deref(), Some("%9"));
    release.send(()).expect("release the holding probe");

    record.tmux_socket = None;
    assert!(!rehome_with(&tmux, &mut record, None).expect("no socket"));
}
