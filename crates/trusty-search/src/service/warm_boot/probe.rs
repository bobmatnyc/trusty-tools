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
//!       `probe_timeout_increments_leaked_thread_count`.

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
/// What: a process-global `AtomicUsize`, incremented by `probe_volume`
/// whenever it hits the deadline. Exposed via `leaked_probe_thread_count()`.
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

// ── Public types ──────────────────────────────────────────────────────────────

/// Whether a volume root is known-accessible or presumed inaccessible.
///
/// Why: a simple bool would work, but a named enum makes match arms
/// self-documenting at call sites in `mod.rs`.
/// What: two variants; constructed by `probe_volume`.
/// Test: constructed in `probe_volume_accessible_tempdir` and
///       `probe_volume_inaccessible_fast_timeout`.
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
/// What: on macOS external volumes are conventionally mounted under
/// `/Volumes/<label>/`. For paths starting with `/Volumes/` we return the
/// first two components (`/Volumes/<label>`). All other paths (boot volume,
/// Linux, or paths that do not follow the convention) return `/` — this is
/// safe to probe and always accessible in the login-shell / FDA-granted path.
///
/// Falls back gracefully to `/` for very short paths or canonicalization
/// failures rather than panicking.
///
/// Test: `volume_key_boot_volume`, `volume_key_external_volume`.
pub(super) fn volume_key(path: &Path) -> PathBuf {
    let mut components = path.components();
    // Skip root "/"
    let first = components.next(); // RootDir
    let second = components.next(); // "Volumes"
    let third = components.next(); // label, e.g. "SSD1"

    // Check if this is a /Volumes/<label> path.
    use std::path::Component;
    if let (
        Some(Component::RootDir),
        Some(Component::Normal(volumes)),
        Some(Component::Normal(label)),
    ) = (first, second, third)
    {
        if volumes.eq_ignore_ascii_case("Volumes") {
            let mut key = PathBuf::from("/");
            key.push("Volumes");
            key.push(label);
            return key;
        }
    }
    // Everything else: boot volume or non-macOS — probe the root.
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

/// Probe whether a volume is accessible within a wall-clock deadline.
///
/// Why (issue #723, review #727 finding 2): we must NOT use `tokio::task::
/// spawn_blocking` here — we deliberately use a bare OS thread (`std::thread::
/// spawn`) so that a frozen syscall never consumes a slot from tokio's blocking
/// pool. The probe thread is intentionally detached (its handle is dropped); if
/// it hangs forever it costs exactly one OS thread, not a pool slot, and does
/// not affect async responsiveness.
///
/// Probing the SAMPLE PATH (review #727 finding 2): on macOS, `stat` on the
/// volume mount-point root (e.g. `/Volumes/SSD1`) can succeed even when TCC
/// denies access to files inside the volume. To catch the
/// TCC-blocked-inside-volume scenario that issue #723 targets, we probe the
/// representative deeper sample path that actually contains index data
/// (e.g. `/Volumes/SSD1/Projects/myrepo`) instead of the bare volume root.
/// `volume_root` is retained for logging only; `probe_path` is what the OS
/// thread actually calls `metadata` on.
///
/// What: spawns a bare OS thread that calls `std::fs::metadata(probe_path)`.
/// Sends the result (success or error) over an `mpsc::channel`. The caller
/// waits with `recv_timeout(deadline)`. On timeout: increments
/// `LEAKED_PROBE_THREAD_COUNT`, emits a loud `tracing::warn!`, and returns
/// `Inaccessible`. On receive: returns `Accessible` regardless of whether
/// the metadata call succeeded (an ENOENT or EACCES means the kernel DID
/// return — no hang — so subsequent calls on that volume will also return
/// promptly). The one leaked OS thread is counted so `/health` can surface
/// the accumulation (review #727 finding 3).
///
/// Note on "accessible" semantics: `Accessible` means "the volume answers
/// quickly" — not "you have read permission". An EACCES / EPERM is still
/// `Accessible` because the kernel returned rather than freezing. The
/// per-index restore timeout handles any subsequent permission errors.
///
/// Test: `probe_volume_accessible_tempdir` (real tmpdir, must return
///       `Accessible`), `probe_uses_sample_path_not_volume_root` (mount root
///       accessible but inner path inaccessible → `Inaccessible`),
///       `probe_timeout_increments_leaked_thread_count` (counter incremented).
pub(super) fn probe_volume(
    volume_root: &Path,
    probe_path: &Path,
    deadline: Duration,
) -> VolumeAccessibility {
    use std::sync::mpsc;

    let probe_owned = probe_path.to_path_buf();
    let (tx, rx) = mpsc::channel::<()>();

    // Spawn a bare OS thread. This thread may freeze permanently on a TCC-denied
    // volume. We drop its `JoinHandle` immediately — the thread becomes detached.
    // Cost: one frozen OS thread per blocked volume (not per index). tokio's
    // blocking-pool slots are unaffected.
    //
    // We probe `probe_path` (the actual sample index directory inside the volume)
    // rather than `volume_root` (the mount-point root). On macOS, stat on the
    // mount root can succeed even when TCC denies inner-file access — probing
    // the deeper path is what catches the #723 scenario (review #727 finding 2).
    let _ = std::thread::spawn(move || {
        // We only care whether the metadata call returned at all, not the value.
        // An error (NotFound, PermissionDenied) still means the kernel answered.
        let _ = std::fs::metadata(&probe_owned);
        // Send a "done" signal. Ignore send errors: the receiver may have
        // already timed out and been dropped.
        let _ = tx.send(());
    });

    match rx.recv_timeout(deadline) {
        Ok(()) => VolumeAccessibility::Accessible,
        Err(_timeout_or_disconnect) => {
            // The probe thread did not return within the deadline — it is
            // abandoned (detached). Increment the process-global counter so
            // `/health` can surface the accumulation (review #727 finding 3).
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
/// Why (issue #723, review #727 finding 2): a single call site in
/// `mod.rs::collect_colocated_entries` and `start.rs::restore_indexes` can
/// obtain the full inaccessible set before any restore work begins, then skip
/// index entries that live on blocked volumes without issuing further `open()`
/// calls.
///
/// Probe target (review #727 finding 2): each volume is probed via its
/// representative SAMPLE INDEX PATH (the actual deeper path that contains index
/// data), not the bare volume mount-point root. On macOS, `stat` on
/// `/Volumes/SSD1` can succeed even when TCC denies access to files inside the
/// volume — probing the deeper path (e.g. `/Volumes/SSD1/Projects/myrepo`) is
/// what actually exercises the access that will be needed for index restoration.
///
/// What: extracts distinct volume keys (via `volume_key`), keeping one sample
/// path per key as the probe target. Probes each volume once (via
/// `probe_volume`), logs the outcome, and returns a `HashSet<PathBuf>` of
/// inaccessible volume keys. An empty set means all probed volumes answered
/// within the deadline.
///
/// Probing is sequential because each probe may block for up to `deadline`
/// seconds and firing all probes in parallel would spawn N OS threads at once
/// (one per volume). For the typical case (1–3 external volumes) sequential is
/// fine; for many volumes the total wait is at most `N × deadline`.
///
/// Test: `probe_all_volumes_accessible_returns_empty`,
///       `probe_all_volumes_distinct_keys`,
///       `probe_uses_sample_path_not_volume_root`.
pub(super) fn probe_all_volumes(
    paths: &[PathBuf],
    deadline: Duration,
) -> std::collections::HashSet<PathBuf> {
    use std::collections::{HashMap, HashSet};

    // Group paths by volume key — we probe each volume key at most once.
    // The sample_path is the representative deeper path used as the actual
    // probe target (review #727 finding 2): the first index path seen for
    // this volume key.
    let mut volume_to_sample: HashMap<PathBuf, &PathBuf> = HashMap::new();
    for path in paths {
        let key = volume_key(path);
        volume_to_sample.entry(key).or_insert(path);
    }

    let mut inaccessible: HashSet<PathBuf> = HashSet::new();

    for (vol_key, sample_path) in &volume_to_sample {
        // Probe the deeper sample path (not the bare volume root) so that
        // TCC denials on inner files are detected (review #727 finding 2).
        let accessibility = probe_volume(vol_key, sample_path, deadline);
        match accessibility {
            VolumeAccessibility::Accessible => {
                tracing::debug!(
                    "warm-boot: volume probe OK for {} (probed sample path: {})",
                    vol_key.display(),
                    sample_path.display(),
                );
            }
            VolumeAccessibility::Inaccessible => {
                // probe_volume already emitted a warn with the leaked-thread
                // count. Emit the actionable operator hint here.
                let is_ext = super::scan::is_likely_external_volume(vol_key);
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
                inaccessible.insert(vol_key.clone());
            }
        }
    }

    inaccessible
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
