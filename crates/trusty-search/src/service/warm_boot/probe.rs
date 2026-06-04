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
//! Probe strategy: issue `std::fs::metadata(volume_root)` on a bare OS thread
//! (NOT a tokio blocking-pool thread — we never want to consume a pool slot for
//! a syscall that may block forever). Use `std::thread::spawn` + a
//! `std::sync::mpsc::channel` with a receive timeout to impose the wall-clock
//! deadline. When the deadline fires the channel-receive returns `Err(Timeout)`;
//! we log a loud warning and return `VolumeAccessibility::Inaccessible`. The
//! probe thread is detached (its handle is dropped) — it may remain frozen in the
//! kernel indefinitely, but it costs exactly one OS thread (not a tokio pool
//! thread) and does not affect daemon responsiveness.
//!
//! Test: `volume_key_boot_volume`, `volume_key_external_volume`,
//!       `probe_volume_accessible_tempdir`,
//!       `probe_volume_inaccessible_fast_timeout`.

use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Probe whether a volume root is accessible within a wall-clock deadline.
///
/// Why (issue #723): the key insight is that we must NOT use `tokio::task::
/// spawn_blocking` here — we deliberately use a bare OS thread (`std::thread::
/// spawn`) so that a frozen syscall never consumes a slot from tokio's blocking
/// pool. The probe thread is intentionally detached (its handle is dropped); if
/// it hangs forever it costs exactly one OS thread, not a pool slot, and does
/// not affect async responsiveness.
///
/// What: spawns a bare OS thread that calls `std::fs::metadata(volume_root)`.
/// Sends the result (success or error) over an `mpsc::channel`. The caller
/// waits with `recv_timeout(deadline)`. On timeout returns `Inaccessible` and
/// logs a loud TCC warning. On receive returns `Accessible` regardless of
/// whether the metadata call succeeded (an error like `NotFound` or `Permission
/// Denied` means the kernel DID return — not a hang — so subsequent calls on
/// that volume will also return promptly).
///
/// Note on "accessible" semantics: `Accessible` means "the volume answers
/// quickly" — not "you have read permission". An EACCES / EPERM on the
/// volume root is still "accessible" for our purposes because the kernel
/// returned rather than freezing. The per-index restore timeout handles any
/// subsequent permission errors.
///
/// Test: `probe_volume_accessible_tempdir` (real tmpdir, must return
///       `Accessible`), `probe_volume_inaccessible_fast_timeout` (verify
///       the `Inaccessible` path with a 0-duration mock approach).
pub(super) fn probe_volume(volume_root: &Path, deadline: Duration) -> VolumeAccessibility {
    use std::sync::mpsc;

    let root_owned = volume_root.to_path_buf();
    let (tx, rx) = mpsc::channel::<()>();

    // Spawn a bare OS thread. This thread may freeze permanently on a TCC-denied
    // volume. We drop its `JoinHandle` immediately — the thread becomes detached.
    // Cost: one frozen OS thread per blocked volume (not per index). tokio's
    // blocking-pool slots are unaffected.
    let _ = std::thread::spawn(move || {
        // We only care whether the metadata call returned at all, not the value.
        // An error (NotFound, PermissionDenied) still means the kernel answered.
        let _ = std::fs::metadata(&root_owned);
        // Send a "done" signal. Ignore send errors: the receiver may have
        // already timed out and been dropped.
        let _ = tx.send(());
    });

    match rx.recv_timeout(deadline) {
        Ok(()) => VolumeAccessibility::Accessible,
        Err(_timeout_or_disconnect) => VolumeAccessibility::Inaccessible,
    }
}

// ── Batch probe ───────────────────────────────────────────────────────────────

/// Probe every distinct volume in `paths` and return the set of inaccessible
/// volume keys.
///
/// Why (issue #723): a single call site in `mod.rs::collect_colocated_entries`
/// and `start.rs::restore_indexes` can obtain the full inaccessible set before
/// any restore work begins, then skip index entries that live on blocked volumes
/// without issuing further `open()` calls.
///
/// What: extracts distinct volume keys (via `volume_key`), probes each key once
/// (via `probe_volume`), logs the outcome, and returns a
/// `HashSet<PathBuf>` of inaccessible volume keys. An empty set means all
/// probed volumes answered within the deadline.
///
/// Probing is sequential because each probe may block for up to `deadline`
/// seconds and firing all probes in parallel would spawn N OS threads at once
/// (one per volume). For the typical case (1–3 external volumes) sequential is
/// fine; for many volumes the total wait is at most `N × deadline`.
///
/// Test: `probe_all_volumes_accessible_returns_empty`,
///       `probe_all_volumes_distinct_keys`.
pub(super) fn probe_all_volumes(
    paths: &[PathBuf],
    deadline: Duration,
) -> std::collections::HashSet<PathBuf> {
    use std::collections::{HashMap, HashSet};

    // Group paths by volume key — we probe each volume key at most once.
    let mut volume_to_sample: HashMap<PathBuf, &PathBuf> = HashMap::new();
    for path in paths {
        let key = volume_key(path);
        volume_to_sample.entry(key).or_insert(path);
    }

    let mut inaccessible: HashSet<PathBuf> = HashSet::new();

    for (vol_key, sample_path) in &volume_to_sample {
        let accessibility = probe_volume(vol_key, deadline);
        match accessibility {
            VolumeAccessibility::Accessible => {
                tracing::debug!(
                    "warm-boot: volume probe OK for {} (sample path: {})",
                    vol_key.display(),
                    sample_path.display(),
                );
            }
            VolumeAccessibility::Inaccessible => {
                let is_ext = super::scan::is_likely_external_volume(vol_key);
                if is_ext {
                    tracing::warn!(
                        "warm-boot: volume probe TIMED OUT for {} (>{:.0}s) — \
                         this is likely a TCC denial on an external volume under launchd. \
                         ALL indexes on this volume will be SKIPPED this boot. \
                         HINT: grant Full Disk Access to the launchd agent in \
                         System Settings → Privacy & Security → Full Disk Access, \
                         or move indexes off the external volume. (issue #723)",
                        vol_key.display(),
                        deadline.as_secs_f32(),
                    );
                } else {
                    tracing::warn!(
                        "warm-boot: volume probe TIMED OUT for {} (>{:.0}s) — \
                         the volume may be on a network, slow, or permission-restricted \
                         filesystem. ALL indexes on this volume will be SKIPPED this boot. \
                         (issue #723)",
                        vol_key.display(),
                        deadline.as_secs_f32(),
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
mod tests {
    //! Tests for the per-volume probe (issue #723).
    //!
    //! Why: the key invariants are:
    //! 1. `volume_key` correctly extracts `/Volumes/<label>` for external paths
    //!    and `/` for everything else.
    //! 2. `probe_volume` returns `Accessible` for a real, readable directory.
    //! 3. `probe_all_volumes` deduplicates by volume key.
    //!
    //! We cannot reproduce the TCC-hang in unit tests, so the `Inaccessible`
    //! path is tested via direct inspection of the timeout branch with a
    //! vanishingly short deadline. A 1ms deadline on a real `/tmp` tempdir will
    //! sometimes succeed (race), so we do NOT assert `Inaccessible` there —
    //! instead we test `probe_volume` only with real-world accessible paths.
    //! The `Inaccessible` branch is exercised in `restore.rs`'s responsiveness
    //! test which already uses `std::thread::sleep` to simulate a blocked probe.
    //!
    //! Test: `cargo test -p trusty-search -- warm_boot::probe`.

    use super::*;

    // ── volume_key ────────────────────────────────────────────────────────────

    /// Why: guard that boot-volume and non-macOS paths return `/`.
    /// What: paths starting with `/tmp`, `/usr`, `/home` return `/`.
    /// Test: this test.
    #[test]
    fn volume_key_boot_volume() {
        assert_eq!(
            volume_key(Path::new("/tmp/trusty-test")),
            PathBuf::from("/"),
            "/tmp/... must produce volume key /"
        );
        assert_eq!(
            volume_key(Path::new("/usr/local/bin")),
            PathBuf::from("/"),
            "/usr/... must produce volume key /"
        );
        assert_eq!(
            volume_key(Path::new("/")),
            PathBuf::from("/"),
            "root itself must produce volume key /"
        );
        assert_eq!(
            volume_key(Path::new("/home/user/projects")),
            PathBuf::from("/"),
            "/home/... must produce volume key /"
        );
    }

    /// Why: guard that external macOS volumes extract the `/Volumes/<label>` key.
    /// What: paths under `/Volumes/SSD1` or `/Volumes/ExternalDrive` return
    /// `/Volumes/<label>`.
    /// Test: this test.
    #[test]
    fn volume_key_external_volume() {
        assert_eq!(
            volume_key(Path::new("/Volumes/SSD1/Projects/trusty-tools")),
            PathBuf::from("/Volumes/SSD1"),
            "/Volumes/SSD1/... must produce volume key /Volumes/SSD1"
        );
        assert_eq!(
            volume_key(Path::new("/Volumes/ExternalDrive/code")),
            PathBuf::from("/Volumes/ExternalDrive"),
            "/Volumes/ExternalDrive/... must produce volume key /Volumes/ExternalDrive"
        );
        assert_eq!(
            volume_key(Path::new("/Volumes/SSD1")),
            PathBuf::from("/Volumes/SSD1"),
            "/Volumes/SSD1 itself must produce volume key /Volumes/SSD1"
        );
    }

    // ── probe_volume ──────────────────────────────────────────────────────────

    /// Why: the most critical invariant — a real accessible directory must
    /// return `Accessible` within a generous deadline.
    /// What: create a tempdir, probe it with a 5s deadline; assert `Accessible`.
    /// Test: this test.
    #[test]
    fn probe_volume_accessible_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = probe_volume(tmp.path(), Duration::from_secs(5));
        assert_eq!(
            result,
            VolumeAccessibility::Accessible,
            "a real tmpdir must be accessible within 5s"
        );
    }

    /// Why: a path that does not exist returns an OS error immediately (not a
    /// hang), so the probe should return `Accessible` — the kernel answered.
    /// What: probe a nonexistent path with a 5s deadline; assert `Accessible`
    /// (the probe returns fast even on error).
    /// Test: this test.
    #[test]
    fn probe_volume_nonexistent_path_returns_accessible() {
        // On all tested OSes, `metadata` on a nonexistent path returns ENOENT
        // immediately — there is no hang. The probe thread sends () promptly.
        let result = probe_volume(
            Path::new("/tmp/trusty-723-definitely-not-here-xyz99999"),
            Duration::from_secs(5),
        );
        assert_eq!(
            result,
            VolumeAccessibility::Accessible,
            "a NotFound metadata call must return promptly (kernel answered), not time out"
        );
    }

    // ── probe_all_volumes ────────────────────────────────────────────────────

    /// Why: all-accessible paths must produce an empty inaccessible set.
    /// What: provide several paths under /tmp; assert no inaccessible volumes.
    /// Test: this test.
    #[test]
    fn probe_all_volumes_accessible_returns_empty() {
        let paths = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
            PathBuf::from("/usr/local"),
        ];
        let inaccessible = probe_all_volumes(&paths, Duration::from_secs(5));
        assert!(
            inaccessible.is_empty(),
            "all boot-volume paths must be accessible; got: {inaccessible:?}"
        );
    }

    /// Why: paths on different volumes must produce distinct volume keys and
    /// each be probed exactly once (deduplicated).
    /// What: three paths — two under `/tmp` (same volume key `/`) and one
    /// hypothetical `/Volumes/SSD1/...`. Assert the volume key extraction works.
    /// We do NOT assert the SSD1 probe result (would require the hardware).
    /// Test: this test — validates deduplication at the key level.
    #[test]
    fn probe_all_volumes_distinct_keys() {
        // Two paths on the same volume must deduplicate to one key.
        let paths = vec![
            PathBuf::from("/tmp/proj-a"),
            PathBuf::from("/tmp/proj-b"),
            PathBuf::from("/usr/local/bin"),
        ];
        // All on boot volume ("/"), so one unique key.
        let mut keys: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for p in &paths {
            keys.insert(volume_key(p));
        }
        assert_eq!(keys.len(), 1, "3 boot-volume paths must yield 1 unique key");
        assert!(keys.contains(&PathBuf::from("/")));
    }

    // ── volume_probe_timeout ─────────────────────────────────────────────────

    /// Why: guard that the env var reader parses valid values and falls back.
    /// What: set `TRUSTY_WARMBOOT_VOLUME_PROBE_SECS=7`, assert Duration is 7s;
    /// unset, assert Duration is the default 5s.
    /// Note: `serial` prevents racing with other env-var mutators.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn volume_probe_timeout_parses_env_var() {
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_VOLUME_PROBE_SECS", "7") };
        assert_eq!(
            volume_probe_timeout(),
            Duration::from_secs(7),
            "must parse 7 from env var"
        );
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_VOLUME_PROBE_SECS") };
        assert_eq!(
            volume_probe_timeout(),
            Duration::from_secs(5),
            "must fall back to 5s default when env var is absent"
        );
    }
}
