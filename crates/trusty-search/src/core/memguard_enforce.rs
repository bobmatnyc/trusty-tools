//! Anonymous-RSS reading and enforcement-measure selection for the
//! steady-state memory-pressure gate (issue #3683 slice 3 — Defect 3).
//!
//! Why: split out of `core::memguard` (which was already at the 500-SLOC
//! production cap) to keep both files under it — see `check_line_cap.sh`.
//! Every function here is re-exported from `core::memguard` via `pub use`, so
//! callers keep writing `memguard::enforcement_rss_mb()` etc. unchanged;
//! this module's existence is an implementation detail of `memguard`, not a
//! new public surface.
//! What: [`anon_rss_mb_for_pid`]/[`anon_rss_mb`] read `/proc/<pid>/status`'s
//! `RssAnon` field on Linux (falling back to
//! [`crate::core::memguard::current_rss_mb_for_pid`] elsewhere);
//! [`EnforcementMeasure`]/[`enforcement_measure`]/[`enforcement_rss_mb_for_pid`]/
//! [`enforcement_rss_mb`] resolve `TRUSTY_MEMORY_ENFORCE_MEASURE` and route
//! every enforcement-path RSS read through one function so the gate, its
//! hysteresis baseline, and its sweep budget never mix anon and total RSS in
//! one comparison chain.
//! Test: see this module's own `tests` submodule.

use crate::core::memguard::current_rss_mb_for_pid;

/// Parse the `RssAnon` field (kB) out of `/proc/<pid>/status` text.
///
/// Why (issue #3683 slice 3 — Defect 3): `current_rss_mb_for_pid`'s Linux path
/// (`sysinfo`'s `Process::memory()`, i.e. `/proc/<pid>/status`'s `VmRSS`) is
/// TOTAL resident memory — anonymous heap/stack pages plus file-backed pages
/// (mmap'd redb/usearch index files, shared libraries). On the #3683
/// production workload (315K-chunk index, redb data mmap'd from an NFS/EFS
/// mount) file-backed pages dominate `VmRSS` and are pages the KERNEL can
/// (and does) reclaim under its own memory pressure without the daemon's
/// help — they are not heap the daemon's own eviction sweep can usefully
/// target. Gating enforcement on `VmRSS` reads the daemon as permanently
/// over its ceiling even when a sweep would free almost nothing, per the
/// #3683 RCA (`reclaimed_entries=415423` yet `rss_before_mb=22939 →
/// rss_after_mb=23286` — RSS *grew* across a sweep because concurrent
/// rehydration re-paged mmap data in while the sweep freed heap the
/// allocator retained anyway). `RssAnon` (`VmRSS` minus `RssFile` minus
/// `RssShmem`) isolates exactly the anonymous-heap portion the eviction
/// sweep's `HashMap::clear()` calls actually shrink.
/// What: scans `status_text` line by line for a line starting with
/// `"RssAnon:"` and parses the first whitespace-delimited token after the
/// prefix as `u64` kB. Returns `None` if the field is absent (older kernels
/// predating `RssAnon` — added in Linux 4.5 — or a malformed/truncated read)
/// or unparseable, so the caller can fall back to total RSS rather than
/// silently returning `0`.
/// Test: `tests::parse_rss_anon_kb_reads_field_from_realistic_status_text`,
/// `tests::parse_rss_anon_kb_missing_field_returns_none`,
/// `tests::parse_rss_anon_kb_malformed_value_returns_none`.
///
/// Kept compiled (and directly tested) on every platform, not just Linux —
/// it is pure string parsing with no `/proc` dependency, so exercising it
/// cross-platform is free and catches a logic regression without needing a
/// real Linux `/proc` mount. Only its sole caller ([`read_rss_anon_kb_for_pid`])
/// is Linux-only, hence the `allow(dead_code)` on non-Linux builds.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_rss_anon_kb(status_text: &str) -> Option<u64> {
    for line in status_text.lines() {
        if let Some(rest) = line.strip_prefix("RssAnon:") {
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

/// Read and parse `RssAnon` (MB) for `pid` from `/proc/<pid>/status`
/// (Linux-only — see [`parse_rss_anon_kb`] for the field semantics).
///
/// What: `None` if the file can't be read (process exited, permission
/// denied sampling a foreign pid) or the field is missing/malformed.
/// Test: covered transitively by `tests::test_anon_rss_for_self_pid_on_linux`
/// (reads this process's own `/proc/self/status`, the one status file every
/// caller is guaranteed permission to read).
#[cfg(target_os = "linux")]
fn read_rss_anon_kb_for_pid(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_rss_anon_kb(&text)
}

/// Anonymous-RSS reading (MB) for `pid`, current process included — the
/// measure [`enforcement_rss_mb_for_pid`] uses by default on Linux.
///
/// Why the macOS fallback (issue #3683 slice 3): Linux's `/proc/<pid>/status`
/// exposes `RssAnon` as a distinct field split out from `RssFile`/`RssShmem`;
/// macOS's process-memory APIs (`proc_pidinfo`, `task_info`) have no
/// equivalent split — `phys_footprint` (what
/// `trusty_common::sys_metrics::physical_footprint_mb` already reads, and
/// what [`current_rss_mb_for_pid`] already prefers on macOS) is Apple's own
/// curated "memory that actually counts against you" accounting: it already
/// EXCLUDES clean/purgeable/compressible file-backed pages the kernel can
/// drop under pressure, which is the same class of page Linux's plain
/// `VmRSS` wrongly includes and `RssAnon` is meant to exclude. So on macOS,
/// [`current_rss_mb_for_pid`] (i.e. total RSS, via `phys_footprint`) is
/// already the closer-to-anonymous-semantics reading — reusing it here
/// (rather than inventing a second macOS-specific "anon-like" sampler) avoids
/// two independently-maintained approximations of the same underlying
/// question. This is also why [`default_enforcement_measure`] resolves to
/// `Total` on macOS: there, "total" (via `phys_footprint`) already IS the
/// anon-equivalent measure, so a distinct "anon" mode would just alias it.
/// What: `None` if `pid == 0`. On Linux, [`read_rss_anon_kb_for_pid`] divided
/// to MB (truncating). On every other platform, delegates to
/// [`current_rss_mb_for_pid`] per the rationale above.
/// Test: `tests::test_anon_rss_for_pid_zero_returns_none`,
/// `tests::test_anon_rss_for_self_pid_on_linux` (Linux),
/// `tests::test_anon_rss_falls_back_to_total_on_non_linux` (non-Linux).
pub fn anon_rss_mb_for_pid(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        read_rss_anon_kb_for_pid(pid).map(|kb| kb / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        current_rss_mb_for_pid(pid)
    }
}

/// Anonymous RSS (MB) for the current process. See [`anon_rss_mb_for_pid`].
pub fn anon_rss_mb() -> Option<u64> {
    anon_rss_mb_for_pid(std::process::id())
}

// ---------------------------------------------------------------------------
// Enforcement-measure selection (issue #3683 slice 3 — Defect 3)
//
// `over_high_water`/`over_memory_limit` (in `core::memguard`) are
// measure-agnostic: they just compare whatever RSS figure the caller hands
// them against a threshold. The steady-state pressure ticker
// (`service::server::tickers::run_memory_pressure_tick`) is the ENFORCEMENT
// caller — it decides whether to reclaim caches — and it is the one gate
// this slice repoints at anonymous RSS by default, so a
// file-backed-mmap-heavy workload stops reading as permanently over its
// ceiling. Total RSS stays available (and is still what `/health` and the
// reindex pipeline's `over_memory_limit` report/gate on) via `current_rss_mb`
// unchanged.
// ---------------------------------------------------------------------------

/// Which RSS reading the steady-state memory-pressure ENFORCEMENT gate
/// (`over_high_water`'s callers in `run_memory_pressure_tick`, plus its own
/// hysteresis baseline) uses.
///
/// Test: `tests::enforcement_measure_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementMeasure {
    /// [`anon_rss_mb_for_pid`] — anonymous RSS only, excluding file-backed
    /// mmap pages the kernel can reclaim on its own.
    Anon,
    /// [`current_rss_mb_for_pid`] — total RSS, including file-backed pages.
    Total,
}

/// Platform default for [`enforcement_measure`] when
/// `TRUSTY_MEMORY_ENFORCE_MEASURE` is unset/unparseable: `Anon` on Linux
/// (where `RssAnon` is a real, distinct field — see [`anon_rss_mb_for_pid`]'s
/// doc comment for why), `Total` everywhere else (macOS's `current_rss_mb`
/// already reads `phys_footprint`, which is already anon-equivalent in spirit
/// — see the same doc comment for the full rationale).
fn default_enforcement_measure() -> EnforcementMeasure {
    #[cfg(target_os = "linux")]
    {
        EnforcementMeasure::Anon
    }
    #[cfg(not(target_os = "linux"))]
    {
        EnforcementMeasure::Total
    }
}

/// Resolve the active enforcement measure from `TRUSTY_MEMORY_ENFORCE_MEASURE`
/// (`"anon"` / `"total"`, case-insensitive), falling back to
/// [`default_enforcement_measure`] when unset or set to anything else.
///
/// Why: operators on an unusual workload (e.g. one where file-backed pages
/// genuinely are the daemon's own leak, not reclaimable kernel-owned cache)
/// need an escape hatch back to the pre-slice-3 total-RSS gate without a
/// rebuild. Mirrors `enforce_interval_secs`/`high_water_pct`'s read-fresh-
/// every-call style (no caching) rather than the `AtomicU64`-cached
/// `memory_limit_mb` pattern — this is read at most once per enforcement
/// tick (default every 30s), not on any request hot path, so there is no
/// performance reason to cache it, and reading fresh means a live `PATCH
/// /config`-style retune (should one ever be added) needs no extra plumbing.
/// Test: `tests::enforcement_measure_defaults_to_platform_default_when_unset`,
/// `tests::enforcement_measure_env_override_anon_and_total`,
/// `tests::enforcement_measure_invalid_value_falls_back_to_default`.
pub fn enforcement_measure() -> EnforcementMeasure {
    match std::env::var("TRUSTY_MEMORY_ENFORCE_MEASURE") {
        Ok(v) if !v.trim().is_empty() => match v.trim().to_ascii_lowercase().as_str() {
            "anon" => EnforcementMeasure::Anon,
            "total" => EnforcementMeasure::Total,
            other => {
                let default = default_enforcement_measure();
                tracing::warn!(
                    "TRUSTY_MEMORY_ENFORCE_MEASURE={other:?} is not \"anon\" or \"total\"; \
                     using platform default ({default:?})"
                );
                default
            }
        },
        _ => default_enforcement_measure(),
    }
}

/// RSS (MB) for `pid` under the currently-active [`enforcement_measure`] —
/// what `run_memory_pressure_tick` feeds to `over_high_water`,
/// `should_reclaim_now`, and the sweep's `target_freed_mb` calculation.
///
/// Why a single entry point: the slice-2 hysteresis invariant is that
/// `last_reclaim_rss_mb` comparisons (`should_reclaim_now`) must use the SAME
/// measure as the gate that set them — mixing anon and total RSS in one
/// comparison chain would make the hysteresis baseline meaningless (e.g. a
/// total-RSS baseline compared against a fresh anon-RSS sample). Routing
/// every enforcement-path RSS read through this one function makes that
/// invariant structural rather than a convention callers have to remember.
/// Test: `tests::enforcement_rss_mb_for_pid_matches_chosen_measure`.
pub fn enforcement_rss_mb_for_pid(pid: u32) -> Option<u64> {
    match enforcement_measure() {
        EnforcementMeasure::Anon => anon_rss_mb_for_pid(pid),
        EnforcementMeasure::Total => current_rss_mb_for_pid(pid),
    }
}

/// [`enforcement_rss_mb_for_pid`] for the current process.
pub fn enforcement_rss_mb() -> Option<u64> {
    enforcement_rss_mb_for_pid(std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // `parse_rss_anon_kb` — pure fixture-driven parser tests (issue #3683
    // slice 3)
    // -----------------------------------------------------------------------

    /// A realistic (trimmed) `/proc/<pid>/status` excerpt, matching the real
    /// kernel's tab-separated `Field:\tvalue kB` format, with `RssAnon`
    /// surrounded by its sibling `Rss*` fields so the parser is proven to
    /// pick out the right line rather than, say, matching on a bare `"Rss"`
    /// substring that would also hit `RssFile`/`RssShmem`/`RssHWM`.
    const SAMPLE_PROC_STATUS: &str = "Name:\tcat\n\
         State:\tR (running)\n\
         VmPeak:\t   25936 kB\n\
         VmSize:\t   25936 kB\n\
         VmRSS:\t     5236 kB\n\
         RssHWM:\t     5236 kB\n\
         RssAnon:\t     1234 kB\n\
         RssFile:\t     3860 kB\n\
         RssShmem:\t     142 kB\n\
         VmData:\t     436 kB\n";

    #[test]
    fn parse_rss_anon_kb_reads_field_from_realistic_status_text() {
        assert_eq!(parse_rss_anon_kb(SAMPLE_PROC_STATUS), Some(1234));
    }

    #[test]
    fn parse_rss_anon_kb_missing_field_returns_none() {
        // Pre-4.5 kernels (or a hidden /proc) never had RssAnon at all.
        let without_field = "Name:\tcat\nVmRSS:\t     5236 kB\nRssFile:\t     3860 kB\n";
        assert_eq!(parse_rss_anon_kb(without_field), None);
        assert_eq!(parse_rss_anon_kb(""), None);
    }

    #[test]
    fn parse_rss_anon_kb_malformed_value_returns_none() {
        // A truncated read (e.g. racing a process exit) could cut the value
        // off entirely, or a hostile/corrupt /proc entry could hold garbage —
        // either way this must return None, never panic or silently return 0.
        assert_eq!(parse_rss_anon_kb("RssAnon:\n"), None);
        assert_eq!(parse_rss_anon_kb("RssAnon:\tnot-a-number kB\n"), None);
    }

    // -----------------------------------------------------------------------
    // `anon_rss_mb_for_pid` (issue #3683 slice 3)
    // -----------------------------------------------------------------------

    #[test]
    fn test_anon_rss_for_pid_zero_returns_none() {
        assert_eq!(anon_rss_mb_for_pid(0), None);
    }

    /// On Linux, sampling this test process's own `/proc/self/status` (via
    /// `pid = std::process::id()`) must return a sane, nonzero anon-RSS
    /// reading that never exceeds the process's TOTAL RSS — anonymous pages
    /// are a subset of total resident pages by definition.
    #[test]
    #[cfg(target_os = "linux")]
    fn test_anon_rss_for_self_pid_on_linux() {
        let pid = std::process::id();
        let Some(anon) = anon_rss_mb_for_pid(pid) else {
            // Only plausible if /proc is hidden (unusual container profile);
            // tolerate it rather than fail CI on an environment quirk.
            return;
        };
        assert!(
            anon > 0,
            "self-process anon RSS should be > 0 MB, got {anon}"
        );
        if let Some(total) = current_rss_mb_for_pid(pid) {
            assert!(
                anon <= total,
                "anon RSS ({anon} MB) must never exceed total RSS ({total} MB) — anon is a \
                 subset of total resident pages"
            );
        }
    }

    /// On non-Linux (macOS/other), `anon_rss_mb_for_pid` must fall back to
    /// exactly the same reading as `current_rss_mb_for_pid` — see that
    /// function's doc comment for why `phys_footprint` already serves as the
    /// anon-equivalent measure there.
    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_anon_rss_falls_back_to_total_on_non_linux() {
        let pid = std::process::id();
        assert_eq!(anon_rss_mb_for_pid(pid), current_rss_mb_for_pid(pid));
    }

    // -----------------------------------------------------------------------
    // `enforcement_measure` / `enforcement_rss_mb_for_pid` (issue #3683
    // slice 3)
    // -----------------------------------------------------------------------

    /// RAII guard saving/restoring `TRUSTY_MEMORY_ENFORCE_MEASURE` — mirrors
    /// `service::server::memory_pressure_tests::ExemptSecsEnvGuard`'s
    /// save/restore convention for a process-global env var this test module
    /// is the sole mutator of.
    struct EnforceMeasureEnvGuard(Option<String>);

    impl EnforceMeasureEnvGuard {
        fn set(v: &str) -> Self {
            let prior = std::env::var("TRUSTY_MEMORY_ENFORCE_MEASURE").ok();
            // SAFETY: this test module is the sole reader/writer of
            // TRUSTY_MEMORY_ENFORCE_MEASURE in this crate's test suite.
            unsafe { std::env::set_var("TRUSTY_MEMORY_ENFORCE_MEASURE", v) };
            Self(prior)
        }

        fn unset() -> Self {
            let prior = std::env::var("TRUSTY_MEMORY_ENFORCE_MEASURE").ok();
            // SAFETY: see above.
            unsafe { std::env::remove_var("TRUSTY_MEMORY_ENFORCE_MEASURE") };
            Self(prior)
        }
    }

    impl Drop for EnforceMeasureEnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`'s comment.
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("TRUSTY_MEMORY_ENFORCE_MEASURE", v),
                    None => std::env::remove_var("TRUSTY_MEMORY_ENFORCE_MEASURE"),
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn enforcement_measure_defaults_to_platform_default_when_unset() {
        let _guard = EnforceMeasureEnvGuard::unset();
        let expected = default_enforcement_measure();
        assert_eq!(enforcement_measure(), expected);
        #[cfg(target_os = "linux")]
        assert_eq!(expected, EnforcementMeasure::Anon);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(expected, EnforcementMeasure::Total);
    }

    #[test]
    #[serial_test::serial]
    fn enforcement_measure_env_override_anon_and_total() {
        {
            let _guard = EnforceMeasureEnvGuard::set("anon");
            assert_eq!(enforcement_measure(), EnforcementMeasure::Anon);
        }
        {
            // Case-insensitive, and tolerant of incidental whitespace.
            let _guard = EnforceMeasureEnvGuard::set(" TOTAL ");
            assert_eq!(enforcement_measure(), EnforcementMeasure::Total);
        }
    }

    #[test]
    #[serial_test::serial]
    fn enforcement_measure_invalid_value_falls_back_to_default() {
        let _guard = EnforceMeasureEnvGuard::set("bogus-value");
        assert_eq!(enforcement_measure(), default_enforcement_measure());
    }

    /// `enforcement_rss_mb_for_pid` must delegate to exactly the reading
    /// [`enforcement_measure`] selects — the structural invariant that keeps
    /// `run_memory_pressure_tick`'s gate, hysteresis baseline, and
    /// `target_freed_mb` calculation all reading the SAME measure (issue
    /// #3683 slice 3's core constraint: never mix anon and total RSS in one
    /// comparison chain).
    #[test]
    #[serial_test::serial]
    fn enforcement_rss_mb_for_pid_matches_chosen_measure() {
        let pid = std::process::id();
        {
            let _guard = EnforceMeasureEnvGuard::set("total");
            assert_eq!(enforcement_rss_mb_for_pid(pid), current_rss_mb_for_pid(pid));
        }
        {
            let _guard = EnforceMeasureEnvGuard::set("anon");
            assert_eq!(enforcement_rss_mb_for_pid(pid), anon_rss_mb_for_pid(pid));
        }
        // pid=0 sentinel must still short-circuit to None regardless of
        // which measure is selected.
        assert_eq!(enforcement_rss_mb_for_pid(0), None);
    }
}
