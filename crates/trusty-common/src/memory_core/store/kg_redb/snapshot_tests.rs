//! Tests for the #9282 palace snapshot and restore.
//!
//! Why: a backup path that degrades any failure to a warning hands the
//! operator a backup that is not one. Every acceptance criterion and every
//! error arm is triggered here, with a real concurrent writer thread for the
//! consistency claim.
//! What: one shared fixture builds an on-disk palace with rooms, drawers, a
//! triple per drawer, a chat session, journals and `drawer_counts.jsonl`
//! lines; tests snapshot it, restore it and break each step on purpose.
//! Test: this file is the test.

#![cfg(test)]

use super::restore::restore_with;
use super::{
    CHAT_FILE, DRAWER_COUNTS_FILE, KG_FILE, PalaceSnapshot, SNAPSHOT_MANIFEST, SnapshotError,
    SnapshotStep, complete_lines, no_fault, restore_palace_snapshot, snapshot_palace,
    table_counts,
};
use crate::memory_core::maintenance_log::{
    MAINTENANCE_LOG_FILENAME, MAINTENANCE_LOG_ROTATED_FILENAME,
};
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::store::chat_sessions::ChatSessionStore;
use crate::memory_core::store::kg::Triple;
use crate::memory_core::store::kg_redb::{BatchWriteOp, KgStoreRedb};
use crate::memory_core::store::palace_store::PalaceStore;
use crate::memory_core::store::rooms::resolve_or_create_room_sync;
use chrono::Utc;
use redb::ReadableDatabase;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

const PALACE: &str = "snap-p1";
const SEED_DRAWERS: usize = 600;

/// An on-disk palace under `<tmp>/palaces/<PALACE>` with every primary file.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    handle: Arc<PalaceHandle>,
    chat: ChatSessionStore,
    rooms: Vec<Uuid>,
}

impl Fixture {
    fn palace_dir(&self) -> PathBuf {
        self.root.join("palaces").join(PALACE)
    }
}

fn triple_for(id: Uuid) -> Triple {
    Triple {
        subject: format!("drawer:{id}"),
        predicate: "snapshot_test".into(),
        object: "present".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    }
}

/// One drawer and its triple in ONE write transaction: any committed state
/// holds them in pairs, so a torn copy shows up as unequal counts.
fn write_pair(store: &KgStoreRedb, room: Uuid, n: u64) {
    let drawer = Drawer::new(room, format!("snapshot drawer {n}"));
    let triple = triple_for(drawer.id);
    store
        .apply_batch(&[BatchWriteOp::UpsertDrawer(drawer), BatchWriteOp::Assert(triple)])
        .expect("write pair");
}

fn fixture() -> Fixture {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let data_dir = root.join("palaces").join(PALACE);
    let palace = Palace {
        id: PalaceId::new(PALACE),
        name: PALACE.into(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_dir.clone(),
    };
    PalaceStore::save_palace(&palace).expect("palace.json");
    PalaceStore::save_identity(&palace.id, "snapshot identity", &data_dir).expect("identity");
    let handle = PalaceHandle::open(&palace).expect("open palace");
    let store = handle.kg.store();
    let rooms: Vec<Uuid> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|r| resolve_or_create_room_sync(&store, &RoomType::Custom((*r).into())).expect("room"))
        .collect();
    for i in 0..SEED_DRAWERS {
        write_pair(&store, rooms[i % rooms.len()], i as u64);
    }
    let chat = ChatSessionStore::open(&data_dir.join(CHAT_FILE)).expect("chat store");
    chat.create_session(Some("kept".into())).expect("session");
    std::fs::write(data_dir.join(MAINTENANCE_LOG_ROTATED_FILENAME), b"{\"old\":1}\n")
        .expect("rotated journal");
    std::fs::write(data_dir.join(MAINTENANCE_LOG_FILENAME), b"{\"a\":1}\n{\"b\":2}\n")
        .expect("journal");
    let counts = format!(
        "{{\"v\":1,\"palace\":\"{PALACE}\",\"drawers\":1}}\n{{\"v\":1,\"palace\":\"other\",\"drawers\":9}}\n{{\"v\":1,\"palace\":\"{PALACE}\",\"drawers\":2}}\n{{\"v\":1,\"palace\":\"{PALACE}\""
    );
    std::fs::write(root.join("palaces").join(DRAWER_COUNTS_FILE), counts).expect("counts");
    Fixture {
        _tmp: tmp,
        root,
        handle,
        chat,
        rooms,
    }
}

/// Holds the snapshot inside its read transaction long enough for the
/// writer to commit many rows that the snapshot must NOT contain.
fn slow_copy(step: SnapshotStep, _: &Path) -> io::Result<()> {
    if step == SnapshotStep::CopyDb {
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// Snapshot `fx` while a writer thread commits drawer/triple pairs.
/// Returns the snapshot and the pairs committed before and after it.
fn snapshot_under_writer(fx: &Fixture) -> (PalaceSnapshot, u64, u64) {
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let store = fx.handle.kg.store();
    let room = fx.rooms[0];
    let writer = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let n = written.load(Ordering::SeqCst);
                write_pair(&store, room, 1_000_000 + n);
                written.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    while written.load(Ordering::SeqCst) < 5 {
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = written.load(Ordering::SeqCst);
    let dest = fx.root.join("snap");
    let snap =
        snapshot_palace(&fx.handle, Some(&fx.chat), Some(&dest), slow_copy).expect("snapshot");
    let after = written.load(Ordering::SeqCst);
    stop.store(true, Ordering::SeqCst);
    writer.join().expect("writer thread");
    (snap, before, after)
}

fn counts_of(path: &Path, names: &[String]) -> std::collections::BTreeMap<String, u64> {
    let db = crate::redb_cache::open_palace_db_read_only(path).expect("open copy read-only");
    let rtx = db.begin_read().expect("read");
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    table_counts(&rtx, path, &names).expect("counts")
}

/// AC1 (#9282): counts in the manifest equal the copied file, the copy opens
/// cleanly, and drawers and their triples stay paired — no torn state.
#[test]
fn snapshot_under_a_concurrent_writer_matches_its_manifest() {
    let fx = fixture();
    let (snap, before, after) = snapshot_under_writer(&fx);
    let m = &snap.manifest;
    assert!(after > before, "the writer must commit during the snapshot");

    let names: Vec<String> = m.kg_tables.keys().cloned().collect();
    assert_eq!(counts_of(&snap.dir.join(KG_FILE), &names), m.kg_tables);
    let chat_names: Vec<String> = m.chat_tables.keys().cloned().collect();
    assert_eq!(counts_of(&snap.dir.join(CHAT_FILE), &chat_names), m.chat_tables);
    assert_eq!(m.chat_tables["chat_sessions"], 1);

    let drawers = m.kg_tables["drawers"];
    assert_eq!(m.drawer_count, drawers);
    assert_eq!(m.room_count, 3);
    assert_eq!(drawers, m.kg_tables["triples"], "drawer/triple pairs torn");
    assert!(drawers >= SEED_DRAWERS as u64 + before, "snapshot predates the writer");
    let live = fx.handle.kg.store().db().begin_read().expect("live read");
    let live_counts = table_counts(&live, Path::new("live"), &["drawers"]).expect("live");
    assert!(
        live_counts["drawers"] > drawers,
        "rows written after the read txn opened must not be in the copy"
    );
    for name in [KG_FILE, CHAT_FILE, "palace.json", "identity.txt"] {
        assert!(m.files.iter().any(|f| f.name == name), "{name} missing");
    }
    for derived in ["index.usearch.redb", "bm25", "l1_cache.json"] {
        assert!(!snap.dir.join(derived).exists(), "{derived} is derived");
    }
    assert!(!PathBuf::from(format!("{}.tmp", snap.dir.display())).exists());
}

/// AC2 (#9282): restore into an empty root, then open the palace.
#[test]
fn restore_into_an_empty_root_reopens_with_the_manifest_counts() {
    let fx = fixture();
    let (snap, _, _) = snapshot_under_writer(&fx);
    let target = fx.root.join("restored").join("palaces").join(PALACE);
    let manifest = restore_palace_snapshot(&snap.dir, &target).expect("restore");
    assert_eq!(manifest, snap.manifest);
    assert!(!target.join(DRAWER_COUNTS_FILE).exists(), "data-root file");

    let palace = PalaceStore::load_palace(&target).expect("restored palace.json");
    let handle = PalaceHandle::open(&palace).expect("open restored palace");
    assert_eq!(handle.drawers.read().len() as u64, manifest.drawer_count);
    let store = handle.kg.store();
    assert_eq!(store.list_rooms().expect("rooms").len() as u64, manifest.room_count);
    let rtx = store.db().begin_read().expect("read");
    let names: Vec<&str> = manifest.kg_tables.keys().map(String::as_str).collect();
    assert_eq!(table_counts(&rtx, &target, &names).expect("c"), manifest.kg_tables);
}

/// AC3 (#9282): a journal's torn final line is dropped, never copied.
#[test]
fn a_journal_torn_final_line_is_not_copied() {
    let fx = fixture();
    let journal = fx.palace_dir().join(MAINTENANCE_LOG_FILENAME);
    std::fs::write(&journal, b"{\"a\":1}\n{\"b\":2}\n{\"torn\":").expect("journal");
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let copied = std::fs::read(snap.dir.join(MAINTENANCE_LOG_FILENAME)).expect("copy");
    assert_eq!(copied, b"{\"a\":1}\n{\"b\":2}\n");
    assert!(snap.dir.starts_with(fx.root.join("backups").join("manual").join(PALACE)));
    assert_eq!(complete_lines(b"no newline at all"), b"");
}

/// #9282: only this palace's complete `drawer_counts.jsonl` lines are taken.
#[test]
fn drawer_count_lines_are_filtered_to_the_palace() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let lines = std::fs::read_to_string(snap.dir.join(DRAWER_COUNTS_FILE)).expect("counts");
    assert_eq!(
        lines,
        format!(
            "{{\"v\":1,\"palace\":\"{PALACE}\",\"drawers\":1}}\n{{\"v\":1,\"palace\":\"{PALACE}\",\"drawers\":2}}\n"
        )
    );
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// AC4 (#9282): a non-empty target is refused and left byte-for-byte as is.
#[test]
fn restore_refuses_a_non_empty_target_and_leaves_it_unchanged() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let parent = fx.root.join("restore-parent");
    let target = parent.join(PALACE);
    std::fs::create_dir_all(&target).expect("target");
    std::fs::write(target.join("kg.redb"), b"existing data").expect("existing");
    let err = restore_palace_snapshot(&snap.dir, &target).expect_err("must refuse");
    assert!(matches!(err, SnapshotError::TargetNotEmpty { .. }), "{err}");
    assert_eq!(listing(&target), vec!["kg.redb".to_string()]);
    assert_eq!(std::fs::read(target.join("kg.redb")).expect("r"), b"existing data");
    assert_eq!(listing(&parent), vec![PALACE.to_string()], "no staging left");
}

/// AC5 (#9282): one flipped byte fails the restore before any write.
#[test]
fn restore_with_a_checksum_mismatch_writes_nothing() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let kg = snap.dir.join(KG_FILE);
    let mut bytes = std::fs::read(&kg).expect("read");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&kg, bytes).expect("tamper");
    let parent = fx.root.join("restore-parent");
    std::fs::create_dir_all(&parent).expect("parent");
    let err = restore_palace_snapshot(&snap.dir, &parent.join(PALACE)).expect_err("tampered");
    assert!(matches!(err, SnapshotError::ChecksumMismatch { .. }), "{err}");
    assert!(listing(&parent).is_empty(), "nothing may be written");
}

/// #9282: a snapshot from a newer palace format is refused before any write.
#[test]
fn restore_refuses_a_newer_palace_format() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let path = snap.dir.join(SNAPSHOT_MANIFEST);
    let mut m = snap.manifest.clone();
    m.palace_format = Some(99);
    std::fs::write(&path, serde_json::to_vec(&m).expect("json")).expect("write");
    let target = fx.root.join("newer").join(PALACE);
    let err = restore_palace_snapshot(&snap.dir, &target).expect_err("newer");
    assert!(matches!(err, SnapshotError::PalaceFormatTooNew { found: 99, .. }), "{err}");
    assert!(!target.exists());
}

fn injected() -> io::Error {
    io::Error::other("injected fault")
}

fn fail_copy(step: SnapshotStep, _: &Path) -> io::Result<()> {
    if step == SnapshotStep::CopyDb { Err(injected()) } else { Ok(()) }
}

fn fail_fsync(step: SnapshotStep, _: &Path) -> io::Result<()> {
    if step == SnapshotStep::Fsync { Err(injected()) } else { Ok(()) }
}

fn fail_manifest(step: SnapshotStep, _: &Path) -> io::Result<()> {
    if step == SnapshotStep::WriteManifest { Err(injected()) } else { Ok(()) }
}

/// Alters `palace.json` after its fsync, before it is re-hashed.
fn alter_after_fsync(step: SnapshotStep, path: &Path) -> io::Result<()> {
    if step == SnapshotStep::Verify && path.ends_with("palace.json") {
        std::fs::write(path, b"altered")?;
    }
    Ok(())
}

/// Run a snapshot with `hook`; it must fail and leave no snapshot directory.
fn assert_fails_closed(hook: super::SnapshotHook) -> SnapshotError {
    let fx = fixture();
    let dest = fx.root.join("snap");
    let err = snapshot_palace(&fx.handle, Some(&fx.chat), Some(&dest), hook)
        .expect_err("fault must surface as Err");
    assert!(!dest.exists(), "no partial snapshot may exist");
    assert!(!fx.root.join("snap.tmp").exists(), "staging must be removed");
    err
}

/// Fail-open check (#9282): a copy failure is an error, not a skip.
#[test]
fn a_kg_copy_failure_leaves_no_snapshot() {
    let err = assert_fails_closed(fail_copy);
    assert!(matches!(err, SnapshotError::Io { op: "copy into", .. }), "{err}");
}

/// Fail-open check (#9282): an fsync failure is an error, not a warning.
#[test]
fn an_fsync_failure_leaves_no_snapshot() {
    let err = assert_fails_closed(fail_fsync);
    assert!(matches!(err, SnapshotError::Io { op: "fsync", .. }), "{err}");
}

/// Fail-open check (#9282): no manifest, no snapshot.
#[test]
fn a_manifest_write_failure_leaves_no_snapshot() {
    let err = assert_fails_closed(fail_manifest);
    assert!(matches!(err, SnapshotError::Io { op: "write", .. }), "{err}");
}

/// Fail-open check (#9282): a copy that differs after fsync is a checksum error.
#[test]
fn a_copy_altered_after_fsync_fails_verification() {
    let err = assert_fails_closed(alter_after_fsync);
    assert!(matches!(err, SnapshotError::ChecksumMismatch { .. }), "{err}");
}

/// Fail-open check (#9282): an unreadable journal fails the snapshot.
#[test]
fn an_unreadable_journal_fails_the_snapshot() {
    let fx = fixture();
    let journal = fx.palace_dir().join(MAINTENANCE_LOG_FILENAME);
    std::fs::remove_file(&journal).expect("rm");
    std::fs::create_dir(&journal).expect("a directory cannot be read as a file");
    let dest = fx.root.join("snap");
    let err = snapshot_palace(&fx.handle, Some(&fx.chat), Some(&dest), no_fault)
        .expect_err("journal read must fail");
    assert!(matches!(err, SnapshotError::Io { op: "read", .. }), "{err}");
    assert!(!dest.exists() && !fx.root.join("snap.tmp").exists());
}

/// Fail-open check (#9282): a restore write failure leaves no target behind.
#[test]
fn a_restore_write_failure_leaves_no_target() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let parent = fx.root.join("restore-parent");
    std::fs::create_dir_all(&parent).expect("parent");
    let err = restore_with(&snap.dir, &parent.join(PALACE), fail_fsync).expect_err("fault");
    assert!(matches!(err, SnapshotError::Io { op: "fsync", .. }), "{err}");
    assert!(listing(&parent).is_empty(), "target and staging must be absent");
}

/// Fail-open check (#9282): a restored copy that differs after fsync is a
/// checksum error, and no target is left behind.
#[test]
fn a_restored_copy_altered_after_fsync_fails_verification() {
    let fx = fixture();
    let snap = fx.handle.snapshot(Some(&fx.chat), None).expect("snapshot");
    let parent = fx.root.join("restore-parent");
    std::fs::create_dir_all(&parent).expect("parent");
    let err = restore_with(&snap.dir, &parent.join(PALACE), alter_after_fsync).expect_err("x");
    assert!(matches!(err, SnapshotError::ChecksumMismatch { .. }), "{err}");
    assert!(listing(&parent).is_empty(), "target and staging must be absent");
}

/// Fail-open check (#9282): the snapshot refuses to overwrite a destination.
#[test]
fn an_existing_destination_is_refused() {
    let fx = fixture();
    let dest = fx.root.join("snap");
    std::fs::create_dir_all(&dest).expect("dest");
    let err = snapshot_palace(&fx.handle, Some(&fx.chat), Some(&dest), no_fault)
        .expect_err("existing destination");
    assert!(matches!(err, SnapshotError::DestinationExists { .. }), "{err}");
    assert!(listing(&dest).is_empty());
}
