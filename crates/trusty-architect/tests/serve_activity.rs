//! #9779 review: `tm architect serve` reads tmux pane activity by polling
//! and never attaches a client.
//!
//! The fake tmux is a shell script in a tempdir; no tmux server is touched.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trusty_architect::SystemTmux;
use trusty_architect::poller::Config;
use trusty_architect::serve::{
    FileWatch, ServeLoop, ServeOptions, TMUX_FAULT, TmuxActivity, Trigger,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type Seen = Mutex<Vec<(Trigger, Option<String>)>>;

/// A fake tmux: logs every argv to `argv.log`; `list-panes` prints the
/// `panes` file, or fails with a `noserver` file; anything else exits 1.
fn fake_tmux(dir: &Path) -> TestResult<SystemTmux> {
    let d = dir.display();
    let script = format!(
        "#!/bin/sh\n\
         echo \"$*\" >> {d}/argv.log\n\
         case \"$1\" in\n\
         list-panes) [ -f {d}/noserver ] && {{ echo 'no server running' >&2; exit 1; }}\n\
           cat {d}/panes ;;\n\
         *) exit 1 ;;\n\
         esac\n"
    );
    let path = dir.join("tmux");
    fs::write(&path, script)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(SystemTmux::new().with_program(path))
}

fn panes(dir: &Path, s1_at: u64, poll_at: u64) -> TestResult {
    let rows = format!("s1\t%1\t{s1_at}\ns1\t%3\t{s1_at}\npoll\t%2\t{poll_at}\n");
    fs::write(dir.join("panes"), rows)?;
    Ok(())
}

/// The first read is the baseline; a newer activity time or a new pane is an
/// event; the excluded poll session's activity is not.
#[test]
fn tmux_activity_change_is_an_event() -> TestResult {
    let dir = tempfile::tempdir()?;
    panes(dir.path(), 100, 100)?;
    let mut tmux = TmuxActivity::new(fake_tmux(dir.path())?, vec!["poll".into()]);
    assert_eq!(tmux.poll(), Ok(false), "baseline");
    assert_eq!(tmux.poll(), Ok(false), "unchanged");
    panes(dir.path(), 100, 200)?;
    assert_eq!(tmux.poll(), Ok(false), "poll session excluded");
    panes(dir.path(), 101, 200)?;
    assert_eq!(tmux.poll(), Ok(true), "activity");
    assert_eq!(tmux.poll(), Ok(false), "new baseline");
    fs::write(dir.path().join("panes"), "s1\t%1\t101\n")?;
    assert_eq!(tmux.poll(), Ok(true), "pane closed");
    Ok(())
}

/// One loop run over the fake tmux: `act` runs while the loop runs; returns
/// each cycle's trigger and its `watch-tmux` fault detail.
fn run_with(
    files: Option<FileWatch>,
    tmux: Option<TmuxActivity>,
    act: impl FnOnce(&Seen),
) -> Vec<(Trigger, Option<String>)> {
    let stop = AtomicBool::new(false);
    let seen: Arc<Seen> = Arc::default();
    std::thread::scope(|s| {
        let seen2 = Arc::clone(&seen);
        let stop = &stop;
        let h = s.spawn(move || {
            let log = |_: &str| {};
            let mut lp = ServeLoop {
                opts: fast(),
                stop,
                files,
                tmux,
                events: None,
                log: &log,
            };
            lp.run(&mut |trigger, faults| {
                let f = faults.get(TMUX_FAULT).map(str::to_owned);
                if let Ok(mut v) = seen2.lock() {
                    v.push((trigger, f));
                }
                Ok(())
            })
        });
        act(&seen);
        stop.store(true, Ordering::SeqCst);
        let _ = h.join();
    });
    seen.lock().map(|v| v.clone()).unwrap_or_default()
}

fn wait_for(seen: &Seen, n: usize) {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end && seen.lock().is_ok_and(|v| v.len() < n) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fast() -> ServeOptions {
    ServeOptions {
        debounce: Duration::from_millis(20),
        tick: Duration::from_secs(30),
        min_gap: Duration::ZERO,
        scan_every: Duration::from_millis(20),
        once: false,
    }
}

/// tmux failing at start is a `watch-tmux` fault the start cycle carries;
/// the first good read clears it and the next cycle carries none.
#[test]
fn tmux_failure_is_a_fault_then_clears() -> TestResult {
    let dir = tempfile::tempdir()?;
    panes(dir.path(), 100, 100)?;
    fs::write(dir.path().join("noserver"), "")?;
    let fake = fake_tmux(dir.path())?;
    let tmux = TmuxActivity::new(fake, vec!["poll".into()]);
    let seen = run_with(None, Some(tmux), |seen| {
        wait_for(seen, 1);
        let _ = fs::remove_file(dir.path().join("noserver"));
        // The recovery is not an event; an activity change is.
        std::thread::sleep(Duration::from_millis(100));
        let _ = panes(dir.path(), 101, 100);
        wait_for(seen, 2);
    });
    assert!(
        seen.first()
            .is_some_and(|c| c.1.as_deref().is_some_and(|d| d.contains("no server"))),
        "{seen:?}"
    );
    assert_eq!(seen.get(1), Some(&(Trigger::Events, None)), "{seen:?}");
    Ok(())
}

/// The production sources ([`ServeLoop::for_config`]) with pane activity
/// changing: events cycle, and no tmux call is `attach` or control mode.
#[test]
fn serve_never_attaches_a_tmux_client() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root");
    fs::create_dir_all(root.join("inbox"))?;
    let config = Config::from_env(&root, dir.path(), &|_| None)?;
    panes(dir.path(), 100, 100)?;
    let fake = fake_tmux(dir.path())?;
    let (stop, log) = (AtomicBool::new(false), |_: &str| {});
    let built = ServeLoop::for_config(&config, fake, &stop, fast(), &log);
    let seen = run_with(built.files, built.tmux, |seen| {
        wait_for(seen, 1);
        for at in 101..105 {
            std::thread::sleep(Duration::from_millis(60));
            let _ = panes(dir.path(), at, 100);
        }
        wait_for(seen, 3);
    });
    assert!(
        seen.iter().filter(|c| c.0 == Trigger::Events).count() >= 2,
        "{seen:?}"
    );
    let argv = fs::read_to_string(dir.path().join("argv.log"))?;
    assert!(argv.lines().count() >= 3, "{argv}");
    for line in argv.lines() {
        assert!(line.starts_with("list-panes -a -F "), "{line}");
        assert!(!line.contains("attach") && !line.contains("-C"), "{line}");
    }
    Ok(())
}
