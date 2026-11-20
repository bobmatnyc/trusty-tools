//! SIGTERM and SIGINT set a stop flag; the service reads it between cycles.
//!
//! Why: launchd and a terminal stop the service with SIGTERM / SIGINT, and a
//! cycle cut in half can leave a wake typed but not recorded (#9779).
//! What: [`install_stop_handlers`] points both signals at a handler that only
//! stores `true` in a static atomic (async-signal-safe) and returns that flag.
//! Test: `sigterm_finishes_the_cycle_and_exits` (tests/serve_signal.rs).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Installs the SIGTERM and SIGINT handlers; returns the flag they set.
///
/// # Errors
/// `signal(2)` refused a handler.
pub fn install_stop_handlers() -> io::Result<&'static AtomicBool> {
    let handler = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `on_stop` only stores to a static atomic, which is
        // async-signal-safe; `signal` has no other precondition.
        if unsafe { libc::signal(sig, handler) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(&STOP)
}
