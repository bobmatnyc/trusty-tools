//! #10101: pane capture, the input-box probe and sends for a legacy `Session`
//! reach the server its stored `tmux_socket` names. A fake tmux logs argv; no
//! real tmux runs.

use std::path::{Path, PathBuf};

use super::TmuxService;
use crate::core::session::{ControlModel, Session, SessionId};

/// A fake tmux in a scratch dir that appends its argv to `argv.log` and
/// prints one pane line. Returns the dir, the binary and the log path.
fn fake_tmux() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("argv.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\necho 'pane line'\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (dir, bin, log)
}

/// A legacy session named `tm-proj` whose server listens on `socket`.
fn session_on(socket: &Path) -> Session {
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.tmux_name = "tm-proj".to_owned();
    session.tmux_socket = Some(socket.to_path_buf());
    session
}

/// Why (#10101): `tm launch`/`connect`/`session start` register PMs on a
/// `tm-ws-*` server; capture, the input-box probe and `tm session run` went to
/// the default server. Red on 7564931e5d.
/// Test: itself.
#[serial_test::serial]
#[tokio::test]
async fn capture_and_send_reach_the_session_server_10101() {
    let (_dir, bin, log) = fake_tmux();
    let socket = crate::core::tmux_server::ambient_socket_dir().join("tm-ws-proj-abc");
    let session = session_on(&socket);
    let captured = crate::core::tmux::with_tmux_binary(bin, async {
        let _ = TmuxService::send_command(&session, "echo hi");
        let _ = TmuxService::input_box(&session);
        TmuxService::capture(&session, 10)
    })
    .await;
    assert!(captured.contains("pane line"), "{captured:?}");
    let lines = std::fs::read_to_string(&log).expect("tmux ran");
    assert!(
        lines.lines().all(|l| l.starts_with("-L tm-ws-proj-abc ")),
        "every call must name the workspace server:\n{lines}"
    );
    assert!(lines.contains("send-keys"), "{lines}");
    assert_eq!(lines.matches("capture-pane").count(), 2, "{lines}");
}

/// Error arm (Fail-Open Check, #10101): a socket outside this process's socket
/// directory names no reachable server, so nothing runs — never the default
/// server in its place. Red on 7564931e5d, which sent and captured there.
/// The `sent: false` answer is asserted by
/// `an_undelivered_command_answers_not_sent_on_both_transports_10101`.
/// Test: itself.
#[serial_test::serial]
#[tokio::test]
async fn an_unroutable_socket_neither_captures_nor_sends_10101() {
    let (dir, bin, log) = fake_tmux();
    let session = session_on(&dir.path().join("tm-ws-proj-abc"));
    let (captured, input_box) = crate::core::tmux::with_tmux_binary(bin, async {
        let _ = TmuxService::send_command(&session, "echo hi");
        (
            TmuxService::capture(&session, 10),
            TmuxService::input_box(&session),
        )
    })
    .await;
    assert_eq!(captured, "");
    assert_eq!(input_box, None);
    assert!(
        !log.exists(),
        "an unroutable socket must reach no tmux server: {:?}",
        std::fs::read_to_string(&log)
    );
}
