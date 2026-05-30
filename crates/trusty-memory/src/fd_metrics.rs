//! File-descriptor usage and limit reporting for the `/health` endpoint.
//!
//! Why: The fd-exhaustion bug (EMFILE at 256 fds with 82 palaces × ~3 redb
//! files) is invisible without observability. Exposing `open_fds` and
//! `fd_soft_limit` in `/health` lets operators (and automated monitors) catch
//! "approaching ceiling" before the daemon becomes non-functional. It also
//! provides a cheap runtime sanity check that the LaunchAgent fd-limit fix
//! (SoftResourceLimits / HardResourceLimits = 8192) has taken effect.
//!
//! What: two platform-specific helpers —
//!   - [`count_open_fds`]: best-effort count of open file descriptors for
//!     the current process. macOS: list entries under `/dev/fd`; Linux: count
//!     entries under `/proc/self/fd`. Returns `None` on any I/O error.
//!   - [`fd_soft_limit`]: the soft `RLIMIT_NOFILE` ceiling via `libc::getrlimit`.
//!     Returns `None` only when the syscall fails (extremely rare).
//!
//! Test: `fd_metrics_returns_sane_values` asserts both helpers return `Some`
//! with plausible non-zero values on the current platform.

/// Count the number of open file descriptors for the current process.
///
/// Why: best-effort fd count without spawning `lsof` or any external process.
/// What: on macOS/iOS counts entries in `/dev/fd`; on Linux counts entries in
/// `/proc/self/fd`; on other Unix-like systems falls back to `/dev/fd`.
/// Entries named `"."` and `".."` are excluded.
/// Returns `None` on any I/O error (e.g. permission denied, procfs not mounted)
/// so callers can skip the field rather than return an error.
/// Test: `fd_metrics_returns_sane_values`.
pub fn count_open_fds() -> Option<u64> {
    #[cfg(target_os = "linux")]
    let dir_path = "/proc/self/fd";
    #[cfg(not(target_os = "linux"))]
    let dir_path = "/dev/fd";

    let count = std::fs::read_dir(dir_path)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s != "." && s != ".."
        })
        .count();
    // Subtract 1 for the directory fd that read_dir itself holds open.
    // On macOS /dev/fd shows the fd opened by opendir() as well; Linux's
    // /proc/self/fd behaves the same way. The subtraction is best-effort:
    // if count is 0 somehow, clamp to 0.
    Some(count.saturating_sub(1) as u64)
}

/// Return the soft `RLIMIT_NOFILE` ceiling for the current process.
///
/// Why: the absolute count is only meaningful relative to the ceiling; showing
/// both in `/health` lets operators see "244 / 256" and act before EMFILE hits.
/// What: calls `libc::getrlimit(RLIMIT_NOFILE)` and returns the soft limit as
/// `u64`. Returns `None` when the syscall fails (EPERM, EFAULT — essentially
/// never under normal operation).
/// Test: `fd_metrics_returns_sane_values`.
pub fn fd_soft_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit is a POSIX call; we pass a valid pointer to a
        // zero-initialised rlimit struct. It cannot fail in any way that
        // would be unsafe (it writes into our stack-allocated struct).
        let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
        if ret == 0 {
            // RLIM_INFINITY is typically u64::MAX (or u32::MAX on 32-bit);
            // clamp to a sane value so the JSON field doesn't carry a sentinel
            // that confuses clients. We compare via the libc constant directly
            // to avoid unnecessary-cast lints on platforms where rlim_t is u64.
            #[allow(clippy::unnecessary_cast)]
            let cur = rlim.rlim_cur as u64;
            #[allow(clippy::unnecessary_cast)]
            let infinity = libc::RLIM_INFINITY as u64;
            if cur == infinity {
                None // indicate "unlimited" as absence rather than max u64
            } else {
                Some(cur)
            }
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the fd-exhaustion fix is only observable if the metrics helpers
    /// return sensible values at runtime. If either returns `None` on the
    /// target platform (macOS/Linux where this daemon runs), the `/health`
    /// gauge is useless. This test acts as a platform smoke-test: it fails
    /// loudly if the procfs/dev-fs path is inaccessible or getrlimit is broken.
    /// What: asserts `count_open_fds()` returns `Some(n)` where `n > 0` and
    /// `fd_soft_limit()` returns `Some(m)` where `m > 0`. On non-Unix platforms
    /// where the helpers are no-ops, the test is a no-op.
    /// Test: itself.
    #[test]
    fn fd_metrics_returns_sane_values() {
        #[cfg(unix)]
        {
            let fds = count_open_fds();
            assert!(
                fds.is_some(),
                "count_open_fds() must return Some on Unix (got None)"
            );
            assert!(
                fds.unwrap() > 0,
                "count_open_fds() must be > 0 (at minimum stdin/stdout/stderr are open)"
            );

            let limit = fd_soft_limit();
            assert!(
                limit.is_some(),
                "fd_soft_limit() must return Some on Unix (getrlimit RLIMIT_NOFILE failed)"
            );
            assert!(limit.unwrap() > 0, "fd_soft_limit() must be > 0");
        }
    }
}
