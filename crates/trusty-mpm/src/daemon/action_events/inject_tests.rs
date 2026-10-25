//! The daemon's inject producer: one `tmux` `message` event per delivered
//! inject, on the hook path's buffer (#9167 AC5).
//!
//! Why: AC5's count match needs every daemon-typed text on the bus, with the
//! same scan and digest as the hook producers, and a bus that is down must
//! neither fail the inject nor lose the event silently.
//! What: a real `ActionPublisher` installed on a manager over the no-op tmux
//! driver; frames are read off a real listener.
//! Test: this file.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    HarnessEvent, HarnessPayload, MessageChannel, MessageDirection, MessageEvent,
};

use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::idle_nudge::{IdleNudgeConfig, NudgeLedger};
use crate::core::session::SessionId;
use crate::core::sm::control::Submit;
use crate::daemon::action_events::ActionPublisher;
use crate::daemon::idle_nudge::{NudgeOutcome, run_nudge};
use crate::daemon::state::DaemonState;
use crate::session_manager::inject_record::Delivery;
use crate::session_manager::{FakeNoopTmuxDriver, ManagedSessionId, SessionManager};

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// An Active session correlated to `claude_id`, as `idle_nudge`'s tests
/// build one: create, stop, resume under the no-op driver.
async fn active(
    dir: &tempfile::TempDir,
    claude_id: &str,
) -> (Arc<SessionManager>, ManagedSessionId) {
    let mgr = SessionManager::new(&dir.path().join("sm"), Arc::new(FakeNoopTmuxDriver))
        .await
        .expect("manager");
    let rec = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    mgr.stop(&rec.id).await.expect("stop");
    mgr.resume(&rec.id).await.expect("resume");
    mgr.set_claude_session_id(&rec.id, claude_id)
        .await
        .expect("claude id");
    (Arc::new(mgr), rec.id)
}

/// A daemon state over `mgr` with a publisher on `sock` and its recorder
/// installed.
async fn installed(mgr: &Arc<SessionManager>, sock: &Path) -> (DaemonState, Arc<ActionPublisher>) {
    let state = DaemonState::with_session_manager(Arc::clone(mgr));
    let publisher = Arc::new(ActionPublisher::new(sock));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    assert!(install(&state).await, "the recorder installs");
    (state, publisher)
}

/// Flush `publisher` into a listener on `sock` and return the frames.
async fn flushed(publisher: &ActionPublisher, sock: &Path) -> Vec<HarnessEvent> {
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
        .expect("nothing reached the bus within 10 s")
        .expect("join");
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

/// The `message` payloads among `frames`, with their events.
fn messages(frames: &[HarnessEvent]) -> Vec<(&HarnessEvent, &MessageEvent)> {
    frames
        .iter()
        .filter_map(|e| match &e.payload {
            HarnessPayload::Message(m) => Some((e, m)),
            _ => None,
        })
        .collect()
}

/// One delivery is one `tmux` `sent` event from the daemon to the session's
/// tmux name, with the hook producers' digest; the body only when enabled.
#[test]
fn an_inject_is_one_tmux_message_event_with_its_digest_9167() {
    let delivery = Delivery {
        at: Utc::now(),
        tmux_name: "tm-proj-01",
        claude_session_id: Some("claude-1"),
        text: "[Ops Architect 12:3xZ] resume",
    };
    let event = inject_event(&delivery, false, None);
    assert_eq!(event.session.as_deref(), Some("claude-1"));
    let HarnessPayload::Message(m) = &event.payload else {
        panic!("{:?}", event.payload);
    };
    assert_eq!(m.channel, MessageChannel::Tmux);
    assert_eq!(m.direction, MessageDirection::Sent);
    assert_eq!(
        (m.from.as_str(), m.to.as_str()),
        (INJECT_FROM, "tm-proj-01")
    );
    assert_eq!(m.bytes, delivery.text.len() as u64);
    assert_eq!(m.sha256, sha(delivery.text));
    assert_eq!(m.relay.as_deref(), Some("[Ops Architect 12:3xZ]"));
    assert_eq!(m.body, None);
    let with_body = inject_event(&delivery, true, None);
    let HarnessPayload::Message(m) = &with_body.payload else {
        panic!("{:?}", with_body.payload);
    };
    assert_eq!(m.body.as_deref(), Some(delivery.text));
}

/// A daemon inject reaches the bus once, a child of the receiving Claude
/// session's root; the `Interrupt` beside it adds nothing.
#[tokio::test]
async fn an_inject_reaches_the_bus_under_its_session_root_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let claude = SessionId::new();
    let (mgr, id) = active(&dir, &claude.0.to_string()).await;
    let (state, publisher) = installed(&mgr, &sock).await;
    state.push_hook_event(HookEventRecord::now(
        claude,
        HookEvent::SessionStart,
        json!({"cwd": "/w"}),
    ));

    mgr.inject(&id, "hello", Submit::Enter)
        .await
        .expect("inject");
    mgr.inject(&id, "", Submit::Interrupt)
        .await
        .expect("interrupt");

    let frames = flushed(&publisher, &sock).await;
    let found = messages(&frames);
    assert_eq!(found.len(), 1, "{frames:?}");
    let (event, m) = found[0];
    assert_eq!(event.parent_id, Some(frames[0].id), "a child of the root");
    assert_eq!(
        event.session.as_deref(),
        Some(claude.0.to_string().as_str())
    );
    assert_eq!(m.sha256, sha("hello"));
}

/// Fail-open check: with the bus down, the inject still succeeds and its
/// state advances, and the event stays buffered for the next flush rather
/// than vanishing; nothing is counted dropped.
#[tokio::test]
async fn an_unreachable_bus_keeps_the_inject_event_buffered_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("absent.sock");
    let (mgr, id) = active(&dir, "claude-1").await;
    let (_state, publisher) = installed(&mgr, &sock).await;
    let before = mgr.get(&id).await.expect("record").last_activity_at;

    mgr.inject(&id, "hello", Submit::Enter)
        .await
        .expect("inject");

    let after = mgr.get(&id).await.expect("record").last_activity_at;
    assert!(after > before, "{before:?} -> {after:?}");
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_some(), "no bus listens: {outcome:?}");
    assert_eq!(publisher.buffered_len(), 1, "kept for the next flush");
    assert_eq!(publisher.dropped(), 0);
    let frames = flushed(&publisher, &sock).await;
    assert_eq!(messages(&frames).len(), 1, "delivered once the bus is up");
}

/// No attached publisher, no recorder: the inject records nowhere.
#[tokio::test]
async fn install_needs_an_attached_publisher_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mgr, _id) = active(&dir, "claude-1").await;
    let state = DaemonState::with_session_manager(Arc::clone(&mgr));
    assert!(!install(&state).await);
    let publisher = Arc::new(ActionPublisher::new(dir.path().join("bus.sock")));
    assert!(mgr.install_inject_recorder(publisher));
}

/// The idle nudge, one of AC5's callers, is one `message` event carrying
/// the nudge text's digest.
#[tokio::test]
async fn an_idle_nudge_is_one_message_event_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let claude = uuid::Uuid::new_v4().to_string();
    let (mgr, _id) = active(&dir, &claude).await;
    let (_state, publisher) = installed(&mgr, &sock).await;
    let cfg = IdleNudgeConfig {
        enabled: true,
        max_nudges_per_session: 2,
        cooldown_secs: 300,
        message: "resume in the foreground now".into(),
    };
    let ledger = parking_lot::Mutex::new(NudgeLedger::new());

    let outcome = run_nudge(
        &mgr,
        &ledger,
        &[],
        &cfg,
        &claude,
        Some("i'll wait for the"),
        Utc::now(),
    )
    .await;

    assert_eq!(outcome, NudgeOutcome::Nudged);
    let frames = flushed(&publisher, &sock).await;
    let found = messages(&frames);
    assert_eq!(found.len(), 1, "{frames:?}");
    assert_eq!(found[0].1.sha256, sha(&cfg.message));
    assert_eq!(found[0].1.to, mgr.list().await[0].tmux_name);
}
