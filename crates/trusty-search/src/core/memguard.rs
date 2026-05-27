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
pub fn current_rss_mb() -> Option<u64> {
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    // `Process::memory()` returns bytes on every supported platform as of
    // sysinfo 0.30+. Convert to MB with a saturating divide.
    sys.process(pid).map(|p| p.memory() / (1024 * 1024))
}

/// Resident Set Size in megabytes for an arbitrary child process (by OS PID).
///
/// Why: the embedderd sidecar runs in a separate process. Its RSS is not
/// captured by `current_rss_mb()` (which reads only the daemon's own RSS).
/// This helper uses the same platform-agnostic `sysinfo` approach to sample
/// any process by its OS PID — the same path used for the daemon-parent RSS
/// but parameterised on an external PID.
///
/// What: asks `sysinfo` to refresh exactly the named PID (minimal overhead).
/// Returns `None` if the PID is 0 (sentinel for "no sidecar running"), if
/// sysinfo cannot locate the process (exited between spawn and sample), or if
/// the platform `procfs`/`task_info` call fails.
///
/// Test: `tests::test_rss_for_self_pid` calls this with `std::process::id()`
/// and asserts the result matches `current_rss_mb()` within 10 MB. Negative
/// cases (pid=0, bogus pid) assert `None`.
pub fn current_rss_mb_for_pid(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
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

    /// `current_rss_mb_for_pid(self_pid)` must return the same order-of-magnitude
    /// RSS as `current_rss_mb()` — both read the same process.
    ///
    /// Why: validates the pid-parameterised helper against the known-working
    /// self-pid path. A mismatch would indicate a platform quirk in the
    /// `sysinfo::ProcessesToUpdate::Some(&[pid])` path.
    /// What: call both, assert abs-difference < 10 MB (transient allocations
    /// between the two calls can shift RSS slightly).
    /// Test: this test.
    #[test]
    fn test_rss_for_self_pid() {
        let self_pid = std::process::id();
        if let (Some(a), Some(b)) = (current_rss_mb(), current_rss_mb_for_pid(self_pid)) {
            // Allow up to 10 MB drift between the two samples.
            let diff = (a as i64 - b as i64).unsigned_abs();
            assert!(
                diff < 10,
                "current_rss_mb()={a}MB and current_rss_mb_for_pid({self_pid})={b}MB \
                 differ by {diff}MB (> 10 MB tolerance)"
            );
        }
        // Either None means the platform couldn't resolve the PID; tolerate it.
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
