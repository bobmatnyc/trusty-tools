//! Tests for `launch_host` (#10101): a CLI launch's tmux calls reach the PM's
//! workspace server, and a held name is refused.

use std::path::{Path, PathBuf};

use super::*;

/// A fake `tmux` under `dir` that appends each argv to `<dir>/argv.log` and
/// exits `exit`.
fn fake_tmux(dir: &Path, exit: u8) -> (PathBuf, PathBuf) {
    let log = dir.join("argv.log");
    let bin = dir.join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit {exit}\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (bin, log)
}

/// The argv lines the fake logged, oldest first.
fn logged(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A host on workspace server `tm-ws-proj-abc` in this process's socket dir.
fn workspace_host() -> LaunchHost {
    let dir = trusty_mpm::core::tmux_server::ambient_socket_dir();
    LaunchHost::of_socket(Some(&dir.join("tm-ws-proj-abc"))).expect("in the socket dir")
}

/// #10101: the CLI's host is the daemon's rule for the workspace — its
/// `tm-ws-` server and that server's socket, or the default server under the
/// opt-out.
#[test]
fn a_cli_host_is_the_workspace_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = LaunchHost::for_workspace(dir.path());
    match trusty_mpm::session_manager::launch_server::cli_launch_server(dir.path()) {
        Some((server, socket)) => {
            assert!(server.starts_with("tm-ws-"), "{server}");
            assert_eq!(host.server(), Some(server.as_str()));
            assert_eq!(host.socket(), Some(socket.as_path()));
        }
        None => assert_eq!(host, LaunchHost::default()),
    }
}

/// #10101: a stored socket routes to its server; no socket is the default
/// server; a socket elsewhere is refused rather than read as the default.
#[test]
fn a_row_socket_routes_to_its_server() {
    assert_eq!(LaunchHost::of_socket(None).expect("default").server(), None);
    assert_eq!(workspace_host().server(), Some("tm-ws-proj-abc"));
    assert!(LaunchHost::of_socket(Some(Path::new("/elsewhere/tm-ws-proj-abc"))).is_err());
}

/// #10101: creating, probing and typing for a workspace-server PM reach that
/// server only — every tmux argv carries its `-L`. Red before #10101: the CLI
/// created on the default server.
#[serial_test::serial]
#[tokio::test]
async fn a_cli_create_reaches_the_workspace_server_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(dir.path(), 0);
    let host = workspace_host();
    trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        host.create("tm-proj", "/w").expect("the fake creates");
        assert!(host.has_session("tm-proj"));
        assert_eq!(
            host.claude_pid("tm-proj"),
            None,
            "the fake prints no pane pid"
        );
    })
    .await;
    let lines = logged(&log);
    assert!(lines.iter().any(|l| l.contains("new-session")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("has-session")), "{lines:?}");
    assert!(
        lines.iter().all(|l| l.starts_with("-L tm-ws-proj-abc ")),
        "a call reached another server: {lines:?}"
    );
}

/// #10101: a failed `new-session` is an error naming the server.
#[serial_test::serial]
#[tokio::test]
async fn a_failed_create_names_the_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, _log) = fake_tmux(dir.path(), 1);
    let host = workspace_host();
    let err = trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        host.create("tm-proj", "/w").expect_err("exit 1 fails")
    })
    .await;
    assert!(err.to_string().contains("tm-ws-proj-abc"), "{err}");
}

/// #1815/#10101: a guard dropped while armed kills its session on the server
/// it was created on; a disarmed one kills nothing.
#[serial_test::serial]
#[tokio::test]
async fn a_dropped_guard_kills_on_its_own_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(dir.path(), 0);
    let host = workspace_host();
    trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        let mut kept = LaunchSessionGuard::new(&host, "tm-kept");
        kept.disarm();
        drop(kept);
        drop(LaunchSessionGuard::new(&host, "tm-proj"));
    })
    .await;
    let lines = logged(&log);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("-L tm-ws-proj-abc kill-session"),
        "{lines:?}"
    );
    assert!(lines[0].contains("tm-proj"), "{lines:?}");
}

/// One managed summary as the daemon sends it.
fn summary(name: &str, state: &str, persisted: Option<&str>) -> ManagedSessionSummary {
    serde_json::from_value(serde_json::json!({
        "id": format!("{name}-{state}-id"),
        "name": name,
        "state": state,
        "persisted_state": persisted,
    }))
    .expect("summary")
}

/// #10101: a non-terminal record holds its name; a decommissioned or deleted
/// one does not; the persisted state decides; an unknown state holds.
#[test]
fn a_live_managed_record_holds_its_name() {
    let held = [summary("tm-proj", "active", None)];
    assert_eq!(
        name_holder(&held, "tm-proj").map(|s| s.id.as_str()),
        Some("tm-proj-active-id")
    );
    assert!(name_holder(&held, "tm-other").is_none());
    for state in ["decommissioned", "deleted"] {
        assert!(name_holder(&[summary("tm-proj", state, None)], "tm-proj").is_none());
    }
    let tombstone = [summary("tm-proj", "stopped", Some("decommissioned"))];
    assert!(
        name_holder(&tombstone, "tm-proj").is_none(),
        "persisted wins"
    );
    let stopped = [summary("tm-proj", "active", Some("stopped"))];
    assert!(
        name_holder(&stopped, "tm-proj").is_some(),
        "stopped still holds"
    );
    let unknown = [summary("tm-proj", "some-future-state", None)];
    assert!(
        name_holder(&unknown, "tm-proj").is_some(),
        "unknown fails closed"
    );
}
