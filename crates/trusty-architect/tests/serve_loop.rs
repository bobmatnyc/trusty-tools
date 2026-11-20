//! #9779: `tm architect serve`'s event loop over a recording cycle.
//!
//! Every watched path is a tempdir; nothing touches a live inbox.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trusty_architect::serve::{
    CYCLE_PANIC, Event, FileWatch, ServeLoop, ServeOptions, ServeStats, Trigger, WatchRoot,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Seen = Arc<Mutex<Vec<(Trigger, Option<String>, Instant)>>>;

fn opts(debounce_ms: u64, tick_ms: u64) -> ServeOptions {
    ServeOptions {
        debounce: Duration::from_millis(debounce_ms),
        tick: Duration::from_millis(tick_ms),
        scan_every: Duration::from_millis(20),
        once: false,
    }
}

/// One loop run: `act` runs on this thread while the loop runs on another;
/// then the stop flag is set. Returns the stats and each cycle's trigger,
/// its `fault` detail (when given) and start time.
fn run_loop(
    opts: ServeOptions,
    files: Option<FileWatch>,
    fault: &str,
    cycle_extra: impl FnMut(u64) + Send,
    act: impl FnOnce(&Sender<Event>, &Seen),
) -> (ServeStats, Vec<(Trigger, Option<String>, Instant)>) {
    let stop = AtomicBool::new(false);
    let seen: Seen = Arc::default();
    let (tx, rx) = channel();
    let stats = std::thread::scope(|s| {
        let seen2 = Arc::clone(&seen);
        let stop = &stop;
        let mut extra = cycle_extra;
        let h = s.spawn(move || {
            let log = |_: &str| {};
            let mut lp = ServeLoop {
                opts,
                stop,
                files,
                control: None,
                events: Some(rx),
                log: &log,
            };
            let mut n = 0;
            lp.run(&mut |trigger, faults| {
                let f = faults.get(fault).map(str::to_owned);
                if let Ok(mut v) = seen2.lock() {
                    v.push((trigger, f, Instant::now()));
                }
                n += 1;
                extra(n);
                Ok(())
            })
        });
        act(&tx, &seen);
        stop.store(true, Ordering::SeqCst);
        h.join()
    });
    let stats = stats.ok().and_then(Result::ok).unwrap_or_default();
    let seen = seen.lock().map(|v| v.clone()).unwrap_or_default();
    (stats, seen)
}

fn wait_for(seen: &Seen, pred: impl Fn(&[(Trigger, Option<String>, Instant)]) -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if seen.lock().is_ok_and(|v| pred(&v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn inbox_watch(dir: &Path) -> FileWatch {
    FileWatch::new(vec![WatchRoot::inbox(dir, Vec::new())])
}

fn events(seen: &[(Trigger, Option<String>, Instant)]) -> usize {
    seen.iter().filter(|c| c.0 == Trigger::Events).count()
}

/// One inbox append starts one cycle, after the debounce and well before
/// the tick.
#[test]
fn inbox_write_triggers_one_cycle_within_debounce() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("events.jsonl"), "")?;
    let mut wrote = None;
    let (_, seen) = run_loop(
        opts(300, 30_000),
        Some(inbox_watch(dir.path())),
        "",
        |_| {},
        |_, seen| {
            wait_for(seen, |v| !v.is_empty());
            std::thread::sleep(Duration::from_millis(100));
            wrote = Some(Instant::now());
            let _ = fs::write(dir.path().join("events.jsonl"), "{\"e\":1}\n");
            wait_for(seen, |v| events(v) >= 1);
            std::thread::sleep(Duration::from_millis(400));
        },
    );
    assert_eq!(seen.first().map(|c| c.0), Some(Trigger::Start));
    assert_eq!(events(&seen), 1, "{seen:?}");
    let at = seen.iter().find(|c| c.0 == Trigger::Events).map(|c| c.2);
    let lag = at.zip(wrote).map(|(at, w)| at.duration_since(w));
    assert!(
        lag.is_some_and(|d| d >= Duration::from_millis(250) && d < Duration::from_millis(1000)),
        "{lag:?}"
    );
    Ok(())
}

/// Ten writes inside one debounce window are one cycle.
#[test]
fn burst_of_inbox_writes_is_one_cycle() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (_, seen) = run_loop(
        opts(500, 30_000),
        Some(inbox_watch(dir.path())),
        "",
        |_| {},
        |_, seen| {
            wait_for(seen, |v| !v.is_empty());
            for i in 0..10 {
                let _ = fs::write(dir.path().join(format!("m{i}.json")), "{}");
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(1200));
        },
    );
    assert_eq!(events(&seen), 1, "{seen:?}");
    Ok(())
}

/// A cycle's own inbox writes are the new baseline, not an event.
#[test]
fn cycle_own_writes_start_no_cycle() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("alerts.jsonl");
    let (_, seen) = run_loop(
        opts(100, 30_000),
        Some(inbox_watch(dir.path())),
        "",
        |n| {
            let _ = fs::write(&path, format!("{n}\n"));
        },
        |_, _| std::thread::sleep(Duration::from_millis(800)),
    );
    assert_eq!(seen.len(), 1, "start cycle only: {seen:?}");
    Ok(())
}

/// An append to a hot transcript is an event.
#[test]
fn transcript_append_is_an_event() -> TestResult {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("-Users-x-proj");
    fs::create_dir_all(&project)?;
    let t = project.join("abc.jsonl");
    fs::write(&t, "{}\n")?;
    let watch = FileWatch::new(vec![WatchRoot::transcripts(dir.path())]);
    let (_, seen) = run_loop(
        opts(100, 30_000),
        Some(watch),
        "",
        |_| {},
        |_, seen| {
            wait_for(seen, |v| !v.is_empty());
            std::thread::sleep(Duration::from_millis(50));
            let _ = fs::write(&t, "{}\n{}\n");
            wait_for(seen, |v| events(v) >= 1);
        },
    );
    assert_eq!(events(&seen), 1, "{seen:?}");
    Ok(())
}

/// With no event the tick still runs cycles.
#[test]
fn tick_fires_with_no_events() {
    let (stats, seen) = run_loop(
        opts(100, 150),
        None,
        "",
        |_| {},
        |_, _| std::thread::sleep(Duration::from_millis(700)),
    );
    assert!(stats.ticks >= 3, "{stats:?}");
    assert_eq!(events(&seen), 0);
}

/// A watch root that cannot be read is a fault at start, carried into
/// every cycle, while the tick keeps cycling.
#[test]
fn watch_root_that_is_not_a_directory_is_a_fault() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("inbox");
    fs::write(&file, "not a dir")?;
    let (stats, seen) = run_loop(
        opts(100, 150),
        Some(inbox_watch(&file)),
        "watch-inbox",
        |_| {},
        |_, _| std::thread::sleep(Duration::from_millis(600)),
    );
    assert!(stats.ticks >= 2, "{stats:?}");
    assert!(
        seen.iter()
            .all(|c| c.1.as_deref().is_some_and(|d| d.contains("cannot read"))),
        "{seen:?}"
    );
    Ok(())
}

/// A source fault event starts a cycle that carries it; a clear drops it.
#[test]
fn fault_event_is_carried_until_cleared() {
    let (_, seen) = run_loop(
        opts(50, 30_000),
        None,
        "tmux-s1",
        |_| {},
        |tx, seen| {
            wait_for(seen, |v| !v.is_empty());
            let fault = Event::Fault {
                kind: "tmux-s1".to_owned(),
                detail: "down 30s".to_owned(),
            };
            let _ = tx.send(fault);
            wait_for(seen, |v| v.len() >= 2);
            let _ = tx.send(Event::Clear("tmux-s1".to_owned()));
            let _ = tx.send(Event::Changed("tmux:s1".to_owned()));
            wait_for(seen, |v| v.len() >= 3);
        },
    );
    let carried: Vec<Option<&str>> = seen.iter().map(|c| c.1.as_deref()).collect();
    assert_eq!(carried, [None, Some("down 30s"), None], "{seen:?}");
}

/// A panicking cycle is caught; the next cycle carries `cycle-panic`, the
/// one after does not, and the loop keeps going.
#[test]
fn panicking_cycle_is_alerted_and_the_loop_continues() {
    let (stats, seen) = run_loop(
        opts(50, 100),
        None,
        CYCLE_PANIC,
        |n| {
            if n == 2 {
                panic!("boom");
            }
        },
        |_, seen| wait_for(seen, |v| v.len() >= 5),
    );
    assert_eq!(stats.panics, 1, "{stats:?}");
    let carried: Vec<bool> = seen.iter().take(4).map(|c| c.1.is_some()).collect();
    assert_eq!(carried, [false, false, true, false], "{seen:?}");
    assert!(seen[2].1.as_deref().is_some_and(|d| d.contains("boom")));
}
