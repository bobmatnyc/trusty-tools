//! Socket-level tests for the hook-event producer (#6849).
//!
//! Why: DOC-73 §4.1's non-blocking invariant and §3.2's by-reference rule are
//! claims about bytes on a socket and time on the hook path, so they are
//! proven against a real listener: an absent bus, a stalled one, an
//! unresolvable one, and the raw frames a working one receives.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    ActionEvent, CallPhase, DEFAULT_PUSH_BUFFER_CAPACITY, HarnessEvent, HarnessPayload,
    PushFlushOutcome,
};

use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;

const RECORDED: &str = include_str!("../../../testdata/hook_session_6849.ndjson");

/// A flush interval no test outlives: the task exists but never drains.
const NO_TICK: Duration = Duration::from_secs(3600);

fn sock_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("bus.sock")
}

/// A daemon state with a publisher aimed at `sock` and no flush task.
fn attached(sock: &Path) -> (DaemonState, Arc<ActionPublisher>) {
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(sock).with_home(Some("/Users/dev".into())));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    (state, publisher)
}

fn pre_tool_use(session: SessionId, id: &str) -> HookEventRecord {
    HookEventRecord::now(
        session,
        HookEvent::PreToolUse,
        json!({"cwd": "/w", "tool": "Bash", "tool_use_id": id, "input": {"command": "ls"}}),
    )
}

/// Accept one connection on `sock` and return every byte it carried.
fn read_one(sock: &Path) -> tokio::task::JoinHandle<Vec<u8>> {
    let listener = trusty_common::uds::bind_hardened(sock).expect("bind");
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    })
}

/// What `read_one` collected, bounded so a red run fails rather than hangs.
async fn received(read: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("nothing reached the bus within 10 s")
        .expect("join")
}

fn frames(bytes: &[u8]) -> Vec<HarnessEvent> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

/// A hook pushed through the daemon's one ingest point reaches the bus as
/// typed actions joined by `parent_id`.
#[tokio::test]
async fn hook_events_reach_the_bus_with_a_parent_chain_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "toolu_9"));
    state.push_hook_event(HookEventRecord::now(
        s,
        HookEvent::PostToolUse,
        json!({"tool": "Bash", "tool_use_id": "toolu_9"}),
    ));
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(outcome.sent > 0, "nothing was buffered to send");
    let got = frames(&received(read).await);
    assert_eq!(got.len(), 3, "inferred root, started, finished: {got:?}");
    assert_eq!(got[0].parent_id, None);
    assert_eq!(got[1].parent_id, Some(got[0].id));
    assert_eq!(got[2].parent_id, Some(got[1].id));
    let phases: Vec<_> = got[1..]
        .iter()
        .map(|e| match &e.payload {
            HarnessPayload::Action(ActionEvent::Tool { phase, call_id, .. }) => {
                assert_eq!(call_id, "toolu_9");
                *phase
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(phases, [CallPhase::Started, CallPhase::Finished]);
    assert!(got.windows(2).all(|w| w[0].seq < w[1].seq), "seq rises");
    // The ring buffer still records the hook as before.
    assert_eq!(state.hook_events_for(s).len(), 2);
}

/// Fail-soft, absent bus: 10,000 hooks stay cheap, the buffer holds its
/// bound, and the overflow is counted rather than queued.
#[test]
fn ten_thousand_hooks_with_no_bus_are_fast_and_bounded_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, publisher) = attached(&sock_in(&dir));
    let s = SessionId::new();
    let started = Instant::now();
    for i in 0..10_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    // 5 s, not 1 s: opt-level 0 on shared CI runners (as #9078).
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(publisher.buffered_len(), DEFAULT_PUSH_BUFFER_CAPACITY);
    // 10,000 tool events plus the inferred session root.
    assert_eq!(
        publisher.dropped(),
        (10_001 - DEFAULT_PUSH_BUFFER_CAPACITY) as u64
    );
}

/// Fail-soft, stalled bus: a flush stuck on a peer that never reads does not
/// slow the hook path, and the flush itself gives up within its timeout.
#[tokio::test]
async fn a_stalled_bus_never_slows_hook_ingest_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("fill{i}")));
    }
    let p = Arc::clone(&publisher);
    let flush = tokio::spawn(async move { p.flush().await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    for i in 0..1_000 {
        state.push_hook_event(pre_tool_use(s, &format!("live{i}")));
    }
    let ingest = started.elapsed();
    let outcome: PushFlushOutcome = flush.await.expect("join");
    hold.abort();
    assert!(
        outcome.error.is_some(),
        "the peer never read, so the flush stalls"
    );
    assert!(
        ingest < Duration::from_secs(2),
        "hook ingest waited on the bus: {ingest:?}"
    );
    assert!(publisher.buffered_len() > 0, "unsent events stay buffered");
}

/// Fail-soft, unresolvable bus: no publisher, a warning, and hooks are
/// recorded exactly as before.
#[tokio::test]
async fn an_unresolvable_bus_still_ingests_hooks_6849() {
    // The control: the same call with a resolvable socket does attach, so
    // the `false` below is the error arm, not a publisher that never attaches.
    let dir = tempfile::tempdir().expect("tempdir");
    let control = DaemonState::new();
    assert!(
        start_with(&control, true, NO_TICK, || Ok(sock_in(&dir))),
        "a resolvable bus attaches"
    );
    control.action_events().shutdown().await;

    let state = DaemonState::new();
    assert!(!start_with(&state, true, NO_TICK, || anyhow::bail!(
        "no data directory"
    )));
    assert!(state.action_events().publisher().is_none());
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "t"));
    assert_eq!(state.hook_events_for(s).len(), 1);
    state.action_events().shutdown().await;
}

/// A daemon isolated from the host (a scratch root or `$HOME`) whose data dir
/// is the host's must not dial the host's bus; the resolver is never called.
#[tokio::test]
async fn a_scratch_daemon_dials_no_host_bus_6849() {
    assert!(!bus_allowed(false, false));
    assert!(
        bus_allowed(true, false),
        "an overridden data dir is its own bus"
    );
    assert!(bus_allowed(false, true), "the operator's daemon");
    assert!(bus_allowed(true, true));
    let state = DaemonState::new();
    let called = AtomicBool::new(false);
    let attached = start_with(&state, false, NO_TICK, || {
        called.store(true, Ordering::SeqCst);
        Ok(PathBuf::from("/nonexistent/bus.sock"))
    });
    assert!(!attached);
    assert!(!called.load(Ordering::SeqCst));
}

/// Every exit path delivers what is buffered: `shutdown` ends the flush task
/// and runs the final flush, and a second call (the binary's signal handler
/// and the serve drain both call it) is a prompt no-op.
#[tokio::test]
async fn shutdown_runs_the_final_flush_once_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    // The flush task never ticks here, so only the final flush can deliver.
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "t1"));
    let read = read_one(&sock);
    state.action_events().shutdown().await;
    let got = frames(&received(read).await);
    assert_eq!(got.len(), 2, "root and started: {got:?}");
    let again = Instant::now();
    state.action_events().shutdown().await;
    assert!(again.elapsed() < Duration::from_millis(500));
}

/// The final flush is bounded even when the bus accepts and never reads.
#[tokio::test]
async fn final_flush_is_bounded_when_the_bus_stalls_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    assert!(publisher.buffered_len() > 1_000);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let started = Instant::now();
    publisher.final_flush().await;
    hold.abort();
    assert!(
        started.elapsed() < FINAL_FLUSH_BUDGET + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

/// Privacy, raw bytes: the recorded session's frames, as written to the
/// socket, hold no prompt, tool input or response, transcript path, message
/// or absolute home path.
#[tokio::test]
async fn projected_frames_carry_no_payload_bytes_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    for line in RECORDED.lines() {
        let record: HookEventRecord = serde_json::from_str(line).expect("fixture");
        state.push_hook_event(record);
    }
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(outcome.sent > 0, "nothing was buffered to send");
    let bytes = received(read).await;
    assert_eq!(frames(&bytes).len(), 21);
    for needle in [
        "SENTINEL",
        "/Users/dev",
        "transcript",
        "\"input\"",
        "\"prompt\"",
        "tool_response",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "{needle} reached the bus"
        );
    }
    // Every frame is a line of JSON carrying a domain the bus knows.
    for line in String::from_utf8_lossy(&bytes).lines() {
        let v: Value = serde_json::from_str(line).expect("json");
        assert!(matches!(
            v["payload"]["domain"].as_str(),
            Some("action" | "hook")
        ));
    }
}

/// One warning per outage, one notice on recovery, drops rate-limited.
#[test]
fn flush_health_warns_once_per_outage_6849() {
    let mut health = FlushHealth::default();
    let now = Instant::now();
    let down = || PushFlushOutcome {
        sent: 0,
        error: Some(trusty_common::uds::UdsRpcError::Timeout {
            path: PathBuf::from("/x"),
            timeout: Duration::from_millis(1),
        }),
    };
    let up = PushFlushOutcome {
        sent: 3,
        error: None,
    };
    assert_eq!(health.report(&down(), 0, now), [Notice::Down]);
    assert!(health.report(&down(), 0, now).is_empty());
    assert_eq!(health.report(&up, 0, now), [Notice::Recovered]);
    assert_eq!(health.report(&up, 5, now), [Notice::Dropped(5)]);
    assert!(
        health
            .report(&up, 9, now + Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(
        health.report(&up, 9, now + DROP_WARN_EVERY),
        [Notice::Dropped(9)]
    );
}

/// #6849 fix round: a hung bus holds tm's SIGTERM path for at most
/// [`SIGNAL_FLUSH_BUDGET`], so `process::exit` still beats the serve drain's
/// `mgr.shutdown()` (#6288). The peer accepts and never reads, so the flush
/// stalls mid-write; the budget is what bounds it.
#[tokio::test]
async fn the_signal_path_flush_returns_within_its_budget_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let started = Instant::now();
    before_signal_exit(&state, || {}).await;
    let took = started.elapsed();
    hold.abort();
    assert!(
        took < SIGNAL_FLUSH_BUDGET + Duration::from_millis(150),
        "the signal path waited {took:?} on a hung bus"
    );
}

/// #6849 fix round: the lock file goes only after the flush, so a spawner
/// never sees "no daemon" while this one still serves its socket.
#[tokio::test]
async fn the_signal_path_flushes_before_removing_the_lock_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    state.push_hook_event(pre_tool_use(SessionId::new(), "t1"));
    let read = read_one(&sock);
    let publisher = Arc::clone(state.action_events().publisher().expect("attached"));
    let mut buffered_at_removal = None;
    before_signal_exit(&state, || {
        buffered_at_removal = Some(publisher.buffered_len());
    })
    .await;
    assert_eq!(
        buffered_at_removal,
        Some(0),
        "the lock went before the flush"
    );
    assert_eq!(frames(&received(read).await).len(), 2);
}
