//! The `sandbox` field of the worker's `hello` (SPEC-OKF-01 §2.16 "OS
//! sandbox", r7.1; Appendix B).
//!
//! Why: the host applies the OS sandbox in its own spawn path and decides
//! from its own spawn evidence alone. It reads this field only as a
//! cross-check: a worker that claims a sandbox the host did not apply, or
//! denies one it did, is `worker_incompatible`. So the worker must report
//! what it observes, never what it was told.
//! What: [`detect`] returns the launcher the worker finds itself under, or
//! `none`. Linux: `bwrap` when PID 1 of the worker's PID namespace is
//! `bwrap` and its network namespace holds only the loopback interface (the
//! S10 host runs `bwrap --unshare-all`). macOS: `seatbelt` when
//! `sandbox_check` reports the process sandboxed (the S10 host runs
//! `sandbox-exec`). Elsewhere: `none`. Each probe only reads; none opens a
//! socket.
//! Test: `outside_a_sandbox_the_worker_reports_none` (unit);
//! `real_worker_sandbox_claim_matches_the_spawn_evidence` in
//! `tests/fixtures.rs` (both halves, under the real launcher).
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16 "OS sandbox", Appendix B `hello`.

/// The sandbox the worker observes itself under, or `none`.
pub fn detect() -> &'static str {
    if platform_sandboxed() {
        LAUNCHER_NAME
    } else {
        "none"
    }
}

#[cfg(target_os = "linux")]
const LAUNCHER_NAME: &str = "bwrap";
#[cfg(target_os = "macos")]
const LAUNCHER_NAME: &str = "seatbelt";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const LAUNCHER_NAME: &str = "none";

/// Linux: PID 1 is `bwrap` and the only network interface is `lo`.
#[cfg(target_os = "linux")]
fn platform_sandboxed() -> bool {
    let pid1 = std::fs::read_to_string("/proc/1/comm").unwrap_or_default();
    pid1.trim_end() == "bwrap" && only_loopback()
}

/// The interfaces in `/proc/self/net/dev` (two header lines, then one
/// `name:` row each) are exactly `lo`.
#[cfg(target_os = "linux")]
fn only_loopback() -> bool {
    let Ok(dev) = std::fs::read_to_string("/proc/self/net/dev") else {
        return false;
    };
    let names: Vec<&str> = dev
        .lines()
        .skip(2)
        .filter_map(|l| l.split(':').next())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    names == ["lo"]
}

/// macOS: `sandbox_check(pid, NULL, SANDBOX_FILTER_NONE)` is non-zero when
/// the process runs under a sandbox profile.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn platform_sandboxed() -> bool {
    use std::ffi::c_char;
    unsafe extern "C" {
        // libsystem_sandbox, re-exported by libSystem; variadic after `type`.
        fn sandbox_check(pid: i32, operation: *const c_char, filter_type: i32, ...) -> i32;
    }
    let pid = std::process::id() as i32;
    // SAFETY: a null operation with SANDBOX_FILTER_NONE (0) takes no further
    // arguments and only queries the kernel; no memory is passed.
    unsafe { sandbox_check(pid, std::ptr::null(), 0) != 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_sandboxed() -> bool {
    false
}

#[cfg(test)]
mod tests {
    /// Why: a test process runs under no host-applied sandbox, so the worker
    /// must not claim one.
    /// What: `detect` is `none` outside the sandbox.
    /// Test: self-contained.
    #[test]
    fn outside_a_sandbox_the_worker_reports_none() {
        assert_eq!(super::detect(), "none");
    }
}
