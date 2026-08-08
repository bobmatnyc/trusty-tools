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

/// Resident memory of an **arbitrary** process, in megabytes.
///
/// Why (#2846): a supervisor that owns child processes has to answer "is this
/// child over its declared limit?" before the kernel answers it with an
/// OOM-kill. trusty-search shipped an `rss_limit_mb` it never compared against
/// anything and grew to 2.2x that limit before the OOM killer intervened; the
/// missing piece was a way to read another process's RSS at all. This is that
/// entry point, and it lives here — next to [`physical_footprint_mb`] and
/// [`SysMetrics`] — so every trusty-* supervisor reads child memory the same
/// way instead of each growing its own `/proc` parser.
/// What: on macOS, delegates to [`physical_footprint_mb`], which counts pages
/// the memory compressor holds (the figure the kernel's Jetsam logic uses). On
/// Linux, reads `VmRSS` from `/proc/<pid>/status`. Everywhere else, returns
/// `None`. `None` means "cannot measure", never "measured zero" — callers must
/// treat it as "no opinion" and leave the process alone rather than reaping it.
/// Test: `process_rss_mb_reports_own_process`,
/// `process_rss_mb_is_none_for_absent_pid`.
#[must_use]
pub fn process_rss_mb(pid: u32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        physical_footprint_mb(pid)
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in status.lines() {
            let Some(rest) = line.strip_prefix("VmRSS:") else {
                continue;
            };
            // Format is `VmRSS:\t   12345 kB`.
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
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

/// Maximum directory nesting the size walk will descend.
///
/// Why (#4764): an explicit bound keeps a pathological or adversarially deep
/// tree from turning a best-effort metric into an unbounded sweep. Real
/// trusty-* data directories nest well under ten levels, so this only ever
/// trips on something already wrong.
///
/// Boundary, stated exactly because it is easy to read either way: the walk
/// opens directories from the root (level 0) down to and *including* level
/// `MAX_WALK_DEPTH`. So a file whose parent sits exactly `MAX_WALK_DEPTH`
/// levels below the root IS counted; a file one level deeper is NOT, because
/// its parent is never opened.
/// Test: `dir_size_depth_cap_boundary_is_exact` pins both sides.
const MAX_WALK_DEPTH: usize = 64;

/// Wall-clock budget for one size walk.
///
/// Why (#4764): the data directory is large and actively mutated by reindex
/// and prune passes. A walk that has already run this long is contending with
/// that churn rather than measuring it; returning the partial total is
/// strictly better than holding a blocking-pool thread indefinitely. Set well
/// above any healthy walk so it is a backstop, not a routine truncation.
const WALK_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Sum the byte sizes of every regular file under `dir`, recursively.
///
/// Why: daemon `/health` reports `disk_bytes` — the on-disk footprint of the
///      data directory (redb + usearch + snapshot files). Walking the tree on
///      demand keeps it accurate without a separate accounting layer.
/// What: descends `dir`, summing `metadata().len()` of each file. Symlinks are
///      not followed (avoids double-counting and cycles). Unreadable entries
///      are skipped rather than failing the whole walk — a health endpoint
///      should degrade gracefully. Returns `0` when `dir` does not exist, and
///      the partial total if the walk is truncated by [`MAX_WALK_DEPTH`],
///      [`WALK_BUDGET`], or a panic.
///
/// # Panic safety (issue #4764)
///
/// This function cannot panic and — critically — cannot *abort* the process.
/// Both properties are load-bearing: it runs on a 60 s metrics ticker inside
/// long-lived daemons, and a best-effort disk figure must never be able to
/// take one down.
///
/// The abort this replaces came from `std`'s `impl Drop for DirStream`, which
/// asserts that `closedir(3)` returned 0. When that assert fires, the panic
/// originates *inside a destructor*. The previous implementation was
/// recursive, so descending N levels kept N `ReadDir` handles alive
/// simultaneously; the unwind from the innermost failing `closedir` then ran
/// the enclosing `ReadDir` destructors, a second `closedir` failed the same
/// way, and a panic raised while unwinding is a non-unwinding panic that Rust
/// aborts on unconditionally (`core::panicking::panic_in_cleanup`).
///
/// Hence the two-layer defence, in this order:
///
/// 1. [`walk_bounded`] is iterative and holds **at most one** `ReadDir` alive
///    at a time, dropping each directory handle before descending into any of
///    its children. That removes the second destructor from the unwind path,
///    so a `closedir` failure is now an ordinary, recoverable panic.
/// 2. `catch_unwind` here contains that now-unwinding panic and returns the
///    bytes counted so far.
///
/// Step 2 alone would **not** have fixed this: `catch_unwind` cannot intercept
/// a double panic (the abort happens before any catch frame is reached), nor
/// an allocation-failure abort. Preventing the *first* panic from being able
/// to become the *second* is what makes the daemon survivable.
///
/// Test: `dir_size_sums_files` (known totals), `dir_size_missing_dir_is_zero`
///      (absent path), `dir_size_survives_concurrent_mutation` (the TOCTOU
///      hypothesis from #4764 made executable), `dir_size_depth_cap_boundary_is_exact`.
#[must_use]
pub fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    // #4764: keep the accumulator outside the unwind boundary so a panic
    // degrades the metric to a partial total rather than a spurious 0.
    let total = std::cell::Cell::new(0u64);
    // `AssertUnwindSafe` is sound here: the only state crossing the boundary
    // is a `Cell<u64>` counter, which has no invariant a partial walk breaks.
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| walk_bounded(dir, &total)));
    if outcome.is_err() {
        // The payload itself is logged by `crate::panic_hook`; this line ties
        // it to the walk that produced it.
        tracing::error!(
            dir = %dir.display(),
            "dir_size_bytes: directory walk panicked (see preceding PANIC log \
             for the payload); reporting the partial total"
        );
    }
    total.get()
}

/// Iterative, depth- and time-bounded directory walk holding one `ReadDir`.
///
/// Why (#4764): see [`dir_size_bytes`] — holding exactly one directory handle
/// at a time is what prevents a panicking `closedir` in one handle's
/// destructor from triggering a second one during the unwind, which is the
/// mechanism that aborted the whole daemon.
/// What: explicit `Vec` stack of `(path, depth)`. Each iteration opens one
/// directory, drains it fully (accumulating file sizes, pushing child
/// directories onto the stack), then drops the handle at the end of the loop
/// body — before any child is opened. Bails out on [`WALK_BUDGET`]; refuses to
/// descend past [`MAX_WALK_DEPTH`].
/// Test: `dir_size_survives_concurrent_mutation`, `dir_size_depth_cap_boundary_is_exact`.
fn walk_bounded(root: &std::path::Path, total: &std::cell::Cell<u64>) {
    let started = std::time::Instant::now();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if started.elapsed() >= WALK_BUDGET {
            tracing::warn!(
                root = %root.display(),
                pending = stack.len() + 1,
                "dir_size_bytes: walk exceeded its {WALK_BUDGET:?} budget; \
                 reporting the partial total"
            );
            return;
        }

        // Scope note (#4764): `entries` is the ONLY live `ReadDir` in this
        // function, and it is dropped at the end of this loop body — before
        // the next `read_dir`. Do not reintroduce recursion here.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_WALK_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                total.set(total.get().saturating_add(meta.len()));
            }
        }
    }
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

    /// Why (#2846): the RSS guardrail is only as good as the measurement it
    /// gates on. If `process_rss_mb` silently returned `None` for a live
    /// process, a supervisor built on it would never reap anything and we
    /// would have re-shipped the unenforced-limit bug.
    /// What: measures this test process by its own pid and asserts a
    /// plausible non-zero figure in MB.
    /// Test: this test itself. Skipped (asserted only on the `Some` arm) on
    /// platforms where the measurement is genuinely unavailable.
    #[test]
    fn process_rss_mb_reports_own_process() {
        let me = std::process::id();
        match process_rss_mb(me) {
            Some(mb) => assert!(
                mb < 1024 * 1024,
                "own RSS implausibly large ({mb} MB) — unit must be MB"
            ),
            // Only acceptable off macOS/Linux; on those two the read must have
            // worked for our own pid.
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            None => panic!("process_rss_mb must resolve the current process on this platform"),
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            None => {}
        }
    }

    /// Why: `None` must mean "cannot measure", so a reaped/absent pid must
    /// never read as `Some(0)` — a supervisor would treat that as "under the
    /// limit" and keep a corpse in its map.
    /// Test: this test itself.
    #[test]
    fn process_rss_mb_is_none_for_absent_pid() {
        // pid 0 is the kernel scheduler on Linux and not addressable via
        // proc_pid_rusage on macOS; either way it must not yield a figure.
        assert_eq!(process_rss_mb(0), None);
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

    /// The walk must survive the tree being mutated underneath it.
    ///
    /// Why (issue #4764): this is the TOCTOU hypothesis made executable, and
    /// it is the test that would have caught the original defect. In
    /// production the disk-size ticker walks the data directory while reindex
    /// and prune passes stage `.tmp` corpora, `rename(2)` them over the live
    /// path, and delete whole subtrees — entries and entire directories
    /// vanish between `read_dir` and `metadata`, and a directory handle can be
    /// closed under a `DIR *` the walk still owns.
    ///
    /// Note on fidelity: no portable test can force `closedir(3)` to return a
    /// failure, so this does not reproduce the exact `std` assert that fired
    /// in production. What it does cover is the whole class — concurrent
    /// rename/delete against a live walk — and the structural property the fix
    /// rests on: with recursion removed, a panic anywhere in the walk is
    /// recoverable rather than an abort. A regression to the recursive form
    /// under a real `closedir` failure aborts the process, which no assertion
    /// can catch; the assertion here is that the walk still returns a
    /// plausible number at all.
    /// What: seeds a fixed, never-mutated subtree (a hard floor on the total),
    /// spins mutator threads doing create → populate → atomic-rename → delete
    /// cycles, walks repeatedly during the churn, then asserts the floor holds
    /// and no thread panicked.
    /// Test: this test.
    #[test]
    fn dir_size_survives_concurrent_mutation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const BRANCHES: u64 = 8;
        const LEAF_BYTES: u64 = 64;
        const TOP_BYTES: u64 = 32;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();

        // Stable subtree the mutators never touch — its bytes are a floor on
        // every observation, so a truncating regression is detectable.
        for i in 0..BRANCHES {
            let branch = root.join(format!("branch-{i}"));
            std::fs::create_dir_all(branch.join("a/b/c")).expect("seed dirs");
            std::fs::write(
                branch.join("a/b/c/leaf.bin"),
                vec![0u8; LEAF_BYTES as usize],
            )
            .expect("seed leaf");
            std::fs::write(branch.join("a/top.bin"), vec![0u8; TOP_BYTES as usize])
                .expect("seed top");
        }

        let stop = Arc::new(AtomicBool::new(false));
        let mutators: Vec<_> = (0..3)
            .map(|t| {
                let root = root.clone();
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut n: u64 = 0;
                    while !stop.load(Ordering::Relaxed) {
                        let staged = root.join(format!("staged-{t}-{n}"));
                        let live = root.join(format!("live-{t}"));
                        if std::fs::create_dir_all(staged.join("nested")).is_ok() {
                            let _ = std::fs::write(staged.join("nested/data.bin"), vec![0u8; 128]);
                            let _ = std::fs::remove_dir_all(&live);
                            let _ = std::fs::rename(&staged, &live);
                        }
                        let _ = std::fs::remove_dir_all(&live);
                        let _ = std::fs::remove_dir_all(&staged);
                        n = n.wrapping_add(1);
                    }
                })
            })
            .collect();

        // Assert on the WORST sample, not the last one: keeping only the final
        // result would let 29 of 30 walks return a truncated total and still
        // pass. Every walk must clear the floor, not just the one we happened
        // to keep.
        let mut min_observed = u64::MAX;
        for _ in 0..30 {
            min_observed = min_observed.min(dir_size_bytes(&root));
        }

        stop.store(true, Ordering::Relaxed);
        for handle in mutators {
            handle.join().expect("mutator thread must not panic");
        }

        let floor = BRANCHES * (LEAF_BYTES + TOP_BYTES);
        assert!(
            min_observed >= floor,
            "a walk under concurrent mutation lost stable bytes: worst sample \
             {min_observed}, floor {floor}"
        );
    }

    /// The depth cap must fire at exactly [`MAX_WALK_DEPTH`], not near it.
    ///
    /// Why (issue #4764): the depth bound is half the blast-radius limit on a
    /// best-effort metric, and an unenforced constant is not a bound. Testing
    /// only that something far below the cap is excluded is too loose — it
    /// passes whether the cap fires at `MAX_WALK_DEPTH`, one level early, or
    /// one level late, so it would not catch an off-by-one that silently
    /// under-counts every deep tree.
    /// What: pins both sides of the boundary in one tree. A file whose parent
    /// sits exactly `MAX_WALK_DEPTH` levels below the root MUST be counted; a
    /// file one level deeper MUST NOT be. A root-level file is included so the
    /// cap is also shown not to disturb ordinary shallow counting.
    /// Test: this test.
    #[test]
    fn dir_size_depth_cap_boundary_is_exact() {
        const TOP_BYTES: u64 = 7;
        const AT_CAP_BYTES: u64 = 11;
        const PAST_CAP_BYTES: u64 = 4096;

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("top.bin"), vec![0u8; TOP_BYTES as usize])
            .expect("write top");

        // `at_cap` is the directory exactly MAX_WALK_DEPTH levels down.
        let mut at_cap = tmp.path().to_path_buf();
        for _ in 0..MAX_WALK_DEPTH {
            at_cap.push("d");
        }
        std::fs::create_dir_all(&at_cap).expect("create at-cap tree");
        std::fs::write(at_cap.join("at-cap.bin"), vec![0u8; AT_CAP_BYTES as usize])
            .expect("write at-cap file");

        // One level deeper — the first directory the walk must refuse to open.
        let past_cap = at_cap.join("d");
        std::fs::create_dir(&past_cap).expect("create past-cap dir");
        std::fs::write(
            past_cap.join("past-cap.bin"),
            vec![0u8; PAST_CAP_BYTES as usize],
        )
        .expect("write past-cap file");

        let total = dir_size_bytes(tmp.path());
        assert_eq!(
            total,
            TOP_BYTES + AT_CAP_BYTES,
            "depth cap is off by one: {} means the cap fired a level early \
             (the at-cap file was dropped); {} means it fired a level late \
             (the past-cap file was counted)",
            TOP_BYTES,
            TOP_BYTES + AT_CAP_BYTES + PAST_CAP_BYTES
        );
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
