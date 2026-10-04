//! Disk-budget eviction of the builder slot pool (#8451).
//!
//! Why: the pool under `builders.slot_pool_root` grows one warm `slot-N`
//! directory per repo and slot, and nothing ever removed one. It reached 2.6 TB
//! (71% of a 3.67 TB volume), pushed the volume past the 90% worktree guard
//! (#7497) and blocked worktree creation for every project. Owner ruling
//! (2026-09-28, option A): the daemon sweeps once the volume holding the pool
//! passes a percent-of-volume threshold set below that guard, evicting whole
//! `slot-N` directories oldest-first and never one a builder is using.
//!
//! What: [`effective_evict_pct`] is the threshold arithmetic; [`list_pool_slots`]
//! enumerates `<root>/<owner>/<repo>/slot-<N>` oldest-first; [`sweep`] evicts
//! while the volume stays at or over the threshold.
//!
//! **In-use guard.** A pool directory `slot-K` is only ever built in, or seeded,
//! by a `tm build-lease` holding lease flock `K` (see `target_dir`). The sweep
//! therefore TAKES flock `K` before touching any `slot-K`, so no new lease can
//! start in it mid-eviction, and spares the slot when:
//!
//! - the flock is held (a live lease, which includes a seed in progress);
//! - the slot file is broken, or its leftover record names a build still
//!   running or cannot be checked (`orphan::Leftover`, the SIGKILLed-holder case);
//! - cargo holds a `.cargo-lock` in the directory (a build pointed there by hand);
//! - a staging tree `.slot-K.seeding.<pid>.<nanos>` beside it belongs to a live
//!   process (a seed whose holder still runs).
//!
//! Under the flock the directory is renamed to `.slot-K.evicting.<pid>.<nanos>`,
//! the flock is released, and the renamed tree is deleted. A deletion that does
//! not finish leaves that tree, which the next sweep removes first.
//!
//! **Root guard.** The walk deletes any real `slot-<u32>` directory two levels
//! under the root, so a root that is `/`, the home directory, or an ancestor of
//! home, or that does not resolve, is refused before anything is read
//! ([`SweepOutcome::Refused`]); identity, not only the path, is compared, and
//! the walk uses the canonical root the check resolved. The pool has no root
//! marker to check instead.
//!
//! **Cancellation.** The sweep checks its cancel signal before each slot and
//! each leftover tree, so a shutdown waits on at most one removal.
//!
//! **Fail direction: keep.** A volume that cannot be measured evicts nothing; a
//! slot whose state cannot be read is spared; a slot whose age cannot be read
//! sorts newest.
//!
//! Test: `evict_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::orphan::Leftover;
use super::slots::SlotDir;
use super::stale_guard::cargo_lock_held;

/// Default eviction threshold, percent of the volume holding the pool.
///
/// Why: the owner ruling names 85% as the example, five points under the
/// default 90% worktree guard, so eviction frees space before that guard
/// refuses a worktree.
/// Test: `the_threshold_defaults_below_the_worktree_guard`.
pub const DEFAULT_EVICT_PCT: u8 = 85;

/// The config key that sets the threshold, spelled once for messages.
pub const EVICT_PCT_KEY: &str = "builders.slot_pool_evict_pct";

/// The eviction threshold in force.
///
/// What: `configured` when it lies in `1..=99`, else [`DEFAULT_EVICT_PCT`];
/// then held strictly below `guard_pct` (the `disk.max_usage_pct` worktree
/// guard), so the sweep always starts before the guard refuses. Never 0.
/// Test: `the_threshold_defaults_below_the_worktree_guard`,
/// `a_threshold_at_or_over_the_guard_is_held_below_it`,
/// `an_out_of_range_threshold_uses_the_default`.
#[must_use]
pub fn effective_evict_pct(configured: Option<u8>, guard_pct: u8) -> u8 {
    let wanted = configured
        .filter(|pct| (1..=99).contains(pct))
        .unwrap_or(DEFAULT_EVICT_PCT);
    wanted.min(guard_pct.saturating_sub(1)).max(1)
}

/// One `slot-N` directory in the pool.
///
/// Test: `slots_are_listed_oldest_first`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PoolSlot {
    /// `<root>/<owner>/<repo>/slot-<index>`.
    pub path: PathBuf,
    /// The lease slot index the directory belongs to.
    pub index: u32,
    /// The newest modification time on the slot and its top two levels.
    pub last_used: SystemTime,
}

/// Every `slot-N` directory under `root`, oldest first.
///
/// Why: a build writes deep inside `slot-N` (`debug/deps`, `debug/incremental`),
/// which never touches the `slot-N` directory's own mtime, so its age is read
/// from the top two levels as well.
/// What: walks `<root>/<owner>/<repo>/` for real directories named `slot-<u32>`.
/// A missing `root` is an empty pool; an owner or repo directory that cannot be
/// listed is skipped. Ties sort by path, so the order is deterministic.
///
/// # Errors
///
/// `root` exists but cannot be listed.
///
/// Test: `slots_are_listed_oldest_first`, `a_missing_pool_is_empty`.
pub fn list_pool_slots(root: &Path) -> std::io::Result<Vec<PoolSlot>> {
    let owners = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut slots = Vec::new();
    for owner in real_dirs(owners) {
        let Ok(repos) = std::fs::read_dir(&owner) else {
            continue;
        };
        for repo in real_dirs(repos) {
            let Ok(entries) = std::fs::read_dir(&repo) else {
                continue;
            };
            for path in real_dirs(entries) {
                if let Some(index) = slot_index(&path) {
                    let last_used = last_used(&path);
                    slots.push(PoolSlot {
                        path,
                        index,
                        last_used,
                    });
                }
            }
        }
    }
    slots.sort_by(|a, b| a.last_used.cmp(&b.last_used).then(a.path.cmp(&b.path)));
    Ok(slots)
}

/// Why one slot was left in place.
///
/// Test: one test per arm in `evict_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Spared {
    /// A lease holds flock `K`: a build or a seed is running.
    LeaseHeld,
    /// The slot file is broken, or its leftover record cannot be checked.
    Unknown(String),
    /// A dead holder's build still runs (pid named).
    OrphanedBuild(u32),
    /// Cargo holds a `.cargo-lock` in the directory.
    CargoLockHeld,
    /// A live process is staging a seed for this slot index.
    Seeding,
}

/// What one sweep did.
///
/// Test: `an_over_threshold_volume_evicts_oldest_first_until_below`.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct EvictReport {
    /// Slot directories removed, oldest first.
    pub evicted: Vec<PathBuf>,
    /// Slots left in place, and why.
    pub spared: Vec<(PathBuf, Spared)>,
    /// Removals that began and did not finish, with the error.
    pub failed: Vec<(PathBuf, String)>,
    /// Earlier `.evicting.` trees this sweep finished removing.
    pub leftovers_removed: usize,
    /// The volume's usage when the sweep stopped; `None` when unmeasurable.
    pub usage_after: Option<f32>,
    /// The cancel signal stopped the sweep at a slot boundary.
    pub cancelled: bool,
}

impl EvictReport {
    /// Whether a removal began and did not finish.
    ///
    /// Test: `a_failed_removal_is_reported_and_left_for_the_next_sweep`.
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// What a sweep found before it acted.
///
/// Test: `an_unmeasurable_volume_evicts_nothing`,
/// `an_under_threshold_volume_evicts_nothing`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SweepOutcome {
    /// No pool directory exists.
    NoPool,
    /// The volume could not be measured; nothing was touched.
    Unmeasurable,
    /// The volume is under the threshold; nothing was touched.
    UnderThreshold(f32),
    /// The pool root exists but cannot be listed; nothing was touched.
    Unlistable(String),
    /// The pool root is unsafe to sweep (see the module's root guard); nothing
    /// was read or touched.
    Refused(String),
    /// The volume was at or over the threshold and the sweep ran.
    Swept(EvictReport),
}

/// Evict `slot-N` directories under `pool_root`, oldest first, while the
/// volume is at or over `threshold_pct`.
///
/// What: `pool_root` first passes [`check_pool_root`] against `home`.
/// `measure` reads the used percent of the volume holding a path
/// (`disk_usage_guard::measure` in production). It is read before the sweep
/// and again after each eviction; the sweep stops as soon as the volume reads
/// under the threshold or cannot be read. Each candidate goes through
/// [`evict_one`] against `store`, the machine's lease store. `cancelled` is
/// polled before each slot; once it reads true the sweep stops there.
/// Test: `an_over_threshold_volume_evicts_oldest_first_until_below`,
/// `an_unmeasurable_volume_evicts_nothing`,
/// `an_under_threshold_volume_evicts_nothing`, `a_missing_pool_is_empty`,
/// `a_cancelled_sweep_evicts_no_further_slots`.
pub fn sweep(
    pool_root: &Path,
    home: &Path,
    store: &SlotDir,
    threshold_pct: u8,
    measure: &mut dyn FnMut(&Path) -> Option<f32>,
    cancelled: &dyn Fn() -> bool,
) -> SweepOutcome {
    // #8451: the walk uses the canonical root the check resolved, so the check
    // and the deletion see one path.
    let pool_root = match check_pool_root(pool_root, home) {
        RootCheck::Missing => return SweepOutcome::NoPool,
        RootCheck::Refused(why) => return SweepOutcome::Refused(why),
        RootCheck::Safe(canon) => canon,
    };
    let pool_root = pool_root.as_path();
    let threshold = f32::from(threshold_pct);
    let Some(usage) = measure(pool_root) else {
        return SweepOutcome::Unmeasurable;
    };
    if usage < threshold {
        return SweepOutcome::UnderThreshold(usage);
    }
    let mut report = EvictReport::default();
    remove_leftovers(pool_root, &mut report, cancelled);
    let slots = match list_pool_slots(pool_root) {
        Ok(slots) => slots,
        Err(err) => return SweepOutcome::Unlistable(err.to_string()),
    };
    report.usage_after = measure(pool_root);
    for slot in slots {
        // #8451: threshold first, so a cancel with nothing left to evict is not
        // reported as a sweep stopped early.
        if report.usage_after.is_none_or(|usage| usage < threshold) {
            break;
        }
        // #8451: stop at a slot boundary so shutdown waits on one removal at most.
        if report.cancelled || cancelled() {
            report.cancelled = true;
            break;
        }
        match evict_one(&slot, store) {
            Ok(()) => report.evicted.push(slot.path),
            Err(Outcome::Spared(why)) => report.spared.push((slot.path, why)),
            Err(Outcome::Failed(err)) => report.failed.push((slot.path, err)),
        }
        report.usage_after = measure(pool_root);
    }
    SweepOutcome::Swept(report)
}

/// What [`check_pool_root`] found.
enum RootCheck {
    /// No directory at the root: nothing to sweep.
    Missing,
    /// Unsafe to sweep, and why.
    Refused(String),
    /// Safe to sweep; carries the canonical root the check resolved.
    Safe(PathBuf),
}

/// A file's `(device, inode)`, or `None` when its metadata cannot be read.
type FileId = (u64, u64);

/// The real [`FileId`] reader; follows symlinks.
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Whether `root` is safe to sweep, given the operator's `home`.
///
/// Why: a misconfigured `builders.slot_pool_root` (`/`, home, an ancestor of
/// home) would point the `<root>/*/*/slot-N` deletion at unrelated directories.
/// What: a root that does not exist, or is not a directory, is `Missing`. A
/// root that fails to canonicalize for any other reason is refused, as is a
/// canonical root that is `/`, or that equals or contains the canonical home.
/// A home that does not canonicalize is refused too: the root cannot be
/// checked against it. A path comparison can miss an alias (a case-insensitive
/// volume, a Unicode-normalization variant), so the root is also refused when
/// it has the same `(dev, ino)` as home or any ancestor of home; a root whose
/// metadata cannot be read is refused, an unreadable ancestor is skipped.
/// Test: `the_filesystem_root_is_refused`, `the_home_directory_is_refused`,
/// `an_ancestor_of_home_is_refused`, `an_unresolvable_root_is_refused`,
/// `an_unresolvable_home_is_refused`, `an_inode_alias_of_an_ancestor_of_home_is_refused`,
/// `a_root_with_unreadable_metadata_is_refused`,
/// `a_case_variant_of_home_is_refused`, `a_symlink_to_home_is_refused`,
/// `a_symlink_to_the_filesystem_root_is_refused`, `a_dot_suffixed_home_is_refused`.
fn check_pool_root(root: &Path, home: &Path) -> RootCheck {
    check_pool_root_with(root, home, &file_id)
}

/// [`check_pool_root`] with the metadata reader injected.
fn check_pool_root_with(
    root: &Path,
    home: &Path,
    id_of: &dyn Fn(&Path) -> Option<FileId>,
) -> RootCheck {
    let canon = match std::fs::canonicalize(root) {
        Ok(canon) => canon,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return RootCheck::Missing,
        // #8451: a root that does not resolve cannot be checked; keep everything.
        Err(err) => {
            return RootCheck::Refused(format!("{} does not resolve ({err})", root.display()));
        }
    };
    if !canon.is_dir() {
        return RootCheck::Missing;
    }
    // #8451: the walk under `/` reaches every `/<a>/<b>/slot-N` on the host.
    if canon.parent().is_none() {
        return RootCheck::Refused(format!("{} is the filesystem root", root.display()));
    }
    let home = match std::fs::canonicalize(home) {
        Ok(home) => home,
        Err(err) => {
            return RootCheck::Refused(format!(
                "home {} does not resolve ({err}), so {} cannot be checked against it",
                home.display(),
                root.display()
            ));
        }
    };
    // #8451: a root at or above home would walk the operator's own files.
    if home.starts_with(&canon) {
        return RootCheck::Refused(format!(
            "{} is the home directory or an ancestor of it",
            root.display()
        ));
    }
    // #8451: the same directory under another spelling has the same (dev, ino).
    let Some(root_id) = id_of(&canon) else {
        return RootCheck::Refused(format!(
            "{} has unreadable metadata, so it cannot be checked against home",
            root.display()
        ));
    };
    if home.ancestors().any(|dir| id_of(dir) == Some(root_id)) {
        return RootCheck::Refused(format!(
            "{} is the same directory as home or an ancestor of it",
            root.display()
        ));
    }
    RootCheck::Safe(canon)
}

/// Why [`evict_one`] did not evict.
enum Outcome {
    Spared(Spared),
    Failed(String),
}

/// Evict one slot directory, holding its lease flock across the rename.
///
/// What: see the module's in-use guard. A failed delete leaves the renamed
/// `.evicting.` tree for the next sweep and is reported, never folded into a
/// success.
///
/// Residual window: the lease flock excludes only builds run under
/// `tm build-lease`. A cargo build pointed at the slot by hand can take
/// `.cargo-lock` after the `cargo_lock_held` check and before the rename; the
/// rename then moves its tree away mid-build.
/// Test: one test per [`Spared`] arm, plus
/// `a_failed_rename_is_reported_and_keeps_the_slot`.
fn evict_one(slot: &PoolSlot, store: &SlotDir) -> Result<(), Outcome> {
    let parent = slot.path.parent().unwrap_or(&slot.path);
    if live_staging(parent, slot.index) {
        return Err(Outcome::Spared(Spared::Seeding));
    }
    let guard = match store.try_acquire(slot.index) {
        Ok(Some(guard)) => guard,
        Ok(None) => return Err(Outcome::Spared(Spared::LeaseHeld)),
        Err(err) => return Err(Outcome::Spared(Spared::Unknown(err.to_string()))),
    };
    match guard.leftover() {
        Leftover::Clear => {}
        Leftover::Running(record) => {
            // The record is what keeps the slot held; it must survive the probe.
            guard.release_keeping_record();
            let pid = record.child_pid.unwrap_or(record.pid);
            return Err(Outcome::Spared(Spared::OrphanedBuild(pid)));
        }
        Leftover::Unknown(err) => {
            guard.release_keeping_record();
            return Err(Outcome::Spared(Spared::Unknown(err)));
        }
    }
    if cargo_lock_held(&slot.path) {
        return Err(Outcome::Spared(Spared::CargoLockHeld));
    }
    let doomed = parent.join(format!(
        ".slot-{}{EVICTING_INFIX}{}.{}",
        slot.index,
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::rename(&slot.path, &doomed)
        .map_err(|err| Outcome::Failed(format!("rename to {}: {err}", doomed.display())))?;
    // The slot name is gone, so the next lease seeds a fresh directory.
    drop(guard);
    std::fs::remove_dir_all(&doomed)
        .map_err(|err| Outcome::Failed(format!("delete {}: {err}", doomed.display())))
}

/// The infix an evicted, not-yet-deleted tree carries.
const EVICTING_INFIX: &str = ".evicting.";

/// Remove `.slot-K.evicting.<pid>.<nanos>` trees an earlier sweep left.
///
/// What: a tree whose pid is this process or a dead one is removed; a live
/// other process's tree is left to it. A removal error is reported as failed.
/// `cancelled` is polled before each tree.
///
/// Two limits. A dead sweep's pid that the OS has recycled to a live process
/// reads as live, so its tree stays until that process exits. And leftovers
/// are only removed by a sweep that runs, which needs the volume at or over
/// the threshold; under it they stay on disk.
/// Test: `a_failed_removal_is_reported_and_left_for_the_next_sweep`,
/// `a_sweep_cancelled_before_it_starts_touches_nothing`.
fn remove_leftovers(root: &Path, report: &mut EvictReport, cancelled: &dyn Fn() -> bool) {
    for repo in repo_dirs(root) {
        let Ok(entries) = std::fs::read_dir(&repo) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|n| n.starts_with(".slot-"))
                .and_then(|n| n.split_once(EVICTING_INFIX))
                .and_then(|(_, rest)| rest.split('.').next()?.parse::<u32>().ok())
            else {
                continue;
            };
            if pid != std::process::id() && crate::core::process::is_process_alive(pid) {
                continue;
            }
            // #8451: a cancel stops before the next tree, not after the last.
            if cancelled() {
                report.cancelled = true;
                return;
            }
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => report.leftovers_removed += 1,
                Err(err) => report.failed.push((entry.path(), err.to_string())),
            }
        }
    }
}

/// Whether a live process is staging a seed for slot `index` in `parent`.
///
/// What: an entry `.slot-<index>.seeding.<pid>.<nanos>` (the name
/// `builder_slot_pool::staging_path` mints) whose pid is alive, or cannot be
/// parsed — an unreadable name is treated as live, and so is a `parent` that
/// cannot be listed.
/// Test: `a_live_seed_staging_spares_its_slot`,
/// `an_unreadable_repo_dir_reads_as_live_staging`.
fn live_staging(parent: &Path, index: u32) -> bool {
    let prefix = format!(".slot-{index}.seeding.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return true;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
            return false;
        };
        rest.split('.')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_none_or(crate::core::process::is_process_alive)
    })
}

/// Every `<root>/<owner>/<repo>` directory.
fn repo_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(owners) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    real_dirs(owners)
        .into_iter()
        .filter_map(|owner| std::fs::read_dir(owner).ok())
        .flat_map(real_dirs)
        .collect()
}

/// The real (non-symlink) directories in `entries`.
fn real_dirs(entries: std::fs::ReadDir) -> Vec<PathBuf> {
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// `slot-<u32>` → the index; anything else → `None`.
fn slot_index(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("slot-")?
        .parse()
        .ok()
}

/// The newest mtime on `path`, its children and its grandchildren.
///
/// What: an unreadable time reads as now, so a slot of unknown age is evicted
/// last rather than first.
fn last_used(path: &Path) -> SystemTime {
    let mtime = |p: &Path| {
        std::fs::symlink_metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| SystemTime::now())
    };
    let mut newest = mtime(path);
    let children = std::fs::read_dir(path).map(real_dirs).unwrap_or_default();
    for child in children {
        newest = newest.max(mtime(&child));
        if let Ok(grand) = std::fs::read_dir(&child) {
            for entry in grand.flatten() {
                newest = newest.max(mtime(&entry.path()));
            }
        }
    }
    newest
}

#[cfg(test)]
#[path = "evict_tests.rs"]
mod tests;
