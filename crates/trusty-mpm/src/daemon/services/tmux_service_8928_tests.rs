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

/// The default socket name tmux uses inside a `TMUX_TMPDIR`.
const DEFAULT_SOCKET: &str = "default";

/// A scratch tmux directory whose servers no other process can reach (#10101).
///
/// Why: a test that starts a default-server session must never start it on
/// the operator's default server, which a bare tmux call reaches through an
/// inherited `$TMUX` or the process `TMUX_TMPDIR`.
/// What: a 0700 `/tmp/t10101-<pid>-<nanos>` directory and a shim that unsets
/// `TMUX`/`TMUX_PANE` and sets `TMUX_TMPDIR` to it before it runs tmux, so
/// the shim's default and `-L` servers live in that directory only. `Drop`
/// kills each named server by its explicit `-S` socket path, then removes
/// the directory. `/tmp` keeps the socket path inside `sun_path`.
struct ScratchTmuxDir {
    dir: std::path::PathBuf,
    shim: String,
    /// The server names `Drop` kills by socket path.
    servers: Vec<String>,
}

impl ScratchTmuxDir {
    /// `None` when no tmux binary resolves on this host.
    fn new(servers: &[&str]) -> Option<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let tmux = trusty_common::bin_resolve::resolve_binary("tmux")?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::path::PathBuf::from(format!("/tmp/t10101-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("scratch tmux dir");
        let shim = dir.join("tmux-scratch");
        let script = format!(
            "#!/bin/sh\nunset TMUX TMUX_PANE\nTMUX_TMPDIR='{}'\nexport TMUX_TMPDIR\n\
             exec '{}' -f /dev/null \"$@\"\n",
            dir.display(),
            tmux.display()
        );
        trusty_common::exec_stub::write_executable(&shim, script).expect("tmux shim");
        Some(Self {
            dir,
            shim: shim.to_string_lossy().into_owned(),
            servers: servers.iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    /// Where the shim's tmux puts every socket: `<dir>/tmux-<uid>`.
    fn socket_dir(&self) -> std::path::PathBuf {
        crate::core::tmux_server::socket_dir(Some(&self.dir), nix::unistd::getuid().as_raw())
    }
}

impl Drop for ScratchTmuxDir {
    fn drop(&mut self) {
        for server in &self.servers {
            let socket = self.socket_dir().join(server);
            let args = ["-S".to_owned(), socket.to_string_lossy().into_owned()];
            let args = [&args[..], &["kill-server".to_owned()]].concat();
            let _ = crate::core::spawn_disclaim::disclaimed_output(&self.shim, &args);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Error arm (Fail-Open Check, #10101): a name held by the default server and
/// by a workspace server is refused, never answered from the default server.
/// Both servers are scratch servers in a [`ScratchTmuxDir`].
#[serial_test::serial]
#[test]
fn a_name_on_two_servers_is_refused() {
    let server = format!("{WORKSPACE_SERVER_PREFIX}10101-{}", std::process::id());
    let Some(scratch) = ScratchTmuxDir::new(&[DEFAULT_SOCKET, &server]) else {
        eprintln!("tmux not available; skipping");
        return;
    };
    let tmux = scratch.shim.as_str();
    let name = reserved_session_name("10101-twice");
    let _named = ScratchTmuxSession::spawn_on_socket(tmux, Some(&server), &name, "sleep 600");
    let _default =
        ScratchTmuxSession::spawn_on_socket(tmux, Some(DEFAULT_SOCKET), &name, "sleep 600");
    let sockets = scratch.socket_dir();
    for socket in [DEFAULT_SOCKET, server.as_str()] {
        assert!(
            sockets.join(socket).exists(),
            "{socket} is not in {sockets:?}"
        );
    }
    let driver = TmuxDriver::with_tmux_path_for_test(tmux);
    assert!(driver.monitor_session(&name, 5).is_ok(), "precondition");
    let err = snapshot_on_any_server(&driver, &sockets, &name, 5).expect_err("two holders");
    assert!(err.to_string().contains("more than one server"), "{err}");
}
