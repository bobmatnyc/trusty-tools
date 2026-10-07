//! Tests for the drawer-count history writer (#9283).
//! Every test runs on a tempdir palace root; none touches the live store.

use super::*;
use chrono::Duration as ChronoDuration;
use trusty_common::memory_core::{Drawer, PalaceId};

fn palace(root: &Path, name: &str) -> Palace {
    Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: root.join(name),
    }
}

/// A real palace holding `drawers` drawers; closed unless `keep` is given a
/// registry to stay resident in.
fn seed(root: &Path, name: &str, drawers: usize, keep: Option<&PalaceRegistry>) {
    let scratch = PalaceRegistry::with_max_open(4);
    let registry = keep.unwrap_or(&scratch);
    let handle = registry
        .create_palace(root, palace(root, name))
        .expect("create_palace");
    for i in 0..drawers {
        let d = Drawer::new(uuid::Uuid::new_v4(), format!("drawer {i}"));
        handle.kg.store().upsert_drawer(&d).expect("upsert");
        if keep.is_some() {
            handle.drawers.write().push(d);
        }
    }
    drop(handle);
    if keep.is_none() {
        scratch.remove(&PalaceId::new(name));
    }
}

fn lines(root: &Path) -> Vec<CountLine> {
    read_history(root).expect("read").lines
}

/// Why (#9283 AC 1): one count per palace per UTC day, from the open handle
/// for a resident palace and from disk for a cold one; a daemon restart on
/// the same day must not add a second line.
#[test]
fn snapshot_task_writes_one_line_per_palace_per_day() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hot = PalaceRegistry::with_max_open(4);
    seed(tmp.path(), "hot", 2, Some(&hot));
    seed(tmp.path(), "cold", 3, None);
    let now = Utc::now();

    let first = take_snapshot(&hot, tmp.path(), now).expect("snapshot");
    assert_eq!(
        first,
        SnapshotOutcome {
            written: 2,
            pruned: 0
        }
    );
    let got = lines(tmp.path());
    let by = |p: &str| got.iter().find(|l| l.palace == p).cloned().expect(p);
    assert_eq!(
        (by("hot").drawers, by("hot").src),
        (Some(2), CountSource::Cache)
    );
    assert_eq!(
        (by("cold").drawers, by("cold").src),
        (Some(3), CountSource::Disk)
    );

    // A restart: a fresh registry, later the same UTC day.
    let restarted = PalaceRegistry::with_max_open(4);
    let again = take_snapshot(&restarted, tmp.path(), now + ChronoDuration::seconds(5))
        .expect("snapshot after restart");
    assert_eq!(again, SnapshotOutcome::default());
    assert_eq!(lines(tmp.path()).len(), 2);

    let next_day =
        take_snapshot(&restarted, tmp.path(), now + ChronoDuration::days(1)).expect("next day");
    assert_eq!(next_day.written, 2);
    assert_eq!(lines(tmp.path()).len(), 4);
}

/// Why (design §1): a palace another process holds for writing cannot be
/// read, and recording zero would read as total loss.
#[test]
fn snapshot_records_unavailable_not_zero_for_a_write_held_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Held open by a registry the snapshot cannot see, as another process.
    let other = PalaceRegistry::with_max_open(4);
    let _held = other
        .create_palace(tmp.path(), palace(tmp.path(), "held"))
        .expect("create_palace");
    let mine = PalaceRegistry::with_max_open(4);
    take_snapshot(&mine, tmp.path(), Utc::now()).expect("snapshot");
    let got = lines(tmp.path());
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].drawers, None);
    assert_eq!(got[0].src, CountSource::Unavailable);
    // #9283: the line names why, so doctor can show it.
    let reason = got[0].reason.as_deref().unwrap_or_default();
    assert!(reason.contains("kg.redb"), "{got:?}");
    let raw = std::fs::read_to_string(history_path(tmp.path())).expect("read");
    assert!(raw.contains(r#""drawers":null"#), "{raw}");
}

/// Why (#9283): a resident count and a disk count of one palace must agree,
/// or a palace counted resident one day and from disk the next shows a drop
/// no journal explains. The in-memory list also holds L1-snapshot drawers the
/// store has deleted.
/// What: a resident palace with 3 stored drawers and one extra drawer only in
/// its in-memory list is counted as 3, from the handle.
#[test]
fn resident_count_matches_disk_not_the_in_memory_list() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let daemon = PalaceRegistry::with_max_open(4);
    seed(tmp.path(), "hot", 3, Some(&daemon));
    let handle = daemon.peek(&PalaceId::new("hot")).expect("resident handle");
    handle.drawers.write().push(Drawer::new(
        uuid::Uuid::new_v4(),
        "L1-only ghost".to_string(),
    ));
    drop(handle);
    take_snapshot(&daemon, tmp.path(), Utc::now()).expect("snapshot");
    let got = lines(tmp.path());
    assert_eq!(
        (got[0].drawers, got[0].src),
        (Some(3), CountSource::Cache),
        "{got:?}"
    );
}

/// Why (#9283, Fail-Open Check): the idle-evict sweep takes a handle out of
/// the registry before dropping it, so for a moment the daemon holds
/// `kg.redb` while `peek` misses. One read per palace recorded 61 of 102 live
/// palaces unavailable.
/// What: a 3-drawer palace whose handle has left the registry but is still
/// open, released 400 ms into the snapshot, is counted from disk.
#[test]
fn snapshot_counts_a_palace_whose_handle_is_still_closing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let daemon = PalaceRegistry::with_max_open(4);
    seed(tmp.path(), "closing", 3, Some(&daemon));
    let id = PalaceId::new("closing");
    let leaving = daemon.peek(&id).expect("resident handle");
    daemon.remove(&id);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        drop(leaving);
    });
    take_snapshot(&daemon, tmp.path(), Utc::now()).expect("snapshot");
    release.join().expect("release thread");
    let got = lines(tmp.path());
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        (got[0].drawers, got[0].src),
        (Some(3), CountSource::Disk),
        "{got:?}"
    );
}

/// Why (design §3): the history is bounded to 90 days; a line exactly 90
/// days old is kept, an older one is dropped.
#[test]
fn history_prunes_to_ninety_days() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed(tmp.path(), "p", 1, None);
    let now = Utc::now();
    let line = |days_ago: i64| CountLine {
        v: SCHEMA_VERSION,
        at: now - ChronoDuration::days(days_ago),
        palace: "p".into(),
        drawers: Some(1),
        src: CountSource::Disk,
        ack: None,
        reason: None,
    };
    let text: String = [91, 90, 89, 1]
        .iter()
        .map(|d| format!("{}\n", serde_json::to_string(&line(*d)).expect("ser")))
        .collect();
    std::fs::write(history_path(tmp.path()), text).expect("w");

    let out = take_snapshot(&PalaceRegistry::with_max_open(4), tmp.path(), now).expect("snap");
    assert_eq!(
        out,
        SnapshotOutcome {
            written: 1,
            pruned: 1
        }
    );
    let kept: Vec<i64> = lines(tmp.path())
        .iter()
        .map(|l| (now - l.at).num_days())
        .collect();
    assert_eq!(kept, vec![90, 89, 1, 0]);
}

/// Why (design §2): an operator ack is the way out of a red the journal
/// cannot explain; it applies to the newest snapshot day by default.
#[test]
fn an_ack_explains_an_unjournaled_drop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let line = |at, n| CountLine {
        v: SCHEMA_VERSION,
        at,
        palace: "p".into(),
        drawers: Some(n),
        src: CountSource::Disk,
        ack: None,
        reason: None,
    };
    let text = format!(
        "{}\n{}\n",
        serde_json::to_string(&line(now - ChronoDuration::days(1), 10)).expect("ser"),
        serde_json::to_string(&line(now, 6)).expect("ser")
    );
    std::fs::write(history_path(tmp.path()), text).expect("w");
    let none = |_: &str| Ok(Vec::new());
    let judge = |root: &Path| analysis::judge(&read_history(root).expect("read"), &none);
    assert!(matches!(
        judge(tmp.path())[0].1,
        analysis::Verdict::Unexplained(_)
    ));

    assert!(append_ack(tmp.path(), "p", 0, "x", None, now).is_err());
    assert!(append_ack(tmp.path(), "nope", 4, "x", None, now).is_err());
    let ack = append_ack(tmp.path(), "p", 4, "reclaim before #9283", None, now).expect("ack");
    assert_eq!(ack.ack.expect("ack body").day, now.date_naive());
    assert!(matches!(
        judge(tmp.path())[0].1,
        analysis::Verdict::Clean(_)
    ));
}

/// Why: the daemon task must snapshot without waiting a day and must stop on
/// the shutdown watch.
#[tokio::test]
async fn snapshot_loop_writes_at_start_and_stops_on_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed(tmp.path(), "p", 1, None);
    let (tx, rx) = watch::channel(false);
    let task = spawn_snapshot_loop(
        Arc::new(PalaceRegistry::with_max_open(4)),
        crate::idle_evict::new_evict_gate(),
        tmp.path().to_path_buf(),
        Duration::ZERO,
        Duration::from_secs(3600),
        rx,
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while read_history(tmp.path()).expect("read").lines.is_empty() {
        assert!(std::time::Instant::now() < deadline, "no snapshot written");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send(true).expect("send");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("task stops on shutdown")
        .expect("join");
}
