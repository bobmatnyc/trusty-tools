//! #9779: SIGTERM stops `tm architect serve` after the running cycle.
//!
//! Its own test binary: the handler is process-wide.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use trusty_architect::serve::{ServeLoop, ServeOptions, install_stop_handlers};

/// A SIGTERM sent mid-cycle lets that cycle finish, then the loop returns.
#[test]
fn sigterm_finishes_the_cycle_and_exits() -> Result<(), Box<dyn std::error::Error>> {
    let stop = install_stop_handlers()?;
    let begun = AtomicU32::new(0);
    let ended = AtomicU32::new(0);
    let started = Instant::now();
    let result = std::thread::scope(|s| {
        let h = s.spawn(|| {
            let log = |_: &str| {};
            let mut lp = ServeLoop {
                opts: ServeOptions {
                    tick: Duration::from_millis(50),
                    ..ServeOptions::default()
                },
                stop,
                files: None,
                tmux: None,
                events: None,
                log: &log,
            };
            lp.run(&mut |_, _| {
                begun.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                ended.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });
        std::thread::sleep(Duration::from_millis(150));
        // SAFETY: signals this process; the handler only sets an atomic.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        h.join()
    });
    assert!(matches!(result, Ok(Ok(_))), "{result:?}");
    assert_eq!(begun.load(Ordering::SeqCst), ended.load(Ordering::SeqCst));
    assert!(started.elapsed() < Duration::from_secs(2));
    Ok(())
}
