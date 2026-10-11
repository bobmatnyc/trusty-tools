//! [`disable_core_dumps`]: the server's core-dump limit is zero (#7520 S2).
//!
//! Why: owner ruling Dd8 — a core file of the server would hold every value
//! its session cache and its in-flight requests hold.
//! What: one call at start, before any secret is read.
//! Test: `tests/core_dump_limit.rs`, and
//! `binary_runs_with_core_dumps_disabled` in `tests/on_demand_server.rs`.

use std::io;

/// Set this process's `RLIMIT_CORE` to 0, soft and hard.
///
/// Why: see the module docs. The hard limit is lowered too, so nothing in
/// the process can raise the soft limit again.
/// What: `setrlimit`, then `getrlimit` to confirm (0, 0). Lowering a limit
/// needs no privilege. Either call failing, or a limit other than (0, 0)
/// read back, is an error; the binary then exits without serving (fail
/// closed, DOC-74 §8.4 S2 amendment).
/// Test: `disable_core_dumps_sets_soft_and_hard_limit_to_zero`.
pub fn disable_core_dumps() -> io::Result<()> {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads one initialized `rlimit` and keeps no pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut now = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: getrlimit writes one `rlimit` into a live local.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // #7520 S2: postcondition; a limit that did not take is a failure.
    if now.rlim_cur != 0 || now.rlim_max != 0 {
        return Err(io::Error::other("RLIMIT_CORE is not 0 after setrlimit"));
    }
    Ok(())
}
