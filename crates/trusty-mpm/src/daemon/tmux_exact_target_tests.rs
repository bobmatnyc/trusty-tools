//! #8443: the daemon's tmux driver addresses a session EXACTLY.
//!
//! Each test runs on a private `-L` tmux server with a live session
//! `<X>-suffix` and no `<X>`. Before #8443 every target was a bare name, and
//! tmux prefix-matched `<X>` onto `<X>-suffix`: `kill-session -t tm-cto`
//! killed `tm-cto-reports`. Skips when tmux is not installed.

use super::TmuxDriver;
use crate::core::tmux::TmuxTarget;
use crate::test_support::tmux_session::{
    PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
};

const TMUX: &str = "tmux";

/// A private server holding `<X>-suffix` (no `<X>`), plus a driver bound to it.
fn sibling_fixture(
    tag: &str,
) -> Option<(PrivateTmuxServer, ScratchTmuxSession, String, TmuxDriver)> {
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return None;
    }
    let server = PrivateTmuxServer::new(TMUX, tag);
    let missing = reserved_session_name(&format!("exact-{tag}"));
    let sibling = ScratchTmuxSession::spawn_on_socket(
        TMUX,
        Some(server.name()),
        &format!("{missing}-suffix"),
        "sh",
    );
    let driver = TmuxDriver::with_tmux_path_for_test(server.shim_bin());
    Some((server, sibling, missing, driver))
}

/// The sibling's `(pane_id, pane_pid)` — both change if the pane is replaced.
fn pane_identity(server: &PrivateTmuxServer, session: &str) -> Option<String> {
    server.query(&[
        "display-message",
        "-p",
        "-t",
        &trusty_common::tmux::exact_window_target(session),
        "#{pane_id}:#{pane_pid}",
    ])
}

#[test]
fn kill_session_leaves_a_prefix_sibling_alive() {
    let Some((server, sibling, missing, driver)) = sibling_fixture("kill") else {
        return;
    };
    let before = pane_identity(&server, sibling.name());
    assert!(
        before.is_some(),
        "fixture precondition: the sibling is live"
    );

    let result = driver.kill_session(&missing);

    assert!(
        result.is_err(),
        "killing the missing session '{missing}' must fail, not succeed on a sibling"
    );
    assert!(
        ScratchTmuxSession::exists_on_socket(TMUX, Some(server.name()), sibling.name()),
        "kill_session('{missing}') destroyed '{}' — the #8443 prefix match",
        sibling.name()
    );
    assert_eq!(pane_identity(&server, sibling.name()), before);
}

#[test]
fn send_and_capture_never_reach_a_prefix_sibling() {
    // `_server` is held (not `_`) so the private server outlives the sibling.
    let Some((_server, sibling, missing, driver)) = sibling_fixture("io") else {
        return;
    };
    let marker = "marker-8443-must-not-land";

    let sent = driver.send_line(&TmuxTarget::session(&missing), &format!("echo {marker}"));
    let captured = driver.capture(&TmuxTarget::session(&missing), Some(50));
    std::thread::sleep(std::time::Duration::from_millis(300));

    assert!(
        sent.is_err(),
        "send-keys to the missing '{missing}' must fail"
    );
    assert!(
        captured.is_err(),
        "capture-pane of the missing '{missing}' must fail, got {captured:?}"
    );
    let sibling_screen = driver
        .capture(&TmuxTarget::session(sibling.name()), Some(50))
        .expect("the sibling itself is capturable by its exact name");
    assert!(
        !sibling_screen.contains(marker),
        "keys aimed at '{missing}' landed in '{}': {sibling_screen}",
        sibling.name()
    );
}
