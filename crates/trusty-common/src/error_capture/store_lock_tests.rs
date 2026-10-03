//! #8028: the append path must not hold the store mutex across the rotation
//! lock wait and the disk write. A child of `store`, so it can read the
//! private mutex.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::ErrorStore;
use crate::error_capture::rotation::RotationPolicy;
use crate::error_capture::test_hook::{self, Point};
use crate::error_capture::types::CapturedError;

fn record(msg: &str) -> CapturedError {
    CapturedError {
        timestamp_secs: 1_000_000,
        crate_target: "test_crate".to_string(),
        crate_version: "0.1.0".to_string(),
        message: msg.to_string(),
        fields: String::new(),
        file: None,
        line: None,
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        fingerprint: "fp".to_string(),
    }
}

/// #8028 HIGH 2: at the point where the append starts the rotation and the
/// disk write, the store mutex must be free. The hook runs on the appending
/// thread, so `try_lock` fails exactly when that thread still holds it; no
/// timing is involved.
#[test]
fn append_releases_the_store_lock_before_touching_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ErrorStore::with_path(Some(dir.path().join("errors.jsonl")), 10);
    let observed: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
    let _hook = {
        let store = store.clone();
        let observed = Rc::clone(&observed);
        test_hook::install(move |point| {
            if point == Point::BeforeDiskWrite {
                observed.set(Some(store.inner.try_lock().is_ok()));
            }
        })
    };

    store.append(record("hot path"));

    assert_eq!(
        observed.get(),
        Some(true),
        "store mutex held across disk I/O"
    );
    assert_eq!(store.len(), 1);
}

/// #8028 fail-open arm: another descriptor holds the rotation lock and never
/// lets go. The append returns within the hot-path bound, refuses and counts
/// the disk write, keeps the record in memory, and leaves the file as it was.
#[test]
fn a_held_rotation_lock_refuses_the_write_without_blocking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let policy = RotationPolicy {
        max_bytes: 64,
        keep: 1,
    };
    std::fs::write(&path, vec![b'\n'; 128]).expect("seed a due file");
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 10, policy);

    let elapsed = crate::file_lock::with_exclusive_lock(&path, || {
        let started = Instant::now();
        store.append(record("while wedged"));
        started.elapsed()
    })
    .expect("test holds the rotation lock");

    // The pre-fix wait was 2 s; the bound is 50 ms. 1 s leaves slack for a
    // loaded test host without accepting the old wait.
    assert!(
        elapsed < Duration::from_secs(1),
        "append waited {elapsed:?}"
    );
    assert_eq!(store.refused_disk_writes(), 1);
    assert_eq!(store.len(), 1, "the record is kept in memory");
    assert_eq!(std::fs::read(&path).expect("live").len(), 128);
}
