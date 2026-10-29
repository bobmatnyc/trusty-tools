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
    assert!(spooled(&spool).is_empty(), "the replay took the spool");
    // A hook in the new run goes out after the replayed events.
    second.push_hook_event(hook(SessionId::new(), "t2"));
    let settled = second.action_events().shutdown().await;
    assert_eq!((settled.sent, settled.lost), (4, 0), "{settled:?}");
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("bus read")
        .expect("join");
    let got: Vec<HarnessEvent> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect();
    assert_eq!(got.len(), 4, "two replayed, then root and started");
    assert_eq!(ids(&got[..2]), expected, "the replay goes first");

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

/// SIGTERM runs the signal handler's settle and the serve drain's at once:
/// both get the one result, and the spool holds each event once.
#[tokio::test]
async fn concurrent_settles_account_for_every_event_once_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool = dir.path().join("spool");
    let state = started(&dir.path().join("absent.sock"), &spool);
    let s = SessionId::new();
    for i in 0..50 {
        state.push_hook_event(hook(s, &format!("t{i}")));
    }
    let events = state.action_events();
    let begun = Instant::now();
    let (drain, signal) = tokio::join!(
        events.shutdown(),
        events.shutdown_within(SIGNAL_FLUSH_BUDGET)
    );
    assert!(begun.elapsed() < FINAL_FLUSH_BUDGET + Duration::from_millis(150));
    assert_eq!(drain, signal, "two settles split the buffer");
    assert_eq!(
        drain,
        Settled {
            sent: 0,
            spooled: 51,
            lost: 0
        }
    );
    assert_eq!(spooled(&spool).len(), 51, "each event spooled once");
}

/// A caller whose budget ends while another settle is still at the bus
/// returns on time and counts every event still at risk as lost.
#[tokio::test]
async fn a_settle_waiter_out_of_budget_counts_what_is_at_risk_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let state = started(&sock, &dir.path().join("spool"));
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
    let publisher = Arc::clone(state.action_events().publisher().expect("attached"));
    let first = Arc::clone(&publisher);
    let settling = tokio::spawn(async move { first.final_flush_within(FINAL_FLUSH_BUDGET).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let begun = Instant::now();
    let waiter = publisher.final_flush_within(SIGNAL_FLUSH_BUDGET).await;
    let took = begun.elapsed();
    assert!(
        took < SIGNAL_FLUSH_BUDGET + Duration::from_millis(150),
        "{took:?}"
    );
    assert_eq!(
        waiter.spooled, 0,
        "the waiter settled on its own: {waiter:?}"
    );
    assert!(waiter.lost > 0, "nothing counted at risk: {waiter:?}");

    let settled = settling.await.expect("join");
    hold.abort();
    assert_eq!(
        settled.sent + settled.spooled + settled.lost,
        3_001,
        "{settled:?}"
    );
}

/// A hook after the stop drained the buffer is counted, not stranded.
#[tokio::test]
async fn an_event_after_the_settle_is_counted_lost_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = started(&dir.path().join("absent.sock"), &dir.path().join("spool"));
    state.action_events().shutdown().await;
    let publisher = state.action_events().publisher().expect("attached");
    state.push_hook_event(hook(SessionId::new(), "late"));
    assert_eq!(publisher.buffered_len(), 0, "stranded in a settled buffer");
    assert_eq!(publisher.late_lost(), 2, "root and started");
}

/// A spool file another live process holds is left to it; once that holder
/// is gone without deleting it (a taker that died), it replays once.
#[test]
fn a_spool_file_held_by_a_live_taker_is_left_alone_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool_dir = dir.path().join("spool");
    let publisher = ActionPublisher::new(dir.path().join("absent.sock"));
    publisher.publish(&hook(SessionId::new(), "t1"));
    let events = publisher.client.drain_buffered();
    spool::persist(&spool_dir, &events).expect("persist");
    let file = std::fs::read_dir(&spool_dir)
        .expect("dir")
        .next()
        .expect("one file")
        .expect("entry")
        .path();

    let holder = std::fs::File::open(&file).expect("open");
    holder.lock().expect("lock");
    let replay = spool::take_all(&spool_dir).expect("take");
    assert!(replay.events.is_empty(), "took a held file");
    assert_eq!(replay.held, 1);
    assert!(file.exists());

    drop(holder);
    let replay = spool::take_all(&spool_dir).expect("take");
    assert_eq!(ids(&replay.events), ids(&events));
    assert!(
        spool::take_all(&spool_dir)
            .expect("again")
            .events
            .is_empty()
    );
}

/// A temp file a stopped write left is swept once past the grace period; a
/// fresh one, or one a live writer holds, is kept.
#[test]
fn a_stale_unlocked_temp_file_is_swept_and_a_live_one_kept_10412() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool_dir = dir.path().join("spool");
    std::fs::create_dir_all(&spool_dir).expect("dir");
    let abandoned = spool_dir.join(".a.ndjson.tmp");
    let writing = spool_dir.join(".b.ndjson.tmp");
    std::fs::write(&abandoned, b"{").expect("abandoned");
    std::fs::write(&writing, b"{").expect("writing");
    let writer = std::fs::File::open(&writing).expect("open");
    writer.lock().expect("lock");

    let now = std::time::SystemTime::now();
    let fresh = spool::take_all_at(&spool_dir, now).expect("take");
    assert_eq!(fresh.swept_tmp, 0, "swept inside the grace period");
    assert!(abandoned.exists());

    let later = now + spool::TMP_GRACE * 2;
    let stale = spool::take_all_at(&spool_dir, later).expect("take");
    assert_eq!(stale.swept_tmp, 1);
    assert!(!abandoned.exists(), "the abandoned temp file stayed");
    assert!(writing.exists(), "removed a file a live writer holds");
    drop(writer);
}
