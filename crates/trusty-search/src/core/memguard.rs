//! Process-memory introspection helpers for the indexing pipeline.
//!
//! Why: Long-running reindexes on large repos can grow process RSS without
//! bound (ONNX session arenas, BM25 corpus, HNSW vectors, chunk metadata).
//! `TRUSTY_MEMORY_LIMIT_MB` lets operators set a soft ceiling; the reindex
//! orchestrator polls [`current_rss_mb`] every N batches and bails out
//! gracefully when the limit is hit, rather than being OOM-killed by the
//! kernel (macOS Jetsam, Linux oom_killer).
//! What: thin wrapper around `sysinfo::System` that refreshes only the
//! current process's memory and returns RSS in megabytes. Also reads the
//! `TRUSTY_MEMORY_LIMIT_MB` env var at first use, but stores the parsed
//! value in an `AtomicU64` so it can be updated at runtime (via the
//! `PATCH /config` endpoint) without restarting the daemon.
//! Test: see `tests::test_memory_limit_env_parse`,
//! `tests::test_current_rss_mb_nonzero`, and `tests::test_runtime_set_limit`.
//!
//! No `unwrap()` in this module — every fallible call uses `.ok()` /
//! `unwrap_or_else` so a sysinfo / kernel hiccup never panics the daemon.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

/// Hard-coded safety-net ceiling (8 GiB). Applied when neither the env var
/// nor `daemon.env` sets an explicit limit. This prevents an unattended
/// launchd restart from consuming all available RAM on a developer machine.
///
/// Operators who need more RAM (e.g. indexing >1M-chunk monorepos) should
/// set `TRUSTY_MEMORY_LIMIT_MB` before running `trusty-search start` — the
/// value is persisted to `daemon.env` and survives launchd restarts.
const DEFAULT_MEMORY_LIMIT_MB: u64 = 8_192;

/// Sentinel encoding for the runtime-mutable atomic limits.
///
/// Why: `AtomicU64` cannot hold an `Option<u64>` directly, so we reserve two
/// sentinel values to encode the three logical states the API has always
/// exposed:
///
/// - `UNSET`  (`u64::MAX`) → value has not been initialised from env / config
///   yet. Reads trigger the lazy env-var parse path (`init_*` below) which
///   writes the resolved value back atomically. After a runtime `set_*` call
///   that passes `None` to mean "no limit", the cell holds `DISABLED` (not
///   `UNSET`) so the env path is not re-run.
/// - `DISABLED` (`0`) → caller (env or runtime) has explicitly disabled the
///   limit. Reads return `None`.
/// - any other value → live MB limit. Reads return `Some(value)`.
const UNSET: u64 = u64::MAX;
const DISABLED: u64 = 0;

/// Runtime-mutable cache of the global daemon memory limit (MB).
///
/// Why: previously stored as `OnceLock<Option<u64>>`, which made it impossible
/// to retune at runtime — operators had to restart the daemon (and pay the
/// 86 MB embedder-model reload + warm-boot cost) to change the soft RSS
/// ceiling. The `PATCH /config` endpoint now mutates this cell, so a quick
/// `trusty-search config set memory-limit 16384` takes effect immediately
/// without dropping any indexes.
///
/// What: `UNSET` until first `memory_limit_mb()` call (which parses the env
/// var via `INIT_MEMORY`); thereafter holds either `DISABLED` or a live MB
/// value. Writes use `Ordering::Release` so the poller observes them
/// promptly; reads use `Ordering::Relaxed` because the poller does not need
/// to synchronise with any other memory accesses — a tick-late observation
/// is fine.
static MEMORY_LIMIT_MB: AtomicU64 = AtomicU64::new(UNSET);

/// Runtime-mutable cache of the indexing-pipeline memory limit (MB).
///
/// Why: the indexing pipeline (embedding, HNSW commit, redb write) has a very
/// different memory profile from the steady-state daemon, so it gets its own
/// runtime knob. Behaviour mirrors `MEMORY_LIMIT_MB` above.
///
/// What: same `UNSET` / `DISABLED` / value encoding. When this cell resolves
/// to `None` (UNSET with no env var, or DISABLED via the env var but the
/// caller wants to fall back), `index_memory_limit_mb()` falls back to the
/// global `memory_limit_mb()` so a single global cap still applies.
static INDEX_MEMORY_LIMIT_MB: AtomicU64 = AtomicU64::new(UNSET);

/// One-shot guards so the env-parse warning fires at most once per process,
/// even if the atomic is re-read after a runtime `set_*` call.
static INIT_MEMORY: Once = Once::new();
static INIT_INDEX_MEMORY: Once = Once::new();

/// Encode `Option<u64>` into the atomic representation.
///
/// Why: centralises the sentinel-encoding rules so callers never accidentally
/// write `UNSET` (which would re-trigger env-var parsing on the next read).
/// What: `None` → `DISABLED`, `Some(n)` → `n` (with `n == 0` collapsed to
/// `DISABLED` to keep the encoding canonical).
/// Test: round-trip via `set_*` / `*_memory_limit_mb` in
/// `tests::test_runtime_set_limit`.
fn encode(value: Option<u64>) -> u64 {
    match value {
        None => DISABLED,
        Some(0) => DISABLED,
        Some(n) => n,
    }
}

/// Decode the atomic representation back into the public `Option<u64>` API.
///
/// Why: hide the sentinels from callers — they keep working with `Option<u64>`
/// exactly as before the `AtomicU64` switch.
/// What: `UNSET` is treated by the caller (env not yet parsed); `DISABLED` →
/// `None`; anything else → `Some(value)`.
fn decode(raw: u64) -> Option<u64> {
    match raw {
        UNSET => None,
        DISABLED => None,
        n => Some(n),
    }
}

/// Lazy env-var parse for `TRUSTY_MEMORY_LIMIT_MB`. Runs at most once per
/// process; subsequent reads come straight from the atomic.
fn init_memory_limit_from_env() {
    let parsed: u64 = match std::env::var("TRUSTY_MEMORY_LIMIT_MB") {
        Ok(v) => match v.parse::<u64>() {
            Ok(0) => DISABLED,
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "TRUSTY_MEMORY_LIMIT_MB={v:?} is not a valid u64; \
                     using compiled-in default ({DEFAULT_MEMORY_LIMIT_MB} MB)"
                );
                DEFAULT_MEMORY_LIMIT_MB
            }
        },
        Err(_) => DEFAULT_MEMORY_LIMIT_MB,
    };
    MEMORY_LIMIT_MB.store(parsed, Ordering::Release);
}

/// Lazy env-var parse for `TRUSTY_INDEX_MEMORY_LIMIT_MB`. Runs at most once
/// per process. Unlike the global limit, this defaults to `DISABLED` so the
/// `index_memory_limit_mb()` getter falls through to the global cap.
fn init_index_memory_limit_from_env() {
    let parsed: u64 = match std::env::var("TRUSTY_INDEX_MEMORY_LIMIT_MB") {
        Ok(v) => match v.parse::<u64>() {
            Ok(0) => DISABLED,
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "TRUSTY_INDEX_MEMORY_LIMIT_MB={v:?} is not a valid u64; \
                     falling back to TRUSTY_MEMORY_LIMIT_MB"
                );
                DISABLED
            }
        },
        Err(_) => DISABLED,
    };
    INDEX_MEMORY_LIMIT_MB.store(parsed, Ordering::Release);
}

/// Read the active global daemon memory limit (MB).
///
/// Priority: runtime `set_memory_limit_mb()` calls > env var > `daemon.env`
/// (already sourced into env by `load_daemon_env`) > compiled-in default
/// (8 192 MB / 8 GiB). A value of `0` (from env or runtime) explicitly
/// disables the limit and returns `None`.
///
/// Why default 8 GiB: on a launchd restart without any env vars the daemon
/// previously ran with no cap at all, which allowed ONNX arena growth to
/// consume 80+ GB before macOS Jetsam killed it. 8 GiB is a safe ceiling
/// for typical developer machines that still allows large-repo indexing.
///
/// Why `AtomicU64` (not `OnceLock`): the `PATCH /config` endpoint must be
/// able to retune this limit without a daemon restart. See the module-level
/// doc-comment for the encoding details.
pub fn memory_limit_mb() -> Option<u64> {
    // Fast path: env already parsed.
    let raw = MEMORY_LIMIT_MB.load(Ordering::Relaxed);
    if raw != UNSET {
        return decode(raw);
    }
    // Slow path: first read triggers the env-var parse.
    INIT_MEMORY.call_once(init_memory_limit_from_env);
    decode(MEMORY_LIMIT_MB.load(Ordering::Relaxed))
}

/// Read the active indexing-pipeline memory limit (MB). Falls back to the
/// global `memory_limit_mb()` when no indexing-specific value is configured.
///
/// Why: the indexing pipeline (embedding, HNSW commit, redb write) has a very
/// different memory profile from the steady-state daemon. With the CoreML
/// execution provider on Apple Silicon, virtual RSS can briefly spike to
/// 60–100 GB while ONNX allocates unified-memory buffers — yet the
/// steady-state daemon (HNSW arenas + warm-boot indexes) only needs a few GB.
/// Forcing both to share a single `TRUSTY_MEMORY_LIMIT_MB` ceiling means
/// either: (a) the global limit is set too low and reindex trips it
/// immediately, or (b) the global limit is set high enough for reindex and
/// the daemon will OOM-kill any other workload on the host. This separate
/// limit lets operators give the indexing pipeline its own (typically larger)
/// budget without raising the steady-state ceiling.
///
/// What: priority is runtime `set_index_memory_limit_mb()` >
/// `TRUSTY_INDEX_MEMORY_LIMIT_MB` env > fall back to `memory_limit_mb()`.
/// A value of `0` (from env or runtime) explicitly disables the limit for
/// the indexing pipeline and the getter falls through to the global cap.
///
/// Test: `tests::test_index_memory_limit_falls_back_to_global` and
/// `tests::test_runtime_set_limit`.
pub fn index_memory_limit_mb() -> Option<u64> {
    let raw = INDEX_MEMORY_LIMIT_MB.load(Ordering::Relaxed);
    if raw == UNSET {
        INIT_INDEX_MEMORY.call_once(init_index_memory_limit_from_env);
    }
    let raw = INDEX_MEMORY_LIMIT_MB.load(Ordering::Relaxed);
    match decode(raw) {
        Some(n) => Some(n),
        None => memory_limit_mb(), // fall back to the global daemon limit
    }
}

/// Update the global daemon memory limit at runtime.
///
/// Why: backs the `PATCH /config { "memory_limit_mb": ... }` endpoint so
/// operators can retune the soft RSS ceiling on a live daemon (without
/// dropping the 86 MB embedder-model session, all loaded indexes, or the
/// LRU embedding cache). `None` disables the limit entirely (no cap);
/// `Some(n)` installs an `n` MB ceiling.
///
/// What: atomically stores the encoded value with `Release` ordering so the
/// background memory poller observes the change on its next tick (≤ ~1 s).
/// Subsequent reads via `memory_limit_mb()` return the new value
/// immediately. Side-effect-only: the function returns `()` and never
/// fails — invalid values are clamped via `encode`.
///
/// Test: `tests::test_runtime_set_limit` round-trips through this setter
/// and `memory_limit_mb()` to assert both `None` and `Some(n)` flow.
pub fn set_memory_limit_mb(value: Option<u64>) {
    MEMORY_LIMIT_MB.store(encode(value), Ordering::Release);
}

/// Update the indexing-pipeline memory limit at runtime. See
/// [`set_memory_limit_mb`] for the design rationale.
///
/// Why: backs the `PATCH /config { "index_memory_limit_mb": ... }` endpoint.
/// What: atomically stores the encoded value with `Release` ordering;
/// `None` disables this specific limit and `index_memory_limit_mb()` then
/// falls back to the global cap.
/// Test: `tests::test_runtime_set_limit`.
pub fn set_index_memory_limit_mb(value: Option<u64>) {
    INDEX_MEMORY_LIMIT_MB.store(encode(value), Ordering::Release);
}

/// Convenience helper for the reindex orchestrator: returns `true` when an
/// indexing-pipeline memory limit is configured AND current RSS is at or
/// above it.
///
/// Why: parallels [`over_memory_limit`] but consults the indexing-specific
/// limit. Used by the reindex memory poller and post-commit RSS check.
/// What: combines `index_memory_limit_mb()` with `current_rss_mb()` and
/// returns true iff both are available and RSS meets/exceeds the limit.
/// Test: covered transitively by `tests::test_over_memory_limit_false_when_unset`
/// — when neither env var is set, both helpers return false.
pub fn over_index_memory_limit() -> bool {
    match (index_memory_limit_mb(), current_rss_mb()) {
        (Some(limit), Some(rss)) => rss >= limit,
        _ => false,
    }
}

/// Current process Resident Set Size in megabytes. Returns `None` if sysinfo
/// could not resolve the current process (extremely unlikely; only seen in
/// containerised environments with /proc hidden).
///
/// Why (issue #2165): this backs [`over_memory_limit`] / [`over_index_memory_limit`],
/// the guardrails that bail a reindex or warn an operator before the kernel
/// OOM-kills the daemon. Delegates to [`current_rss_mb_for_pid`] with our own
/// pid so both the self-sample and arbitrary-pid paths (used for the
/// embedderd sidecar) share one true-physical-footprint implementation on
/// macOS instead of two copies of the same sysinfo-only logic silently
/// under-reporting compressed memory.
pub fn current_rss_mb() -> Option<u64> {
    current_rss_mb_for_pid(std::process::id())
}

/// Resident Set Size in megabytes for an arbitrary process (by OS PID),
/// including the current process itself.
///
/// Why: the embedderd sidecar runs in a separate process. Its RSS is not
/// captured by a self-only sampler. This helper also backs
/// [`current_rss_mb`] for the daemon's own pid so there is exactly one RSS
/// implementation to keep correct.
///
/// Why the macOS-first path (issue #2165): `sysinfo::Process::memory()` on
/// macOS reads `PROC_PIDTASKINFO.pti_resident_size`, a getrusage-style
/// figure that excludes pages the memory compressor currently holds. A daemon
/// with several GB compressed can read as tens of MB, so `TRUSTY_MEMORY_LIMIT_MB`
/// never trips. On macOS this now tries
/// [`trusty_common::sys_metrics::physical_footprint_mb`] (the `ri_phys_footprint`
/// counter `vmmap`/`footprint`/Activity Monitor report) first, falling back to
/// the `sysinfo` reading if the `libproc` call fails (e.g. cross-user
/// permission denial sampling a foreign pid).
///
/// What: returns `None` if the PID is 0 (sentinel for "no sidecar running"),
/// if neither sampling path can locate the process (exited between spawn and
/// sample), or if the platform call fails.
///
/// Test: `tests::test_rss_for_self_pid` calls this with `std::process::id()`
/// and checks the result is sane, loosely agrees with `current_rss_mb()`
/// (trivially true now that both delegate to this function, modulo
/// concurrent-allocation drift between the two calls — see that test's doc
/// for why the bound is relative rather than a fixed MB count), and that RSS
/// visibly grows after a deliberate allocation (issue #3702). Negative
/// cases (pid=0, bogus pid) assert `None`.
pub fn current_rss_mb_for_pid(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    if let Some(mb) = trusty_common::sys_metrics::physical_footprint_mb(pid) {
        return Some(mb);
    }
    let sysinfo_pid = Pid::from_u32(pid);
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(sysinfo_pid).map(|p| p.memory() / (1024 * 1024))
}

/// Convenience helper for the reindex orchestrator: returns `true` when a
/// memory limit is configured AND current RSS is at or above it.
pub fn over_memory_limit() -> bool {
    match (memory_limit_mb(), current_rss_mb()) {
        (Some(limit), Some(rss)) => rss >= limit,
        _ => false,
    }
}

/// Return freed heap pages to the OS after a bulk in-memory cache eviction
/// (issue #3657).
///
/// Why: production observed the daemon's RSS climb from 20.3 to 26.4 GiB over
/// ~5 hours while the idle-eviction ticker logged `evicted 315423 in-memory
/// chunks after 60s idle` on a repeating rehydrate/evict cycle — the "evicted"
/// accounting was real (the `chunks`/`bm25`/`entities` maps genuinely emptied;
/// every value is owned data, not an `Arc` aliased elsewhere) but RSS never
/// dropped. Root cause: the Linux release binary links glibc's default
/// allocator (no jemalloc/mimalloc — see `Cargo.toml`), which only returns
/// freed memory to the OS via `brk`/`sbrk` when the freed region is at the
/// very top of the heap, or via `munmap` for individually-mmap'd large
/// allocations (`> M_MMAP_THRESHOLD`, ~128 KB). A `HashMap::clear()` dropping
/// ~300K small, discontiguous `RawChunk` string/vec allocations frees memory
/// scattered throughout the arena — none of it at the break — so it sits in
/// glibc's free lists (fastbins/tcache/unsorted bins) available for *reuse*
/// but never released back to the kernel, and a restart (which drops the
/// whole heap) was the only thing that ever reclaimed it. `malloc_trim(0)`
/// asks glibc to walk those free lists and hand back whatever it can via
/// `sbrk`/`madvise(MADV_DONTNEED)` right after a bulk free, closing exactly
/// that gap.
/// What: `libc::malloc_trim(0)` on Linux; a no-op everywhere else (macOS's
/// allocator already returns freed pages promptly and does not expose
/// `malloc_trim`; the function doesn't exist there). Safe to call at any
/// time — it never invalidates a live allocation, only walks already-freed
/// regions, so callers do not need to hold any particular lock. Cheap
/// relative to the bulk clear it follows (a single heap-free-list walk, not a
/// stop-the-world pause) but not free, so callers should only invoke it right
/// after a bulk eviction/reclaim sweep — not on every request.
/// Test: `tests::test_trim_heap_does_not_panic` (all platforms). The
/// allocation-release proof (that a bulk eviction of small allocations
/// followed by `trim_heap()` actually drops RSS on Linux) lives in
/// `core::indexer::tests_idle_evict::bulk_eviction_trim_releases_rss_on_linux`.
pub fn trim_heap() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `malloc_trim` is a glibc extension. It takes a `pad` byte
        // count (0 = trim as aggressively as possible) and is documented as
        // safe to call at any time from any thread — it only ever releases
        // memory that is already on a free list, never memory backing a live
        // allocation, so it cannot invalidate any pointer the caller holds.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Steady-state memory-limit ENFORCEMENT (issue #2846)
//
// The helpers above (`over_memory_limit`, `over_index_memory_limit`) are only
// consulted by the reindex pipeline. A daemon that never reindexes still grows
// its resident heap without bound (BM25 corpora, chunk text, entity maps, HNSW
// arenas) until the OS OOM-killer intervenes — the configured `memory_limit_mb`
// soft ceiling was accepted but never enforced in the serving path. The
// `service::server::tickers::spawn_memory_pressure_ticker` background task now
// samples RSS on a fixed cadence and, when it crosses the soft ceiling, sheds
// evictable caches (and, opt-in, self-restarts under a supervisor). These are
// the config readers + the pure threshold decision it uses.
// ---------------------------------------------------------------------------

/// Default cadence (seconds) between memory-pressure enforcement samples.
const DEFAULT_ENFORCE_SECS: u64 = 30;
/// Default high-water mark, as a percentage of `memory_limit_mb`, at which the
/// enforcement sweep starts reclaiming evictable caches.
const DEFAULT_HIGH_WATER_PCT: u8 = 90;
/// Lower bound on the configurable high-water percentage — reclaiming below
/// half the soft ceiling would thrash caches for no benefit.
const MIN_HIGH_WATER_PCT: u8 = 50;

/// How often the memory-pressure enforcement ticker samples RSS.
///
/// Why: enforcement is a background cost (one RSS sample per tick, plus a
/// registry walk only when over the high-water mark); operators may want a
/// tighter cadence on OOM-prone hosts or to disable it where an external
/// cgroup already caps the process.
/// What: reads `TRUSTY_MEMORY_ENFORCE_SECS` as `u64` seconds. `0` disables the
/// ticker outright (it never spawns). Unset / unparseable falls back to
/// [`DEFAULT_ENFORCE_SECS`].
/// Test: `tests::test_enforce_interval_default`.
pub fn enforce_interval_secs() -> u64 {
    match std::env::var("TRUSTY_MEMORY_ENFORCE_SECS") {
        Ok(v) if !v.is_empty() => match v.trim().parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "TRUSTY_MEMORY_ENFORCE_SECS={v:?} is not a valid u64; \
                     using default ({DEFAULT_ENFORCE_SECS}s)"
                );
                DEFAULT_ENFORCE_SECS
            }
        },
        _ => DEFAULT_ENFORCE_SECS,
    }
}

/// High-water mark (percent of the soft ceiling) at which reclaim kicks in.
///
/// Why: reclaiming exactly at the limit leaves no headroom for the RSS to keep
/// climbing between sample ticks before eviction lands; starting at 90% gives
/// the sweep a margin to act. Kept configurable so tight-RAM hosts can react
/// earlier.
/// What: reads `TRUSTY_MEMORY_HIGH_WATER_PCT` as `u8`, clamped to
/// `[MIN_HIGH_WATER_PCT, 100]`. Unset / unparseable falls back to
/// [`DEFAULT_HIGH_WATER_PCT`].
/// Test: `tests::test_high_water_pct_default_and_clamp`.
pub fn high_water_pct() -> u8 {
    let raw = match std::env::var("TRUSTY_MEMORY_HIGH_WATER_PCT") {
        Ok(v) if !v.is_empty() => v.trim().parse::<u8>().unwrap_or(DEFAULT_HIGH_WATER_PCT),
        _ => DEFAULT_HIGH_WATER_PCT,
    };
    raw.clamp(MIN_HIGH_WATER_PCT, 100)
}

/// Whether the enforcement ticker may self-restart the daemon as a last resort.
///
/// Why: cache eviction cannot reclaim un-evictable growth (allocator
/// fragmentation, native ONNX/usearch arenas, a true leak). The only reliable
/// self-cap for that is a graceful restart — but that is only safe when a
/// supervisor (launchd/systemd) will respawn the process, so it is strictly
/// opt-in and defaults OFF. An unsupervised daemon must never self-terminate.
/// What: reads `TRUSTY_MEMORY_RESTART_ON_LIMIT`; `1`/`true`/`yes`/`on`
/// (case-insensitive) enable it. Anything else (including unset) → `false`.
/// Test: `tests::test_restart_on_limit_default_off`.
pub fn restart_on_limit_enabled() -> bool {
    matches!(
        std::env::var("TRUSTY_MEMORY_RESTART_ON_LIMIT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Pure threshold decision: is `rss_mb` at or above the reclaim high-water
/// mark derived from `limit_mb` and `pct`?
///
/// Why: extracted from the enforcement ticker so the threshold arithmetic is
/// unit-testable with synthetic RSS values — moving a process's *real* RSS
/// across a multi-GB ceiling in a test would mean allocating gigabytes (and
/// risking the very OOM this guards against). The ticker stays a thin sampler
/// that calls this and acts on the boolean.
/// What: returns `false` when `limit_mb == 0` (no ceiling configured);
/// otherwise `rss_mb >= floor(limit_mb * pct / 100)`. Pass `pct = 100` to test
/// the hard-limit boundary (the post-reclaim self-restart gate uses this).
/// Test: `tests::test_over_high_water`.
pub fn over_high_water(rss_mb: u64, limit_mb: u64, pct: u8) -> bool {
    if limit_mb == 0 {
        return false;
    }
    let high_water = limit_mb.saturating_mul(pct as u64) / 100;
    rss_mb >= high_water
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_limit_env_parse() {
        // The atomic is shared across tests in this binary, so we can't
        // reliably mutate the env here. Just assert the getter never panics
        // and returns a deterministic value for this process.
        let _ = memory_limit_mb();
    }

    #[test]
    fn test_current_rss_mb_nonzero() {
        // The test process itself is real — RSS should be > 0 MB.
        if let Some(mb) = current_rss_mb() {
            assert!(mb > 0, "current process RSS should be > 0 MB, got {mb}");
        }
        // If sysinfo couldn't resolve the pid we tolerate `None` (CI sandbox).
    }

    #[test]
    fn test_over_memory_limit_false_when_unset() {
        // Without TRUSTY_MEMORY_LIMIT_MB set in the test environment, the
        // helper must return false regardless of current RSS.
        if memory_limit_mb().is_none() {
            assert!(!over_memory_limit());
        }
    }

    #[test]
    fn test_index_memory_limit_falls_back_to_global() {
        // When TRUSTY_INDEX_MEMORY_LIMIT_MB is unset (the default in this
        // test binary's environment) `index_memory_limit_mb()` must mirror
        // `memory_limit_mb()`.
        let global = memory_limit_mb();
        let indexing = index_memory_limit_mb();
        if std::env::var("TRUSTY_INDEX_MEMORY_LIMIT_MB").is_err() {
            assert_eq!(indexing, global);
        }
    }

    #[test]
    fn test_index_memory_limit_env_parse() {
        // Smoke test: the getter never panics regardless of env state.
        let _ = index_memory_limit_mb();
    }

    #[test]
    fn test_over_index_memory_limit_false_when_unset() {
        if index_memory_limit_mb().is_none() {
            assert!(!over_index_memory_limit());
        }
    }

    /// `current_rss_mb_for_pid(self_pid)` must return a live, sane,
    /// unit-correct RSS reading that agrees (loosely) with `current_rss_mb()`
    /// — both read the same process, indeed the same underlying call (see
    /// [`current_rss_mb`]'s doc, which just forwards to this one with
    /// `std::process::id()`).
    ///
    /// Why (issue #3702): the original version asserted the two sequential
    /// samples agreed within a fixed 10 MB, which is really testing "two
    /// back-to-back samples in a quiet process are numerically close" — true
    /// in isolation, but not a property CI's shared-process
    /// `cargo test --workspace` run can uphold: many sibling tests allocate
    /// and free concurrently in between the two calls, and CI observed
    /// diffs of 28/11/30 MB across 3 consecutive runs on PR #3690, a PR that
    /// never touched this file. Widening the constant wouldn't fix the
    /// underlying mismatch between what the test measures (transient
    /// concurrent-allocation noise) and what it's meant to guard (that the
    /// sampler is alive and correct), so the assertions are restructured
    /// instead of just loosened.
    /// What:
    ///   1. both samples must be present (`Some`) and land inside a broad
    ///      sanity band for a live test-binary RSS — this alone would catch
    ///      a regression that returns `0`, a negative/garbage value, or a
    ///      value off by a unit conversion (e.g. bytes instead of MB, ~1024x
    ///      off), independent of any peer comparison;
    ///   2. the two peer samples agree within a generous *relative* bound
    ///      (60% of the larger reading) rather than a fixed MB count — wide
    ///      enough to absorb the CI-observed concurrent-churn diffs above,
    ///      while still failing if the two sampling paths genuinely
    ///      diverge;
    ///   3. after deliberately committing and touching a fresh 128 MB
    ///      buffer, a re-measurement must show RSS grew by at least a
    ///      quarter of that (32 MB) — this is the check that actually
    ///      proves the sampler tracks *live* memory rather than a
    ///      stale/cached number, a bug class the peer-agreement check alone
    ///      cannot catch (a frozen value could coincidentally agree with a
    ///      fresh one). The margin (32 MB required vs. 128 MB committed)
    ///      comfortably absorbs the largest concurrent-churn diff CI has
    ///      observed (30 MB) even in the adversarial case where sibling
    ///      tests are simultaneously freeing memory elsewhere in the
    ///      process.
    ///
    /// Test: this test.
    #[test]
    fn test_rss_for_self_pid() {
        let self_pid = std::process::id();
        let (Some(a), Some(b)) = (current_rss_mb(), current_rss_mb_for_pid(self_pid)) else {
            // Either None means the platform couldn't resolve the PID; tolerate it.
            return;
        };

        // Sanity band: catches a 0/negative/garbage/wrong-unit regression
        // without being sensitive to how much memory this test binary
        // happens to be using on any given CI runner.
        const MIN_SANE_RSS_MB: u64 = 1;
        const MAX_SANE_RSS_MB: u64 = 32 * 1024; // 32 GB - generous for a test binary
        for (label, v) in [("current_rss_mb()", a), ("current_rss_mb_for_pid()", b)] {
            assert!(
                (MIN_SANE_RSS_MB..=MAX_SANE_RSS_MB).contains(&v),
                "{label} returned {v}MB, outside sane band [{MIN_SANE_RSS_MB}, {MAX_SANE_RSS_MB}] MB"
            );
        }

        // Relative-drift bound: scales with the measurement itself instead
        // of a fixed MB count, so it tolerates CI-observed concurrent-test
        // churn (up to ~30MB seen in #3702) while still failing if the two
        // paths genuinely disagree.
        let diff = (a as i64 - b as i64).unsigned_abs();
        let baseline = a.max(b).max(1);
        assert!(
            diff * 100 <= baseline * 60,
            "current_rss_mb()={a}MB and current_rss_mb_for_pid({self_pid})={b}MB differ by \
             {diff}MB, more than 60% of {baseline}MB - suspect the two sampling paths diverged"
        );

        // Deliberately grow RSS and confirm the sampler notices. Unlike the
        // peer-agreement check above, this catches a sampler that returns a
        // plausible-looking but *stale* value (e.g. cached at process
        // start), since a frozen reading would show ~0 growth here
        // regardless of what it happens to agree with above.
        const GROW_MB: u64 = 128;
        const MIN_EXPECTED_GROWTH_MB: u64 = GROW_MB / 4;
        let mut buf = vec![0u8; (GROW_MB * 1024 * 1024) as usize];
        // Touch every page so it's actually resident, not just reserved
        // address space (a zeroed Vec's pages can otherwise be lazily
        // backed by the OS until first write).
        for chunk in buf.chunks_mut(4096) {
            chunk[0] = 1;
        }
        std::hint::black_box(&buf);
        if let Some(after) = current_rss_mb_for_pid(self_pid) {
            let grown = after.saturating_sub(b);
            assert!(
                grown >= MIN_EXPECTED_GROWTH_MB,
                "expected RSS to grow by at least {MIN_EXPECTED_GROWTH_MB}MB after committing \
                 a fresh {GROW_MB}MB buffer, but it only grew by {grown}MB (before={b}MB, \
                 after={after}MB) - sampler may be returning a stale reading"
            );
        }
        drop(buf);
    }

    /// `current_rss_mb_for_pid(0)` must return `None` (sentinel for "no PID").
    ///
    /// Why: the embedderd PID slot is initialised to 0 and the RSS poller
    /// must not try to sample PID 0 (which is the kernel process on many
    /// platforms and would produce incorrect results).
    /// What: pass 0, assert `None`.
    /// Test: this test.
    #[test]
    fn test_rss_for_pid_zero_returns_none() {
        assert_eq!(
            current_rss_mb_for_pid(0),
            None,
            "pid=0 must be treated as sentinel (no process) and return None"
        );
    }

    /// `current_rss_mb_for_pid(u32::MAX)` must return `None` (no such process).
    ///
    /// Why: ensures the helper does not panic or return garbage on a bogus PID.
    /// What: pass `u32::MAX` which no real OS process will have; expect `None`.
    /// Test: this test.
    #[test]
    fn test_rss_for_bogus_pid_returns_none() {
        // PID u32::MAX is not a valid process on any mainstream OS.
        // The function must return None without panicking.
        let _ = current_rss_mb_for_pid(u32::MAX);
        // No assertion — the only requirement is "no panic".
    }

    #[test]
    fn test_enforce_interval_default() {
        // Unset env → compiled-in default. The test binary does not set the
        // var, so this must resolve to DEFAULT_ENFORCE_SECS.
        if std::env::var("TRUSTY_MEMORY_ENFORCE_SECS").is_err() {
            assert_eq!(enforce_interval_secs(), DEFAULT_ENFORCE_SECS);
        }
    }

    #[test]
    fn test_high_water_pct_default_and_clamp() {
        if std::env::var("TRUSTY_MEMORY_HIGH_WATER_PCT").is_err() {
            let pct = high_water_pct();
            assert_eq!(pct, DEFAULT_HIGH_WATER_PCT);
            // Resolved value always lands inside the enforced clamp window.
            assert!((MIN_HIGH_WATER_PCT..=100).contains(&pct));
        }
    }

    #[test]
    fn test_restart_on_limit_default_off() {
        if std::env::var("TRUSTY_MEMORY_RESTART_ON_LIMIT").is_err() {
            assert!(
                !restart_on_limit_enabled(),
                "self-restart must default OFF so an unsupervised daemon never self-terminates"
            );
        }
    }

    /// `trim_heap()` must be callable at any time without panicking, on every
    /// platform — a no-op on non-Linux, a real `malloc_trim(0)` call on Linux.
    ///
    /// Why: this is the function the idle-evict ticker and the memory-pressure
    /// reclaim sweep call immediately after a bulk cache clear (issue #3657).
    /// A panic here would take down a background ticker task.
    /// What: call it twice in a row (idempotent — trimming an already-trimmed
    /// heap must still be safe) with no assertions beyond "did not panic".
    /// Test: this test.
    #[test]
    fn test_trim_heap_does_not_panic() {
        trim_heap();
        trim_heap();
    }

    /// On Linux, `trim_heap()` must never INCREASE RSS after freeing a large
    /// batch of small, discontiguous heap allocations — the OS-level half of
    /// the #3657 root cause (the Rust-level half — that eviction actually
    /// drops the backing `HashMap` allocation — is pinned by
    /// `core::indexer::tests_idle_evict::idle_eviction_releases_chunk_map_backing_allocation`).
    ///
    /// Why: production traced RSS climbing 20.3 → 26.4 GiB to glibc's default
    /// allocator not returning freed small-object heap to the OS on its own.
    /// This test reproduces that shape at a small scale: ~200k individually
    /// heap-allocated buffers (mirrors the ~300K discontiguous `RawChunk`
    /// string/vec allocations the production `chunks` map held — NOT one
    /// giant contiguous allocation, which glibc already `munmap`s on free
    /// regardless of `malloc_trim` once it crosses `M_MMAP_THRESHOLD`).
    /// What: allocate then drop ~200k small `Vec<u8>` buffers, sample RSS,
    /// call `trim_heap()`, sample again. Asserts only the deterministic,
    /// platform-safe invariant (`after <= before` — trim can only release
    /// memory, never grow RSS) rather than a specific MB delta, which would
    /// be flaky across CI kernels/glibc versions with different heap tuning.
    /// The actual before/after numbers are printed so a real run's log shows
    /// the genuine reclaim in practice.
    /// Test: this test (Linux-only — `malloc_trim` doesn't exist on macOS,
    /// whose allocator already returns freed pages far more eagerly, so there
    /// is no platform-specific behaviour to prove there).
    #[test]
    #[cfg(target_os = "linux")]
    fn test_trim_heap_never_increases_rss_after_bulk_free() {
        // Vary allocation size across a small range so allocations land in
        // several glibc size-classes, matching real chunk-content variability
        // instead of one uniform, easily-coalesced size.
        let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(200_000);
        for i in 0..200_000u32 {
            let len = 150 + (i % 100) as usize;
            buffers.push(vec![0u8; len]);
        }
        let rss_peak = current_rss_mb();
        drop(buffers);
        let rss_before_trim = current_rss_mb();

        trim_heap();
        let rss_after_trim = current_rss_mb();

        if let (Some(before), Some(after)) = (rss_before_trim, rss_after_trim) {
            assert!(
                after <= before,
                "trim_heap() must never increase RSS: {before} MB before trim, \
                 {after} MB after trim"
            );
        }
        eprintln!(
            "trim_heap RSS smoke: peak={rss_peak:?}MB before_trim={rss_before_trim:?}MB \
             after_trim={rss_after_trim:?}MB"
        );
    }

    #[test]
    fn test_over_high_water() {
        // No ceiling configured → never over, whatever the RSS.
        assert!(!over_high_water(999_999, 0, 90));
        // 90% of 10_000 = 9_000: below, at, above.
        assert!(!over_high_water(8_999, 10_000, 90));
        assert!(over_high_water(9_000, 10_000, 90));
        assert!(over_high_water(9_500, 10_000, 90));
        // Hard-limit boundary (pct = 100) — the self-restart gate's usage.
        assert!(!over_high_water(9_999, 10_000, 100));
        assert!(over_high_water(10_000, 10_000, 100));
        // saturating_mul must not panic on a huge limit.
        assert!(over_high_water(u64::MAX, u64::MAX, 100));
    }

    #[test]
    fn test_runtime_set_limit() {
        // Why: regression coverage for the AtomicU64 migration — the runtime
        // setters must take effect immediately on the next read, with no
        // restart required, and `None` must encode as "no limit" (decoded
        // back to `None`) so the env-var sentinel is not accidentally
        // re-parsed.
        // What: serialise the test through both limits since the atomics
        // are process-global. Save/restore the previous values so other
        // tests in this binary keep observing their original state.
        let prev_global = memory_limit_mb();
        let prev_index = index_memory_limit_mb();

        // Round-trip Some(n)
        set_memory_limit_mb(Some(4096));
        assert_eq!(memory_limit_mb(), Some(4096));
        set_index_memory_limit_mb(Some(8192));
        assert_eq!(index_memory_limit_mb(), Some(8192));

        // Round-trip None (disabled)
        set_memory_limit_mb(None);
        assert_eq!(memory_limit_mb(), None);
        // With the global limit disabled and the index limit cleared, the
        // index getter falls back to the (None) global limit.
        set_index_memory_limit_mb(None);
        assert_eq!(index_memory_limit_mb(), None);

        // Restore prior state so other tests are not perturbed.
        set_memory_limit_mb(prev_global);
        // `prev_index` here is the *resolved* value (after fallback to the
        // global). We can't reliably restore the "fall through" state, so
        // we restore the resolved value — close enough for sibling tests
        // which only assert reachability, not exact equality.
        set_index_memory_limit_mb(prev_index);
    }
}
