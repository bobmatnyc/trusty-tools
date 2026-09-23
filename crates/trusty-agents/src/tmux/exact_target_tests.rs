//! #8443: trusty-agents' two tmux wrappers address sessions EXACTLY.
//!
//! Each test runs on a private `-L` tmux server — never the default one —
//! with a live session `<X>-suffix` and no `<X>`. A bare `-t <X>` prefix-matches
//! `<X>-suffix`, which is how `kill-session -t tm-cto` destroyed
//! `tm-cto-reports`. Skips when tmux is not installed.

use std::path::PathBuf;
use std::process::Command;

use crate::debugger::tmux::TmuxAdapter;
use crate::tmux::orchestrator::TmuxOrchestrator;

/// A private tmux server, torn down on drop, reachable through a shim binary.
struct PrivateServer {
    socket: String,
    shim: PathBuf,
    _dir: tempfile::TempDir,
}

impl PrivateServer {
    fn start(tag: &str) -> Option<Self> {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("tmux not available; skipping");
            return None;
        }
        let dir = tempfile::tempdir().expect("shim dir");
        let socket = format!("tagent-8443-{tag}-{}", std::process::id());
        let shim = dir.path().join("tmux-private");
        std::fs::write(
            &shim,
            format!("#!/bin/sh\nexec tmux -L '{socket}' \"$@\"\n"),
        )
        .expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Some(Self {
            socket,
            shim,
            _dir: dir,
        })
    }

    fn shim(&self) -> String {
        self.shim.to_string_lossy().into_owned()
    }

    fn tmux(&self, args: &[&str]) -> Option<String> {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Spawn `name` running `sh`; returns its `pane_id:pane_pid` identity.
    fn spawn(&self, name: &str) -> String {
        self.tmux(&["new-session", "-d", "-s", name, "sh"])
            .expect("spawn session");
        self.identity(name).expect("identity of a fresh session")
    }

    fn identity(&self, name: &str) -> Option<String> {
        self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &trusty_common::tmux::exact_window_target(name),
            "#{pane_id}:#{pane_pid}",
        ])
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

#[test]
fn orchestrator_never_reaches_a_prefix_sibling() {
    let Some(server) = PrivateServer::start("orch") else {
        return;
    };
    let before = server.spawn("tga-x-suffix");
    let tmux = TmuxOrchestrator::with_tmux_path(server.shim());

    assert!(!tmux.session_exists("tga-x"), "has-session must be exact");
    assert!(tmux.destroy_session("tga-x").is_err());
    assert!(tmux.send_line("tga-x", None, "echo marker-8443").is_err());
    assert!(tmux.capture_output("tga-x", None, Some(20)).is_err());

    assert_eq!(server.identity("tga-x-suffix"), Some(before));
    let screen = server
        .tmux(&["capture-pane", "-p", "-t", "=tga-x-suffix:"])
        .expect("capture the sibling");
    assert!(
        !screen.contains("marker-8443"),
        "keys landed in the sibling"
    );
}

#[test]
fn debugger_adapter_never_reaches_a_prefix_sibling() {
    let Some(server) = PrivateServer::start("dbg") else {
        return;
    };
    let before = server.spawn("tga-d-suffix");
    let adapter = TmuxAdapter::with_tmux_path(server.shim());

    assert!(
        !adapter.session_exists("tga-d"),
        "has-session must be exact"
    );
    assert!(adapter.kill_session("tga-d").is_err());
    assert!(adapter.send_line("tga-d", "echo marker-8443").is_err());
    assert!(adapter.capture_output("tga-d", 20).is_err());
    assert!(adapter.get_pane_id("tga-d").is_err());

    assert_eq!(server.identity("tga-d-suffix"), Some(before));
    let screen = server
        .tmux(&["capture-pane", "-p", "-t", "=tga-d-suffix:"])
        .expect("capture the sibling");
    assert!(
        !screen.contains("marker-8443"),
        "keys landed in the sibling"
    );
}
