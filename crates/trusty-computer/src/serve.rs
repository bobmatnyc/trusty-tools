//! `trusty-computer serve --state DIR`, Phase 2 form: run the supervised daemon.
//!
//! Why: live checks must start through `scripts/sandbox_computer_daemon.sh`,
//! which runs `serve --state <dir>/state`; until the Phase 3 broker exists,
//! `serve` owns only the bounded daemon so the live test can drive it.
//! What: reads the lease scope from `<state>/scope.json` (`{"pid", "window_id"}`),
//! starts the [`Supervisor`] and prints `ready pid=<p> socket=<s>`. SIGHUP
//! re-reads the scope and rescopes, printing `rescoped restart_ms=<n>` (or
//! `rescope unchanged`). SIGTERM or SIGINT stops the daemon and exits 0.
//! Test: `live_bounded_daemon_drives_fixture` in `tests/live_supervisor.rs`.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::launcher::CuaAppLauncher;
use crate::manifest::LeaseScope;
use crate::supervisor::Supervisor;

static HUP: AtomicBool = AtomicBool::new(false);
static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    if sig == libc::SIGHUP {
        HUP.store(true, Ordering::SeqCst);
    } else {
        TERM.store(true, Ordering::SeqCst);
    }
}

/// Read `<state>/scope.json`.
pub fn read_scope(state: &Path) -> Result<LeaseScope, String> {
    let path = state.join("scope.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let pid = v.get("pid").and_then(Value::as_u64);
    let window_id = v.get("window_id").and_then(Value::as_u64);
    match (pid.and_then(|p| u32::try_from(p).ok()), window_id) {
        (Some(pid), Some(window_id)) => Ok(LeaseScope { pid, window_id }),
        _ => Err(format!(
            "{}: needs integer pid and window_id",
            path.display()
        )),
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn drive(
    sup: &mut Supervisor<CuaAppLauncher>,
    state: &Path,
    scope: LeaseScope,
) -> Result<(), String> {
    sup.start(scope).map_err(|e| e.to_string())?;
    say(&format!(
        "ready pid={} socket={} session={}",
        sup.pid().unwrap_or(0),
        sup.socket().display(),
        sup.session()
    ));
    while !TERM.load(Ordering::SeqCst) {
        if HUP.swap(false, Ordering::SeqCst) {
            let outcome = read_scope(state).and_then(|s| sup.rescope(s).map_err(|e| e.to_string()));
            match outcome {
                Ok(Some(took)) => say(&format!(
                    "rescoped restart_ms={} pid={}",
                    took.as_millis(),
                    sup.pid().unwrap_or(0)
                )),
                Ok(None) => say("rescope unchanged"),
                Err(e) => return Err(format!("rescope failed: {e}")),
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    sup.stop().map_err(|e| e.to_string())?;
    say("stopped");
    Ok(())
}

/// Run until SIGTERM/SIGINT; the exit code.
pub fn run(state: &Path) -> u8 {
    for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores to atomics, which is async-signal-safe.
        unsafe {
            libc::signal(
                sig,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
    }
    let result = read_scope(state).and_then(|scope| {
        let mut sup = Supervisor::new(CuaAppLauncher, state).map_err(|e| e.to_string())?;
        let driven = drive(&mut sup, state, scope);
        if driven.is_err() {
            // Never leave our daemon behind on a failed start or rescope.
            let _ = sup.stop();
        }
        driven
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("trusty-computer serve: {e}");
            1
        }
    }
}
