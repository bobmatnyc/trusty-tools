//! Tests for the daemon singleton lock (#8760).
//!
//! Every interleaving here is forced by ordering the steps, or by a barrier;
//! nothing sleeps to synchronise and nothing asserts on wall-clock time.

use super::*;
use std::sync::{Arc, Barrier};

/// A pid no process can hold: above every platform's `pid_max`, and below
/// `i32::MAX` so `kill` does not read it as a process group.
const DEAD_PID: u32 = 2_000_000_000;

fn open_rw(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open the lock file")
}

/// #8760: a starter that arrives while the holder is between its flock and its
/// pid write must be refused, never granted a lock on a fresh inode.
///
/// Why: the file still names the predecessor's dead pid in that window. The
/// old stale-lock path read that pid, unlinked the live holder's file and
/// locked a new inode, so two daemons each held an exclusive lock.
/// What: forces the interleaving with no threads — a predecessor's dead pid is
/// on disk, the holder takes the flock and writes nothing, then a second
/// `acquire_lock` runs. It must return `AlreadyRunning`, and the path must
/// still name the holder's inode.
/// Test: this function.
#[cfg(unix)]
#[test]
fn a_starter_in_the_pid_write_window_is_refused_not_given_a_second_lock() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    // The predecessor exited; its pid is on disk and no longer alive.
    std::fs::write(&path, DEAD_PID.to_string()).unwrap();
    assert!(!super::super::pid_alive(DEAD_PID));

    // The holder: flock taken, own pid not yet written.
    let holder = open_rw(&path);
    holder.try_lock_exclusive().unwrap();
    let held_ino = holder.metadata().unwrap().ino();

    let second = acquire_lock(&path);
    let path_ino = std::fs::metadata(&path).map(|m| m.ino()).ok();

    assert!(
        matches!(second, Err(DaemonError::AlreadyRunning(_))),
        "a second starter was granted the daemon lock while a live process held it: {second:?}"
    );
    assert_eq!(
        path_ino,
        Some(held_ino),
        "the live holder's lock file was unlinked and replaced"
    );
}

/// #8760: starters released together over a predecessor's dead pid produce
/// exactly one holder; every other one is `AlreadyRunning`.
#[test]
fn concurrent_starters_yield_exactly_one_holder() {
    const STARTERS: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let path = Arc::new(dir.path().join("daemon.lock"));
    std::fs::write(path.as_ref(), DEAD_PID.to_string()).unwrap();
    let barrier = Arc::new(Barrier::new(STARTERS));

    let results: Vec<Result<File, DaemonError>> = (0..STARTERS)
        .map(|_| {
            let (path, barrier) = (Arc::clone(&path), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                acquire_lock(&path)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().expect("starter thread panicked"))
        .collect();

    // Every returned `File` is still alive here, so each `Ok` is a live holder.
    let holders = results.iter().filter(|r| r.is_ok()).count();
    let refused = results
        .iter()
        .filter(|r| matches!(r, Err(DaemonError::AlreadyRunning(_))))
        .count();
    assert_eq!(
        holders, 1,
        "exactly one starter may hold the lock: {results:?}"
    );
    assert_eq!(
        refused,
        STARTERS - 1,
        "every other starter is refused: {results:?}"
    );
}

/// A flock on an inode the path no longer names grants nothing (#8760).
#[test]
fn a_lock_on_an_unlinked_inode_is_reported_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    std::fs::write(&path, "").unwrap();

    // A starter opens the path, then loses the race to an unlink and a new
    // holder before it reaches its flock.
    let late = open_rw(&path);
    std::fs::remove_file(&path).unwrap();
    let _holder = acquire_lock(&path).expect("the new inode is free");

    let attempt = lock_opened(&late, &path).expect("lock_opened must not error");
    assert_eq!(attempt, LockAttempt::Replaced);
}

/// The holder's pid replaces whatever the file named before.
#[test]
fn acquire_lock_records_the_holder_pid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    std::fs::write(&path, format!("{DEAD_PID}\ntrailing")).unwrap();

    let _held = acquire_lock(&path).expect("free lock");

    let recorded = std::fs::read_to_string(&path).unwrap();
    assert_eq!(recorded, std::process::id().to_string());
}

/// Error arm: a pid that cannot be written is an `Err`, never ignored.
#[test]
fn write_holder_pid_fails_on_a_read_only_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    std::fs::write(&path, "").unwrap();
    let read_only = File::open(&path).unwrap();

    assert!(write_holder_pid(&read_only, 42).is_err());
}

/// Error arm: an unopenable lock path is an I/O error, not a grant and not a
/// false `AlreadyRunning`.
#[test]
fn acquire_lock_propagates_an_open_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-dir").join("daemon.lock");

    let result = acquire_lock(&path);
    assert!(matches!(result, Err(DaemonError::Io(_))), "{result:?}");
}

/// #8760: cleanup must not delete a live holder's lock or port file, even
/// when the lock file still names a dead predecessor.
#[test]
fn cleanup_leaves_a_held_lock_and_its_port_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    let port = dir.path().join("daemon.port");
    std::fs::write(&path, DEAD_PID.to_string()).unwrap();
    std::fs::write(&port, "7878").unwrap();
    let holder = open_rw(&path);
    holder.try_lock_exclusive().unwrap();

    let outcome = remove_daemon_files_if_unheld(&path, &[&port]).unwrap();

    assert_eq!(outcome, StaleLockRemoval::HeldByLiveDaemon);
    assert!(path.exists(), "a held lock file was unlinked");
    assert!(port.exists(), "a live daemon's port file was deleted");
}

/// An unheld lock and its port file are removed; a missing lock is `Absent`.
#[test]
fn cleanup_removes_an_unheld_lock_and_its_port_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    let port = dir.path().join("daemon.port");
    std::fs::write(&path, DEAD_PID.to_string()).unwrap();
    std::fs::write(&port, "7878").unwrap();

    let outcome = remove_daemon_files_if_unheld(&path, &[&port]).unwrap();

    assert_eq!(outcome, StaleLockRemoval::Removed);
    assert!(!path.exists() && !port.exists());
    assert_eq!(
        remove_daemon_files_if_unheld(&path, &[&port]).unwrap(),
        StaleLockRemoval::Absent
    );
}
