//! Integration test: a tainted managed session's new Claude id reads tainted
//! at SessionStart, even when the daemon cannot re-apply the taint (#8433 P0,
//! owner ruling D09).
//!
//! Why: D09 fixes the outcome — a failed re-apply fails the SessionStart hook
//! call AND the egress guard denies for the new id. Only the real binaries
//! prove both halves: `tm hook` on the SessionStart, then `tm hook
//! --egress-guard` on the new id's next path tool call.
//! What: an isolated daemon (home pinned to `<root>/home`) serves its socket;
//! one Active managed session works in `ws` and is tainted under that home.
//! The built `tm` runs with `HOME=<root>/home` for both hooks. Cases: the
//! daemon re-applies (hook exit 0, egress deny); the daemon's own write is
//! forced to fail through the in-process test seam (hook exit 1, egress
//! deny); no daemon answers at all (hook exit 1, egress deny).
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_session_start_taint_8433::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use trusty_mpm::core::channels_taint::{self, TaintMark};
use trusty_mpm::daemon::state::DaemonState;

/// The loopback URL that makes `tm hook` pick the socket; nothing listens.
const LOOPBACK: &str = "http://127.0.0.1:1";
const OLD: &str = "550e8400-e29b-41d4-a716-446655448433";
const NEW: &str = "550e8400-e29b-41d4-a716-446655448434";

/// An isolated daemon, one tainted Active managed session in `ws`; the home.
async fn tainted_daemon(root: &Path, ws: &Path) -> (Arc<DaemonState>, PathBuf) {
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await);
    let mgr = state.session_manager().await;
    let rec = mgr
        .create(
            "taint e2e".into(),
            Some(ws.to_path_buf()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    mgr.stop(&rec.id).await.expect("stop");
    mgr.resume(&rec.id).await.expect("resume");
    let home = state.user_home().expect("pinned home");
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: std::path::PathBuf::from("/"),
    };
    let dir = channels_taint::taint_dir(&home);
    channels_taint::taint_managed_session(&dir, &rec.id.to_string(), OLD, &mark).expect("taint");
    (state, home)
}

/// Serve the daemon socket from `state` until the sender fires.
async fn serve_socket(state: Arc<DaemonState>, socket: &Path) -> tokio::sync::oneshot::Sender<()> {
    let bound = trusty_mpm::daemon::socket::bind(socket)
        .await
        .expect("bind socket");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(trusty_mpm::daemon::socket::serve_until_shutdown(
        bound,
        state,
        async {
            let _ = shutdown.await;
        },
    ));
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop
}

/// Run `tm` under `home` with `args`, `stdin` piped in.
fn run_tm(
    home: &Path,
    args: &[&str],
    socket: &Path,
    cwd: &Path,
    stdin: &serde_json::Value,
) -> Output {
    let mut cmd = common::tm_command_in(home);
    cmd.args(args)
        .current_dir(cwd)
        .env("TRUSTY_MPM_SOCKET", socket)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn tm");
    let _ = child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.to_string().as_bytes());
    child.wait_with_output().expect("wait")
}

/// `tm hook` with a SessionStart for `id` in `ws`.
fn session_start(home: &Path, socket: &Path, ws: &Path, id: &str) -> Output {
    let payload = serde_json::json!({
        "session_id": id,
        "hook_event_name": "SessionStart",
        "source": "resume",
        "cwd": ws.display().to_string(),
    });
    run_tm(home, &["--url", LOOPBACK, "hook"], socket, ws, &payload)
}

/// The egress guard's verdict on a `Read` from session `id`: its stdout.
fn egress_read(home: &Path, socket: &Path, ws: &Path, id: &str) -> String {
    let payload = serde_json::json!({
        "session_id": id,
        "hook_event_name": "PreToolUse",
        "cwd": ws.display().to_string(),
        "tool_name": "Read",
        "tool_input": { "file_path": ws.join("notes.md").display().to_string() },
    });
    let out = run_tm(
        home,
        &["--url", LOOPBACK, "hook", "--egress-guard"],
        socket,
        ws,
        &payload,
    );
    assert!(out.status.success(), "the egress guard exits 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Assert `stdout` is a deny from a taint file that existed BEFORE the guard ran.
fn assert_taint_deny(stdout: &str, case: &str) {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{case}: no deny JSON ({e}): {stdout:?}"));
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{case}: {value}"
    );
    let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    // #9923: the lazy path says "now channel-tainted" and never prints the
    // state, so only a taint file that existed before the guard ran matches.
    assert!(
        reason.contains("channel-tainted (Tainted(Present))"),
        "{case}: the taint file must pre-exist, not be written lazily: {reason}"
    );
}

/// Run one blocking `tm` step off the runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("join")
}

/// #8433 (Dfc): the daemon re-applies; the hook succeeds and the new id is denied.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(channels_taint)]
async fn session_start_reapplied_by_the_daemon_exits_zero_and_egress_denies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("ws");
    let (state, home) = tainted_daemon(&dir.path().join("root"), &ws).await;
    let socket = dir.path().join("mpm.sock");
    let stop = serve_socket(Arc::clone(&state), &socket).await;

    let (h, s, w) = (home.clone(), socket.clone(), ws.clone());
    let out = blocking(move || session_start(&h, &s, &w, NEW)).await;
    let _ = stop.send(());
    assert!(out.status.success(), "the hook succeeds: {out:?}");
    let stdout = blocking(move || egress_read(&home, &socket, &ws, NEW)).await;
    assert_taint_deny(&stdout, "re-applied by the daemon");
}

/// 🔴 #8433 (D09): the daemon-side write is forced to fail; the SessionStart
/// hook fails and the next egress verdict for the new id is deny.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(channels_taint)]
async fn reapply_failure_fails_session_start_hook_and_egress_denies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("ws");
    let (state, home) = tainted_daemon(&dir.path().join("root"), &ws).await;
    let socket = dir.path().join("mpm.sock");
    let stop = serve_socket(Arc::clone(&state), &socket).await;

    // The in-process daemon's taint writes fail; the spawned `tm` is unaffected.
    channels_taint::inject_write_failure_for_test(true);
    let (h, s, w) = (home.clone(), socket.clone(), ws.clone());
    let out = blocking(move || session_start(&h, &s, &w, NEW)).await;
    channels_taint::inject_write_failure_for_test(false);
    let _ = stop.send(());

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the SessionStart hook call fails: {out:?}"
    );
    assert!(
        stderr.contains("re-apply"),
        "the failure is named: {stderr}"
    );
    let stdout = blocking(move || egress_read(&home, &socket, &ws, NEW)).await;
    assert_taint_deny(&stdout, "daemon-side write failed");
}

/// 🔴 #8433 (D09): no daemon answers the SessionStart; the hook fails and the
/// next egress verdict for the new id is deny.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(channels_taint)]
async fn reapply_daemon_unreachable_at_session_start_egress_denies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).expect("ws");
    let (_state, home) = tainted_daemon(&dir.path().join("root"), &ws).await;
    let socket = dir.path().join("nobody.sock");

    let (h, s, w) = (home.clone(), socket.clone(), ws.clone());
    let out = blocking(move || session_start(&h, &s, &w, NEW)).await;
    assert!(
        !out.status.success(),
        "the SessionStart hook call fails: {out:?}"
    );
    let stdout = blocking(move || egress_read(&home, &socket, &ws, NEW)).await;
    assert_taint_deny(&stdout, "daemon unreachable");
}
