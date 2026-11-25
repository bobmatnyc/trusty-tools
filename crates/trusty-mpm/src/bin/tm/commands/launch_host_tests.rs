//! Tests for `launch_host` (#10101): a CLI launch's tmux calls reach the PM's
//! workspace server.

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

/// #10101: the CLI's host is the daemon's rule for the workspace — the
/// default server, or its `tm-ws-` server and that server's socket under
/// `TM_WORKSPACE_TMUX=1`.
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
        host.create_for("tm-proj", "/w", false)
            .expect("the fake creates");
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
        host.create_for("tm-proj", "/w", false)
            .expect_err("exit 1 fails")
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

/// A launch spec in `/w` stamped with `profile` (#11270).
fn stamped(
    profile: trusty_mpm::core::session_profile::SessionProfile,
) -> trusty_mpm::runtime::launch_spec::LaunchSpec {
    trusty_mpm::runtime::launch_spec::LaunchSpec {
        session_id: String::new(),
        launch_id: String::new(),
        cwd: PathBuf::from("/w"),
        program: "claude".to_owned(),
        args: Vec::new(),
        env_unset: Vec::new(),
        env_set: vec![trusty_mpm::core::session_profile::launch_env(profile)],
    }
}

/// #11270 (M1): a running session refuses an Architect with the remedy and
/// passes a PM; the probe is the only tmux call, so nothing is typed.
#[serial_test::serial]
#[tokio::test]
async fn a_running_session_refuses_only_an_architect_and_types_nothing_11270() {
    use trusty_mpm::core::session_profile::SessionProfile;
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(dir.path(), 0);
    let host = workspace_host();
    let (architect, pm) = trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        (
            host.refuse_running_architect("tm-a", &stamped(SessionProfile::Architect)),
            host.refuse_running_architect("tm-a", &stamped(SessionProfile::Pm)),
        )
    })
    .await;
    let err = architect.expect_err("an Architect is refused");
    assert!(err.to_string().contains("tm fleet relaunch"), "{err}");
    pm.expect("a PM passes");
    let lines = logged(&log);
    assert!(lines.iter().all(|l| l.contains("has-session")), "{lines:?}");
}

/// #11270 (M3): an Architect create never carries `-A`; a PM one still does.
#[serial_test::serial]
#[tokio::test]
async fn an_architect_create_never_attaches_11270() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, log) = fake_tmux(dir.path(), 0);
    let host = workspace_host();
    trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        host.create_for("tm-a", "/w", true).expect("exclusive");
        host.create_for("tm-p", "/w", false).expect("idempotent");
    })
    .await;
    let lines = logged(&log);
    let create = |name: &str| {
        lines
            .iter()
            .find(|l| l.contains("new-session") && l.contains(name))
            .cloned()
            .unwrap_or_else(|| panic!("no create for {name}: {lines:?}"))
    };
    assert!(!create("tm-a").contains(" -A"), "{}", create("tm-a"));
    assert!(create("tm-p").contains(" -A"), "{}", create("tm-p"));
}

/// #11270 (H2, Fail-Open Check): the CLI step `tm launch`, `tm connect` and
/// `tm session start` share types the launch, finds no `claude` to bind, kills
/// the session it created and errors with the remedy.
#[serial_test::serial]
#[tokio::test]
async fn a_fresh_cli_architect_left_unbound_is_killed_and_errors_11270() {
    use trusty_mpm::core::session_profile::SessionProfile;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    let specs = dir.path().join("specs");
    let (bin, log) = fake_tmux(dir.path(), 0);
    let host = workspace_host();
    let spec = stamped(SessionProfile::Architect);
    let err = trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        host.start_claude_at(Some(&root), "tm-a", &spec, &specs)
    })
    .await
    .expect_err("an unbound Architect is an error");
    let text = format!("{err:#}");
    for part in ["NOT bound", "was killed", "tm fleet relaunch"] {
        assert!(text.contains(part), "{part} missing: {text}");
    }
    let lines = logged(&log);
    let typed = lines.iter().position(|l| l.contains("send-keys"));
    let killed = lines.iter().position(|l| l.contains("kill-session"));
    assert!(typed.is_some() && killed > typed, "{lines:?}");
    assert!(!root.join("architect-launch").join("1.architect").exists());
}

/// #11270 (M2): an Architect launch into a running session is refused before
/// `POST /sessions`, so it adds no record; a PM launch still registers.
#[serial_test::serial]
#[tokio::test]
async fn an_architect_launch_into_a_running_session_registers_nothing_11270() {
    use trusty_mpm::core::session_profile::SessionProfile;
    let dir = tempfile::tempdir().expect("tempdir");
    let (bin, _log) = fake_tmux(dir.path(), 0);
    let (seen, url) = crate::test_support::launch_stub::spawn_empty().await;
    let daemon = DaemonClient::with_client(reqwest::Client::new(), &url);
    let host = workspace_host();
    let body = || serde_json::json!({ "project": "/w", "project_path": "/w", "name": "tm-a" });
    let (architect, pm) = trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        let a = register_launch(
            &daemon,
            &host,
            "tm-a",
            body(),
            &stamped(SessionProfile::Architect),
        )
        .await;
        let p = register_launch(&daemon, &host, "tm-a", body(), &stamped(SessionProfile::Pm)).await;
        (a, p)
    })
    .await;
    let err = architect.expect_err("refused");
    assert!(err.to_string().contains("tm fleet relaunch"), "{err}");
    pm.expect("a PM launch registers");
    assert_eq!(
        seen.lock().expect("seen").posts.len(),
        1,
        "only the PM posted"
    );
}
