//! Stop-and-restart tests for the action-event spool (#10412).
//!
//! Why: the 2026-10-29 restart lost 4096 buffered events because the bus was
//! down at stop. These tests stop a daemon state with the bus absent, hung
//! or back, and with a spool that works or cannot be written.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{EventId, HarnessEvent};

use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;

/// A flush interval no test outlives: only the final flush delivers.
const NO_TICK: Duration = Duration::from_secs(3600);

fn hook(session: SessionId, id: &str) -> HookEventRecord {
    HookEventRecord::now(
        session,
        HookEvent::PreToolUse,
        json!({"cwd": "/w", "tool": "Bash", "tool_use_id": id}),
    )
}

/// A daemon state started on `sock` with its spool in `spool`.
fn started(sock: &Path, spool: &Path) -> DaemonState {
    let state = DaemonState::new();
    let sock = sock.to_path_buf();
    assert!(start_with_spool(
        &state,
        true,
        NO_TICK,
        || Ok(sock),
        Some(spool.to_path_buf())
    ));
    state
}

/// Every complete event in `spool`, read without claiming.
fn spooled(spool: &Path) -> Vec<HarnessEvent> {
    let Ok(entries) = std::fs::read_dir(spool) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = entries
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    names.sort();
    names
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .expect("read")
                .lines()
                .map(|l| serde_json::from_str(l).expect("event"))
                .collect::<Vec<HarnessEvent>>()
        })
        .collect()
}

fn ids(events: &[HarnessEvent]) -> Vec<EventId> {
    events.iter().map(|e| e.id).collect()
}

/// The bus down at stop, as on 2026-10-29: nothing is lost, everything
/// buffered is in the spool, and the stop is prompt.
#[tokio::test]
async fn events_buffered_at_shutdown_with_the_bus_down_are_spooled_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool = dir.path().join("spool");
    let state = started(&dir.path().join("absent.sock"), &spool);
    let s = SessionId::new();
    for i in 0..50 {
        state.push_hook_event(hook(s, &format!("t{i}")));
    }
    let publisher = Arc::clone(state.action_events().publisher().expect("attached"));
    let buffered = publisher.buffered_len();
    assert_eq!(buffered, 51, "50 tool events and the inferred root");

    let begun = Instant::now();
    let settled = before_signal_exit_settled(&state).await;
    assert!(begun.elapsed() < SIGNAL_FLUSH_BUDGET + Duration::from_millis(150));
    assert_eq!(
        settled,
        Settled {
            sent: 0,
            spooled: buffered,
            lost: 0
        }
    );
    assert_eq!(publisher.buffered_len(), 0);
    assert_eq!(spooled(&spool).len(), buffered);
}

/// The signal path's settle, as `before_signal_exit` runs it.
async fn before_signal_exit_settled(state: &DaemonState) -> Settled {
    state
        .action_events()
        .shutdown_within(SIGNAL_FLUSH_BUDGET)
        .await
}

/// A hung bus and a spool path that is a file: the stop still ends inside
/// the signal budget, and every event is reported lost by count.
#[tokio::test]
async fn a_hung_bus_and_an_unwritable_spool_stop_in_bound_and_count_the_loss_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let spool = dir.path().join("not-a-dir");
    std::fs::write(&spool, b"x").expect("block the spool path");
    let state = started(&sock, &spool);
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(hook(s, &format!("t{i}")));
    }
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });

    let begun = Instant::now();
    let settled = before_signal_exit_settled(&state).await;
    let took = begun.elapsed();
    hold.abort();
    assert!(
        took < SIGNAL_FLUSH_BUDGET + Duration::from_millis(150),
        "the stop waited {took:?}"
    );
    assert_eq!(settled.spooled, 0);
    assert!(settled.lost > 0, "{settled:?}");
    assert_eq!(settled.sent + settled.lost, 3_001, "{settled:?}");
}

/// Spooled at one stop, sent at the next start, and never sent again.
#[tokio::test]
async fn spooled_events_replay_on_the_next_start_exactly_once_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool = dir.path().join("spool");
    let sock = dir.path().join("bus.sock");

    let first = started(&sock, &spool);
    first.push_hook_event(hook(SessionId::new(), "t1"));
    let settled = first.action_events().shutdown().await;
    assert_eq!(settled.spooled, 2, "{settled:?}");
    let expected = ids(&spooled(&spool));

    // The bus is back for the second run.
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let read = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    });
    let second = started(&sock, &spool);
    assert!(spooled(&spool).is_empty(), "the replay claimed the spool");
    let settled = second.action_events().shutdown().await;
    assert_eq!((settled.sent, settled.lost), (2, 0), "{settled:?}");
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("bus read")
        .expect("join");
    let got: Vec<HarnessEvent> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect();
    assert_eq!(ids(&got), expected);

    let third = started(&sock, &spool);
    let publisher = third.action_events().publisher().expect("attached");
    assert_eq!(publisher.buffered_len(), 0, "replayed a second time");
    third.action_events().shutdown().await;
}

/// A torn last line is skipped and counted; the complete lines replay.
#[test]
fn a_torn_spool_line_is_skipped_and_the_rest_replay_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool_dir = dir.path().join("spool");
    let s = SessionId::new();
    let publisher = ActionPublisher::new(dir.path().join("absent.sock"));
    publisher.publish(&hook(s, "t1"));
    let events = publisher.client.drain_buffered();
    assert_eq!(spool::persist(&spool_dir, &events).expect("persist"), 2);
    let file = std::fs::read_dir(&spool_dir)
        .expect("dir")
        .next()
        .expect("one file")
        .expect("entry")
        .path();
    let mut text = std::fs::read_to_string(&file).expect("read");
    text.push_str("{\"torn\":");
    std::fs::write(&file, text).expect("tear");

    let replay = spool::take_all(&spool_dir).expect("take");
    assert_eq!(ids(&replay.events), ids(&events));
    assert_eq!((replay.skipped, replay.failed_files), (1, 0));
    assert!(
        spool::take_all(&spool_dir)
            .expect("again")
            .events
            .is_empty()
    );
}
