//! Integration test: `tm hook` relays reach the trusty-events bus as `message`
//! events (#9167).
//!
//! Why: the producer is only proven by the real path — stdin JSON → the built
//! `tm` binary → the daemon's `POST /hooks` ingest → the action-event
//! publisher → bytes on a bus socket — and the non-blocking rule is only
//! proven by running that path with the bus down.
//! What: serves a daemon state with a publisher attached on its socket, runs
//! `tm hook` with a `SendMessage` `PreToolUse` and with a pushed
//! `Notification`, flushes, and reads the frames a listening bus received.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_message_events_9167::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{HarnessEvent, HarnessPayload, MessageChannel, MessageEvent};
use trusty_mpm::core::hook::HookEvent;
use trusty_mpm::daemon::action_events::ActionPublisher;
use trusty_mpm::daemon::state::DaemonState;

const SESSION: &str = "7f3c0000-0000-4000-8000-000000009167";
const TEXT: &str = "[Ops Architect 08:0xZ] rebase onto main and report";

fn send_message_stdin() -> String {
    serde_json::json!({
        "session_id": SESSION,
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/work/my-app",
        "hook_event_name": "PreToolUse",
        "tool_name": "SendMessage",
        "tool_use_id": "toolu_9167",
        "tool_input": {"to": "a1b2c3", "message": TEXT},
    })
    .to_string()
}

fn notification_stdin() -> String {
    serde_json::json!({
        "session_id": SESSION,
        "cwd": "/work/my-app",
        "hook_event_name": "Notification",
        "notification_type": "idle_prompt",
        "message": "Claude is waiting for your input",
    })
    .to_string()
}

/// Run `tm hook` aimed at the daemon `url` locates, with `stdin` and the
/// inbox (if any) as the push target; return the output and the wall time.
fn run_hook(url: &str, home: &Path, stdin: &str, inbox: Option<&Path>) -> (Output, Duration) {
    let mut cmd = common::tm_command_in(home);
    common::socket::aim(&mut cmd, url);
    cmd.args(["hook"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_NOTIFY_INBOX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(inbox) = inbox {
        cmd.env("TRUSTY_MPM_NOTIFY_INBOX", inbox);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn `tm hook`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for tm hook");
    (out, start.elapsed())
}

/// A served daemon whose publisher dials `bus`.
struct Served {
    state: Arc<DaemonState>,
    publisher: Arc<ActionPublisher>,
    daemon: common::socket::DaemonSocket,
}

async fn serve(home: &Path, bus: PathBuf) -> Served {
    let state = Arc::new(DaemonState::with_root_isolated_managed(home.join("root")).await);
    let publisher = Arc::new(ActionPublisher::new(bus));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let daemon = common::socket::serve_state(state.clone()).await;
    Served {
        state,
        publisher,
        daemon,
    }
}

/// Run the hook off the async runtime, as Claude Code would, and require exit 0.
async fn hook(served: &Served, home: &Path, stdin: String, inbox: Option<PathBuf>) -> Duration {
    let (url, home) = (served.daemon.url.clone(), home.to_path_buf());
    let (out, took) =
        tokio::task::spawn_blocking(move || run_hook(&url, &home, &stdin, inbox.as_deref()))
            .await
            .expect("join");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    took
}

/// Flush `publisher` to a bus listening on `bus` and return the frames.
async fn flush_to_bus(publisher: &ActionPublisher, bus: &Path) -> Vec<HarnessEvent> {
    let listener = trusty_common::uds::bind_hardened(bus).expect("bind bus");
    let read = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    });
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("bus read within 10 s")
        .expect("join");
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

fn messages(frames: &[HarnessEvent]) -> Vec<MessageEvent> {
    frames
        .iter()
        .filter_map(|e| match &e.payload {
            HarnessPayload::Message(m) => Some(m.clone()),
            _ => None,
        })
        .collect()
}

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_message_hook_reaches_the_bus_as_one_message_9167() {
    let home = tempfile::tempdir().expect("home");
    let bus = home.path().join("bus.sock");
    let served = serve(home.path(), bus.clone()).await;
    hook(&served, home.path(), send_message_stdin(), None).await;

    let frames = flush_to_bus(&served.publisher, &bus).await;
    let sent = messages(&frames);
    assert_eq!(sent.len(), 1, "{frames:?}");
    assert_eq!(sent[0].channel, MessageChannel::SendMessage);
    assert_eq!(
        (sent[0].from.as_str(), sent[0].to.as_str()),
        (SESSION, "a1b2c3")
    );
    assert_eq!(sent[0].sha256, sha(TEXT));
    assert_eq!(sent[0].bytes, TEXT.len() as u64);
    assert_eq!(sent[0].body, None, "the body is off by default");
    let wire = serde_json::to_string(&frames).expect("serialize");
    assert!(
        !wire.contains("rebase onto main"),
        "the text reached the bus"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notification_push_reaches_the_bus_with_the_inbox_line_digest_9167() {
    let home = tempfile::tempdir().expect("home");
    let inbox = tempfile::tempdir().expect("inbox");
    let bus = home.path().join("bus.sock");
    let served = serve(home.path(), bus.clone()).await;
    hook(
        &served,
        home.path(),
        notification_stdin(),
        Some(inbox.path().to_path_buf()),
    )
    .await;

    let appended = std::fs::read_to_string(inbox.path().join("events.jsonl")).expect("inbox");
    let line = appended.strip_suffix('\n').expect("one newline-ended line");
    let pushed = messages(&flush_to_bus(&served.publisher, &bus).await);
    assert_eq!(pushed.len(), 1, "{pushed:?}");
    assert_eq!(pushed[0].channel, MessageChannel::HookPush);
    assert_eq!(pushed[0].to, inbox.path().display().to_string());
    assert_eq!(
        pushed[0].sha256,
        sha(line),
        "the digest is of the line sent"
    );
}

/// The bus being down costs the event, never the hook: `tm hook` exits 0
/// promptly and the daemon still records the relay.
#[tokio::test(flavor = "multi_thread")]
async fn a_down_bus_leaves_the_relay_hook_exit_zero_9167() {
    let home = tempfile::tempdir().expect("home");
    let served = serve(home.path(), home.path().join("no-bus.sock")).await;
    let took = hook(&served, home.path(), send_message_stdin(), None).await;
    assert!(took < Duration::from_secs(5), "hook took {took:?}");
    let recorded = served
        .state
        .recent_hook_events()
        .into_iter()
        .filter(|r| r.event == HookEvent::PreToolUse)
        .count();
    assert_eq!(recorded, 1, "the daemon must still ingest the relay");
    assert!(
        served.publisher.buffered_len() > 0,
        "the event waits, bounded"
    );
    assert!(
        served.publisher.flush().await.error.is_some(),
        "the bus is down"
    );
}
