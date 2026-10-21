//! #8928 step 4: the daemon snapshot finds a session on a workspace server.
//!
//! Runs real tmux on this test process's relocated socket directory
//! (`core::tmux_test_isolation`), never the operator's server.

use super::snapshot_on_any_server;
use crate::core::tmux_server::{WORKSPACE_SERVER_PREFIX, ambient_socket_dir};
use crate::daemon::tmux::TmuxDriver;
use crate::test_support::tmux_session::{ScratchTmuxSession, reserved_session_name};

/// Kills the named server when the test ends, however it ends.
struct KillServer(String);

impl Drop for KillServer {
    fn drop(&mut self) {
        let _ = crate::core::tmux::run_tmux_argv(&[
            "-L".to_owned(),
            self.0.clone(),
            "kill-server".to_owned(),
        ]);
    }
}

#[serial_test::serial]
#[test]
fn a_snapshot_finds_a_session_on_a_workspace_server() {
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = format!("{WORKSPACE_SERVER_PREFIX}8928-{}", std::process::id());
    let _kill = KillServer(server.clone());
    let name = reserved_session_name("8928-ws");
    let _session = ScratchTmuxSession::spawn_on_socket("tmux", Some(&server), &name, "sleep 600");
    let driver = TmuxDriver::with_tmux_path_for_test("tmux");
    // The default server does not hold it; a driver on the named server does.
    assert!(
        driver.monitor_session(&name, 5).is_err(),
        "not on the default server"
    );
    let snapshot = snapshot_on_any_server(&driver, &ambient_socket_dir(), &name, 5)
        .expect("found on the workspace server");
    assert_eq!(snapshot.name, name);
    // A session on no server is still an error.
    let missing = snapshot_on_any_server(&driver, &ambient_socket_dir(), "tm-absent-8928", 5);
    assert!(missing.is_err());
}

#[serial_test::serial]
#[test]
fn a_driver_on_a_named_server_reads_only_that_server() {
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = format!("{WORKSPACE_SERVER_PREFIX}8928b-{}", std::process::id());
    let _kill = KillServer(server.clone());
    let name = reserved_session_name("8928-named");
    let _session = ScratchTmuxSession::spawn_on_socket("tmux", Some(&server), &name, "sleep 600");
    let named = TmuxDriver::with_tmux_path_for_test("tmux").on_server(&server);
    let names: Vec<String> = named
        .list_sessions()
        .expect("list the named server")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(names.contains(&name), "{names:?}");
    let default = TmuxDriver::with_tmux_path_for_test("tmux")
        .list_sessions()
        .unwrap_or_default();
    assert!(default.iter().all(|s| s.name != name), "{default:?}");
}
