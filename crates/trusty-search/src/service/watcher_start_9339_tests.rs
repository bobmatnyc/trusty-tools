//! #9339: a watcher start that never returns cannot hang its caller.
//!
//! Each test hands `start_bounded` a closure that blocks until the test
//! releases it, as `FsEventWatcher::watch` blocks when fseventsd does not
//! answer `FSEventStreamStart`. No test needs fseventsd to stall.

use super::*;
use crate::service::watcher_teardown::tests::WarnCapture;
use std::sync::atomic::AtomicBool;

/// The start bound every test passes.
const BOUND: Duration = Duration::from_millis(200);
/// How long a test waits before it calls a start hung.
const OUTER: Duration = Duration::from_secs(2);

/// Run `start_bounded` on its own thread, under `capture`, and wait at most
/// `OUTER` for it. `None` means the call itself hung.
fn start_on_thread<T, F>(root: &Path, capture: &WarnCapture, start: F) -> Option<Result<T>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let (done_tx, done_rx) = mpsc::channel();
    let dispatch = capture.dispatch();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let result =
            tracing::dispatcher::with_default(&dispatch, || start_bounded(&root, BOUND, start));
        let _ = done_tx.send(result);
    });
    done_rx.recv_timeout(OUTER).ok()
}

/// Poll the stuck-start count until it reads `expected` or `OUTER` passes.
fn stuck_settles_at(expected: usize) -> bool {
    let deadline = Instant::now() + OUTER;
    while Instant::now() < deadline {
        if stuck_starts() == expected {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stuck_starts() == expected
}

/// Sets its flag when dropped: a late watcher that was shut down.
struct Reclaimed(Arc<AtomicBool>);

impl Drop for Reclaimed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A start that never returns gives its caller an error at the bound, counts
/// one stuck thread, and warns once naming the root and the bound.
#[test]
#[serial_test::serial(watcher_start)]
fn a_start_that_never_returns_is_bounded_and_warns_with_the_root() {
    let base = stuck_starts();
    let capture = WarnCapture::default();
    let root = PathBuf::from("/tmp/stuck-start-9339");
    let (release, held) = mpsc::channel::<()>();

    let started = Instant::now();
    let outcome = start_on_thread(&root, &capture, move || {
        let _ = held.recv();
        Ok(())
    });
    let elapsed = started.elapsed();
    let stuck_while_held = stuck_starts();
    // Release before asserting, so a failure never leaves a thread blocked.
    drop(release);

    let Some(result) = outcome else {
        panic!("start must return within {OUTER:?} while the OS start blocks");
    };
    let message = match result {
        Ok(()) => panic!("a timed-out start must not report a running watcher"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        elapsed >= BOUND,
        "start returned before the bound: {elapsed:?}"
    );
    assert!(
        message.contains("/tmp/stuck-start-9339") && message.contains(&format!("{BOUND:?}")),
        "the error must name the root and the bound: {message}"
    );
    assert_eq!(stuck_while_held, base + 1, "one stuck start is counted");
    let warnings = capture.warnings();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one warn per stuck start: {warnings:?}"
    );
    assert!(
        warnings[0].contains("/tmp/stuck-start-9339")
            && warnings[0].contains(&format!("{BOUND:?}")),
        "the warning must name the root and the bound: {warnings:?}"
    );
    assert!(
        stuck_settles_at(base),
        "the released start must leave the count"
    );
}

/// A start that arrives after its caller gave up is dropped on its own
/// thread. While it is stuck, a second start for the root fails at once
/// without running; once it is reclaimed, the root starts normally.
#[test]
#[serial_test::serial(watcher_start)]
fn a_late_start_is_reclaimed_and_its_root_reopens() {
    let base = stuck_starts();
    let capture = WarnCapture::default();
    let root = PathBuf::from("/tmp/late-start-9339");
    let (release, held) = mpsc::channel::<()>();
    let reclaimed = Arc::new(AtomicBool::new(false));
    let late = Reclaimed(Arc::clone(&reclaimed));

    let first = start_on_thread(&root, &capture, move || {
        let _ = held.recv();
        Ok(late)
    });
    // The second start must not run its closure while the first is stuck.
    let second_ran = Arc::new(AtomicBool::new(false));
    let ran = Arc::clone(&second_ran);
    let second = start_on_thread(&root, &capture, move || {
        ran.store(true, Ordering::SeqCst);
        Ok(())
    });
    let stuck_while_held = stuck_starts();
    drop(release);

    assert!(
        matches!(first, Some(Err(_))),
        "the first start must time out with an error"
    );
    let skipped = match second {
        Some(Err(e)) => format!("{e:#}"),
        other => panic!("a start for a stuck root must fail at once: {other:?}"),
    };
    assert!(skipped.contains("skipped"), "{skipped}");
    assert!(
        !second_ran.load(Ordering::SeqCst),
        "the gated start must not run"
    );
    assert_eq!(stuck_while_held, base + 1, "the gated start adds no thread");

    assert!(
        stuck_settles_at(base),
        "the late start must leave the count"
    );
    assert!(
        reclaimed.load(Ordering::SeqCst),
        "the late watcher must be dropped on its thread"
    );
    let third = start_on_thread(&root, &capture, || Ok(42));
    assert!(
        matches!(third, Some(Ok(42))),
        "the root must start once its stuck start is reclaimed"
    );
}

/// The manager never records a watcher whose start timed out: a root with a
/// stuck start is not watched, so `is_watching` (and the status `watcher.active`
/// field built on it) reads `false` until the root starts for real.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(watcher_start)]
async fn a_root_whose_start_timed_out_is_not_reported_as_watched() {
    use crate::core::registry::{IndexHandle, IndexId};
    use crate::core::CodeIndexer;
    use crate::service::network_fs::MountKind;
    use crate::service::watcher_manager::WatcherManager;

    let base = stuck_starts();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let id = IndexId::new("stuck-start-9339");
    let indexer = Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(
        "stuck-start-9339",
        root.clone(),
    )));
    let handle = Arc::new(IndexHandle::bare(id.clone(), indexer, root.clone()));

    // Wedge this root's start the way a silent fseventsd does.
    let (release, held) = mpsc::channel::<()>();
    let stuck_root = root.clone();
    let wedged = tokio::task::spawn_blocking(move || {
        start_bounded(&stuck_root, BOUND, move || {
            let _ = held.recv();
            Ok(())
        })
    })
    .await
    .expect("join the wedged start");
    assert!(wedged.is_err(), "the wedged start must time out");

    let manager = WatcherManager::new();
    let spawn = manager.spawn_for_index_with_mount_kind(&handle, MountKind::Local);
    let spawned_in_time = tokio::time::timeout(OUTER, spawn).await.is_ok();
    let watching_while_stuck = manager.is_watching(&id).await;
    drop(release);

    assert!(
        spawned_in_time,
        "spawn_for_index must not hang on a stuck start"
    );
    assert!(
        !watching_while_stuck,
        "a watcher whose start did not finish must not be reported as running"
    );
    assert!(
        tokio::task::block_in_place(|| stuck_settles_at(base)),
        "the released start must leave the count"
    );
    manager
        .spawn_for_index_with_mount_kind(&handle, MountKind::Local)
        .await;
    assert!(
        manager.is_watching(&id).await,
        "the root must watch again once its stuck start is reclaimed"
    );
    manager.stop_all().await;
}
