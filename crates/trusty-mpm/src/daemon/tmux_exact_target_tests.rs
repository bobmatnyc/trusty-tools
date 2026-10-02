//! #8443: the daemon's tmux driver addresses a session EXACTLY.
//!
//! Each test runs on a private `-L` tmux server with a live session
//! `<X>-suffix` and no `<X>`. Before #8443 every target was a bare name, and
//! tmux prefix-matched `<X>` onto `<X>-suffix`: `kill-session -t tm-cto`
//! killed `tm-cto-reports`. Skips when tmux is not installed.
//!
//! `#[serial]`: the driver's spawn guard (#5784) refuses tmux while another
//! test has `$HOME` reassigned, and those tests are `#[serial]` too. Each
//! refusal-shaped failure is asserted to be tmux's own "can't find" reply, so
//! a guard refusal can never pass as an exact-target miss.

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

/// Assert `result` failed because tmux found no such session — not because
/// the spawn guard refused to run tmux at all.
fn assert_tmux_miss<T: std::fmt::Debug>(what: &str, result: crate::core::Result<T>) {
    let err = result.expect_err(what);
    let msg = err.to_string();
    assert!(
        msg.contains("can't find"),
        "{what}: expected tmux's own 'can't find' reply, got: {msg}"
    );
}

#[serial_test::serial]
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

    assert_tmux_miss(
        "killing the missing session must fail, not succeed on a sibling",
        driver.kill_session(&missing),
    );
    assert!(
        ScratchTmuxSession::exists_on_socket(TMUX, Some(server.name()), sibling.name()),
        "kill_session('{missing}') destroyed '{}' — the #8443 prefix match",
        sibling.name()
    );
    assert_eq!(pane_identity(&server, sibling.name()), before);
}

#[serial_test::serial]
#[test]
fn send_and_capture_never_reach_a_prefix_sibling() {
    // `_server` is held (not `_`) so the private server outlives the sibling.
    let Some((_server, sibling, missing, driver)) = sibling_fixture("io") else {
        return;
    };
    let marker = "marker-8443-must-not-land";

    driver
        .capture(&TmuxTarget::session(sibling.name()), Some(5))
        .expect("fixture precondition: the driver reaches the private server");

    assert_tmux_miss(
        "send-keys to the missing session must fail",
        driver.send_line(&TmuxTarget::session(&missing), &format!("echo {marker}")),
    );
    assert_tmux_miss(
        "capture-pane of the missing session must fail",
        driver.capture(&TmuxTarget::session(&missing), Some(50)),
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let sibling_screen = driver
        .capture(&TmuxTarget::session(sibling.name()), Some(50))
        .expect("the sibling itself is capturable by its exact name");
    assert!(
        !sibling_screen.contains(marker),
        "keys aimed at '{missing}' landed in '{}': {sibling_screen}",
        sibling.name()
    );
}

/// #8443 critic HIGH: an empty name rendered `=:`, which tmux 3.6b resolves
/// to the CURRENT session — the private server's only session here.
#[serial_test::serial]
#[test]
fn an_empty_session_name_never_reaches_a_session() {
    let Some((server, sibling, _missing, driver)) = sibling_fixture("empty") else {
        return;
    };
    let before = pane_identity(&server, sibling.name());
    assert!(
        before.is_some(),
        "fixture precondition: the session is live"
    );
    let marker = "marker-8443-empty";

    for name in ["", "="] {
        let target = TmuxTarget::session(name);
        assert!(
            driver
                .send_line(&target, &format!("echo {marker}"))
                .is_err()
        );
        assert!(driver.send_interrupt(&target).is_err());
        assert!(driver.capture(&target, Some(5)).is_err());
        assert!(driver.kill_session(name).is_err());
        assert!(driver.pane_id(name).is_none());
        assert!(driver.pane_current_path(name).is_none());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));

    assert_eq!(pane_identity(&server, sibling.name()), before);
    let screen = driver
        .capture(&TmuxTarget::session(sibling.name()), Some(50))
        .expect("capture the live session");
    assert!(
        !screen.contains(marker),
        "an empty target typed into '{}'",
        sibling.name()
    );
}
