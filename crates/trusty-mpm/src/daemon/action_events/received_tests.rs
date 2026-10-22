//! Tests for the receiver-side `message` events (#9167 PR2).
//!
//! Why: the receiving transcript is the only record of an arrival, so the
//! parser, the read positions and the count rule are each pinned here: one
//! arrival is one event, a rescan or a restart repeats none, the body is off
//! by default, a secret reaches no field, and N messages between two
//! sessions are N `sent` plus N `received` events with matching digests.
//! The fixture lines copy the shape of a live Claude Code transcript.
//! Test: this file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    HarnessEvent, HarnessPayload, MessageChannel, MessageDirection, MessageEvent,
};

use super::message::messages;
use super::received::{Job, Tails, arrival, read, received_event};
use super::{ActionPublisher, start_with};
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::daemon::DaemonState;

const FROM: &str = "uds:/tmp/cc-socks/77772.sock";

/// A flush interval no test outlives: the task exists but never drains.
const NO_TICK: Duration = Duration::from_secs(3600);

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn element(text: &str) -> String {
    format!(
        "<cross-session-message from=\"{FROM}\" from-name=\"tm-architect\" from-mode=\"bypass\">\n{text}\n</cross-session-message>"
    )
}

/// The lines one arrival writes: the enqueue, its dequeue, and the peer turn
/// that carries the same text again.
fn arrival_lines(session: &str, text: &str, at: DateTime<Utc>) -> String {
    let ts = stamp(at);
    let lines = [
        json!({"type": "queue-operation", "operation": "enqueue", "timestamp": ts,
               "sessionId": session, "content": element(text)}),
        json!({"type": "queue-operation", "operation": "dequeue", "timestamp": ts,
               "sessionId": session}),
        json!({"type": "user", "isMeta": true, "timestamp": ts, "sessionId": session,
               "origin": {"kind": "peer", "from": FROM, "name": "tm-architect", "body": text},
               "message": {"role": "user",
                           "content": format!("Another Claude session sent a message:\n{}", element(text))}}),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn append(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open transcript");
    f.write_all(text.as_bytes()).expect("append");
}

fn hook(session: SessionId, event: HookEvent, transcript: &Path) -> HookEventRecord {
    HookEventRecord::now(
        session,
        event,
        json!({"cwd": "/w", "tool": "Bash", "tool_use_id": "toolu_r1",
               "input": {"command": "ls"}, "transcript_path": transcript.display().to_string()}),
    )
}

fn send(session: SessionId, event: HookEvent, text: &str, i: usize) -> HookEventRecord {
    HookEventRecord::now(
        session,
        event,
        json!({"cwd": "/w", "tool": "SendMessage", "tool_use_id": format!("toolu_s{i}"),
               "input": {"to": "uds:/tmp/cc-socks/1011.sock", "message": text}}),
    )
}

fn job(path: &Path, since: DateTime<Utc>) -> Job {
    Job {
        path: path.to_path_buf(),
        session: SessionId::new(),
        offset: None,
        since,
        mid_line: false,
    }
}

async fn drain(publisher: &ActionPublisher, sock: &Path) -> Vec<HarnessEvent> {
    let listener = trusty_common::uds::bind_hardened(sock).expect("bind");
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

fn messages_in(frames: &[HarnessEvent]) -> Vec<(&HarnessEvent, &MessageEvent)> {
    frames
        .iter()
        .filter_map(|e| match &e.payload {
            HarnessPayload::Message(m) => Some((e, m)),
            _ => None,
        })
        .collect()
}

/// Read every bus connection on `sock` until one carries a `received`
/// message; returns all frames read. The caller bounds the wait.
fn until_received(sock: &Path) -> tokio::task::JoinHandle<Vec<HarnessEvent>> {
    let listener = trusty_common::uds::bind_hardened(sock).expect("bind");
    tokio::spawn(async move {
        let mut frames: Vec<HarnessEvent> = Vec::new();
        while !messages_in(&frames)
            .iter()
            .any(|(_, m)| m.direction == MessageDirection::Received)
        {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            conn.read_to_end(&mut buf).await.expect("read");
            let text = String::from_utf8_lossy(&buf);
            frames.extend(
                text.lines()
                    .map(|l| serde_json::from_str(l).expect("frame")),
            );
        }
        frames
    })
}

/// The `received` digests among `frames`.
fn received_digests(frames: &[HarnessEvent]) -> Vec<String> {
    messages_in(frames)
        .iter()
        .filter(|(_, m)| m.direction == MessageDirection::Received)
        .map(|(_, m)| m.sha256.clone())
        .collect()
}

/// Only the enqueue line is an arrival; its dequeue, the peer turn, a queued
/// prompt that is not a cross-session message, and noise are not.
#[test]
fn only_the_enqueue_line_is_an_arrival_9167() {
    let at = Utc::now();
    let text = "[Ops Architect 08:0xZ] rebase PR 10105\nsecond line";
    let lines = arrival_lines("s", text, at);
    let got: Vec<_> = lines.lines().filter_map(arrival).collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].from, FROM);
    assert_eq!(got[0].text, text);
    assert_eq!(stamp(got[0].at), stamp(at));
    let other = json!({"type": "queue-operation", "operation": "enqueue",
                       "timestamp": stamp(at), "content": "/compact"});
    let no_from = json!({"type": "queue-operation", "operation": "enqueue",
                         "timestamp": stamp(at),
                         "content": "<cross-session-message name=\"x\">\nhi\n</cross-session-message>"});
    for line in [
        other.to_string(),
        no_from.to_string(),
        "not json".into(),
        String::new(),
    ] {
        assert_eq!(arrival(&line), None, "{line}");
    }
}

/// A received message is one `received` `sendmessage` event from the
/// sender's socket to the receiving session, under its root, with the byte
/// count and sha256 of the text and no body by default.
#[tokio::test]
async fn a_received_message_is_one_received_event_with_no_body_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("b.jsonl");
    append(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
    );
    let publisher = Arc::new(ActionPublisher::new(dir.path().join("bus.sock")));
    let b = SessionId::new();
    publisher.publish(&hook(b, HookEvent::PreToolUse, &transcript));
    let text = "[Global Architect 00:46Z] run the critic, then send me the head";
    append(
        &transcript,
        &arrival_lines(
            &b.0.to_string(),
            text,
            Utc::now() + chrono::Duration::seconds(1),
        ),
    );
    publisher.publish(&hook(b, HookEvent::Stop, &transcript));
    assert_eq!(publisher.collect_received().await, 1);
    assert_eq!(
        publisher.collect_received().await,
        0,
        "nothing new, nothing repeated"
    );

    let frames = drain(&publisher, &dir.path().join("bus.sock")).await;
    let got = messages_in(&frames);
    assert_eq!(got.len(), 1, "{frames:?}");
    let (envelope, m) = got[0];
    assert_eq!(m.direction, MessageDirection::Received);
    assert_eq!(m.channel, MessageChannel::SendMessage);
    assert_eq!(m.from, FROM);
    assert_eq!(m.to, b.0.to_string());
    assert_eq!(m.bytes, text.len() as u64);
    assert_eq!(m.sha256, sha(text));
    assert_eq!(m.relay.as_deref(), Some("[Global Architect 00:46Z]"));
    assert_eq!(m.body, None, "the body is off by default");
    let root = frames.first().expect("the session's root").id;
    assert_eq!(envelope.parent_id, Some(root));
    assert_eq!(envelope.session, Some(b.0.to_string()));
}

/// A secret in a received message reaches no field; with bodies on the body
/// is the scanned text, capped like the sender's, and the digest equals the
/// sender's digest for the same text.
#[test]
fn a_secret_in_a_received_message_reaches_no_field_9167() {
    let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
    let raw = format!("[Ops Architect 08:0xZ] deploy with {token} then report");
    let lines = arrival_lines("s", &raw, Utc::now());
    let a = lines.lines().find_map(arrival).expect("arrival");
    let sent = messages(
        &send(SessionId::new(), HookEvent::PreToolUse, &raw, 0),
        false,
    );
    for bodies in [false, true] {
        let m = received_event(&a, "s", bodies);
        let wire = serde_json::to_string(&m).expect("serialize");
        assert!(!wire.contains(&token), "secret leaked: {wire}");
        assert_eq!(m.sha256, sent[0].sha256, "the two ends' digests match");
        assert_eq!(m.body.is_some(), bodies);
    }
    let body = received_event(&a, "s", true).body.expect("body");
    assert!(
        body.contains("REDACTED") && body.contains("then report"),
        "{body}"
    );
}

/// A rescan reads only new lines, and a restarted daemon (a fresh first look)
/// counts no arrival older than its first hook.
#[test]
fn rescanning_and_a_restart_never_repeat_an_arrival_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.jsonl");
    let start = Utc::now();
    append(
        &path,
        &arrival_lines("s", "first", start + chrono::Duration::seconds(1)),
    );

    let mut tails = Tails::default();
    let s = SessionId::new();
    let record = HookEventRecord {
        at: start,
        ..hook(s, HookEvent::PreToolUse, &path)
    };
    tails.note(&record);
    let jobs = tails.take_due();
    assert_eq!(jobs.len(), 1);
    let scan = read(&jobs[0]).expect("read");
    assert_eq!(scan.arrivals.len(), 1);
    tails.finish(&jobs[0], &scan);
    assert!(tails.take_due().is_empty(), "not due without a new hook");
    tails.note(&record);
    let again = tails.take_due();
    assert_eq!(read(&again[0]).expect("reread").arrivals, vec![]);

    // A restart: the first hook it sees is after the arrival.
    let after = start + chrono::Duration::seconds(5);
    assert_eq!(
        read(&job(&path, after)).expect("first look").arrivals,
        vec![]
    );
    append(
        &path,
        &arrival_lines("s", "second", after + chrono::Duration::seconds(1)),
    );
    let fresh = read(&job(&path, after)).expect("first look");
    let texts: Vec<_> = fresh.arrivals.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["second"]);
}

/// A line still being written is not read until it is complete, then once.
#[test]
fn a_partial_last_line_is_read_once_it_is_complete_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.jsonl");
    let since = Utc::now();
    let lines = arrival_lines("s", "whole", since + chrono::Duration::seconds(1));
    let enqueue = lines.lines().next().expect("enqueue line");
    let (head, tail) = enqueue.split_at(enqueue.len() / 2);
    append(&path, head);
    let first = read(&job(&path, since)).expect("read");
    assert_eq!(first.arrivals, vec![]);
    assert_eq!(first.offset, 0, "the partial line is left for later");
    append(&path, &format!("{tail}\n"));
    let next = Job {
        offset: Some(first.offset),
        mid_line: first.mid_line,
        ..job(&path, since)
    };
    let second = read(&next).expect("read");
    assert_eq!(second.arrivals.len(), 1);
    let done = Job {
        offset: Some(second.offset),
        ..next
    };
    assert_eq!(read(&done).expect("read").arrivals, vec![]);
}

/// The count rule: N messages from session A to session B are N `sent`
/// events (A's `PreToolUse`, not its `PostToolUse`) plus N `received`
/// events (B's transcript), however often B's hooks fire and the flush task
/// scans, and each received digest matches one sent digest.
#[tokio::test]
async fn n_messages_between_two_sessions_are_n_sent_and_n_received_9167() {
    const N: usize = 5;
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript: PathBuf = dir.path().join("b.jsonl");
    append(&transcript, "");
    let publisher = Arc::new(ActionPublisher::new(dir.path().join("bus.sock")));
    let (a, b) = (SessionId::new(), SessionId::new());
    publisher.publish(&hook(b, HookEvent::SessionStart, &transcript));
    for i in 0..N {
        let text = format!("[Ops Architect 08:{i}xZ] relay number {i}");
        publisher.publish(&send(a, HookEvent::PreToolUse, &text, i));
        publisher.publish(&send(a, HookEvent::PostToolUse, &text, i));
        append(
            &transcript,
            &arrival_lines(
                &b.0.to_string(),
                &text,
                Utc::now() + chrono::Duration::seconds(1),
            ),
        );
        publisher.publish(&hook(b, HookEvent::PreToolUse, &transcript));
        publisher.publish(&hook(b, HookEvent::PostToolUse, &transcript));
        publisher.collect_received().await;
    }
    publisher.publish(&hook(b, HookEvent::Stop, &transcript));
    publisher.collect_received().await;
    publisher.collect_received().await;

    let frames = drain(&publisher, &dir.path().join("bus.sock")).await;
    let got = messages_in(&frames);
    let digests = |d: MessageDirection| {
        let mut v: Vec<&str> = got
            .iter()
            .filter(|(_, m)| m.direction == d)
            .map(|(_, m)| m.sha256.as_str())
            .collect();
        v.sort_unstable();
        v
    };
    let sent = digests(MessageDirection::Sent);
    let received = digests(MessageDirection::Received);
    assert_eq!(sent.len(), N, "{got:?}");
    assert_eq!(received.len(), N, "{got:?}");
    assert_eq!(sent, received, "every received message matches a sent one");
}

/// The daemon's own flush task carries a received message to the bus: the
/// scan runs on the task's tick, with no test calling `collect_received`.
#[tokio::test]
async fn the_flush_task_carries_a_received_message_to_the_bus_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("b.jsonl");
    let sock = dir.path().join("bus.sock");
    let bus = until_received(&sock);
    let state = DaemonState::new();
    assert!(start_with(&state, true, Duration::from_millis(20), || Ok(
        sock.clone()
    )));
    let b = SessionId::new();
    let text = "[Global Architect 01:10Z] the flush task delivers this";
    append(
        &transcript,
        &arrival_lines(
            &b.0.to_string(),
            text,
            Utc::now() + chrono::Duration::seconds(1),
        ),
    );
    state.push_hook_event(hook(b, HookEvent::PreToolUse, &transcript));
    // No shutdown before the bus sees it: only the task's tick can scan.
    let frames = tokio::time::timeout(Duration::from_secs(5), bus)
        .await
        .expect("the flush task sent no received message within 5 s")
        .expect("join");
    assert_eq!(received_digests(&frames), [sha(text)], "{frames:?}");
    state.action_events().shutdown().await;
}

/// Fail-Open Check: an arrival written after the session's last hook and
/// still unread at shutdown is sent by shutdown's last scan, and a restarted
/// daemon does not send it again. Exactly once, never zero times.
#[tokio::test]
async fn an_arrival_unread_at_shutdown_is_sent_exactly_once_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("b.jsonl");
    append(&transcript, "");
    let sock = dir.path().join("bus.sock");
    let bus = until_received(&sock);
    let state = DaemonState::new();
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    let b = SessionId::new();
    state.push_hook_event(hook(b, HookEvent::PreToolUse, &transcript));
    let publisher = Arc::clone(state.action_events().publisher().expect("attached"));
    assert_eq!(publisher.collect_received().await, 0, "read to the end");
    // The arrival lands after the last hook, and no hook follows it.
    let text = "[Ops Architect 02:20Z] pending at shutdown";
    append(
        &transcript,
        &arrival_lines(&b.0.to_string(), text, Utc::now()),
    );
    state.action_events().shutdown().await;
    let frames = tokio::time::timeout(Duration::from_secs(5), bus)
        .await
        .expect("shutdown sent no received message within 5 s")
        .expect("join");
    assert_eq!(received_digests(&frames), [sha(text)], "{frames:?}");

    // A restart: its first hook for the session comes after the arrival.
    let restarted = Arc::new(ActionPublisher::new(dir.path().join("bus2.sock")));
    let later = HookEventRecord {
        at: Utc::now() + chrono::Duration::seconds(1),
        ..hook(b, HookEvent::Stop, &transcript)
    };
    restarted.publish(&later);
    assert_eq!(restarted.collect_received().await, 0, "never sent twice");
}

/// A scan the shutdown abort left running finishes after the final flush:
/// it buffers nothing (nothing would flush it) and moves no offset, while
/// shutdown's own scan has already sent the arrival once.
#[tokio::test]
async fn a_scan_that_outlives_shutdown_buffers_nothing_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("b.jsonl");
    append(&transcript, "");
    let sock = dir.path().join("bus.sock");
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(&sock));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let b = SessionId::new();
    state.push_hook_event(hook(b, HookEvent::PreToolUse, &transcript));
    let text = "[Ops Architect 03:30Z] read by a scan still in flight";
    append(
        &transcript,
        &arrival_lines(
            &b.0.to_string(),
            text,
            Utc::now() + chrono::Duration::seconds(1),
        ),
    );
    // The in-flight scan took its job before the abort.
    let in_flight = publisher.received.lock().take_due();
    assert_eq!(in_flight.len(), 1);
    let bus = until_received(&sock);
    state.action_events().shutdown().await;
    let frames = tokio::time::timeout(Duration::from_secs(5), bus)
        .await
        .expect("shutdown sent no received message within 5 s")
        .expect("join");
    assert_eq!(received_digests(&frames), [sha(text)], "{frames:?}");

    assert_eq!(
        publisher.scan_jobs(in_flight),
        0,
        "closed: nothing buffered"
    );
    assert_eq!(publisher.buffered_len(), 0);
}

/// #9167: a final scan held past half the shutdown budget logs the timeout
/// warning and still closes reading, and shutdown returns on time.
#[tokio::test]
async fn a_final_scan_past_its_budget_warns_and_still_closes_reading_9167() {
    use tracing_subscriber::layer::SubscriberExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("b.jsonl");
    append(&transcript, "");
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(dir.path().join("bus.sock")));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    state.push_hook_event(hook(SessionId::new(), HookEvent::PreToolUse, &transcript));

    // #4931: a thread-local capture needs the global level raised first.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let _capture = tracing::subscriber::set_default(tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    ));

    // Another thread holds the scan lock, so the final scan cannot start.
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let publisher = Arc::clone(&publisher);
        std::thread::spawn(move || {
            let _held = publisher.scanning.lock();
            locked_tx.send(()).expect("signal locked");
            // Returns on release or when the test drops the sender.
            let _ = release_rx.recv();
        })
    };
    locked_rx.recv().expect("holder took the scan lock");

    tokio::time::timeout(
        Duration::from_secs(5),
        state
            .action_events()
            .shutdown_within(Duration::from_millis(200)),
    )
    .await
    .expect("shutdown hung past 5 s with the scan lock held");

    let lines = buffer.tail(64);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("shutdown transcript scan outlived its budget")),
        "{lines:?}"
    );
    // The scan never took its job; only `close()` makes the due list empty.
    assert!(
        publisher.received.lock().take_due().is_empty(),
        "reading still open after the timed-out scan"
    );
    drop(release_tx);
    holder.join().expect("holder thread");
}
