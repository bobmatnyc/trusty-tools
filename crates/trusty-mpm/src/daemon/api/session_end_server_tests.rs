//! #11175: the `SessionEnd` gate reads the panes on the session's own tmux
//! server. A fake tmux answers a live `claude` pane only on the workspace
//! server; no real tmux runs.

use super::session_end_defers;
use crate::daemon::orphan_gc::AlwaysIdleProbe;

/// A fake tmux that lists one `claude` pane of `tm-proj` only when called
/// with `-L tm-ws-proj-abc`, and no pane on any other server.
fn fake_tmux() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("fake-tmux");
    let script = "#!/bin/sh\n\
        if [ \"$1\" = \"-L\" ] && [ \"$2\" = \"tm-ws-proj-abc\" ]; then\n\
        printf 'tm-proj\\tclaude\\t4242\\t%%1\\t/tmp\\n'\n\
        fi\n";
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (dir, bin)
}

/// Why (#11175): a live PM on its workspace server was marked Stopped on
/// `SessionEnd` because the gate listed the default server's panes only.
/// Test: itself.
#[serial_test::serial]
#[tokio::test]
async fn the_session_end_gate_reads_the_workspace_server_11175() {
    let (_dir, bin) = fake_tmux();
    let socket = crate::core::tmux_server::ambient_socket_dir().join("tm-ws-proj-abc");
    let (on_server, on_default) = crate::core::tmux::with_tmux_binary(bin, async {
        (
            session_end_defers("tm-proj", Some(&socket), &AlwaysIdleProbe),
            session_end_defers("tm-proj", None, &AlwaysIdleProbe),
        )
    })
    .await;
    assert!(
        on_server,
        "a live pane on the workspace server must defer the stop"
    );
    assert!(
        !on_default,
        "the default server holds no pane, so the stop proceeds"
    );
}

/// A fake tmux that appends its argv to `argv.log`, then runs `body`.
fn logging_tmux(body: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("argv.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (dir, bin, log)
}

/// Error arm (Fail-Open Check, #11175): a socket outside the socket directory
/// names no reachable server, so the gate defers and runs no tmux at all —
/// never the default server in its place.
/// Test: itself.
#[serial_test::serial]
#[tokio::test]
async fn an_unroutable_socket_defers_without_running_tmux_11175() {
    let (dir, bin, log) = logging_tmux("");
    let socket = std::path::Path::new("/elsewhere/tm-ws-x");
    let defers = crate::core::tmux::with_tmux_binary(bin, async {
        super::session_end_defers_in("tm-proj", Some(socket), dir.path(), &AlwaysIdleProbe)
    })
    .await;
    assert!(defers, "an unroutable socket must defer the stop");
    assert!(
        !log.exists(),
        "no tmux call may run for an unroutable socket"
    );
}

/// Timeout arm (#11175): a workspace server that never answers is listed
/// within the probe budget, and the stop proceeds as for a failed listing.
/// Test: itself.
#[serial_test::serial]
#[tokio::test]
async fn a_hung_workspace_server_lets_the_stop_proceed_in_time_11175() {
    let (dir, bin, log) = logging_tmux("sleep 8");
    let socket = dir.path().join("tm-ws-hung-11175");
    let started = std::time::Instant::now();
    let defers = crate::core::tmux::with_tmux_binary(bin, async {
        super::session_end_defers_in("tm-proj", Some(&socket), dir.path(), &AlwaysIdleProbe)
    })
    .await;
    let took = started.elapsed();
    assert!(!defers, "a listing that outlives the budget proceeds");
    assert!(
        took < std::time::Duration::from_secs(5),
        "the gate must return within the probe budget, took {took:?}"
    );
    let argv = std::fs::read_to_string(&log).expect("tmux ran");
    assert!(argv.starts_with("-L tm-ws-hung-11175 "), "{argv}");
}
