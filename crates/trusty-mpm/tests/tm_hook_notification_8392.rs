//! Integration test: `tm hook` on a real `Notification` payload (#8392).
//!
//! Why: the forward and the daemon ingest are only proven by the real path —
//! stdin JSON → the built `tm` binary → the real `POST /hooks` router and its
//! `HookPost` deserialize — not by a capture stub.
//! What: serves `daemon::api::router` on a loopback port, runs `tm --url <it>
//! hook` with a Claude Code `Notification` payload, and asserts the daemon
//! recorded a `Notification` and the configured inbox got one line. The
//! failure paths — no target, a stuck target, a down daemon, a `TMUX_PANE`
//! that is not a `%N` pane id — each exit 0.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_notification_8392::`.

use crate::common;

use std::future::IntoFuture;
use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use trusty_mpm::core::hook::HookEvent;
use trusty_mpm::daemon::api;
use trusty_mpm::daemon::state::DaemonState;

const PAYLOAD: &str = r#"{"session_id":"7f3c0000-0000-4000-8000-000000000001","transcript_path":"/tmp/t.jsonl","cwd":"/work/my-app","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#;

/// Serve the real router on an ephemeral loopback port; return its base URL.
async fn serve(state: Arc<DaemonState>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, api::router(state)).into_future());
    format!("http://{addr}")
}

/// Run `tm --url <url> hook` with [`PAYLOAD`] on stdin, `inbox` (if any) as
/// the push target and `pane` (if any) as `TMUX_PANE`; return the output and
/// the wall time.
fn run_hook(
    url: &str,
    home: &Path,
    inbox: Option<&Path>,
    pane: Option<&str>,
) -> (Output, Duration) {
    let mut cmd = common::tm_command_in(home);
    cmd.args(["--url", url, "hook"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(inbox) = inbox {
        cmd.env("TRUSTY_MPM_NOTIFY_INBOX", inbox);
    }
    if let Some(pane) = pane {
        cmd.env("TMUX_PANE", pane);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn `tm hook`");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(PAYLOAD.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait for tm hook");
    (out, start.elapsed())
}

fn forward_failures(out: &Output) -> usize {
    String::from_utf8_lossy(&out.stderr)
        .matches("Notification forward failed")
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notification_reaches_the_daemon_and_the_inbox() {
    let home = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(home.path().join("root")).await);
    let url = serve(state.clone()).await;

    let (out, _) = tokio::task::spawn_blocking({
        let (url, home, inbox) = (
            url.clone(),
            home.path().to_path_buf(),
            inbox.path().to_path_buf(),
        );
        move || run_hook(&url, &home, Some(&inbox), None)
    })
    .await
    .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(forward_failures(&out), 0);
    let recorded: Vec<_> = state
        .recent_hook_events()
        .into_iter()
        .filter(|r| r.event == HookEvent::Notification)
        .collect();
    assert_eq!(recorded.len(), 1, "the daemon must ingest one Notification");

    let text = std::fs::read_to_string(inbox.path().join("events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["type"], "permission_prompt");
    assert_eq!(line["session_id"], "7f3c0000-0000-4000-8000-000000000001");
    assert_eq!(line["project"], "my-app");
}

#[test]
fn an_unset_target_forwards_nothing_and_logs_nothing() {
    let home = tempfile::tempdir().unwrap();
    let (out, _) = run_hook("http://127.0.0.1:9", home.path(), None, None);
    assert!(out.status.success());
    assert_eq!(forward_failures(&out), 0);
}

/// A FIFO with no reader never answers the open; the hook still exits 0 at the
/// 1 s forward bound, with one logged failure.
#[test]
fn a_stuck_inbox_costs_one_logged_failure_and_exit_zero() {
    let home = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let fifo = inbox.path().join("events.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let (out, took) = run_hook("http://127.0.0.1:9", home.path(), Some(inbox.path()), None);
    assert!(out.status.success());
    assert_eq!(
        forward_failures(&out),
        1,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(took < Duration::from_secs(5), "hook took {took:?}");
}

/// The daemon being down costs the POST, never the hook or the forward.
#[test]
fn a_down_daemon_does_not_block_the_hook() {
    let home = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (out, took) = run_hook(
        &format!("http://127.0.0.1:{port}"),
        home.path(),
        Some(inbox.path()),
        None,
    );
    assert!(out.status.success());
    assert!(took < Duration::from_secs(5), "hook took {took:?}");
    assert!(inbox.path().join("events.jsonl").exists());
}

/// #8392: a `TMUX_PANE` that is not a `%N` pane id is dropped — it never
/// reaches `tmux -t`, where tmux would prefix-match it — and the hook still
/// forwards and exits 0.
#[test]
fn a_non_pane_id_tmux_pane_is_dropped_and_the_hook_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    for bad in ["main", "s:0", "=foo", "%", "%12a"] {
        let inbox = tempfile::tempdir().unwrap();
        let (out, _) = run_hook(
            "http://127.0.0.1:9",
            home.path(),
            Some(inbox.path()),
            Some(bad),
        );
        assert!(out.status.success(), "{bad}");
        assert_eq!(forward_failures(&out), 0, "{bad}");
        let text = std::fs::read_to_string(inbox.path().join("events.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["tmux_pane"], serde_json::Value::Null, "{bad}");
        assert_eq!(line["tmux_session"], serde_json::Value::Null, "{bad}");
    }
}
