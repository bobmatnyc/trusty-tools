//! Tests for the maintenance deletion journal (#8732).

use super::dream::{DreamConfig, Dreamer};
use super::maintenance_log::{
    DeletionReason, MAINTENANCE_LOG_ROTATED_FILENAME, MaintenanceDeletion, RecordOutcome, append,
    journal_path, read_journal, record,
};
use super::palace::{Drawer, Palace, PalaceId, RoomType};
use super::retrieval::{ForgetOutcome, PalaceHandle, seed_shared_embedder_with_mock};
use super::store::kg::KnowledgeGraph;
use chrono::{Duration, Utc};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::{TempDir, tempdir};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

/// Collects formatted log output so a test can assert on what reached the log.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Capture events at the daemon's default level (`warn`) on this thread.
fn capture_warn() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

fn palace_at(root: &Path, name: &str) -> Palace {
    let data_dir = root.join(name);
    std::fs::create_dir_all(&data_dir).unwrap();
    Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: Utc::now(),
        data_dir,
    }
}

fn open_palace(name: &str) -> (TempDir, PathBuf, Arc<PalaceHandle>) {
    seed_shared_embedder_with_mock();
    let dir = tempdir().unwrap();
    let palace = palace_at(dir.path(), name);
    let handle = PalaceHandle::open(&palace).unwrap();
    (dir, palace.data_dir, handle)
}

fn dedup_only_config() -> DreamConfig {
    DreamConfig {
        semantic: super::semantic_consolidation::SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        ..DreamConfig::default()
    }
}

/// Why: #8729 — dedup removed 70 drawers and nothing said which drawer each
/// one duplicated. The record must name the palace, the removed drawer, the
/// survivor and the score, and a `warn` summary must reach the default filter.
#[tokio::test]
async fn dream_dedup_records_the_removed_and_surviving_drawer() {
    let (_dir, data_dir, handle) = open_palace("dedup-trail");
    let content = "Rust uses HNSW for vector search";
    let keep = handle
        .remember(content.into(), RoomType::Backend, vec![], 0.7)
        .await
        .unwrap();
    let lose = handle
        .remember(content.into(), RoomType::Backend, vec![], 0.6)
        .await
        .unwrap();

    let (log, _guard) = capture_warn();
    let stats = Dreamer::new(dedup_only_config())
        .dream_cycle(&handle)
        .await
        .unwrap();
    assert_eq!(stats.merged, 1);

    let journal = read_journal(&data_dir).unwrap();
    assert_eq!(journal.records.len(), 1, "{:?}", journal.records);
    let rec = &journal.records[0];
    assert_eq!(rec.palace, "dedup-trail");
    assert_eq!(rec.drawer_id, lose);
    assert_eq!(rec.reason, DeletionReason::DreamDedup);
    assert_eq!(rec.survivor_id, Some(keep));
    let score = rec.score.expect("dedup records its score");
    assert!(score >= dedup_only_config().dedup_threshold, "{score}");
    assert_eq!(rec.pid, std::process::id());

    let text = log.text();
    assert!(
        text.contains("WARN") && text.contains("dream cycle removed 1 drawer"),
        "{text}"
    );
}

/// Why: the palace-open sweep deletes rows before any handle exists; its
/// deletions must reach the journal too.
#[test]
fn the_open_time_purge_records_each_expired_drawer() {
    let dir = tempdir().unwrap();
    let palace = palace_at(dir.path(), "open-purge-trail");
    let mut expired = Drawer::new(Uuid::new_v4(), "an expired session event");
    expired.expires_at = Some(Utc::now() - Duration::days(1));
    let expired_id = expired.id;
    {
        let kg = KnowledgeGraph::open(&palace.data_dir.join("kg.db")).unwrap();
        kg.upsert_drawer_sync(&expired).unwrap();
    }

    let handle = PalaceHandle::open(&palace).unwrap();
    assert!(handle.drawers.read().is_empty());

    let journal = read_journal(&palace.data_dir).unwrap();
    assert_eq!(journal.records.len(), 1, "{:?}", journal.records);
    let rec = &journal.records[0];
    assert_eq!(rec.palace, "open-purge-trail");
    assert_eq!(rec.drawer_id, expired_id);
    assert_eq!(rec.reason, DeletionReason::ExpiredPurgeAtOpen);
    assert_eq!(rec.survivor_id, None);
}

/// Why: `purge_expired` is the handle-level TTL purge; same trail required.
#[tokio::test]
async fn purge_expired_records_each_drawer() {
    let (_dir, data_dir, handle) = open_palace("ttl-purge-trail");
    let mut expired = Drawer::new(Uuid::new_v4(), "expired");
    expired.expires_at = Some(Utc::now() - Duration::days(1));
    let expired_id = expired.id;
    handle.add_drawer(expired);

    assert_eq!(handle.purge_expired().await.unwrap(), 1);

    let records = read_journal(&data_dir).unwrap().records;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].drawer_id, expired_id);
    assert_eq!(records[0].reason, DeletionReason::ExpiredPurge);
}

/// Why: a user's `memory_forget` is not maintenance and must not be recorded
/// as one.
#[tokio::test]
async fn user_forget_writes_no_maintenance_record() {
    let (_dir, data_dir, handle) = open_palace("user-forget");
    let id = handle
        .remember(
            "a fact the user chose to forget".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    assert_eq!(handle.forget(id).await.unwrap(), ForgetOutcome::Deleted);
    assert!(!journal_path(&data_dir).exists());
    assert!(read_journal(&data_dir).unwrap().records.is_empty());
}

/// Why (Fail-Open Check): a journal that cannot be written must not undo the
/// deletion, and must not lose the record either — it goes to the log at
/// `error`, which the daemon's default `warn` filter keeps.
#[tokio::test]
async fn a_failed_record_write_logs_the_record_and_still_deletes() {
    let (_dir, data_dir, handle) = open_palace("broken-journal");
    // A directory where the journal file belongs makes every append fail.
    std::fs::create_dir_all(journal_path(&data_dir)).unwrap();
    let survivor = Uuid::new_v4();
    let mut doomed = Drawer::new(Uuid::new_v4(), "a drawer dedup will remove");
    doomed.importance = 0.4;
    let doomed_id = doomed.id;
    handle.add_drawer(doomed);

    let (log, _guard) = capture_warn();
    let outcome = handle
        .forget_for_maintenance(
            doomed_id,
            DeletionReason::DreamDedup,
            Some((survivor, Some(0.97))),
        )
        .await
        .unwrap();

    assert_eq!(outcome, ForgetOutcome::Deleted, "the deletion proceeds");
    assert!(handle.drawers.read().iter().all(|d| d.id != doomed_id));
    let text = log.text();
    for needle in [
        "ERROR",
        "broken-journal",
        &doomed_id.to_string(),
        &survivor.to_string(),
        "dream_dedup",
        "0.97",
    ] {
        assert!(text.contains(needle), "missing {needle} in {text}");
    }

    let rec = MaintenanceDeletion::new(&handle.id, doomed_id, DeletionReason::DreamPrune);
    assert_eq!(record(Some(&data_dir), &rec), RecordOutcome::LoggedOnly);
}

/// Why: the journal is bounded by one rotation, and a reader must see both
/// generations in order and survive a torn line.
#[test]
fn the_journal_rotates_and_reads_back_oldest_first() {
    let dir = tempdir().unwrap();
    let palace = PalaceId::new("rotate");
    let first = MaintenanceDeletion::new(&palace, Uuid::new_v4(), DeletionReason::DreamPrune);
    let second = MaintenanceDeletion::new(&palace, Uuid::new_v4(), DeletionReason::ExpiredPurge)
        .with_survivor(Uuid::new_v4(), None);
    append(dir.path(), &first, u64::MAX).unwrap();
    append(dir.path(), &second, 1).unwrap();
    assert!(dir.path().join(MAINTENANCE_LOG_ROTATED_FILENAME).exists());
    let mut live = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path(dir.path()))
        .unwrap();
    live.write_all(b"{\"torn\":\n").unwrap();

    let journal = read_journal(dir.path()).unwrap();
    assert_eq!(journal.records, vec![first, second]);
    assert_eq!(journal.malformed, 1);
}

/// Why (critic HIGH on 1e27c71bd): up to ~16 processes append to one palace's
/// journal (#8733). Two writers that both saw the full size each renamed the
/// live file over `.1`, and the second rename threw away the whole previous
/// generation while both callers reported success.
/// What: pre-fills the live file to exactly the rotation size, then releases 16
/// threads at once, each appending one record through its own descriptor (the
/// lock is a per-descriptor `flock`, so threads contend as processes do). One
/// rotation is due, so every record must be in the live file or `.1`. Twenty
/// rounds make the pre-fix race fail on every run, not occasionally.
#[test]
fn concurrent_appends_across_the_rotation_boundary_lose_no_record() {
    const WRITERS: usize = 16;
    let palace = PalaceId::new("rotate-race");
    for round in 0..20 {
        let dir = tempdir().unwrap();
        let prefill = 40;
        for _ in 0..prefill {
            let rec = MaintenanceDeletion::new(&palace, Uuid::new_v4(), DeletionReason::DreamPrune);
            append(dir.path(), &rec, u64::MAX).unwrap();
        }
        let rotate_at = std::fs::metadata(journal_path(dir.path())).unwrap().len();
        let barrier = Arc::new(std::sync::Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|_| {
                let (barrier, dir, palace) = (
                    Arc::clone(&barrier),
                    dir.path().to_path_buf(),
                    palace.clone(),
                );
                std::thread::spawn(move || {
                    let rec = MaintenanceDeletion::new(
                        &palace,
                        Uuid::new_v4(),
                        DeletionReason::DreamDedup,
                    );
                    barrier.wait();
                    append(&dir, &rec, rotate_at).map_err(|e| format!("{e:#}"))
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let journal = read_journal(dir.path()).unwrap();
        let errors: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
        assert_eq!(
            journal.records.len(),
            prefill + WRITERS,
            "round {round}: a record was lost; append errors: {errors:?}"
        );
        assert!(errors.is_empty(), "round {round}: {errors:?}");
    }
}

/// Why (Fail-Open Check): a journal lock that cannot be taken, or a rotation
/// that fails, must not drop the record. Both fall back to one plain append to
/// the live file.
#[test]
fn a_lock_or_rotation_failure_still_appends_the_record() {
    let dir = tempdir().unwrap();
    let palace = PalaceId::new("lock-fail");
    // A directory at the lock sidecar path makes the lock unobtainable.
    let sidecar = crate::file_lock::lock_path(&journal_path(dir.path()));
    std::fs::create_dir_all(&sidecar).unwrap();
    let first = MaintenanceDeletion::new(&palace, Uuid::new_v4(), DeletionReason::DreamPrune);
    append(dir.path(), &first, u64::MAX).unwrap();
    std::fs::remove_dir(&sidecar).unwrap();

    // A non-empty directory at `.1` makes the rotation's rename fail.
    let rotated = dir.path().join(MAINTENANCE_LOG_ROTATED_FILENAME);
    std::fs::create_dir_all(rotated.join("occupied")).unwrap();
    let second = MaintenanceDeletion::new(&palace, Uuid::new_v4(), DeletionReason::ExpiredPurge);
    append(dir.path(), &second, 1).unwrap();

    let live = std::fs::read_to_string(journal_path(dir.path())).unwrap();
    assert!(live.contains(&first.drawer_id.to_string()), "{live}");
    assert!(live.contains(&second.drawer_id.to_string()), "{live}");
}
