//! Tests for the tui Focus hook across tmux servers (#10101, ruling Dc3).
//!
//! Every tmux call goes to a fake binary through `with_tmux_binary`, a
//! task-local override, so no test touches `$HOME` or the process env.

use std::path::{Path, PathBuf};

use trusty_mpm::core::tmux::with_tmux_binary;

use super::*;

const NAME: &str = "tm-a";

/// [`fake_tmux_holding`] where the target server holds the session.
fn fake_tmux(window: &str, code: i32, stderr: &str) -> (tempfile::TempDir, PathBuf) {
    fake_tmux_holding(window, code, stderr, true)
}

/// A scratch dir holding a fake tmux that logs each argv to `calls.log`.
/// `display-message` prints `window`; `new-window` prints `stderr` to stderr
/// and exits with `code`; a `-S <socket>` call (`has-session`) exits 0 only
/// when `holds`; every other call exits 0 and prints nothing.
fn fake_tmux_holding(
    window: &str,
    code: i32,
    stderr: &str,
    holds: bool,
) -> (tempfile::TempDir, PathBuf) {
    let held = if holds { 0 } else { 1 };
    let dir = tempfile::tempdir().expect("scratch dir");
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1\" in\n\
         display-message) printf '%s\\n' '{window}' ;;\n\
         new-window) echo '{stderr}' >&2; exit {code} ;;\n-S) exit {held} ;;\nesac\nexit 0\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (dir, bin)
}

/// Every argv the fake tmux saw, one per line; empty when it never ran.
fn calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
}

/// The `$TMUX` value of a client on the server at `<dir>/<server>`.
fn tmux_env(dir: &Path, server: &str) -> String {
    format!("{},4242,0", dir.join(server).display())
}

/// Run [`focus_with`] with every tmux call sent to `bin`.
async fn focus(
    bin: &Path,
    caller_tmux: Option<&str>,
    pane: Option<&str>,
    target: anyhow::Result<PathBuf>,
) -> FocusResult {
    with_tmux_binary(bin.to_path_buf(), async {
        focus_with(NAME, caller_tmux, pane, target)
    })
    .await
}

#[tokio::test]
async fn a_cross_server_focus_opens_the_attach_window() {
    let (dir, bin) = fake_tmux("@3", 0, "");
    let caller = tmux_env(dir.path(), "default");
    let target = dir.path().join("tm-ws-a");
    let result = focus(&bin, Some(&caller), Some("%7"), Ok(target.clone())).await;
    assert_eq!(result, FocusResult::Switched);
    let log = calls(dir.path());
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "three tmux calls: {log}");
    // The caller pane's window id, the target server holds the session, then
    // the window after the caller's (#10101: Dc3).
    assert_eq!(lines[0], "display-message -p -t %7 #{window_id}");
    assert_eq!(
        lines[1],
        format!("-S {} has-session -t =tm-a", target.display())
    );
    assert!(
        lines[2].starts_with("new-window -a -t @3 -n tm-a env -u TMUX -u TMUX_PANE "),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "-S '{}' attach-session -t '=tm-a'",
            target.display()
        )),
        "{log}"
    );
    assert!(!log.contains("switch-client"), "{log}");
}

#[tokio::test]
async fn a_same_server_focus_keeps_the_guarded_switch() {
    let (dir, bin) = fake_tmux("@3", 0, "");
    // A workspace server, and the default server (a record with no socket).
    for server in ["tm-ws-a", "default"] {
        let caller = tmux_env(dir.path(), server);
        let target = dir.path().join(server);
        let result = focus(&bin, Some(&caller), Some("%7"), Ok(target)).await;
        // No controlling tty under `cargo test`: the #2678 guard fails closed.
        assert_eq!(result, FocusResult::Unresolved, "{server}");
    }
    assert!(!calls(dir.path()).contains("new-window"));
}

#[tokio::test]
async fn a_cross_server_focus_without_a_pane_moves_nothing() {
    let (dir, bin) = fake_tmux("@3", 0, "");
    let caller = tmux_env(dir.path(), "default");
    let result = focus(&bin, Some(&caller), None, Ok(dir.path().join("tm-ws-a"))).await;
    assert_eq!(result, FocusResult::Unresolved);
    assert_eq!(calls(dir.path()), "", "no tmux call");
}

#[tokio::test]
async fn focus_with_no_caller_server_moves_nothing() {
    let (dir, bin) = fake_tmux("@3", 0, "");
    let target = dir.path().join("tm-ws-a");
    for caller in [None, Some(""), Some(",4242,0")] {
        let result = focus(&bin, caller, Some("%7"), Ok(target.clone())).await;
        assert!(
            matches!(&result, FocusResult::Failed(e) if e.contains("which tmux server")),
            "{caller:?}: {result:?}"
        );
    }
    assert_eq!(calls(dir.path()), "", "no tmux call");
}

#[tokio::test]
async fn focus_with_no_target_server_moves_nothing() {
    let (dir, bin) = fake_tmux("@3", 0, "");
    let caller = tmux_env(dir.path(), "default");
    let target = Err(anyhow::anyhow!("tmux session tm-a is on 2 servers"));
    let result = focus(&bin, Some(&caller), Some("%7"), target).await;
    assert!(
        matches!(&result, FocusResult::Failed(e) if e.contains("on 2 servers")),
        "{result:?}"
    );
    assert_eq!(calls(dir.path()), "", "no tmux call");
}

#[tokio::test]
async fn a_failed_attach_window_is_a_focus_failure() {
    let (dir, bin) = fake_tmux("@3", 1, "create window failed: index in use");
    let caller = tmux_env(dir.path(), "default");
    let target = dir.path().join("tm-ws-a");
    let result = focus(&bin, Some(&caller), Some("%7"), Ok(target)).await;
    assert!(
        matches!(&result, FocusResult::Failed(e) if e.contains("index in use")),
        "{result:?}"
    );
    assert!(calls(dir.path()).contains("\nnew-window -a -t @3 "));
}

#[tokio::test]
async fn a_caller_pane_with_no_window_moves_nothing() {
    let (dir, bin) = fake_tmux("", 0, "");
    let caller = tmux_env(dir.path(), "default");
    let target = dir.path().join("tm-ws-a");
    let result = focus(&bin, Some(&caller), Some("%7"), Ok(target)).await;
    assert_eq!(result, FocusResult::Unresolved);
    assert!(!calls(dir.path()).contains("new-window"));
}

#[tokio::test]
async fn focus_on_a_session_no_server_holds_moves_nothing() {
    // #10101: `target_socket` answers the default server for a session no
    // server holds; the caller is on a workspace server.
    let (dir, bin) = fake_tmux_holding("@3", 0, "", false);
    let caller = tmux_env(dir.path(), "tm-ws-a");
    let target = dir.path().join("default");
    let result = focus(&bin, Some(&caller), Some("%7"), Ok(target)).await;
    assert!(
        matches!(&result, FocusResult::Failed(e) if e.contains("not found on any tmux server")),
        "{result:?}"
    );
    assert!(!calls(dir.path()).contains("new-window"));
}
