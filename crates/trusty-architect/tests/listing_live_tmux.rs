//! Live-tmux checks for `list_panes` (#11543): pane discovery must not depend
//! on the caller's locale, and a missing server is an error, not zero panes.
//! Each test skips when no `tmux` binary is on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use trusty_architect::SystemTmux;
use trusty_architect::poller::{LIST_FORMAT, list_panes, parse_list_panes};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A per-test socket path, short enough for the unix socket limit.
fn socket(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ta-{tag}-{}.sock", std::process::id()))
}

/// Kills the throwaway server when the test ends, pass or fail.
struct Server(PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `tmux`'s absolute path, so a cleared environment can still spawn it.
fn tmux_path() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v tmux"])
        .output()
        .expect("resolve tmux");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

fn start_server(sock: &Path) -> Server {
    let status = Command::new("tmux")
        .arg("-S")
        .arg(sock)
        .args(["new-session", "-d", "-s", "probe", "sleep 60"])
        .status();
    assert!(status.is_ok_and(|s| s.success()), "tmux new-session failed");
    Server(sock.to_path_buf())
}

#[test]
fn list_panes_reads_rows_without_a_utf8_locale() {
    if !tmux_available() {
        eprintln!("skipping: no tmux on PATH");
        return;
    }
    let sock = socket("locale");
    let _server = start_server(&sock);
    // The exact command SystemTmux spawns, run with no locale at all.
    let mut cmd = SystemTmux::with_socket(&sock)
        .with_program(tmux_path())
        .command(&["list-panes", "-a", "-F", LIST_FORMAT]);
    let out = cmd.env_clear().output().expect("spawn tmux");
    assert!(out.status.success(), "list-panes failed: {out:?}");
    let listing = parse_list_panes(&String::from_utf8_lossy(&out.stdout));
    assert!(listing.unreadable.is_empty(), "{:?}", listing.unreadable);
    assert_eq!(listing.panes.len(), 1, "{listing:?}");
    assert_eq!(listing.panes[0].session, "probe");
}

#[test]
fn list_panes_on_a_missing_server_is_an_error() {
    if !tmux_available() {
        eprintln!("skipping: no tmux on PATH");
        return;
    }
    let sock = socket("missing");
    let _ = std::fs::remove_file(&sock);
    let got = list_panes(&SystemTmux::with_socket(&sock));
    assert!(got.is_err(), "expected a tmux fault, got {got:?}");
}
