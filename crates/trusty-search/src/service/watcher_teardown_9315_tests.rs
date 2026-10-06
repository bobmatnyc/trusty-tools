//! #9315: a watcher drop that never returns cannot hang `WatcherTask`.
//!
//! Each test installs a `BlockingGuard` in place of the OS watcher. Its drop
//! blocks until the test releases it, as `FsEventWatcher::stop` blocks when
//! fseventsd does not answer. No test touches fseventsd.

use super::*;
use crate::service::watch_loop::WatcherTask;
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt as _;

/// The teardown bound every test task carries.
const BOUND: Duration = Duration::from_millis(200);
/// How long a test waits before it calls a stop hung.
const OUTER: Duration = Duration::from_secs(2);

/// Stands in for a wedged `FsEventWatcher`: its drop blocks until released.
struct BlockingGuard(mpsc::Receiver<()>);

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        // Returns once the test sends on, or drops, the release sender.
        let _ = self.0.recv();
    }
}

/// A task whose watcher drop blocks until the returned sender is dropped.
/// Must run inside a tokio runtime (it spawns the consumer stand-in).
fn stuck_task(label: &str) -> (WatcherTask, mpsc::Sender<()>) {
    let (release, rx) = mpsc::channel();
    let join = tokio::spawn(std::future::pending::<()>());
    let task = WatcherTask::with_guard_for_test(Box::new(BlockingGuard(rx)), join, label, BOUND);
    (task, release)
}

/// Poll the process-wide counter until it reads `expected` or `OUTER` passes.
fn counter_settles_at(expected: usize) -> bool {
    let deadline = Instant::now() + OUTER;
    while Instant::now() < deadline {
        if stuck_teardowns() == expected {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stuck_teardowns() == expected
}

/// Records the message of every WARN event seen by its dispatcher.
#[derive(Clone, Default)]
struct WarnCapture(Arc<Mutex<Vec<String>>>);

impl WarnCapture {
    fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(tracing_subscriber::registry().with(self.clone()))
    }

    fn warnings(&self) -> Vec<String> {
        self.0.lock().expect("capture lock").clone()
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut message = MessageField(String::new());
        event.record(&mut message);
        self.0.lock().expect("capture lock").push(message.0);
    }
}

struct MessageField(String);

impl tracing::field::Visit for MessageField {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// Exactly one warning, naming the watcher and the bound.
fn assert_one_stuck_warning(capture: &WarnCapture, label: &str) {
    let warnings = capture.warnings();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one warn per stuck teardown: {warnings:?}"
    );
    assert!(
        warnings[0].contains(label) && warnings[0].contains(&format!("{BOUND:?}")),
        "the warning must name the watcher and the bound: {warnings:?}"
    );
}

/// `stop` returns at the bound when the watcher drop never returns, counts the
/// detached thread, warns once, and uncounts it when the drop finally ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(watcher_teardown)]
async fn stop_returns_within_the_bound_when_the_watcher_drop_never_returns() {
    use tracing::instrument::WithSubscriber as _;
    let base = stuck_teardowns();
    let capture = WarnCapture::default();
    let label = "index stuck-9315 (/tmp/stuck-9315)";
    let (task, release) = stuck_task(label);

    let started = Instant::now();
    // Spawned, so a stop that wedges its worker cannot also wedge this timer.
    let stop = tokio::spawn(task.stop().with_subscriber(capture.dispatch()));
    let outcome = tokio::time::timeout(OUTER, stop).await;
    let elapsed = started.elapsed();
    let stuck_while_held = stuck_teardowns();
    // Release before asserting, so a failure never leaves a worker wedged.
    drop(release);

    assert!(
        matches!(outcome, Ok(Ok(()))),
        "stop must return within {OUTER:?} while the watcher drop blocks; got {outcome:?}"
    );
    assert!(
        elapsed >= BOUND,
        "stop returned before the bound: {elapsed:?}"
    );
    assert_eq!(stuck_while_held, base + 1, "one stuck teardown is counted");
    assert_one_stuck_warning(&capture, label);
    assert!(
        tokio::task::block_in_place(|| counter_settles_at(base)),
        "the released teardown must leave the counter"
    );
}

/// N stuck stops leak one counted thread each, do not hold runtime shutdown,
/// and releasing their guards drains the counter back to where it started.
#[test]
#[serial_test::serial(watcher_teardown)]
fn repeated_stuck_stops_count_one_thread_each_and_release_drains_the_counter() {
    const N: usize = 3;
    let base = stuck_teardowns();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");

    let mut releases = Vec::new();
    let mut all_returned = true;
    for i in 0..N {
        let (returned, release) = rt.block_on(async {
            let (task, release) = stuck_task(&format!("index stuck-{i}"));
            let stop = tokio::time::timeout(OUTER, tokio::spawn(task.stop())).await;
            (stop.is_ok(), release)
        });
        all_returned &= returned;
        releases.push(release);
        if !returned {
            // A wedged stop holds a worker; stop before both are held.
            break;
        }
    }
    if !all_returned {
        drop(releases);
        panic!("every stuck stop must return within {OUTER:?}");
    }
    assert_eq!(
        stuck_teardowns(),
        base + N,
        "one counted thread per stuck stop"
    );

    // The detached teardown threads are not runtime threads, so shutting the
    // runtime down does not wait for them.
    let started = Instant::now();
    drop(rt);
    let shutdown = started.elapsed();
    drop(releases);
    assert!(shutdown < OUTER, "runtime shutdown waited {shutdown:?}");
    assert!(
        counter_settles_at(base),
        "released teardowns must leave the counter"
    );
}

/// Dropping a `WatcherTask` without `stop` is bounded the same way.
#[test]
#[serial_test::serial(watcher_teardown)]
fn dropping_a_watcher_task_is_bounded_too() {
    let base = stuck_teardowns();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let label = "index drop-9315 (/tmp/drop-9315)";
    let (task, release) = rt.block_on(async { stuck_task(label) });
    let capture = WarnCapture::default();
    let dispatch = capture.dispatch();

    let (dropped_tx, dropped_rx) = mpsc::channel();
    std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatch, || drop(task));
        let _ = dropped_tx.send(());
    });
    let returned = dropped_rx.recv_timeout(OUTER).is_ok();
    let stuck_while_held = stuck_teardowns();
    drop(release);

    assert!(
        returned,
        "drop must return within {OUTER:?} while the watcher drop blocks"
    );
    assert_eq!(stuck_while_held, base + 1, "one stuck teardown is counted");
    assert_one_stuck_warning(&capture, label);
    assert!(
        counter_settles_at(base),
        "the released teardown must leave the counter"
    );
}

/// A teardown that finishes inside the bound is neither counted nor warned.
#[tokio::test]
#[serial_test::serial(watcher_teardown)]
async fn a_prompt_teardown_is_neither_counted_nor_warned() {
    use tracing::instrument::WithSubscriber as _;
    let base = stuck_teardowns();
    let capture = WarnCapture::default();
    let join = tokio::spawn(std::future::pending::<()>());
    let task = WatcherTask::with_guard_for_test(Box::new(()), join, "index prompt-9315", BOUND);

    task.stop().with_subscriber(capture.dispatch()).await;

    assert_eq!(stuck_teardowns(), base, "a prompt teardown is not stuck");
    assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());
}
