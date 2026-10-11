//! `disable_core_dumps` leaves `RLIMIT_CORE` at (0, 0) (#7520 S2).
//!
//! Why: owner ruling Dd8 — the server must never write a core file.
//! What: its own test binary, because the call lowers the hard limit of the
//! whole process for good; no other test shares this process.
//! Test: itself.
#![cfg(unix)]

use trusty_secrets::server::disable_core_dumps;

/// This process's `RLIMIT_CORE`, soft and hard.
fn core_limit() -> (libc::rlim_t, libc::rlim_t) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one `rlimit` into a live local.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) };
    assert_eq!(rc, 0, "getrlimit: {}", std::io::Error::last_os_error());
    (limit.rlim_cur, limit.rlim_max)
}

/// Why: #7520 S2 — both limits must be 0: a soft 0 alone can be raised
/// again by the process up to its hard limit.
/// What: raises the soft limit to the hard one first, so a call that does
/// nothing leaves a non-zero limit and fails.
/// Test: itself.
#[test]
fn disable_core_dumps_sets_soft_and_hard_limit_to_zero() {
    let (_, hard) = core_limit();
    let raised = libc::rlimit {
        rlim_cur: hard,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads one initialized `rlimit`.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raised) };
    assert_eq!(rc, 0, "setrlimit: {}", std::io::Error::last_os_error());
    eprintln!("RLIMIT_CORE before the call: {:?}", core_limit());

    disable_core_dumps().expect("disable_core_dumps");
    assert_eq!(core_limit(), (0, 0), "RLIMIT_CORE after disable_core_dumps");
}
