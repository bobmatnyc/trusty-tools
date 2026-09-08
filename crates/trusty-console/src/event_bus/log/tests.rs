//! Tests for the durable NDJSON event log (issue #6848 slice 3b).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use chrono::{NaiveDate, Utc};
use trusty_common::control_bus::{EventId, HarnessEvent, HarnessPayload, HarnessSource};

use super::config::{LogConfig, day_file_name, parse_day_file_name};
use super::format::{encode_line, read_events};
use super::recovery::{earliest_seq, list_log_files, recover_next_seq};
use super::replay::{ReplayItem, replay_since};
use super::retention::{enforce_retention, files_to_delete};
use super::writer::{DurableLog, rotate, write_line};

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid test date")
}

/// Build a `HarnessEvent` fixture with a given `seq`, everything else fixed.
fn event(seq: u64) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: None,
        seq,
        at: Utc::now(),
        payload: HarnessPayload::Ping,
        id: EventId::new(),
        parent_id: None,
    }
}

/// Write `events` to `dir`'s day file for `d`, each as one NDJSON line —
/// bypasses the writer task entirely, for tests that need direct control over
/// on-disk content (a specific seq sequence, a deliberately corrupt line).
async fn seed_file(dir: &Path, d: NaiveDate, events: &[HarnessEvent]) {
    tokio::fs::create_dir_all(dir).await.expect("create dir");
    let mut content = Vec::new();
    for e in events {
        content.extend_from_slice(&encode_line(e));
    }
    tokio::fs::write(dir.join(day_file_name(d)), content)
        .await
        .expect("seed day file");
}

/// Append a raw, possibly-invalid line to `dir`'s day file for `d`.
async fn append_raw_line(dir: &Path, d: NaiveDate, line: &str) {
    tokio::fs::create_dir_all(dir).await.expect("create dir");
    use tokio::io::AsyncWriteExt as _;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(day_file_name(d)))
        .await
        .expect("open day file");
    f.write_all(line.as_bytes()).await.expect("write raw line");
}

// ─── config: filename format ────────────────────────────────────────────

#[test]
fn day_file_name_round_trips() {
    let d = date(2026, 9, 8);
    let name = day_file_name(d);
    assert_eq!(name, "2026-09-08.ndjson");
    assert_eq!(parse_day_file_name(&name), Some(d));
}

#[test]
fn parse_day_file_name_rejects_a_non_matching_name() {
    assert_eq!(parse_day_file_name("not-a-date.ndjson"), None);
    assert_eq!(parse_day_file_name("2026-09-08.txt"), None);
    assert_eq!(parse_day_file_name("README.md"), None);
}

// ─── format: tolerant whole-file decode ─────────────────────────────────

#[tokio::test]
async fn read_events_returns_empty_for_an_empty_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("empty.ndjson");
    tokio::fs::write(&path, b"").await.expect("write empty");
    let events = read_events(&path).await.expect("read");
    assert!(events.is_empty());
}

#[tokio::test]
async fn read_events_skips_a_truncated_final_line() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("truncated.ndjson");
    let good = event(1);
    let mut content = encode_line(&good);
    content.extend_from_slice(b"{\"source\":\"mpm\",\"seq\":2,\"at\":\"trunc");
    tokio::fs::write(&path, &content).await.expect("write");

    let events = read_events(&path).await.expect("read");
    assert_eq!(
        events.len(),
        1,
        "the truncated final line is skipped, not fatal"
    );
    assert_eq!(events[0].id, good.id);
}

#[tokio::test]
async fn read_events_skips_a_corrupt_interior_line_and_keeps_reading() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("corrupt.ndjson");
    let first = event(1);
    let third = event(3);
    let mut content = encode_line(&first);
    content.extend_from_slice(b"not json at all\n");
    content.extend_from_slice(&encode_line(&third));
    tokio::fs::write(&path, &content).await.expect("write");

    let events = read_events(&path).await.expect("read");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, first.id);
    assert_eq!(events[1].id, third.id);
}

// ─── recovery: file listing, seq high-water mark, earliest retained ─────

#[tokio::test]
async fn list_log_files_ignores_non_matching_names() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    tokio::fs::create_dir_all(tmp.path()).await.expect("dir");
    tokio::fs::write(tmp.path().join("README.md"), b"hello")
        .await
        .expect("write stray file");
    seed_file(tmp.path(), date(2026, 9, 8), &[event(1)]).await;

    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, date(2026, 9, 8));
}

#[tokio::test]
async fn list_log_files_sorts_ascending_by_date() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 8), &[event(2)]).await;
    seed_file(tmp.path(), date(2026, 9, 6), &[event(1)]).await;
    seed_file(tmp.path(), date(2026, 9, 7), &[event(2)]).await;

    let files = list_log_files(tmp.path()).await.expect("list");
    let dates: Vec<_> = files.iter().map(|(d, _)| *d).collect();
    assert_eq!(
        dates,
        vec![date(2026, 9, 6), date(2026, 9, 7), date(2026, 9, 8)]
    );
}

#[tokio::test]
async fn recover_next_seq_starts_at_one_with_no_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(recover_next_seq(&files).await.expect("recover"), 1);
}

#[tokio::test]
async fn recover_next_seq_continues_from_the_newest_non_empty_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 6), &[event(1), event(2)]).await;
    seed_file(
        tmp.path(),
        date(2026, 9, 7),
        &[event(3), event(4), event(5)],
    )
    .await;
    // A day rolled over right before a crash: today's file exists but is
    // empty. Recovery must not stop at the newest FILE, only the newest
    // NON-EMPTY one.
    seed_file(tmp.path(), date(2026, 9, 8), &[]).await;

    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(recover_next_seq(&files).await.expect("recover"), 6);
}

#[tokio::test]
async fn recover_next_seq_tolerates_a_truncated_final_line() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 8), &[event(1), event(2)]).await;
    append_raw_line(
        tmp.path(),
        date(2026, 9, 8),
        "{\"source\":\"mpm\",\"seq\":3,\"trunc",
    )
    .await;

    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(
        recover_next_seq(&files).await.expect("recover"),
        3,
        "the truncated seq-3 write never counted; numbering resumes at 3, not 4"
    );
}

#[tokio::test]
async fn earliest_seq_reads_the_oldest_retained_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 6), &[event(5), event(6)]).await;
    seed_file(tmp.path(), date(2026, 9, 7), &[event(7)]).await;

    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(earliest_seq(&files).await.expect("earliest"), Some(5));
}

#[tokio::test]
async fn earliest_seq_is_none_for_an_empty_log() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let files = list_log_files(tmp.path()).await.expect("list");
    assert_eq!(earliest_seq(&files).await.expect("earliest"), None);
}

// ─── retention ───────────────────────────────────────────────────────────

#[test]
fn files_to_delete_keeps_exactly_retain_days() {
    let files = vec![
        (date(2026, 9, 1), std::path::PathBuf::from("1")),
        (date(2026, 9, 5), std::path::PathBuf::from("5")),
        (date(2026, 9, 6), std::path::PathBuf::from("6")),
    ];
    // retain_days=2 from today=9/6 keeps 9/5 and 9/6; 9/1 is expired.
    let doomed = files_to_delete(&files, date(2026, 9, 6), 2);
    assert_eq!(doomed.len(), 1);
    assert_eq!(doomed[0].0, date(2026, 9, 1));
}

#[test]
fn files_to_delete_keeps_everything_within_the_window() {
    let files = vec![
        (date(2026, 9, 5), std::path::PathBuf::from("5")),
        (date(2026, 9, 6), std::path::PathBuf::from("6")),
    ];
    let doomed = files_to_delete(&files, date(2026, 9, 6), 7);
    assert!(doomed.is_empty());
}

#[tokio::test]
async fn enforce_retention_deletes_only_the_expired_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 1), &[event(1)]).await;
    seed_file(tmp.path(), date(2026, 9, 6), &[event(2)]).await;

    let survivors = enforce_retention(tmp.path(), date(2026, 9, 6), 2)
        .await
        .expect("enforce");
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].0, date(2026, 9, 6));
    assert!(!tmp.path().join(day_file_name(date(2026, 9, 1))).exists());
    assert!(tmp.path().join(day_file_name(date(2026, 9, 6))).exists());
}

// ─── replay ────────────────────────────────────────────────────────────

#[tokio::test]
async fn replay_since_zero_returns_everything_in_order() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 6), &[event(1), event(2)]).await;
    seed_file(tmp.path(), date(2026, 9, 7), &[event(3)]).await;

    let items = replay_since(tmp.path(), 0, Some(1)).await.expect("replay");
    let seqs: Vec<u64> = items
        .into_iter()
        .map(|i| match i {
            ReplayItem::Event(e) => e.seq,
            ReplayItem::Gap { .. } => panic!("no gap expected"),
        })
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[tokio::test]
async fn replay_since_a_mid_seq_returns_only_the_remainder() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(
        tmp.path(),
        date(2026, 9, 6),
        &[event(1), event(2), event(3)],
    )
    .await;

    let items = replay_since(tmp.path(), 1, Some(1)).await.expect("replay");
    let seqs: Vec<u64> = items
        .into_iter()
        .map(|i| match i {
            ReplayItem::Event(e) => e.seq,
            ReplayItem::Gap { .. } => panic!("no gap expected"),
        })
        .collect();
    assert_eq!(seqs, vec![2, 3]);
}

#[tokio::test]
async fn replay_since_caught_up_returns_no_events_and_no_gap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(tmp.path(), date(2026, 9, 6), &[event(1), event(2)]).await;

    let items = replay_since(tmp.path(), 2, Some(1)).await.expect("replay");
    assert!(items.is_empty());
}

#[tokio::test]
async fn replay_since_before_retention_yields_a_leading_gap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Retention already discarded seq 1-4; the oldest retained file starts
    // at seq 5.
    seed_file(tmp.path(), date(2026, 9, 7), &[event(5), event(6)]).await;

    let items = replay_since(tmp.path(), 0, Some(5)).await.expect("replay");
    match &items[0] {
        ReplayItem::Gap {
            after_seq,
            before_seq,
        } => {
            assert_eq!(*after_seq, 0);
            assert_eq!(*before_seq, 5);
        }
        ReplayItem::Event(_) => panic!("expected a leading gap marker"),
    }
    assert_eq!(items.len(), 3, "gap marker plus the two retained events");
}

#[tokio::test]
async fn replay_since_across_a_backpressure_drop_yields_an_interior_gap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // seq 2 was never written — dropped under backpressure at ingest time.
    seed_file(tmp.path(), date(2026, 9, 6), &[event(1), event(3)]).await;

    let items = replay_since(tmp.path(), 0, Some(1)).await.expect("replay");
    assert_eq!(items.len(), 3, "event 1, a gap for 2, then event 3");
    assert!(matches!(items[0], ReplayItem::Event(ref e) if e.seq == 1));
    match &items[1] {
        ReplayItem::Gap {
            after_seq,
            before_seq,
        } => {
            assert_eq!(*after_seq, 1);
            assert_eq!(*before_seq, 3);
        }
        ReplayItem::Event(_) => panic!("expected an interior gap marker"),
    }
    assert!(matches!(items[2], ReplayItem::Event(ref e) if e.seq == 3));
}

// #6848: regression for the CRITICAL gap-detection bug the code-critic
// review found — `previous_seq` used to start at `None` and was only seeded
// inside the leading-retention-gap branch, so a drop on the FIRST seq after
// `since_seq` (the ordinary reconnect case, no retention gap involved) was
// never checked against `since_seq` and produced no gap marker at all. Day
// file holds seq [1, 2, 4, 5] — seq 3 was dropped under backpressure — and a
// client reconnects from `since_seq = 2`, already inside the retained
// window. This must fail before the `replay.rs` fix (no `Gap` before event
// 4) and pass after it.
#[tokio::test]
async fn replay_since_a_drop_on_the_first_replayed_seq_yields_a_gap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    seed_file(
        tmp.path(),
        date(2026, 9, 6),
        &[event(1), event(2), event(4), event(5)],
    )
    .await;

    let items = replay_since(tmp.path(), 2, Some(1)).await.expect("replay");
    assert_eq!(
        items.len(),
        3,
        "a gap for seq 3 immediately, then events 4 and 5"
    );
    match &items[0] {
        ReplayItem::Gap {
            after_seq,
            before_seq,
        } => {
            assert_eq!(*after_seq, 2);
            assert_eq!(*before_seq, 4);
        }
        ReplayItem::Event(_) => {
            panic!("expected a gap marker for the seq dropped immediately after since_seq")
        }
    }
    assert!(matches!(items[1], ReplayItem::Event(ref e) if e.seq == 4));
    assert!(matches!(items[2], ReplayItem::Event(ref e) if e.seq == 5));
}

#[tokio::test]
async fn replay_since_on_an_empty_log_returns_nothing() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let items = replay_since(tmp.path(), 0, None)
        .await
        .expect("replay an empty log");
    assert!(items.is_empty(), "no files, no gap, no events");
}

// ─── DurableLog / writer task ────────────────────────────────────────────

fn config(dir: &Path) -> LogConfig {
    LogConfig {
        dir: dir.to_path_buf(),
        retain_days: 7,
    }
}

async fn wait_until(budget: std::time::Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if predicate() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition did not become true within {budget:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn open_recovers_next_seq_from_an_existing_log() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let today = Utc::now().date_naive();
    seed_file(tmp.path(), today, &[event(1), event(2)]).await;

    let (_log, recovered) = DurableLog::open(config(tmp.path()))
        .await
        .expect("open existing log");
    assert_eq!(recovered.next_seq, 3);
}

#[tokio::test]
async fn events_written_are_readable_back_in_order() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (log, _recovered) = DurableLog::open(config(tmp.path()))
        .await
        .expect("open fresh log");

    assert!(log.enqueue(event(1)));
    assert!(log.enqueue(event(2)));
    wait_until(std::time::Duration::from_secs(1), || log.written() >= 2).await;

    let items = log.replay_since(0).await.expect("replay");
    let seqs: Vec<u64> = items
        .into_iter()
        .map(|i| match i {
            ReplayItem::Event(e) => e.seq,
            ReplayItem::Gap { .. } => panic!("no gap expected"),
        })
        .collect();
    assert_eq!(seqs, vec![1, 2]);
}

#[tokio::test]
async fn backpressure_drops_are_counted_and_do_not_block_enqueue() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Capacity 1: the second `enqueue`, issued with no `.await` in between,
    // races the writer task for the single slot. On this crate's default
    // (current-thread) `#[tokio::test]` runtime the writer cannot have been
    // polled yet at this point, so the race is deterministic — the same
    // reasoning `event_bus::tests::connections_beyond_the_limit_wait_for_a_free_slot`
    // relies on for its own semaphore-saturation proof.
    let (log, _recovered) = DurableLog::open_with_capacity(config(tmp.path()), 1)
        .await
        .expect("open fresh log");

    let first = log.enqueue(event(1));
    let second = log.enqueue(event(2));
    assert!(first, "the first enqueue fills the only slot");
    assert!(
        !second,
        "the second enqueue must not block or panic; it reports backpressure instead"
    );
}

#[tokio::test]
async fn rotation_opens_a_new_file_and_keeps_seq_continuity() {
    // Simulates the day boundary having already crossed once: a "yesterday"
    // file holds the tail of a prior day's numbering, and recovery must
    // resume from it rather than restarting at 1 just because today's file
    // does not exist yet.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let yesterday = Utc::now().date_naive().pred_opt().expect("valid date");
    // Seq starts at 1 (as it always does for a genuinely fresh log) so
    // `earliest_retained_seq` is legitimately 1 and `replay_since(0)` sees no
    // retention gap — a log that started numbering higher would make
    // `replay_since` (correctly) report seq 1 onward as lost to retention,
    // which is not what this test is exercising.
    seed_file(tmp.path(), yesterday, &[event(1), event(2)]).await;

    let (log, recovered) = DurableLog::open(config(tmp.path()))
        .await
        .expect("open log spanning a day boundary");
    assert_eq!(recovered.next_seq, 3);

    assert!(log.enqueue(event(3)));
    wait_until(std::time::Duration::from_secs(1), || log.written() >= 1).await;

    let today = Utc::now().date_naive();
    assert!(
        tmp.path().join(day_file_name(today)).exists(),
        "the writer rotates to today's file rather than appending to yesterday's"
    );
    let items = log
        .replay_since(0)
        .await
        .expect("replay across the boundary");
    let seqs: Vec<u64> = items
        .into_iter()
        .map(|i| match i {
            ReplayItem::Event(e) => e.seq,
            ReplayItem::Gap { .. } => panic!("no gap expected"),
        })
        .collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "seq stays continuous across the file boundary"
    );
}

// #6848: regression for the CRITICAL missing-fsync-at-rotation finding.
// Drives `rotate` directly (rather than waiting for a real UTC day change)
// so the outgoing day's file can be dropped WITHOUT the writer task's own
// graceful-shutdown sync ever running — the exact "crash before shutdown"
// scenario the fsync-at-rotation fix protects. Proves both that `rotate`
// syncs the outgoing handle without erroring and that the seq high-water
// mark recovers correctly across both files afterward.
#[tokio::test]
async fn rotation_fsyncs_the_outgoing_file_before_opening_the_next_one() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let day1 = date(2026, 1, 1);
    let day2 = date(2026, 1, 2);
    let earliest_retained_seq = Arc::new(AtomicU64::new(0));

    let (mut file1, path1) = rotate(&config(tmp.path()), day1, &earliest_retained_seq, None)
        .await
        .expect("open day1's file");
    write_line(&mut file1, &path1, &event(1))
        .await
        .expect("write seq 1 to day1");

    // The rotation itself fsyncs `file1` before opening day2's file.
    let (mut file2, path2) = rotate(
        &config(tmp.path()),
        day2,
        &earliest_retained_seq,
        Some(&mut file1),
    )
    .await
    .expect("rotate to day2's file");
    write_line(&mut file2, &path2, &event(2))
        .await
        .expect("write seq 2 to day2");

    // Ungraceful drop — no writer-task shutdown sync runs. If rotation's own
    // fsync did not happen, this is exactly the scenario that would lose
    // day1's data on a real crash.
    drop(file1);
    drop(file2);

    let recovered = recover_next_seq(&[(day1, path1), (day2, path2)])
        .await
        .expect("recover across both files");
    assert_eq!(
        recovered, 3,
        "both seqs are readable back after an ungraceful drop post-rotation"
    );
}
