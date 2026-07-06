//! Automatic self-healing of orphaned index registrations (orphan self-heal).
//!
//! Why: MPM spins up an ephemeral git worktree per session under
//! `.worktrees/<uuid>/` and deletes it when the session ends. Each such root
//! can get registered as an index (auto-discovery or the colocated rescan);
//! when the worktree is removed the registration is left behind pointing at a
//! now-deleted path. Over a long-lived daemon these accumulate without bound —
//! production saw 485 dead registrations build up over 26 days, each re-read
//! and re-warned on every warm-boot and, worse, each holding an FSEvents watch
//! that pinned macOS `fseventsd` at ~100% CPU / 8 GB RSS. Nothing in the daemon
//! ever removed them; `prune-orphans` / `cleanup` were manual CLI commands
//! only, so an operator who never ran them accumulated the leak silently.
//!
//! What: two entry points share one safety predicate ([`is_reapable_orphan`]):
//!   * [`heal_boot_orphans`] — called once at warm-boot start; drops legacy
//!     (non-colocated) registrations whose `root_path` was deleted so they stop
//!     being re-read on every boot.
//!   * `server::tickers::spawn_orphan_reaper_ticker` — a periodic task that
//!     unregisters live indexes whose root vanished while the daemon runs
//!     (the actual accumulation vector, since the daemon ran continuously).
//!
//! Safety: a registration is only reaped when its `root_path` is missing AND
//! its immediate parent still exists. A deleted worktree leaves `.worktrees/`
//! behind (→ reap), whereas an unmounted external volume takes the whole parent
//! chain with it (→ do NOT reap; the data returns when the volume remounts).
//! Neither path ever deletes on-disk index *data* — only the registration is
//! removed — so a false-positive detection is always recoverable by
//! re-registering the path.
//!
//! Test: `is_reapable_orphan_*` and `heal_boot_orphans_*` unit tests below.

use std::path::Path;

use crate::service::persistence::{save_index_registry, PersistedIndex};

/// Environment variable overriding the reaper cadence, in seconds. `0` disables
/// the runtime reaper entirely; any unparseable value falls back to the default.
pub const REAP_INTERVAL_ENV: &str = "TRUSTY_ORPHAN_REAP_SECS";

/// Default runtime-reaper cadence: hourly.
///
/// Why: worktree churn happens on the timescale of minutes-to-hours, and a dead
/// registration costs an idle FSEvents watch, not correctness, so an hourly
/// sweep reclaims it promptly without adding meaningful wakeups. The boot-time
/// [`heal_boot_orphans`] pass covers everything accumulated while the daemon
/// was down, so the ticker only needs to catch roots deleted mid-run.
const DEFAULT_REAP_INTERVAL_SECS: u64 = 3600;

/// Resolve the runtime reaper cadence from the environment.
///
/// Why: lets an operator retune or disable (`TRUSTY_ORPHAN_REAP_SECS=0`) the
/// sweep without a rebuild — mirroring the idle-eviction ticker's env knob.
/// What: returns `None` when the var is exactly `0` (reaper disabled),
/// `Some(n)` for a positive integer, and `Some(DEFAULT_REAP_INTERVAL_SECS)`
/// when unset or unparseable.
/// Test: `reap_interval_secs_*` unit tests below.
pub fn reap_interval_secs() -> Option<u64> {
    match std::env::var(REAP_INTERVAL_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(DEFAULT_REAP_INTERVAL_SECS),
        },
        Err(_) => Some(DEFAULT_REAP_INTERVAL_SECS),
    }
}

/// True iff `root_path` is a safely-reapable orphan.
///
/// Why: distinguishing a genuinely-deleted project directory from a transiently
/// unavailable one is the whole safety story, and there are two failure modes.
/// (1) A temporarily-unmounted volume (`/Volumes/Ext/project`) makes the root
/// vanish along with its parent — keying on "immediate parent still exists"
/// spares it while still reaping a deleted worktree
/// (`~/repo/.worktrees/<uuid>`), whose parent `.worktrees/` survives. (2) On
/// macOS a *mounted-but-TCC-blocked* external volume (issues #723 / #873, the
/// recurring FDA-loss-after-`cargo install` scenario) can make `exists()`
/// return `false` — or even hang — for a perfectly live root. So external
/// mounts under `/Volumes` are excluded outright *before any syscall*: the
/// reaper's target (MPM worktree churn) always lives under `$HOME`, never on a
/// removable volume, and manual `prune-orphans` still covers `/Volumes` when a
/// human confirms.
/// What: returns `false` for any `/Volumes/…` path (no stat performed) and for
/// paths that still exist; otherwise returns whether the immediate parent is a
/// non-empty, present directory.
/// Test: `is_reapable_orphan_*` unit tests below.
pub fn is_reapable_orphan(root_path: &Path) -> bool {
    // Cheap prefix check first — never stat (or risk hanging on) an external
    // volume that may be mounted-but-TCC-blocked.
    if root_path.starts_with("/Volumes") {
        return false;
    }
    if root_path.exists() {
        return false;
    }
    match root_path.parent() {
        Some(parent) => !parent.as_os_str().is_empty() && parent.exists(),
        None => false,
    }
}

/// Remove legacy registrations whose root was deleted, returning the survivors.
///
/// Why: warm-boot previously *detected* a missing non-colocated `root_path`,
/// logged "run `trusty-search prune-orphans`", and skipped it — leaving the
/// entry in `indexes.toml` to be re-read, re-warned, and re-skipped on every
/// subsequent boot forever. This converts that detection into action so the
/// registry self-heals across restarts.
/// What: partitions `entries` into reapable orphans (`!colocated` AND
/// [`is_reapable_orphan`]) and survivors. Colocated entries are deliberately
/// left alone so warm-boot's relocation scan can still recover a *moved* repo.
/// When any orphan is found, atomically rewrites `indexes.toml` to the
/// survivors (one write, matching the manual `prune-orphans` path and the
/// existing last-write-wins persistence contract), best-effort scrubs each
/// orphan's root from `roots.toml` so the colocated rescan cannot resurrect it,
/// and returns the survivors. On-disk index *data* is never touched.
/// Test: `heal_boot_orphans_*` unit tests below (partition logic); the save
/// path is shared with the `prune-orphans` CLI, whose tests cover it.
pub fn heal_boot_orphans(entries: Vec<PersistedIndex>) -> Vec<PersistedIndex> {
    let (orphans, kept) = partition_boot_orphans(entries);

    if orphans.is_empty() {
        return kept;
    }

    // `entries` was the full contents of `indexes.toml`, so `kept` is exactly
    // "everything minus the orphans" — a single atomic rewrite is correct.
    if let Err(e) = save_index_registry(&kept) {
        tracing::warn!(
            "orphan-reaper(boot): could not rewrite indexes.toml ({e}); {} orphaned \
             registration(s) left on disk (they will be retried next boot)",
            orphans.len()
        );
        // Still return `kept` so this boot does not attempt to restore the dead
        // entries; only the on-disk cleanup was skipped.
        return kept;
    }

    for orphan in &orphans {
        if let Err(e) = crate::service::roots_registry::remove_root(&orphan.root_path) {
            tracing::debug!(
                "orphan-reaper(boot): could not remove root {} from roots.toml: {e}",
                orphan.root_path.display()
            );
        }
    }

    tracing::info!(
        "orphan-reaper(boot): removed {} orphaned registration(s) whose root_path was \
         deleted (self-heal); {} registration(s) remain",
        orphans.len(),
        kept.len()
    );

    kept
}

/// Pure split of registry entries into reapable orphans and survivors.
///
/// Why: extracted from [`heal_boot_orphans`] so the reap decision can be
/// unit-tested hermetically, without touching the real `indexes.toml`.
/// What: partitions on `!colocated && is_reapable_orphan(root_path)` — the
/// first `Vec` is the orphans to drop, the second the survivors to keep.
/// Test: `heal_boot_orphans_*` unit tests below.
pub(crate) fn partition_boot_orphans(
    entries: Vec<PersistedIndex>,
) -> (Vec<PersistedIndex>, Vec<PersistedIndex>) {
    entries
        .into_iter()
        .partition(|e| !e.colocated && is_reapable_orphan(&e.root_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn entry(id: &str, root: PathBuf, colocated: bool) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: root,
            colocated,
            ..Default::default()
        }
    }

    /// Why: a live directory must never be treated as an orphan.
    /// What: the tempdir itself exists → not reapable.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_for_live_root() {
        let tmp = tempdir().unwrap();
        assert!(!is_reapable_orphan(tmp.path()));
    }

    /// Why: the core self-heal case — a deleted leaf whose parent survives (the
    /// vanished-worktree shape) must be reapable.
    /// What: point at a non-existent child of an existing tempdir.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_true_for_deleted_leaf_with_live_parent() {
        let tmp = tempdir().unwrap();
        let dead = tmp.path().join("worktree-abc123");
        assert!(!dead.exists());
        assert!(is_reapable_orphan(&dead));
    }

    /// Why: the safety guarantee — an unmounted volume (whole parent chain gone)
    /// must NOT be reaped, or a remount would find its registration destroyed.
    /// What: a path whose parent also does not exist is not reapable.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_when_parent_also_missing() {
        let missing = PathBuf::from("/no/such/mount/point/project");
        assert!(!missing.exists());
        assert!(!is_reapable_orphan(&missing));
    }

    /// Why: external `/Volumes/…` mounts are excluded outright — a mounted-but-
    /// TCC-blocked or unmounted volume must never be auto-reaped (issues
    /// #723/#873), even when its parent `/Volumes` exists.
    /// What: a non-existent `/Volumes/Ext/project` is not reapable despite
    /// `/Volumes` being present.
    /// Test: this test.
    #[test]
    fn is_reapable_orphan_false_for_external_volume() {
        let vol = PathBuf::from("/Volumes/DefinitelyNotMounted-xyz/project");
        assert!(!vol.exists());
        assert!(!is_reapable_orphan(&vol));
    }

    /// Why: `0` disables the reaper; anything else parses or falls back.
    /// What: exercises the three branches via a scoped env guard.
    /// Test: this test.
    #[test]
    fn reap_interval_secs_env_branches() {
        // Serialize on a process-global to avoid cross-test env races.
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(REAP_INTERVAL_ENV, "0");
        assert_eq!(reap_interval_secs(), None);
        std::env::set_var(REAP_INTERVAL_ENV, "120");
        assert_eq!(reap_interval_secs(), Some(120));
        std::env::set_var(REAP_INTERVAL_ENV, "not-a-number");
        assert_eq!(reap_interval_secs(), Some(DEFAULT_REAP_INTERVAL_SECS));
        std::env::remove_var(REAP_INTERVAL_ENV);
        assert_eq!(reap_interval_secs(), Some(DEFAULT_REAP_INTERVAL_SECS));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Why: a colocated orphan must be preserved so warm-boot's relocation scan
    /// can still recover a moved repo; only non-colocated dead roots are reaped.
    /// What: two dead-leaf entries (one colocated) plus one live entry → only
    /// the non-colocated dead one is dropped.
    /// Test: this test.
    #[test]
    fn heal_boot_orphans_reaps_only_non_colocated_dead_roots() {
        let tmp = tempdir().unwrap();
        let dead_legacy = tmp.path().join("gone-legacy");
        let dead_colocated = tmp.path().join("gone-colocated");
        let live = tmp.path().to_path_buf();

        let (orphans, kept) = partition_boot_orphans(vec![
            entry("legacy", dead_legacy, false),
            entry("colocated", dead_colocated, true),
            entry("live", live, false),
        ]);

        let orphan_ids: Vec<&str> = orphans.iter().map(|e| e.id.as_str()).collect();
        let kept_ids: Vec<&str> = kept.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            orphan_ids,
            ["legacy"],
            "only the dead legacy root is reaped"
        );
        assert!(
            kept_ids.contains(&"colocated"),
            "colocated orphan must be preserved for relocation"
        );
        assert!(kept_ids.contains(&"live"), "live root must be preserved");
    }

    /// Why: with no orphans the split must return everything as survivors.
    /// What: all-live input yields zero orphans.
    /// Test: this test.
    #[test]
    fn heal_boot_orphans_noop_when_all_live() {
        let tmp = tempdir().unwrap();
        let (orphans, kept) =
            partition_boot_orphans(vec![entry("a", tmp.path().to_path_buf(), false)]);
        assert!(orphans.is_empty());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "a");
    }
}
