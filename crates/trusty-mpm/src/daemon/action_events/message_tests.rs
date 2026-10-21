//! Tests for the `message` events the hook projection emits (#9167).
//!
//! Why: the body policy, the digest rule and the relay recognisers are claims
//! a wrong implementation could pass silently, so each one is pinned here: the
//! body is absent by default, the sha256 covers the text as sent onto the bus
//! (after the credential scan), a secret never reaches any field, a call is
//! counted once, and a bus that is down never fails or slows the hook path.
//! Test: this file.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    ActionEvent, CallPhase, HarnessEvent, HarnessPayload, MessageChannel, MessageEvent,
};

use super::message::{MAX_BODY_BYTES, messages, relay_marker};
use super::projection::Threader;
use super::tmux_send::{SendKeys, send_keys_calls};
use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::core::standalone::hooks::notification::PUSHED_MESSAGE_FIELD;

const RELAY: &str = "[Ops Architect 08:0xZ] rebase PR 10105 onto main";

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn record(session: SessionId, event: HookEvent, payload: Value) -> HookEventRecord {
    HookEventRecord::now(session, event, payload)
}

fn send_message(session: SessionId, event: HookEvent, text: &str) -> HookEventRecord {
    record(
        session,
        event,
        json!({"cwd": "/w", "tool": "SendMessage", "tool_use_id": "toolu_m1",
               "input": {"to": "a1b2c3", "message": text}}),
    )
}

fn bash(session: SessionId, command: &str) -> HookEventRecord {
    record(
        session,
        HookEvent::PreToolUse,
        json!({"cwd": "/w", "tool": "Bash", "tool_use_id": "toolu_b1",
               "input": {"command": command}}),
    )
}

fn only(mut got: Vec<MessageEvent>) -> MessageEvent {
    assert_eq!(got.len(), 1, "{got:?}");
    got.remove(0)
}

/// A `SendMessage` relay is one `sendmessage` event naming its sender and
/// recipient, with the byte count and sha256 of the text, and no body.
#[test]
fn a_send_message_relay_is_one_message_event_9167() {
    let s = SessionId::new();
    let m = only(messages(
        &send_message(s, HookEvent::PreToolUse, RELAY),
        false,
    ));
    assert_eq!(m.channel, MessageChannel::SendMessage);
    assert_eq!(m.from, s.0.to_string());
    assert_eq!(m.to, "a1b2c3");
    assert_eq!(m.bytes, RELAY.len() as u64);
    assert_eq!(m.sha256, sha(RELAY));
    assert_eq!(m.relay.as_deref(), Some("[Ops Architect 08:0xZ]"));
    assert_eq!(m.body, None, "the body is off by default");
}

/// The same call seen again at `PostToolUse` (or failing) adds no second
/// event, and through the projection the one message hangs off its call.
#[test]
fn a_relay_is_counted_once_and_parented_by_its_call_9167() {
    let s = SessionId::new();
    for later in [HookEvent::PostToolUse, HookEvent::PostToolUseFailure] {
        assert!(messages(&send_message(s, later, RELAY), false).is_empty());
    }
    let mut threader = Threader::new(None);
    let mut events = threader.project(&send_message(s, HookEvent::PreToolUse, RELAY));
    events.extend(threader.project(&send_message(s, HookEvent::PostToolUse, RELAY)));
    let started = events
        .iter()
        .find(|e| {
            matches!(
                &e.payload,
                HarnessPayload::Action(ActionEvent::Tool {
                    phase: CallPhase::Started,
                    ..
                })
            )
        })
        .expect("tool started");
    let sent: Vec<&HarnessEvent> = events
        .iter()
        .filter(|e| matches!(e.payload, HarnessPayload::Message(_)))
        .collect();
    assert_eq!(sent.len(), 1, "{events:?}");
    assert_eq!(sent[0].parent_id, Some(started.id));
    assert_eq!(sent[0].session, Some(s.0.to_string()));
}

/// A Bash relay into another pane is a `tmux` event naming the pane; the
/// follow-up `Enter` types no text and adds nothing.
#[test]
fn a_tmux_send_keys_relay_names_its_pane_9167() {
    let s = SessionId::new();
    let command = format!(
        "tmux send-keys -t =dev-pm: -l '{RELAY}' && sleep 1 && tmux send-keys -t =dev-pm: Enter"
    );
    let m = only(messages(&bash(s, &command), false));
    assert_eq!(m.channel, MessageChannel::Tmux);
    assert_eq!(m.to, "=dev-pm:");
    assert_eq!(m.sha256, sha(RELAY));
    assert_eq!(m.relay.as_deref(), Some("[Ops Architect 08:0xZ]"));
    assert!(messages(&bash(s, "ls -la && git status"), false).is_empty());
}

/// The parser follows tmux's own argument rules and counts only a real call.
#[test]
fn send_keys_text_follows_tmux_argument_rules_9167() {
    let call = |target: &str, text: &str| SendKeys {
        target: Some(target.to_string()),
        text: text.to_string(),
    };
    let cases: &[(&str, Vec<SendKeys>)] = &[
        // Arguments are typed back to back; a key name is a key, not text.
        (
            "tmux send-keys -t s hello world Enter",
            vec![call("s", "helloworld")],
        ),
        ("tmux -L sock send -t s 'a b' C-m", vec![call("s", "a b")]),
        // `-l` makes every argument literal; flags may be combined.
        (
            "TMUX= tmux send-keys -lt=x: Enter",
            vec![call("=x:", "Enter")],
        ),
        (
            "tmux send-keys -t s hi \\; send-keys -t s Enter",
            vec![call("s", "hi")],
        ),
        ("/opt/bin/tmux send-keys -t p -- -n", vec![call("p", "-n")]),
        // Not a send-keys call, or no text typed.
        ("echo \"tmux send-keys -t s hi\"", vec![]),
        ("git commit -m 'tmux send-keys -t s hi'", vec![]),
        ("tmux send-keys -X -t s copy-selection", vec![]),
        ("tmux send-keys -t s C-c", vec![]),
        ("tmux list-panes -a", vec![]),
    ];
    for (command, want) in cases {
        assert_eq!(&send_keys_calls(command), want, "{command}");
    }
    let untargeted = send_keys_calls("tmux send-keys hi");
    assert_eq!(untargeted.len(), 1);
    assert_eq!(untargeted[0].target, None);
}

/// The fleet's token shapes, built at run time so no literal token sits in
/// the source: GitHub classic and fine-grained, Slack bot/refresh/app, AWS
/// long-term and STS.
fn fleet_tokens() -> Vec<String> {
    vec![
        format!("ghp_{}", "A1b2C3d4E5".repeat(4)),
        format!("github_pat_{}", "A1b2C3d4E5_".repeat(4)),
        format!("xoxb-{}", "1234-A1b2C3".repeat(3)),
        format!("xoxe-1-{}", "A1B2C3D4E5".repeat(3)),
        format!("xapp-1-{}", "A1B2C3D4E5".repeat(3)),
        format!("AKIA{}", "Q7R2".repeat(4)),
        format!("ASIA{}", "Q7R2".repeat(4)),
    ]
}

/// A secret in the text reaches no field, for every fleet token shape. The
/// digest and byte count describe the scanned text the event would carry as
/// its body, never the raw text.
#[test]
fn a_secret_in_the_text_never_reaches_any_field_9167() {
    for token in fleet_tokens() {
        secret_reaches_no_field(&token);
    }
}

fn secret_reaches_no_field(token: &str) {
    let raw = format!("deploy with {token} then report");
    let s = SessionId::new();
    let off = only(messages(
        &send_message(s, HookEvent::PreToolUse, &raw),
        false,
    ));
    let on = only(messages(
        &send_message(s, HookEvent::PreToolUse, &raw),
        true,
    ));
    for m in [&off, &on] {
        let wire = serde_json::to_string(m).expect("serialize");
        assert!(!wire.contains(token), "secret leaked: {wire}");
    }
    assert_ne!(off.sha256, sha(&raw), "never a digest of the raw text");
    let body = on.body.clone().expect("enabled body");
    assert!(
        body.contains("deploy with") && body.contains("REDACTED"),
        "{body}"
    );
    assert_eq!(off.sha256, sha(&body), "the digest is of the text as sent");
    assert_eq!(off.bytes, body.len() as u64);
    assert_eq!(off.body, None);

    let tmux = format!("tmux send-keys -t =p: -l 'key {token}'");
    let wire = serde_json::to_string(&messages(&bash(s, &tmux), true)).expect("ser");
    assert!(!wire.contains(token), "secret leaked via tmux: {wire}");
}

/// The body rides only when enabled, and is cut at [`MAX_BODY_BYTES`] while
/// the byte count and digest still cover the whole text.
#[test]
fn the_body_is_carried_only_when_enabled_9167() {
    let s = SessionId::new();
    let r = send_message(s, HookEvent::PreToolUse, RELAY);
    assert_eq!(only(messages(&r, false)).body, None);
    assert_eq!(only(messages(&r, true)).body.as_deref(), Some(RELAY));

    let long = "x".repeat(MAX_BODY_BYTES * 2);
    let m = only(messages(
        &send_message(s, HookEvent::PreToolUse, &long),
        true,
    ));
    assert_eq!(m.body.map(|b| b.len()), Some(MAX_BODY_BYTES));
    assert_eq!(m.bytes, long.len() as u64);
    assert_eq!(m.sha256, sha(&long));
}

/// The relay marker lands in `relay` only when it starts the trimmed first
/// line, as `relay.sh` and `relay-marker.py` read it; text without one, or
/// with one later in the text, leaves it empty.
#[test]
fn a_relay_marker_is_found_or_left_empty_9167() {
    let found = [
        ("[Ops Architect 22:2xZ] go", "[Ops Architect 22:2xZ]"),
        ("  [Architect 08:15Z] ack", "[Architect 08:15Z]"),
        (
            "[Dev Fleet supervisor 23:59Z] x",
            "[Dev Fleet supervisor 23:59Z]",
        ),
    ];
    for (text, marker) in found {
        assert_eq!(relay_marker(text).as_deref(), Some(marker), "{text}");
    }
    for text in [
        "ack [Architect 08:15Z]",
        "quoting the PM:\n[Ops Architect 22:21Z] go",
        "no marker here",
        "[Ops Architect 2:1Z]",
        "[Ops Architect 22:21]",
    ] {
        assert_eq!(relay_marker(text), None, "{text}");
    }
}

/// The marker is read from the raw text: a line-start marker is found even
/// when the credential scan rewrites part of its first line (the `Bearer`
/// rule spans the marker's name here), and the secret is still masked.
#[test]
fn a_line_start_marker_is_read_from_the_raw_text_9167() {
    let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
    let raw = format!("[Bearer Ops Architect 08:0xZ] rotate {token}");
    let m = only(messages(
        &send_message(SessionId::new(), HookEvent::PreToolUse, &raw),
        true,
    ));
    assert_eq!(m.relay.as_deref(), Some("[Bearer Ops Architect 08:0xZ]"));
    let wire = serde_json::to_string(&m).expect("serialize");
    assert!(!wire.contains(&token), "secret leaked: {wire}");
}

/// A completed Notification push is a `hook_push` event to the inbox, whose
/// digest is of the line appended; a Notification with no push adds nothing.
#[test]
fn a_notification_push_is_a_hook_push_message_9167() {
    let s = SessionId::new();
    let line = r#"{"type":"idle_prompt","display":"[app]: waiting"}"#;
    let pushed = record(
        s,
        HookEvent::Notification,
        json!({"message": "waiting", PUSHED_MESSAGE_FIELD: {"to": "/srv/inbox", "text": line}}),
    );
    let m = only(messages(&pushed, false));
    assert_eq!(m.channel, MessageChannel::HookPush);
    assert_eq!(m.to, "/srv/inbox");
    assert_eq!(m.sha256, sha(line));
    let quiet = record(s, HookEvent::Notification, json!({"message": "waiting"}));
    assert!(messages(&quiet, false).is_empty());
}

fn relay_record(s: SessionId, i: usize) -> HookEventRecord {
    send_message(s, HookEvent::PreToolUse, &format!("{RELAY} #{i}"))
}

/// A relay reaches a real bus as a `message` frame that carries no text.
#[tokio::test]
async fn a_relay_reaches_the_bus_without_its_text_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(&sock));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    state.push_hook_event(send_message(SessionId::new(), HookEvent::PreToolUse, RELAY));

    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
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
    let text = String::from_utf8_lossy(&bytes);
    let frames: Vec<HarnessEvent> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect();
    let sent = frames
        .iter()
        .filter(|e| e.payload.domain() == "message")
        .count();
    assert_eq!(sent, 1, "{text}");
    assert!(!text.contains("onto main"), "message text reached the bus");
}

/// A bus that is down never fails or slows the hook path: every relay is
/// still recorded, quickly, and only the flush reports the outage.
#[tokio::test]
async fn a_down_bus_never_fails_or_slows_a_relay_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(dir.path().join("absent.sock")));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let s = SessionId::new();
    let started = Instant::now();
    for i in 0..1_000 {
        state.push_hook_event(relay_record(s, i));
    }
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "{took:?}");
    // 1,000 is under the ring buffer's 1,024 bound, so every relay is kept.
    assert_eq!(state.hook_events_for(s).len(), 1_000);
    // Root, then a started and a message per relay.
    assert_eq!(publisher.buffered_len() as u64 + publisher.dropped(), 2_001);
    assert!(publisher.flush().await.error.is_some(), "the bus is down");
}
