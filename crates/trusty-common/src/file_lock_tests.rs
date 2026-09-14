//! Tests for [`super`] — the cross-process advisory lock primitive (#5344).

use super::{
    DEFAULT_LOCK_TIMEOUT, LockTimeout, lock_path, with_exclusive_lock, with_exclusive_lock_timeout,
};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Names the file whose lock the child-process helper below must hold.
const HOLD_LOCK_PATH_ENV: &str = "TRUSTY_COMMON_FILE_LOCK_TEST_HOLD";

/// What the child prints once it holds the lock.
///
/// Matched with `contains`, never equality: under `--nocapture` libtest's own
/// unterminated `test <name> ... ` progress text shares the line.
const HELD_MARKER: &str = "TRUSTY-FILE-LOCK-HELD";

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

/// Take the sidecar's lock through a descriptor this module did not hand out.
///
/// Why: the contending holder in a timeout test must not be the entry point
/// under test. `flock(2)` conflicts per open file description, so a second
/// descriptor contends exactly as a second process does — which is what
/// `with_exclusive_lock_serialises_separate_descriptors` already relies on.
/// What: opens (creating if needed) `lock_path(path)`. The caller takes the
/// exclusive lock off the returned value and holds the guard.
/// Test: used by the timeout tests below.
fn open_sidecar(path: &Path) -> fd_lock::RwLock<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))
        .expect("open the sidecar directly");
    fd_lock::RwLock::new(file)
}

/// Kill the child holder however the parent test ends, panic included.
struct ChildHolder(Child);

impl Drop for ChildHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Why: #7762 — an unbounded `write()` turned a wedged holder into a silent
/// hang on the interactive `tm launch` path. The bound must expire into a
/// typed, self-describing error, and the closure must not run.
/// What: holds the sidecar through a second descriptor, then asserts the
/// acquisition times out, took at least its bound, names the sidecar and the
/// recorded pid, and never entered the section.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_timeout_errors_while_another_descriptor_holds_it() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");
    // One uncontended pass first, so the sidecar names a pid to read back.
    with_exclusive_lock(&path, || {}).expect("uncontended acquisition");

    let mut sidecar = open_sidecar(&path);
    let _held = sidecar.write().expect("hold the sidecar");

    let ran = AtomicUsize::new(0);
    let bound = Duration::from_millis(200);
    let started = Instant::now();
    let err = with_exclusive_lock_timeout(&path, bound, || {
        ran.fetch_add(1, Ordering::SeqCst);
    })
    .expect_err("a held lock must never read as acquired");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::TimedOut,
        "expiry must be reported as a timeout: {err:?}"
    );
    assert!(
        started.elapsed() >= bound,
        "returned before the bound elapsed: {:?}",
        started.elapsed()
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the closure must never run unlocked"
    );

    let timeout = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<LockTimeout>())
        .expect("the io::Error must carry a typed LockTimeout");
    assert_eq!(timeout.lock_path, lock_path(&path));
    assert_eq!(timeout.waited, bound);
    assert_eq!(
        timeout.holder_pid,
        Some(std::process::id()),
        "the pid recorded at acquisition must survive into the error"
    );

    let rendered = err.to_string();
    assert!(
        rendered.contains(&lock_path(&path).display().to_string()),
        "the message must name the sidecar: {rendered}"
    );
    assert!(
        rendered.contains(&std::process::id().to_string()),
        "the message must name the holder pid: {rendered}"
    );
}

/// Why: fail-closed. A sidecar whose pid cannot be read is a worse diagnostic,
/// never a better outcome — it must not turn a timeout into a success.
/// What: seeds the sidecar with content that is not a pid, holds it, and
/// asserts the wait still errors and simply says the holder is unknown.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_timeout_reports_unknown_pid_for_a_garbage_sidecar() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");
    std::fs::write(lock_path(&path), b"not-a-pid\n").expect("seed garbage");

    let mut sidecar = open_sidecar(&path);
    let _held = sidecar.write().expect("hold the sidecar");

    let ran = AtomicUsize::new(0);
    let err = with_exclusive_lock_timeout(&path, Duration::from_millis(120), || {
        ran.fetch_add(1, Ordering::SeqCst);
    })
    .expect_err("an unreadable holder pid must never read as acquired");

    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err:?}");
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the closure must never run unlocked"
    );
    let timeout = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<LockTimeout>())
        .expect("the io::Error must carry a typed LockTimeout");
    assert_eq!(timeout.holder_pid, None, "garbage is not a pid");
    assert!(
        err.to_string().contains("holder pid unknown"),
        "{}",
        err.to_string()
    );
}

/// Why: the pid is diagnostic content, so a sidecar carrying anything else —
/// a leftover from an older build, a stray write — must not block a free lock.
/// What: seeds garbage, acquires, and asserts the acquirer replaced it.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_acquires_over_a_garbage_sidecar() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");
    std::fs::write(lock_path(&path), b"not-a-pid\n").expect("seed garbage");

    let ran = with_exclusive_lock(&path, || 7).expect("a free lock must be acquired");

    assert_eq!(ran, 7);
    assert_eq!(
        std::fs::read_to_string(lock_path(&path))
            .expect("read the sidecar")
            .trim(),
        std::process::id().to_string(),
        "the acquirer must replace the garbage with its own pid"
    );
}

/// Why: a waiter can only name the holder if the holder left its name, and the
/// sidecar must carry nothing else — the whole file is the pid.
/// What: one uncontended acquisition, then reads the sidecar back.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_records_the_acquiring_pid() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");

    with_exclusive_lock(&path, || {}).expect("uncontended acquisition");

    assert_eq!(
        std::fs::read_to_string(lock_path(&path)).expect("read the sidecar"),
        std::process::id().to_string(),
        "the sidecar's whole content is the acquiring pid"
    );
}

/// Why: bounding acquisition must not change what an uncontended caller sees —
/// every existing caller keeps its behaviour, the bound is the only addition.
/// What: two sequential default-path acquisitions, each returning its closure's
/// value without approaching the default bound.
/// Test: this IS the test.
#[test]
fn with_exclusive_lock_default_path_is_unchanged_when_uncontended() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");

    let started = Instant::now();
    let first = with_exclusive_lock(&path, || 42).expect("uncontended acquisition");
    let second = with_exclusive_lock(&path, || 43).expect("the lock must be free again");

    assert_eq!((first, second), (42, 43));
    assert!(
        started.elapsed() < DEFAULT_LOCK_TIMEOUT,
        "an uncontended acquisition must not wait: {:?}",
        started.elapsed()
    );
}

/// Why: the pid in the error only earns its place if it is ANOTHER process's —
/// the wedged-holder case #7762 is about is always cross-process.
/// What: re-invokes this test binary as a child that takes the lock through
/// this module's own entry point (so it records its pid the way any real writer
/// does), waits for it to report holding, and asserts the parent's bounded wait
/// times out naming that child's pid.
/// Test: this IS the test; `holds_the_lock_until_killed` is its child half.
#[test]
fn with_exclusive_lock_timeout_names_the_pid_of_a_holding_process() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("guarded.toml");
    let exe = std::env::current_exe().expect("the test binary's own path");

    let spawned = Command::new(exe)
        .args([
            "--exact",
            "file_lock::tests::holds_the_lock_until_killed",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLD_LOCK_PATH_ENV, &path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the holder child");
    let mut child = ChildHolder(spawned);
    let child_pid = child.0.id();
    let stdout = child.0.stdout.take().expect("piped stdout");

    let mut reader = BufReader::new(stdout);
    let mut transcript = String::new();
    let mut line = String::new();
    let mut holding = false;
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        holding = line.contains(HELD_MARKER);
        transcript.push_str(&line);
        line.clear();
        if holding {
            break;
        }
    }
    assert!(
        holding,
        "the child never reported holding the lock; it printed: {transcript}"
    );

    let ran = AtomicUsize::new(0);
    let err = with_exclusive_lock_timeout(&path, Duration::from_millis(300), || {
        ran.fetch_add(1, Ordering::SeqCst);
    })
    .expect_err("a lock held by another process must never read as acquired");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the closure must never run unlocked"
    );
    let timeout = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<LockTimeout>())
        .expect("the io::Error must carry a typed LockTimeout");
    assert_eq!(
        timeout.holder_pid,
        Some(child_pid),
        "the error must name the holding process: {err}"
    );
    assert!(
        err.to_string().contains(&child_pid.to_string()),
        "{}",
        err.to_string()
    );
    assert!(
        err.to_string()
            .contains(&lock_path(&path).display().to_string()),
        "{}",
        err.to_string()
    );
}

/// Child half of `with_exclusive_lock_timeout_names_the_pid_of_a_holding_process`.
///
/// Why: a second PROCESS is the only honest way to prove the pid in the error
/// belongs to someone else. Re-invoking this binary keeps the holder on this
/// module's own entry point — `flock(1)` is absent on macOS and would record no
/// pid anyway.
/// What: returns immediately unless `HOLD_LOCK_PATH_ENV` names a path, so an
/// ordinary suite run costs nothing. When set, it acquires the lock, prints
/// `HELD_MARKER`, and sleeps until the parent kills it.
/// Test: this IS the helper.
#[test]
fn holds_the_lock_until_killed() {
    let Ok(raw) = std::env::var(HOLD_LOCK_PATH_ENV) else {
        return;
    };
    with_exclusive_lock(&PathBuf::from(raw), || {
        println!("{HELD_MARKER}");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(30));
    })
    .expect("the child holder must acquire a free lock");
}
