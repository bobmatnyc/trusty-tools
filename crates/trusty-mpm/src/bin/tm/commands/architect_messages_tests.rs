//! Tests for the poller's `poll_wake` producer (#9167 PR2).
//!
//! Why: the wake runs through the real `trusty_architect` send path against a
//! scripted tmux, and the event reaches a real socket, so the count, the byte
//! count and the digest are checked against what was actually typed.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use trusty_architect::poller::{Pane, PollState, StoredCount, Waker};
use trusty_architect::{FakeClock, PaneId, ScriptedTmux};
use trusty_common::control_bus::{HarnessEvent, HarnessPayload, MessageChannel, MessageDirection};

use super::*;

const RULE: &str = "────────────";
const ALERTS: &str = "/srv/inbox/alerts.jsonl";

fn screen(text: &str) -> String {
    format!("{RULE}\n❯\u{a0}{text}\n{RULE}\n")
}

/// The one `tm-architect` pane, `%1`, active.
fn panes() -> Vec<Pane> {
    vec![Pane {
        session: "tm-architect".to_owned(),
        pane: PaneId::parse("%1").expect("pane id"),
        cmd: "claude".to_owned(),
        active: "11".to_owned(),
        path: String::new(),
        dead: "0".to_owned(),
    }]
}

/// What a dry run would type, so the scripted box can show it after typing.
fn pointer() -> String {
    let tmux = ScriptedTmux::new().fallback("capture-pane", &screen(""));
    let clock = FakeClock::new(1_760_000_000.5);
    let dry = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new(ALERTS),
        dry_run: true,
        messages: &NoMessages,
    };
    let out = dry.wake(&panes(), 3, None).expect("dry run");
    out.strip_prefix("dry-run: would send to %1: ")
        .expect("dry-run phrase")
        .to_owned()
}

/// One real wake through `sink`: the phrase, the state and the typed literal.
fn wake_once(sink: &dyn MessageSink) -> (String, PollState, String) {
    let msg = pointer();
    let tmux = ScriptedTmux::new()
        .fallback("display-message", "tm-architect\n")
        .fallback("send-keys", "")
        .script("capture-pane", &screen(""))
        .script("capture-pane", &screen(&msg))
        .fallback("capture-pane", &screen(""));
    let clock = FakeClock::new(1_760_000_000.5);
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: Path::new(ALERTS),
        dry_run: false,
        messages: sink,
    };
    let mut state = PollState::default();
    let phrase = waker.dispatch(&mut state, &panes(), 3, None);
    let typed = tmux
        .sends()
        .into_iter()
        .filter_map(|argv| argv.last().cloned())
        .find(|last| last != "Enter")
        .expect("a literal was typed");
    (phrase, state, typed)
}

fn frames(bytes: &[u8]) -> Vec<HarnessEvent> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

async fn listen(sock: PathBuf) -> tokio::task::JoinHandle<Vec<u8>> {
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    })
}

/// A wake the poller submitted reaches the bus as exactly one `poll_wake`
/// `message` event, sent by the poller to the Architect's session, whose byte
/// count and sha256 are those of the literal typed into the pane, with no
/// body by default.
#[tokio::test(flavor = "multi_thread")]
async fn a_poller_wake_is_one_poll_wake_event_with_its_bytes_and_sha256_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let read = listen(sock.clone()).await;
    let (phrase, state, typed) = tokio::task::spawn_blocking(move || {
        let sink = BusMessages::new(&sock, "tm-architect-poll", false);
        wake_once(&sink)
    })
    .await
    .expect("join");
    assert_eq!(phrase, "sent to %1");
    assert_eq!(state.wake_pending, Some(StoredCount::Count(0)));
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("bus read within 10 s")
        .expect("join");
    let got = frames(&bytes);
    assert_eq!(got.len(), 1, "{got:?}");
    let HarnessPayload::Message(m) = &got[0].payload else {
        panic!("not a message: {got:?}");
    };
    assert_eq!(m.channel, MessageChannel::PollWake);
    assert_eq!(m.direction, MessageDirection::Sent);
    assert_eq!(m.from, "tm-architect-poll");
    assert_eq!(m.to, "tm-architect");
    assert!(typed.starts_with("[poll 08:53Z] 3 new alerts"), "{typed}");
    assert_eq!(m.bytes, typed.len() as u64);
    assert_eq!(m.sha256, format!("{:x}", Sha256::digest(typed.as_bytes())));
    assert_eq!(m.body, None, "the body is off by default");
}

/// Fail-Open Check: with the bus down the wake is still typed and submitted
/// and its count cleared; the phrase the poller logs names the failure; the
/// one event is kept for the next flush, never duplicated.
#[test]
fn a_down_bus_never_stops_the_wake_and_records_it_once_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sink = BusMessages::new(dir.path().join("absent.sock"), "tm-architect-poll", false);
    let (phrase, state, _) = wake_once(&sink);
    assert!(
        phrase.starts_with("sent to %1 (message event not recorded: trusty-events unreachable"),
        "{phrase}"
    );
    assert_eq!(state.wake_pending, Some(StoredCount::Count(0)));
    assert_eq!(sink.buffered_len(), 1, "one wake, one buffered event");
}

/// A bus that cannot be resolved reports every wake as not recorded rather
/// than dropping it silently.
#[test]
fn an_unresolvable_bus_reports_every_wake_as_not_recorded_9167() {
    let sink = NoBus("no trusty-events socket: no data dir".to_owned());
    let (phrase, state, _) = wake_once(&sink);
    assert_eq!(
        phrase,
        "sent to %1 (message event not recorded: no trusty-events socket: no data dir)"
    );
    assert_eq!(state.wake_pending, Some(StoredCount::Count(0)));
}
