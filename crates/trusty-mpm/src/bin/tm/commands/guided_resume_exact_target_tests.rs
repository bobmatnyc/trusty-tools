//! #8443: `tm sessions resume X` with `X` gone and `X-suffix` alive.
//!
//! On 2026-09-23 the bare `-t tm-cto` probes prefix-matched `tm-cto-reports`:
//! `has-session` said the missing session was live, the plan chose a restart,
//! and the daemon's bare `kill-session -t tm-cto` destroyed `tm-cto-reports`.
//! Every test here runs on a private `-L` tmux server and skips when tmux is
//! not installed. `#[serial]` because the `has-session` and `kill-session`
//! spawns pass the #5784 `$HOME` guard, which `#[serial]` tests reassign.

use super::{ResumeAction, plan_resume, session_runtime_live_with_bin};
use crate::formatters::banner::tmux_has_session_with_bin;
use crate::test_support::tmux_session::{
    PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
};

const TMUX: &str = "tmux";

/// A private server holding `<X>-suffix` and no `<X>`; returns `<X>` too.
fn sibling_fixture(tag: &str) -> Option<(PrivateTmuxServer, ScratchTmuxSession, String)> {
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return None;
    }
    let server = PrivateTmuxServer::new(TMUX, tag);
    let missing = reserved_session_name(&format!("resume-{tag}"));
    let sibling = ScratchTmuxSession::spawn_on_socket(
        TMUX,
        Some(server.name()),
        &format!("{missing}-suffix"),
        "sh",
    );
    Some((server, sibling, missing))
}

/// The sibling's `pane_id:pane_pid:session_created` — any replacement of its
/// pane or session changes it.
fn identity(server: &PrivateTmuxServer, session: &str) -> Option<String> {
    server.query(&[
        "display-message",
        "-p",
        "-t",
        &trusty_common::tmux::exact_window_target(session),
        "#{pane_id}:#{pane_pid}:#{session_created}",
    ])
}

#[serial_test::serial]
#[test]
fn tmux_has_session_ignores_a_prefix_sibling() {
    let Some((server, sibling, missing)) = sibling_fixture("has") else {
        return;
    };
    let shim = server.shim_bin();
    assert!(
        tmux_has_session_with_bin(&shim, sibling.name()),
        "fixture precondition: the sibling answers by its exact name"
    );
    assert!(
        !tmux_has_session_with_bin(&shim, &missing),
        "has-session '{missing}' must not match the live '{}'",
        sibling.name()
    );
}

#[serial_test::serial]
#[test]
fn session_runtime_live_never_reports_a_prefix_sibling() {
    let Some((server, sibling, missing)) = sibling_fixture("rtlive") else {
        return;
    };
    assert!(
        !session_runtime_live_with_bin(&server.shim_bin(), &missing),
        "'{missing}' does not exist, so no runtime in it is live — the probe must not \
         answer from '{}'",
        sibling.name()
    );
}

#[serial_test::serial]
#[test]
fn resume_of_a_missing_session_never_touches_a_prefix_sibling() {
    let Some((server, sibling, missing)) = sibling_fixture("plan") else {
        return;
    };
    let shim = server.shim_bin();
    let before = identity(&server, sibling.name());
    assert!(
        before.is_some(),
        "fixture precondition: the sibling is live"
    );

    // The exact probes `resume_session` runs, in its order.
    assert!(
        tmux_has_session_with_bin(&shim, sibling.name()),
        "fixture precondition: the probe reaches the private server"
    );
    let tmux_live = tmux_has_session_with_bin(&shim, &missing);
    let runtime_live = !tmux_live || session_runtime_live_with_bin(&shim, &missing);
    assert!(!tmux_live, "the plan must see '{missing}' as absent");

    for state in ["stopped", "active", "provisioning", "errored"] {
        let action = plan_resume(state, tmux_live, runtime_live);
        assert!(
            matches!(
                action,
                ResumeAction::Restart | ResumeAction::ReconcileThenRestart
            ),
            "state {state}: a missing session restarts, got {action:?}"
        );
    }

    // What that restart does to tmux: the daemon's teardown kill, then the
    // idempotent `new-session -A -s <X>` that recreates the session.
    let kill = trusty_mpm::core::tmux::run_tmux_with_bin(
        &shim,
        &trusty_mpm::core::tmux::TmuxCommand::KillSession {
            name: missing.clone(),
        },
    )
    .expect("spawn kill-session");
    assert!(!kill.status.success(), "there is no '{missing}' to kill");
    let workdir = std::env::temp_dir();
    let created =
        trusty_mpm::core::tmux::create_managed_session(Some(&shim), &missing, workdir.to_str())
            .expect("spawn new-session");
    assert!(
        created.output.status.success(),
        "the restart creates '{missing}'"
    );

    assert!(
        tmux_has_session_with_bin(&shim, &missing),
        "'{missing}' now exists in its own right"
    );
    assert_eq!(
        identity(&server, sibling.name()),
        before,
        "the restart of '{missing}' killed or respawned '{}'",
        sibling.name()
    );
}

/// A live session on a fresh `tm-ws-*` server in this test binary's private
/// `TMUX_TMPDIR` (#6542), never the operator's server; `None` without tmux.
fn on_workspace_server(tag: &str) -> Option<(ScratchTmuxSession, String)> {
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return None;
    }
    let server = format!("tm-ws-{tag}-{}", std::process::id());
    let name = reserved_session_name(tag);
    let session = ScratchTmuxSession::spawn_on_socket(TMUX, Some(&server), &name, "sh");
    Some((session, server))
}

/// Remove `session` and its whole server.
fn tear_down(session: ScratchTmuxSession, server: &str) {
    drop(session);
    let _ = std::process::Command::new(TMUX)
        .args(["-L", server, "kill-server"])
        .output();
}

/// #10101: the picker's liveness probe reads the workspace server that holds
/// the session. On the default server alone a live PM read as gone, and the
/// resume plan reconciled and restarted it.
#[serial_test::serial]
#[test]
fn the_picker_probes_read_the_workspace_server_10101() {
    let Some((session, server)) = on_workspace_server("10101pick") else {
        return;
    };
    let live = crate::formatters::banner::tmux_has_session(session.name());
    tear_down(session, &server);
    assert!(live, "has-session must reach the workspace server");
}

/// #10101: the attach hint the picker prints names the workspace socket.
#[serial_test::serial]
#[test]
fn a_printed_attach_hint_names_the_workspace_socket_10101() {
    let Some((session, server)) = on_workspace_server("10101hint") else {
        return;
    };
    let hint = crate::commands::tmux_attach_server::shell_attach_command_on(session.name());
    tear_down(session, &server);
    assert!(
        hint.starts_with("tmux -S '") && hint.contains(&format!("/{server}' attach-session")),
        "{hint}"
    );
}
