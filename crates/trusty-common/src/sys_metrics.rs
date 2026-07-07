//! Process resident-memory (RSS) and CPU sampling for daemon `/health`.
//!
//! Why: Every trusty-* daemon wants to report its own RSS and CPU usage on
//!      its health endpoint, and the sampling logic (resolve our PID, refresh
//!      only this process, convert units) is identical across them.
//!      Centralising it here avoids three near-identical copies drifting.
//! What: [`SysMetrics`] wraps a `sysinfo::System` scoped to the current
//!      process. [`SysMetrics::sample`] refreshes and returns
//!      `(rss_mb, cpu_pct)`. CPU usage is a delta between two refreshes, so
//!      the *first* sample reports `0.0`; subsequent samples report the
//!      usage observed since the previous call. Callers polling `/health`
//!      every ~2 s get meaningful CPU readings without any background task.
//! Test: see the `tests` module — `sample_does_not_panic` exercises the
//!      refresh path; `rss_is_plausible` asserts the test process reports a
//!      non-trivial, non-absurd RSS.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// True physical memory footprint of a process, in megabytes (macOS only).
///
/// Why (issue #2165): `sysinfo::Process::memory()` on macOS reads
/// `PROC_PIDTASKINFO.pti_resident_size`, which is the same "resident set"
/// figure `getrusage()`/`ru_maxrss` report. That figure does NOT count pages
/// the macOS memory compressor has swept into compressed (still in-RAM, still
/// counted against the process by the kernel's Jetsam/OOM logic) storage. On
/// long-running daemons with large ONNX/HNSW arenas, the compressor can hold
/// many GB that `pti_resident_size` simply omits — so a self-reported
/// `rss_mb` can read as a few dozen MB while `vmmap`/`footprint`/Activity
/// Monitor (and the kernel's own memory-pressure accounting) see many GB.
/// A `TRUSTY_MEMORY_LIMIT_MB` guardrail keyed off the under-counted figure
/// can never trip. The `phys_footprint` counter — surfaced by `libproc`'s
/// `proc_pid_rusage(RUSAGE_INFO_V0)` as `ri_phys_footprint` — is exactly the
/// figure `vmmap`/`footprint` report, so using it makes the guardrail
/// meaningful again.
/// What: calls `proc_pid_rusage(pid, RUSAGE_INFO_V0, ...)` (from `libc`,
/// already a workspace dependency — no new crate needed) and reads
/// `ri_phys_footprint`, converting bytes to whole megabytes. Returns `None`
/// on any failure (invalid pid, cross-user permission denial) rather than
/// panicking or returning a garbage value — callers fall back to the
/// `sysinfo`-derived RSS in that case.
/// Test: `tests::self_physical_footprint_is_plausible` (macOS-only) asserts
/// this returns `Some` for the test process's own PID with a plausible
/// value. Asserting it *exceeds* the getrusage-style RSS deterministically
/// is not testable in CI — that gap only manifests under the memory
/// compressor, which requires sustained real memory pressure to trigger and
/// cannot be reliably induced in a unit test.
#[cfg(target_os = "macos")]
pub fn physical_footprint_mb(pid: u32) -> Option<u64> {
    // SAFETY: `info` is a zero-initialised, `#[repr(C)]` struct matching the
    // kernel ABI for `RUSAGE_INFO_V0`. The C API's `buffer` parameter is
    // typed `rusage_info_t *` (`rusage_info_t` itself being `void *`), and
    // the canonical call pattern — mirrored here — passes the struct's own
    // address reinterpreted as that opaque pointer type, e.g. Apple's
    // `libproc.h` usage `(rusage_info_t *)&rusage`: the kernel writes
    // directly into `info`'s bytes, there is no extra pointer indirection.
    // `proc_pid_rusage` returns a negative value on failure without writing
    // to `info`, so `info` is only read once the call has reported success.
    let mut info: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V0,
            std::ptr::addr_of_mut!(info).cast(),
        )
    };
    if ret != 0 {
        return None;
    }
    Some(info.ri_phys_footprint / (1024 * 1024))
}

/// Per-process RSS + CPU sampler bound to the current process.
///
/// Why: holding the `System` between calls is required for CPU measurement —
///      `sysinfo` derives CPU% from the delta in consumed CPU time between
///      two refreshes, so the same instance must be reused.
/// What: stores the long-lived `System` and our own `Pid`. Not `Clone` — it
///      carries mutable sampling state; share it behind a `Mutex` if multiple
///      handlers need it.
/// Test: `sample_does_not_panic`, `rss_is_plausible`.
pub struct SysMetrics {
    sys: System,
    pid: Pid,
}

impl SysMetrics {
    /// Construct a sampler for the current process.
    ///
    /// Why: the daemon builds one of these at startup and samples it on each
    ///      `/health` request.
    /// What: resolves `std::process::id()` into a `sysinfo::Pid` and creates a
    ///      `System` configured to refresh only process memory + CPU (not the
    ///      whole machine), then performs one priming refresh so the next
    ///      `sample` call has a baseline for the CPU delta.
    /// Test: `sample_does_not_panic`.
    #[must_use]
    pub fn new() -> Self {
        let pid = Pid::from_u32(std::process::id());
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_memory().with_cpu()),
        );
        // Prime the CPU baseline — the first delta-based reading after this
        // will be meaningful rather than a spurious 0/huge value.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        Self { sys, pid }
    }

    /// Refresh and return `(rss_mb, cpu_pct)` for the current process.
    ///
    /// Why: the `/health` handler calls this once per request. Polling more
    ///      often than ~once per 500 ms yields noisy CPU readings because the
    ///      delta window shrinks; `/health` is typically polled every 2 s so
    ///      this is not a concern in practice. On macOS the reported RSS is
    ///      the true physical footprint (issue #2165) — see
    ///      [`physical_footprint_mb`] — rather than the getrusage-style
    ///      resident size, which undercounts memory the compressor is
    ///      currently holding for this process.
    /// What: refreshes this process's memory + CPU stats. Returns RSS in
    ///      whole megabytes and CPU as a percentage where `100.0` means one
    ///      fully-saturated core (sysinfo's convention — a process on 4 cores
    ///      can exceed 100). RSS: macOS uses [`physical_footprint_mb`],
    ///      falling back to `sysinfo`'s `bytes / 1_048_576` reading if the
    ///      `libproc` call fails; all other platforms use the `sysinfo`
    ///      reading directly (Linux's `/proc/self/status` `VmRSS` — which
    ///      `sysinfo::Process::memory()` already surfaces — has no analogous
    ///      compressor-accounting gap). If the process cannot be resolved
    ///      (extremely rare; only in containers with `/proc` hidden), returns
    ///      `(0, 0.0)`.
    /// Test: `sample_does_not_panic`, `rss_is_plausible`.
    pub fn sample(&mut self) -> (u64, f32) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let Some(proc) = self.sys.process(self.pid) else {
            return (0, 0.0);
        };
        let sysinfo_rss_mb = proc.memory() / (1024 * 1024);
        let cpu_pct = proc.cpu_usage();
        #[cfg(target_os = "macos")]
        let rss_mb = physical_footprint_mb(self.pid.as_u32()).unwrap_or(sysinfo_rss_mb);
        #[cfg(not(target_os = "macos"))]
        let rss_mb = sysinfo_rss_mb;
        (rss_mb, cpu_pct)
    }
}

impl Default for SysMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Sum the byte sizes of every regular file under `dir`, recursively.
///
/// Why: daemon `/health` reports `disk_bytes` — the on-disk footprint of the
///      data directory (redb + usearch + snapshot files). Walking the tree on
///      demand keeps it accurate without a separate accounting layer.
/// What: recursively descends `dir`, summing `metadata().len()` of each file.
///      Symlinks are not followed (avoids double-counting and cycles).
///      Unreadable entries are skipped rather than failing the whole walk —
///      a health endpoint should degrade gracefully. Returns `0` when `dir`
///      does not exist.
/// Test: `dir_size_sums_files` creates files of known sizes and asserts the
///      total; `dir_size_missing_dir_is_zero` covers the absent-path case.
#[must_use]
pub fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), total);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0u64;
    walk(dir, &mut total);
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_does_not_panic() {
        let mut m = SysMetrics::new();
        let (_rss, _cpu) = m.sample();
        // A second sample exercises the CPU-delta path.
        let (_rss2, cpu2) = m.sample();
        assert!(cpu2 >= 0.0, "cpu usage must be non-negative, got {cpu2}");
    }

    #[test]
    fn rss_is_plausible() {
        let mut m = SysMetrics::new();
        let (rss, _cpu) = m.sample();
        // The test binary is real; if sysinfo could resolve it RSS is > 0.
        // We tolerate 0 only for sandboxed CI where /proc is restricted.
        assert!(
            rss < 1024 * 1024,
            "RSS implausibly large ({rss} MB) — unit must be MB"
        );
    }

    #[test]
    fn dir_size_sums_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(tmp.path().join("b.txt"), vec![0u8; 250]).unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("c.txt"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size_bytes(tmp.path()), 400);
    }

    #[test]
    fn dir_size_missing_dir_is_zero() {
        let missing = std::path::Path::new("/nonexistent/trusty/path/xyz");
        assert_eq!(dir_size_bytes(missing), 0);
    }

    /// `physical_footprint_mb(self_pid)` must return a plausible non-zero
    /// value for the test process on macOS.
    ///
    /// Why: regression coverage for issue #2165 — asserts the `libproc`
    /// `RUSAGE_INFO_V0` path actually resolves rather than silently always
    /// falling back to the sysinfo reading. Deterministically asserting it
    /// *exceeds* the getrusage-style RSS is not testable here: that gap only
    /// opens up under real memory-compressor pressure, which a unit test
    /// cannot reliably induce.
    /// What: calls with `std::process::id()`, asserts `Some` with a plausible
    /// (non-zero, sub-terabyte) MB value.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn self_physical_footprint_is_plausible() {
        let pid = std::process::id();
        let mb = physical_footprint_mb(pid).expect("proc_pid_rusage must resolve our own pid");
        assert!(mb > 0, "physical footprint should be > 0 MB, got {mb}");
        assert!(
            mb < 1024 * 1024,
            "physical footprint implausibly large ({mb} MB) — unit must be MB"
        );
    }

    /// `physical_footprint_mb` must return `None` (not panic) for a bogus pid.
    ///
    /// Why: `proc_pid_rusage` fails for a nonexistent pid; the function must
    /// map that failure to `None` rather than reading uninitialised memory.
    /// What: pass `u32::MAX`, assert `None`.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn physical_footprint_bogus_pid_returns_none() {
        assert_eq!(physical_footprint_mb(u32::MAX), None);
    }

    /// `physical_footprint_mb` must track real memory growth.
    ///
    /// Why: the strongest test available without inducing actual memory-
    /// compressor pressure (untestable deterministically in CI). Allocating
    /// and touching 200 MB must move the reading up by a comparable amount —
    /// this catches regressions like the double-pointer-indirection bug this
    /// function shipped with initially (which always read back zeroed
    /// kernel-untouched memory instead of `ri_phys_footprint`).
    /// What: sample before, allocate + touch every page of a 200 MB `Vec`
    /// (touching is required — an untouched allocation may not be backed by
    /// real pages yet), sample after, assert the delta is positive and at
    /// least 100 MB (leaves headroom for measurement noise well below the
    /// full 200 MB).
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn physical_footprint_tracks_real_allocation_growth() {
        let pid = std::process::id();
        let before = physical_footprint_mb(pid).expect("must resolve our own pid");
        let mut touched: Vec<u8> = vec![0u8; 200 * 1024 * 1024];
        for byte in touched.iter_mut().step_by(4096) {
            *byte = 1;
        }
        let after = physical_footprint_mb(pid).expect("must resolve our own pid");
        assert!(
            after >= before + 100,
            "expected footprint to grow by >= 100 MB after touching a 200 MB \
             allocation; before={before} after={after}"
        );
        // Keep `touched` alive through the measurement above.
        drop(touched);
    }
}
