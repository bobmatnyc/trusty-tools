//! Per-volume accessibility probe for warm-boot (issue #723).
//!
//! Why (issue #723): when index data lives on a TCC-restricted external/removable
//! volume (e.g. `/Volumes/SSD1`) under macOS launchd, `open()` hangs
//! uninterruptibly in kernel space. With the #718 fix each blocked `open()` leaks
//! one blocking-pool thread; with 57 indexes on one volume that is 57 leaked
//! threads even though the root cause is a single volume denial.
//!
//! This module probes each DISTINCT volume root ONCE on a throwaway detached OS
//! thread with a wall-clock deadline. If the probe does not return in time the
//! whole volume is marked inaccessible — no further `open()` calls are issued for
//! indexes on that volume, so total leaked threads are bounded at ONE per blocked
//! volume instead of one per index.
//!
//! Probe strategy (review #727 finding 2): probe the SAMPLE INDEX PATH inside the
//! volume (e.g. `/Volumes/SSD1/Projects/myrepo`) rather than the bare volume
//! mount root (e.g. `/Volumes/SSD1`). On macOS, `stat("/Volumes/SSD1")` can
//! succeed even when TCC denies access to files inside the volume, because the
//! volume mount-point itself is accessible while its contents are not. Probing
//! the representative deeper path that actually contains index data is what
//! detects the TCC-blocked-inside-volume scenario that issue #723 targets.
//!
//! Issue a `std::fs::metadata` on a bare OS thread (NOT a tokio blocking-pool
//! thread — we never want to consume a pool slot for a syscall that may block
//! forever). Use `std::thread::spawn` + a `std::sync::mpsc::channel` with a
//! receive timeout to impose the wall-clock deadline. When the deadline fires the
//! channel-receive returns `Err(Timeout)`; we log a loud warning and return
//! `VolumeAccessibility::Inaccessible`. The probe thread is detached (its handle
//! is dropped) — it may remain frozen in the kernel indefinitely, but it costs
//! exactly one OS thread (not a tokio pool thread) and does not affect daemon
//! responsiveness.
//!
//! Parallel probing (review #727 finding 1): `probe_all_volumes` spawns ALL
//! per-volume probe threads simultaneously, then collects results under a single
//! shared wall-clock deadline. Total probe time is bounded at ≈ONE deadline
//! regardless of how many distinct volumes are being probed. Each blocked volume
//! still leaks exactly one OS thread (invariant unchanged).
//!
//! Leaked-thread visibility (review #727 finding 3): every timed-out probe
//! increments `LEAKED_PROBE_THREAD_COUNT`, a process-global `AtomicUsize`.
//! The daemon's `/health` endpoint exposes this count as
//! `warmboot_leaked_probe_threads` so operators monitoring a launchd-managed
//! daemon that restarts repeatedly can detect accumulation before it matters.
//!
//! Test: `volume_key_boot_volume`, `volume_key_external_volume`,
//!       `probe_volume_accessible_tempdir`,
//!       `probe_volume_inaccessible_fast_timeout`,
//!       `probe_uses_sample_path_not_volume_root`,
//!       `probe_timeout_increments_leaked_thread_count`,
//!       `probe_all_volumes_parallel_bounded_time`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ── Process-global leaked-probe-thread counter (review #727 finding 3) ───────

/// Running count of OS probe threads that were abandoned due to a deadline
/// timeout (review #727 finding 3).
///
/// Why: each timed-out probe leaks exactly one OS thread (the bare-OS thread
/// we spawn so a frozen `stat()` cannot consume a tokio pool slot). On a
/// launchd-managed daemon that restarts repeatedly these can accumulate.
/// Making the count visible in `/health` lets operators detect accumulation
/// before it becomes a problem.
///
/// What: a process-global `AtomicUsize`, incremented by `probe_all_volumes`
/// (and by `probe_volume` when called directly from tests) whenever a probe
/// hits the deadline. Exposed via `leaked_probe_thread_count()`.
///
/// Test: `probe_timeout_increments_leaked_thread_count` below.
static LEAKED_PROBE_THREAD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Read the current count of abandoned (timed-out) probe threads.
///
/// Why: `GET /health` surfaces this as `warmboot_leaked_probe_threads` so
/// operators can detect leaked thread accumulation across daemon restarts.
/// What: loads `LEAKED_PROBE_THREAD_COUNT` with `Relaxed` ordering; a
/// slightly stale value is acceptable for an observability field.
/// Test: `probe_timeout_increments_leaked_thread_count` verifies the counter
/// is incremented; the health endpoint test verifies it appears in responses.
pub fn leaked_probe_thread_count() -> usize {
    LEAKED_PROBE_THREAD_COUNT.load(Ordering::Relaxed)
}

// ── Types used only in tests (probe_volume is a test helper) ─────────────────

/// Whether a volume root is known-accessible or presumed inaccessible.
///
/// Why: used by `probe_volume` (a test-level helper that exercises the
/// single-probe path directly). `probe_all_volumes` inlines the same logic
/// for parallel collection and does not use this enum outside of tests.
/// What: two variants.
/// Test: constructed in `probe_volume_accessible_tempdir` and
///       `probe_timeout_increments_leaked_thread_count`.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VolumeAccessibility {
    /// The probe returned (successfully or with a non-hang error) within the
    /// deadline. The volume can be opened.
    Accessible,
    /// The probe timed out or the probe thread panicked. The volume must be
    /// skipped; no further `open()` calls should be issued for it.
    Inaccessible,
}

// ── Volume key extraction ─────────────────────────────────────────────────────

/// Extract a stable "volume key" from an index path for grouping purposes.
///
/// Why (issue #723): before probing, we must identify which distinct volume
/// each index lives on so we can probe each volume exactly once. Two paths
/// that share the same volume root (e.g. `/Volumes/SSD1/proj-a` and
/// `/Volumes/SSD1/proj-b`) produce the same key and share a single probe.
///
/// What: on macOS, external volumes are conventionally mounted under
/// `/Volumes/<label>/`. For paths starting with `/Volumes/` (exact, case-
/// sensitive — review #727 finding 3: the previous `eq_ignore_ascii_case`
/// mis-classified Linux paths like `/volumes/...` as external macOS volumes)
/// we return the first two components (`/Volumes/<label>`). This special-
/// casing is gated behind `#[cfg(target_os = "macos")]`; on all other
/// platforms every path returns `/`. Boot-volume paths and all non-macOS
/// paths return `/` — this is always safe to probe.
///
/// Falls back gracefully to `/` for very short paths rather than panicking.
///
/// Test: `volume_key_boot_volume`, `volume_key_external_volume`,
///       `volume_key_linux_lowercase_volumes_is_root`.
pub(super) fn volume_key(path: &Path) -> PathBuf {
    // The `/Volumes/<label>` convention is macOS-specific.
    #[cfg(target_os = "macos")]
    {
        let mut components = path.components();
        // Skip root "/"
        let first = components.next(); // RootDir
        let second = components.next(); // "Volumes"
        let third = components.next(); // label, e.g. "SSD1"

        use std::path::Component;
        if let (
            Some(Component::RootDir),
            Some(Component::Normal(volumes)),
            Some(Component::Normal(label)),
        ) = (first, second, third)
        {
            // Exact match: on macOS the canonical mount directory is "Volumes"
            // (capital V). Using eq_ignore_ascii_case would incorrectly treat
            // `/volumes/...` (lowercase) as an external-volume key on platforms
            // where that prefix has different semantics (review #727 finding 3).
            if volumes == "Volumes" {
                let mut key = PathBuf::from("/");
                key.push("Volumes");
                key.push(label);
                return key;
            }
        }
    }
    // Everything else: boot volume, Linux, Windows, or non-standard macOS
    // path — probe the root.
    PathBuf::from("/")
}

// ── Probe implementation ──────────────────────────────────────────────────────

/// Read the per-volume probe deadline from `TRUSTY_WARMBOOT_VOLUME_PROBE_SECS`.
///
/// Why (issue #723): provides a single configurable knob for the per-volume
/// accessibility probe deadline. Operators on machines with very fast or very
/// slow storage can tune this value to balance safety vs. prompt feedback.
///
/// What: parses `TRUSTY_WARMBOOT_VOLUME_PROBE_SECS` as a `u64` of seconds.
/// Falls back to `DEFAULT_PROBE_TIMEOUT` (5 s) on parse failure or if the
/// variable is unset. A value of `0` is treated as the default.
///
/// Test: `volume_probe_timeout_parses_env_var` in this module.
pub(super) fn volume_probe_timeout() -> Duration {
    const DEFAULT_PROBE_SECS: u64 = 5;
    let secs = std::env::var("TRUSTY_WARMBOOT_VOLUME_PROBE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_PROBE_SECS);
    Duration::from_secs(secs)
}

/// Probe whether a single volume is accessible within a wall-clock deadline.
///
/// Why: this is the unit-testable single-probe building block. Production code
/// uses `probe_all_volumes` (which inlines the same pattern in parallel for
/// all volumes at once). `probe_volume` is retained as a `#[cfg(test)]`
/// helper so tests can exercise the probe/counter/timeout path in isolation,
/// without needing multiple volumes.
///
/// What: spawns a bare OS thread that calls `std::fs::metadata(probe_path)`.
/// The JoinHandle is dropped immediately (thread is detached). The caller
/// waits with `recv_timeout(deadline)`. On timeout: increments
/// `LEAKED_PROBE_THREAD_COUNT`, emits a `tracing::warn!`, and returns
/// `Inaccessible`. On receive: returns `Accessible` regardless of the
/// `metadata` result (ENOENT / EACCES means the kernel answered — no hang).
///
/// `volume_root` is used for logging only; `probe_path` is the actual target
/// (review #727 finding 2: probing the deeper sample path, not the mount root).
///
/// Test: `probe_volume_accessible_tempdir`,
///       `probe_timeout_increments_leaked_thread_count`.
#[cfg(test)]
pub(super) fn probe_volume(
    volume_root: &Path,
    probe_path: &Path,
    deadline: Duration,
) -> VolumeAccessibility {
    use std::sync::mpsc;

    let probe_owned = probe_path.to_path_buf();
    let (tx, rx) = mpsc::channel::<()>();

    let _ = std::thread::spawn(move || {
        let _ = std::fs::metadata(&probe_owned);
        let _ = tx.send(());
    });

    match rx.recv_timeout(deadline) {
        Ok(()) => VolumeAccessibility::Accessible,
        Err(_timeout_or_disconnect) => {
            let prev = LEAKED_PROBE_THREAD_COUNT.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                "warm-boot: probe thread for volume {} (probing {}) timed out and was abandoned \
                 (leaked_probe_threads total: {}). (issue #723, review #727)",
                volume_root.display(),
                probe_path.display(),
                prev + 1,
            );
            VolumeAccessibility::Inaccessible
        }
    }
}

// ── Batch probe ───────────────────────────────────────────────────────────────

/// Probe every distinct volume in `paths` and return the set of inaccessible
/// volume keys.
///
/// Why (issue #723, review #727 findings 1 and 2): a single call site in
/// `mod.rs::collect_colocated_entries` and `start.rs::restore_indexes` can
/// obtain the full inaccessible set before any restore work begins, then skip
/// index entries that live on blocked volumes without issuing further `open()`
/// calls.
///
/// Parallel probing (review #727 finding 1): all per-volume probe threads are
/// spawned simultaneously, then collected under a SINGLE shared wall-clock
/// deadline. Total probe time is bounded at ≈ONE `deadline` regardless of how
/// many distinct volumes are being probed — the previous sequential design
/// caused a worst-case stall of N×deadline (one full deadline per blocked
/// volume before the next was even started). Each blocked volume still leaks
/// exactly one OS thread; the `LEAKED_PROBE_THREAD_COUNT` counter is
/// incremented once per timed-out volume as before.
///
/// Probe target (review #727 finding 2): each volume is probed via its
/// representative SAMPLE INDEX PATH (the actual deeper path that contains index
/// data), not the bare volume mount-point root. On macOS, `stat` on
/// `/Volumes/SSD1` can succeed even when TCC denies access to files inside the
/// volume — probing the deeper path (e.g. `/Volumes/SSD1/Projects/myrepo`) is
/// what actually exercises the access that will be needed for index restoration.
///
/// What: extracts distinct volume keys (via `volume_key`), keeping one sample
/// path per key as the probe target. Spawns all probe threads (one per distinct
/// volume key), then collects their results by waiting on each channel receiver
/// for the remaining time until the shared deadline. Returns a
/// `HashSet<PathBuf>` of inaccessible volume keys. An empty set means all
/// probed volumes answered within the deadline.
///
/// Test: `probe_all_volumes_accessible_returns_empty`,
///       `probe_all_volumes_distinct_keys`,
///       `probe_uses_sample_path_not_volume_root`,
///       `probe_all_volumes_parallel_bounded_time`.
pub(super) fn probe_all_volumes(
    paths: &[PathBuf],
    deadline: Duration,
) -> std::collections::HashSet<PathBuf> {
    use std::collections::{HashMap, HashSet};
    use std::sync::mpsc;
    use std::time::Instant;

    // Group paths by volume key — we probe each volume key at most once.
    // The sample_path is the representative deeper path used as the actual
    // probe target (review #727 finding 2): the first index path seen for
    // this volume key.
    let mut volume_to_sample: HashMap<PathBuf, PathBuf> = HashMap::new();
    for path in paths {
        let key = volume_key(path);
        volume_to_sample.entry(key).or_insert_with(|| path.clone());
    }

    if volume_to_sample.is_empty() {
        return HashSet::new();
    }

    // Shared wall-clock deadline: record the end instant once, then all
    // per-volume recv_timeout calls count down against the same clock.
    // This means total probe time ≈ one deadline regardless of N volumes.
    let end = Instant::now() + deadline;

    // Phase 1: spawn ALL probe threads simultaneously. Each thread is a bare
    // OS thread (not a tokio pool slot) that calls std::fs::metadata on the
    // sample path and sends () when done. The JoinHandle is dropped immediately
    // (thread is detached). We keep the Receiver for the collection phase.
    let pending: Vec<(PathBuf, PathBuf, mpsc::Receiver<()>)> = volume_to_sample
        .into_iter()
        .map(|(vol_key, sample_path)| {
            let probe_owned = sample_path.clone();
            let (tx, rx) = mpsc::channel::<()>();
            let _ = std::thread::spawn(move || {
                // Only care whether the call returned at all, not the result.
                let _ = std::fs::metadata(&probe_owned);
                // Ignore send errors: receiver may have already timed out.
                let _ = tx.send(());
            });
            (vol_key, sample_path, rx)
        })
        .collect();

    // Phase 2: collect results under the shared deadline. Each recv_timeout
    // call gets the remaining time until `end`; if the budget is already
    // exhausted the timeout fires immediately (Duration::ZERO).
    let mut inaccessible: HashSet<PathBuf> = HashSet::new();

    for (vol_key, sample_path, rx) in pending {
        let remaining = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(()) => {
                tracing::debug!(
                    "warm-boot: volume probe OK for {} (probed sample path: {})",
                    vol_key.display(),
                    sample_path.display(),
                );
            }
            Err(_timeout_or_disconnect) => {
                // The probe thread did not return within the remaining budget.
                // Increment the process-global counter so `/health` surfaces it.
                let prev = LEAKED_PROBE_THREAD_COUNT.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "warm-boot: probe thread for volume {} (probing {}) timed out and was \
                     abandoned (leaked_probe_threads total: {}). (issue #723, review #727)",
                    vol_key.display(),
                    sample_path.display(),
                    prev + 1,
                );
                // Emit the actionable operator hint.
                let is_ext = super::scan::is_likely_external_volume(&vol_key);
                if is_ext {
                    tracing::warn!(
                        "warm-boot: volume probe TIMED OUT for {} (>{:.0}s, probed: {}) — \
                         this is likely a TCC denial on an external volume under launchd. \
                         ALL indexes on this volume will be SKIPPED this boot. \
                         HINT: grant Full Disk Access to the launchd agent in \
                         System Settings → Privacy & Security → Full Disk Access, \
                         or move indexes off the external volume. (issue #723)",
                        vol_key.display(),
                        deadline.as_secs_f32(),
                        sample_path.display(),
                    );
                } else {
                    tracing::warn!(
                        "warm-boot: volume probe TIMED OUT for {} (>{:.0}s, probed: {}) — \
                         the volume may be on a network, slow, or permission-restricted \
                         filesystem. ALL indexes on this volume will be SKIPPED this boot. \
                         (issue #723)",
                        vol_key.display(),
                        deadline.as_secs_f32(),
                        sample_path.display(),
                    );
                }
                inaccessible.insert(vol_key);
            }
        }
    }

    inaccessible
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
