//! Portable resident-set-size (RSS) measurement.
//!
//! Why: issue #24 was triggered by a 72 GB RSS spike during indexing on
//! Apple Silicon (CoreML unified-memory pool). Before we can responsibly
//! switch any new embedding backend (candle Metal, issue #54) into the
//! default position, we need a portable way to observe RSS in-process so
//! the validation harness (issue #55) can produce a defensible go/no-go
//! recommendation rather than relying on after-the-fact `ps` snapshots.
//!
//! What: a single free function `current_rss_bytes()` returning the
//! current process RSS in bytes. Implemented on top of the `sysinfo`
//! crate (already a workspace dependency) so the same code works on
//! macOS, Linux, and Windows without per-platform `libc` glue.
//!
//! Test: `rss::tests::rss_is_nonzero` and `rss::tests::rss_is_under_64gb`
//! assert the basic sanity invariants without depending on any specific
//! platform. The benchmark binary uses `current_rss_bytes()` repeatedly
//! around each embed batch to compute deltas and peak RSS.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Return the current process's resident-set-size in bytes.
///
/// Why: lets the candle Metal validation harness measure peak RSS around
/// each embedding batch so we can decide whether candle Metal is safe to
/// promote past the original 72 GB jetsam-SIGKILL incident (#24).
/// What: queries `sysinfo` for the current PID's memory and returns it as
/// a raw byte count. Returns `0` if the process is not visible to
/// `sysinfo` (should never happen on supported platforms — we still
/// return `0` rather than panic so callers can degrade gracefully).
/// Test: `rss::tests::rss_is_nonzero` verifies the value is non-zero and
/// under 64 GB during the unit-test process.
pub fn current_rss_bytes() -> u64 {
    let mut system = System::new();
    let pid = Pid::from_u32(std::process::id());
    // Refresh only this process's memory info — much cheaper than
    // `refresh_all()` which scans every PID on the system.
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map(|p| p.memory()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a non-zero RSS is the load-bearing precondition for the
    /// candle Metal validation harness. If `current_rss_bytes()` returns
    /// zero we'd produce a misleading "0 GB peak" verdict.
    /// What: calls `current_rss_bytes()` once and asserts it is > 0.
    /// Test: this test.
    #[test]
    fn rss_is_nonzero() {
        let rss = current_rss_bytes();
        assert!(rss > 0, "RSS should be measurable, got {rss}");
    }

    /// Why: an absurdly large RSS reading would also invalidate the
    /// harness (e.g. signed-vs-unsigned bug surfacing as ~2^63 bytes).
    /// What: asserts the reading is under 64 GB — far above any realistic
    /// unit-test process, far below the integer-overflow danger zone.
    /// Test: this test.
    #[test]
    fn rss_is_under_64gb() {
        let rss = current_rss_bytes();
        let limit = 64u64 * 1024 * 1024 * 1024;
        assert!(rss < limit, "RSS should be < 64GB, got {rss}");
    }
}
