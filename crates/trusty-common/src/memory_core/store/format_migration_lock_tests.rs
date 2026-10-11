//! #9274 PR3 critic HIGH 1: one format migration of a palace at a time.
//!
//! Why: the daemon's startup hydration and a request — or two processes — can
//! open one format-0 palace at the same moment. Two migrations raced: one
//! deleted the other's record and failed it, or stranded the palace at mirror
//! 1, marker 0 and no record.
//! What: a seam parks the first migration after its record is written while a
//! second writer open of the same palace runs; another process is stood in for
//! by a `flock` this test takes on the palace directory.
//! Test: this file.

use super::tests::{assert_migrated, build_v0, complete_backups, hash_tree};
use super::{MigrationRecord, Seams, Step, open_gate, open_gate_with, tmp_path, write_record};
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::palace_format::MIGRATION_RECORD;
use crate::memory_core::store::palace_store::PalaceStoreError;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// 0 idle; 1 the first migration is parked after its record; 2 the second
/// open has started; 3 the second open has returned.
static PHASE: (Mutex<u8>, Condvar) = (Mutex::new(0), Condvar::new());

fn set_phase(phase: u8) {
    let (lock, changed) = &PHASE;
    *lock.lock().unwrap_or_else(PoisonError::into_inner) = phase;
    changed.notify_all();
}

/// Wait for `phase` for at most `bound`; whether it was reached.
fn reached(phase: u8, bound: Duration) -> bool {
    let (lock, changed) = &PHASE;
    let now = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let (now, _) = changed
        .wait_timeout_while(now, bound, |p| *p < phase)
        .unwrap_or_else(PoisonError::into_inner);
    *now >= phase
}

/// The first migration's seam: after its record is on disk, give a second
/// open of the palace up to two seconds to run to completion.
fn park_after_record(step: Step) -> std::io::Result<()> {
    if step == Step::Record {
        set_phase(1);
        if !reached(2, Duration::from_secs(30)) {
            return Err(std::io::Error::other("the second open never started"));
        }
        reached(3, Duration::from_secs(2));
    }
    Ok(())
}

/// Two writer opens of one format-0 palace in one process — the daemon's
/// hydration and a request — migrate it once: the second waits for the first
/// and finds the palace at format 1. Before the fix the second resumed the
/// first one's migration, removed its record, and the first failed with
/// `NotFound` on that record.
#[test]
fn two_writer_opens_in_one_process_migrate_a_palace_once() {
    let v0 = build_v0("v0-two-openers");
    set_phase(0);
    let dir = v0.dir().to_path_buf();
    let id = v0.palace.id.as_str().to_string();

    let first = {
        let (dir, id) = (dir.clone(), id.clone());
        std::thread::spawn(move || {
            let seams = Seams {
                after: park_after_record,
                ..Seams::default()
            };
            open_gate_with(&dir, &id, OpenIntent::Writer, seams).map_err(|e| format!("{e:#}"))
        })
    };
    assert!(
        reached(1, Duration::from_secs(30)),
        "the first migration never wrote its record"
    );
    let second = std::thread::spawn(move || {
        set_phase(2);
        let result = open_gate(&dir, &id, OpenIntent::Writer).map_err(|e| format!("{e:#}"));
        set_phase(3);
        result
    });

    let first = first.join().expect("first opener");
    let second = second.join().expect("second opener");
    assert_eq!(first, Ok(()), "the first open");
    assert_eq!(second, Ok(()), "the second open");
    assert_migrated(&v0);
    assert_eq!(complete_backups(&v0).len(), 1, "one migration, one backup");
}

/// A migration lock another process holds refuses the open at once: no
/// backup, no record, no palace byte changed. The next open migrates.
#[test]
fn a_palace_locked_by_another_process_is_refused_with_nothing_written() {
    let v0 = build_v0("v0-locked-elsewhere");
    let before = hash_tree(v0.dir());
    let other = std::fs::File::open(v0.dir()).expect("open the palace directory");
    other
        .try_lock()
        .expect("stand in for another process's lock");

    let err = open_gate(v0.dir(), v0.palace.id.as_str(), OpenIntent::Writer)
        .expect_err("a held migration lock refuses the open");
    assert!(
        matches!(
            err.downcast_ref::<PalaceStoreError>(),
            Some(PalaceStoreError::FormatMigrationFailed { .. })
        ),
        "{err:#}"
    );
    assert!(format!("{err:#}").contains("another process"), "{err:#}");
    assert_eq!(before, hash_tree(v0.dir()), "a palace byte changed");
    assert!(
        !v0.dir().join(MIGRATION_RECORD).exists(),
        "a record was written"
    );
    assert!(!v0.backups().exists(), "a backup was started");

    drop(other);
    open_gate(v0.dir(), v0.palace.id.as_str(), OpenIntent::Writer).expect("the next open");
    assert_migrated(&v0);
}

/// Two writes never share a temp file name, in this process or another.
#[test]
fn temp_names_never_repeat() {
    let path = Path::new("/palace/format_migration.json");
    let (a, b) = (tmp_path(path), tmp_path(path));
    assert_ne!(a, b);
    let pid = std::process::id().to_string();
    assert!(a.to_string_lossy().contains(&pid), "{}", a.display());
}

/// A record write that fails at the rename removes its temp file.
#[test]
fn a_failed_record_write_leaves_no_temp_file() {
    let v0 = build_v0("v0-tmp-cleanup");
    std::fs::create_dir_all(v0.dir().join(MIGRATION_RECORD).join("blocker"))
        .expect("a directory where the record goes");
    let record = MigrationRecord {
        palace: v0.palace.id.as_str().to_string(),
        from: 0,
        to: 1,
        backup: PathBuf::from("/nowhere"),
    };
    write_record(v0.dir(), &record).expect_err("the rename onto a directory fails");
    let left: Vec<_> = std::fs::read_dir(v0.dir())
        .expect("read palace dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "temp files left: {left:?}");
}
