//! Tests for [`super`] — the cross-process advisory lock primitive (#5344).

use super::{lock_path, with_exclusive_lock};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

/// The sidecar sits next to the guarded file and is named after it.
#[test]
fn lock_path_is_a_sidecar() {
    let p = Path::new("/tmp/some/dir/indexes.toml");
    assert_eq!(
        lock_path(p),
        Path::new("/tmp/some/dir/indexes.toml.lock"),
        "the lock must be a sibling sidecar, never the guarded file itself"
    );
}

/// Why: the whole point of the module — two independently-opened descriptors on
/// the same sidecar must not both be inside the critical section. Each thread
/// opens its OWN descriptor, which is exactly the conflict a separate process
/// produces; no in-process mutex is involved.
/// What: 8 threads × 5 rounds each increment a shared counter while asserting
/// that no other thread is inside the section.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_serialises_separate_descriptors() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");
    let inside = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let path = path.clone();
            let inside = &inside;
            let total = &total;
            scope.spawn(move || {
                for _ in 0..5 {
                    with_exclusive_lock(&path, || {
                        assert_eq!(
                            inside.fetch_add(1, Ordering::SeqCst),
                            0,
                            "two holders inside the critical section at once"
                        );
                        std::thread::yield_now();
                        total.fetch_add(1, Ordering::SeqCst);
                        inside.fetch_sub(1, Ordering::SeqCst);
                    })
                    .expect("lock acquisition");
                }
            });
        }
    });

    assert_eq!(total.load(Ordering::SeqCst), 40);
}

/// Why: RAII release must survive a panicking closure, or one bad write would
/// wedge every later writer of that file for the process's lifetime.
/// What: panics inside the section, then proves a later acquisition succeeds.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_releases_on_panic() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");

    let panicked = std::panic::catch_unwind(|| {
        let _ = with_exclusive_lock(&path, || panic!("boom"));
    });
    assert!(panicked.is_err(), "the closure's panic must propagate");

    let ran = with_exclusive_lock(&path, || 42).expect("lock must be free again");
    assert_eq!(ran, 42);
}

/// Why: fail-closed. An unusable lock path must be an error and the closure
/// must never run — running it unlocked is the lost-update bug itself.
/// What: points the sidecar's parent at a regular FILE so `create_dir_all`
/// cannot succeed, and asserts the closure was not invoked.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_unopenable_errors() {
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let path = blocker.join("guarded.toml");

    let ran = AtomicUsize::new(0);
    let result = with_exclusive_lock(&path, || {
        ran.fetch_add(1, Ordering::SeqCst);
    });
    assert!(result.is_err(), "an unusable lock path must be an error");
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the closure must never run unlocked"
    );
}
