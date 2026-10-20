//! Tests for the #8928 step 3 sccache unix-socket server.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

/// A scratch directory, canonicalized; the socket path inside it.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    (dir, root)
}

/// Write an executable `sh` script at `path`.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A socket file nothing can stream-connect to. A dropped `UnixListener`
/// is not one under a parallel suite: macOS sets `FD_CLOEXEC` after
/// `socket()`, so a child another test forks in that window keeps the
/// listener alive (#10083). A datagram socket refuses a stream connect
/// whoever holds its fd.
fn dead_socket(path: &Path) {
    drop(UnixDatagram::bind(path).expect("bind dead socket"));
}

#[test]
fn the_socket_lives_under_trusty_mpm() {
    assert_eq!(
        socket_path_at(Path::new("/h")),
        PathBuf::from("/h/.trusty-mpm/sccache.sock")
    );
}

#[test]
fn client_and_server_env_name_the_socket() {
    let socket = Path::new("/h/.trusty-mpm/sccache.sock");
    let client = client_env(socket);
    assert!(client.contains(&("SCCACHE_SERVER_UDS", socket.display().to_string())));
    assert!(client.contains(&("SCCACHE_CLIENT_SIDE", "1".to_owned())));
    let server = server_env(socket);
    assert!(server.contains(&("SCCACHE_SERVER_UDS", socket.display().to_string())));
    // The default 600 s idle timeout would let the server exit for good.
    assert!(server.contains(&("SCCACHE_IDLE_TIMEOUT", "0".to_owned())));
}

#[test]
fn versions_parse_and_compare() {
    assert_eq!(parse_version("sccache 0.17.0\n"), Some((0, 17, 0)));
    assert_eq!(parse_version("sccache 0.9.1-beta"), Some((0, 9, 1)));
    assert_eq!(parse_version("sccache 0.8"), Some((0, 8, 0)));
    assert_eq!(parse_version("garbage"), None);
    assert!(Some((0, 8, 3)) < Some(MIN_UDS_VERSION));
    assert!(Some((0, 17, 0)) >= Some(MIN_UDS_VERSION));
}

#[test]
fn a_listening_socket_is_listening() {
    let (_dir, root) = scratch();
    let path = root.join("s.sock");
    let _listener = UnixListener::bind(&path).expect("bind");
    assert_eq!(probe_socket(&path), SocketState::Listening);
}

#[test]
fn a_stale_socket_is_stale() {
    let (_dir, root) = scratch();
    let path = root.join("s.sock");
    dead_socket(&path);
    assert!(matches!(probe_socket(&path), SocketState::Stale(_)));
    assert_eq!(
        probe_socket(&root.join("absent.sock")),
        SocketState::Missing
    );
}

#[test]
fn a_regular_file_is_unknown() {
    let (_dir, root) = scratch();
    let path = root.join("s.sock");
    std::fs::write(&path, "x").expect("write");
    assert!(matches!(probe_socket(&path), SocketState::Unknown(_)));
}

#[test]
fn ensure_server_leaves_a_listening_server_alone() {
    let (_dir, root) = scratch();
    let socket = root.join("s.sock");
    let _listener = UnixListener::bind(&socket).expect("bind");
    let bin = root.join("never-run");
    let out = ensure_server(&bin, &socket).expect("already listening");
    assert_eq!(out, StartOutcome::AlreadyListening);
}

#[test]
fn ensure_server_starts_and_verifies() {
    let (_dir, root) = scratch();
    let socket = root.join("run/s.sock");
    let marker = root.join("marker");
    let bin = root.join("fake-sccache");
    // The fake records its argv and env, then waits for the test to bind.
    script(
        &bin,
        &format!(
            "echo \"$1 $SCCACHE_SERVER_UDS $SCCACHE_IDLE_TIMEOUT ${{SCCACHE_CLIENT_SIDE:-unset}}\" \
             > '{m}.tmp' && mv '{m}.tmp' '{m}'\n\
             while [ ! -S \"$SCCACHE_SERVER_UDS\" ]; do sleep 0.05; done",
            m = marker.display()
        ),
    );
    // A stale socket file is cleared first.
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("mkdir");
    dead_socket(&socket);
    let (bind_at, wait_for) = (socket.clone(), marker.clone());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !wait_for.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        UnixListener::bind(&bind_at).expect("bind live")
    });
    let out = ensure_server(&bin, &socket).expect("started");
    let _listener = server.join().expect("join");
    assert_eq!(out, StartOutcome::Started);
    let seen = std::fs::read_to_string(&marker).expect("marker");
    assert_eq!(
        seen.trim(),
        format!("--start-server {} 0 unset", socket.display())
    );
}

#[test]
fn ensure_server_refuses_a_non_socket() {
    let (_dir, root) = scratch();
    let socket = root.join("s.sock");
    std::fs::write(&socket, "not a socket").expect("write");
    let err = ensure_server(&root.join("never-run"), &socket).expect_err("refused");
    assert!(matches!(err, SccacheSocketError::BadPath { .. }), "{err}");
    assert!(socket.is_file(), "a non-socket is never removed");
}

#[test]
fn ensure_server_reports_a_server_that_never_listens() {
    let (_dir, root) = scratch();
    let socket = root.join("s.sock");
    let quiet = root.join("quiet-sccache");
    script(&quiet, "exit 0");
    let err = ensure_server(&quiet, &socket).expect_err("nothing listens");
    assert!(
        matches!(err, SccacheSocketError::NotListening { .. }),
        "{err}"
    );
    let failing = root.join("failing-sccache");
    script(&failing, "exit 3");
    let err = ensure_server(&failing, &socket).expect_err("start fails");
    assert!(matches!(err, SccacheSocketError::Start { .. }), "{err}");
    let missing = ensure_server(&root.join("absent"), &socket).expect_err("no binary");
    assert!(
        matches!(missing, SccacheSocketError::Start { .. }),
        "{missing}"
    );
}

/// Write `home`'s crate config.
fn config(home: &Path, body: &str) {
    let path = trusty_common::crate_config::crate_config_path_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    );
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write config");
}

#[test]
fn start_if_requested_is_a_no_op_without_the_opt_in() {
    let (_dir, home) = scratch();
    assert!(start_if_requested(&home).is_none(), "no config");
    config(&home, "build:\n  build_jobs: 4\n");
    assert!(start_if_requested(&home).is_none(), "no sccache key");
    config(&home, "build:\n  sccache: false\n");
    assert!(start_if_requested(&home).is_none(), "sccache false");
    assert_eq!(sccache_requested(&home), Ok(false));
    config(&home, "build:\n  sccache: true\n");
    assert_eq!(sccache_requested(&home), Ok(true));
}

#[test]
fn an_unreadable_config_is_an_error_not_a_skip() {
    let (_dir, home) = scratch();
    config(&home, "build: [unclosed\n");
    assert!(sccache_requested(&home).is_err());
    let out = start_if_requested(&home);
    assert!(
        matches!(out, Some(Err(SccacheSocketError::Config(_)))),
        "{out:?}"
    );
}
