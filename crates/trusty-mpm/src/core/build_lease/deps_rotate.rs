//! Rotate an oversized cargo profile directory aside (#10212 Part 1).
//!
//! Why: nothing pruned `debug/deps`, so it grew to 844,490 entries and stalled
//! syspolicyd on every exec. Trimming files needs a full readdir of that very
//! directory, which is the stall; a rename is O(1). The operator's manual fix,
//! `mv debug debug.old-<date>`, brought syspolicyd to 0% within a minute.
//! What: [`rotate_if_over`] counts `<target>/debug/deps` with the bounded
//! [`count_deps`], and at or over the line renames the whole `debug/` aside
//! with [`discard`] (one rename, deletion on a background thread) while
//! holding every cargo lock of the target — a build that holds one is never
//! rotated. [`reclaim_aside`] deletes aside trees whose deleter died.
//! [`seed_refusal`] tells a seed not to clone an over-line source. The
//! operator's repair and the daemon's reclaim live in `deps_repair`. Nothing
//! here lists `deps/` beyond the line.
//! Test: `deps_rotate_tests.rs`.

use std::path::{Path, PathBuf};

use super::deps_budget::{DEPS_ROTATE_ENTRIES, DEPS_WARN_ENTRIES, DepsCount, count_deps};
use super::evict::EVICTING_INFIX;
use super::stale_guard::cargo_lock_held;
use crate::core::builder_slot_pool::hold_idle_build_locks;
use crate::core::builder_slot_pool::staging::{Discarded, discard};

/// The profile directory rotated as a whole (#10212, Q2a).
pub const PROFILE: &str = "debug";

/// What one rotation check did to a target directory.
///
/// Test: `deps_rotate_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rotation {
    /// Under the line, or no `deps/` yet: nothing to do.
    NotNeeded(DepsCount),
    /// Over the line; a dry run, so nothing was moved.
    WouldRotate,
    /// `debug/` was renamed to `aside`.
    Rotated {
        /// Where the old tree was moved.
        aside: PathBuf,
        /// `true` once the aside tree is deleted; `false` while a background
        /// thread deletes it.
        deleted: bool,
    },
    /// A build holds, or may hold, the target: this names what holds it.
    Busy(String),
    /// `deps/` could not be counted, so nothing was moved.
    Unknown(String),
    /// The rename failed; the live directory is where it was.
    Failed(String),
    /// A rotated-aside tree could not be deleted; the next pass retries. The
    /// live directory is not involved.
    DeleteFailed {
        /// The aside tree still on disk.
        tree: PathBuf,
        /// Why it could not be deleted.
        error: String,
    },
}

impl Rotation {
    /// One line for a report: what happened to `target`.
    #[must_use]
    pub fn describe(&self, target: &Path) -> String {
        let profile = target.join(PROFILE);
        match self {
            Self::NotNeeded(DepsCount::Under(n)) => {
                format!("{}: {n} entries in deps, under the line", profile.display())
            }
            Self::NotNeeded(_) => format!("{}: no deps dir, nothing to rotate", profile.display()),
            Self::WouldRotate => format!(
                "{}: deps is over the line; would rename it aside (dry run)",
                profile.display()
            ),
            Self::Rotated { aside, deleted } => format!(
                "{}: deps was over the line; renamed aside to {} and {} (#10212)",
                profile.display(),
                aside.display(),
                if *deleted {
                    "deleted it"
                } else {
                    "deleting it in the background"
                }
            ),
            Self::Busy(holder) => format!(
                "{}: deps is over the line but NOT rotated — held by {holder}",
                profile.display()
            ),
            Self::Unknown(err) => format!(
                "{}: NOT rotated — deps could not be counted ({err})",
                profile.display()
            ),
            Self::Failed(err) => format!(
                "{}: rotation FAILED, the directory is untouched ({err})",
                profile.display()
            ),
            Self::DeleteFailed { tree, error } => format!(
                "{}: a rotated-aside tree could NOT be deleted ({error}); the next pass retries",
                tree.display()
            ),
        }
    }

    /// Whether this outcome is a failure an operator should see.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Unknown(_) | Self::Failed(_) | Self::DeleteFailed { .. }
        )
    }
}

/// Rename `<target>/debug` aside when its `deps/` holds `line` entries or more.
///
/// Why: see the module doc. The caller decides `line`: the lease uses
/// [`DEPS_ROTATE_ENTRIES`], the operator's repair the doctor's warn line.
/// What: bounded count; at the line, [`cargo_lock_held`] (fail-closed: an
/// unlistable target or unreadable lock reads busy), then every cargo lock of
/// the target is taken and held across the rename, so a cargo that starts
/// meanwhile blocks instead of writing into the moved tree. A dry run
/// (`apply == false`) stops before the locks. The rename is the only
/// synchronous step; the moved tree is deleted on a background thread.
/// The caller must already hold the slot's build lease when `target` is a slot.
/// Residual window: a cargo that opens `.cargo-lock` between the busy check and
/// the lock grab is caught by the grab; one that starts after the rename builds
/// cold into a fresh `debug/`.
/// Test: `a_target_over_the_line_is_rotated_aside`,
/// `a_held_cargo_lock_spares_the_rotation`, `a_dir_under_the_line_is_left_alone`,
/// `a_failed_rename_is_reported_and_leaves_the_live_dir`,
/// `an_uncountable_deps_dir_is_not_rotated`.
#[must_use]
pub fn rotate_if_over(target: &Path, line: usize, apply: bool) -> Rotation {
    rotate_with(target, line, apply, || Ok(()), discard)
}

/// [`rotate_if_over`] with two seams: `hold` takes any extra guard (a slot's
/// build lease) after the count and keeps it across the rename, and `mover`
/// does the rename, so a test can fail it.
pub(crate) fn rotate_with<G>(
    target: &Path,
    line: usize,
    apply: bool,
    hold: impl FnOnce() -> Result<G, String>,
    mover: impl FnOnce(&Path) -> std::io::Result<Discarded>,
) -> Rotation {
    let profile = target.join(PROFILE);
    match count_deps(&profile.join("deps"), line) {
        DepsCount::AtLeast(_) => {}
        DepsCount::Unreadable(err) => return Rotation::Unknown(err),
        other => return Rotation::NotNeeded(other),
    }
    // #10212 round 1: a leased slot is spared between its cargo commands too.
    let _guard = match hold() {
        Ok(guard) => guard,
        Err(holder) => return Rotation::Busy(holder),
    };
    // #10212: a hand-pointed or orphaned build is spared, lease or no lease.
    if cargo_lock_held(target) {
        return Rotation::Busy(format!("a cargo lock under {}", target.display()));
    }
    if !apply {
        return Rotation::WouldRotate;
    }
    // #10212: held until the rename is done, so no build starts mid-move.
    let _locks = match hold_idle_build_locks(target) {
        Ok(locks) => locks,
        Err(lock) => return Rotation::Busy(lock.display().to_string()),
    };
    match mover(&profile) {
        Ok(moved) => {
            tracing::info!(profile = %profile.display(), aside = %moved.path.display(), "rotated an oversized cargo profile aside (#10212)");
            Rotation::Rotated {
                aside: moved.path,
                deleted: false,
            }
        }
        Err(err) => {
            tracing::warn!(profile = %profile.display(), "could not rotate an oversized cargo profile aside (#10212): {err}");
            Rotation::Failed(err.to_string())
        }
    }
}

/// The lease's check, run in the held-lease window before the build spawns.
///
/// What: starts [`reclaim_aside`] for `target` on a background thread, then
/// [`rotate_if_over`] at [`DEPS_ROTATE_ENTRIES`].
/// Test: `a_target_over_the_line_is_rotated_aside` covers the rotation.
#[must_use]
pub fn rotate_before_build(target: &Path) -> Rotation {
    let dir = target.to_path_buf();
    // #10212: deleting an earlier run's aside tree never blocks this build.
    drop(
        std::thread::Builder::new()
            .name("deps-reclaim".to_string())
            .spawn(move || log_reclaim(&dir, &reclaim_aside(&dir, &|| false))),
    );
    rotate_if_over(target, DEPS_ROTATE_ENTRIES, true)
}

/// The line `tm build-lease` prints for a rotation check, or `None` when quiet.
///
/// What: quiet for a dir under the warn line or with no `deps/`; otherwise
/// [`Rotation::describe`], or a warn-line notice for a dir between the lines.
/// Test: `the_lease_is_quiet_under_the_warn_line`.
#[must_use]
pub fn lease_notice(target: &Path, rotation: &Rotation) -> Option<String> {
    match rotation {
        Rotation::NotNeeded(DepsCount::Under(n)) if *n >= DEPS_WARN_ENTRIES => Some(format!(
            "{}/deps holds {n} entries, over the {DEPS_WARN_ENTRIES} warn line; it is rotated \
             aside at {DEPS_ROTATE_ENTRIES} (#10212)",
            target.join(PROFILE).display()
        )),
        Rotation::NotNeeded(_) => None,
        other => Some(other.describe(target)),
    }
}

/// What one [`reclaim_aside`] pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reclaim {
    /// Aside trees deleted.
    pub removed: usize,
    /// Aside trees that could not be deleted, with the error; left for next time.
    pub failed: Vec<(PathBuf, String)>,
    /// The pass stopped at a cancel before every tree was tried.
    pub cancelled: bool,
}

/// Delete the aside trees in `target` whose deleting process is gone.
///
/// Why: [`discard`] deletes on a thread of the process that renamed; a
/// `tm build-lease` can exit before an 844k-entry tree is gone, so the next
/// pass finishes it (#10212).
/// What: lists `target` itself (a handful of profile entries, never `deps/`)
/// for `.slot-<n>.evicting.<pid>.<nanos>` names whose pid is dead. This
/// process's own and a live process's trees are left to them. `cancelled` is
/// polled before each tree.
/// Test: `a_dead_deleters_aside_tree_is_reclaimed`,
/// `a_failed_reclaim_is_reported_and_left`, `a_cancel_stops_the_reclaim_before_the_next_tree`.
#[must_use]
pub fn reclaim_aside(target: &Path, cancelled: &dyn Fn() -> bool) -> Reclaim {
    reclaim_with(target, cancelled, |tree: &Path| {
        std::fs::remove_dir_all(tree)
    })
}

/// [`reclaim_aside`] with the deletion as a seam.
pub(crate) fn reclaim_with(
    target: &Path,
    cancelled: &dyn Fn() -> bool,
    remove: impl Fn(&Path) -> std::io::Result<()>,
) -> Reclaim {
    let mut report = Reclaim::default();
    for tree in stale_aside(target) {
        // #10212 round 1: a shutdown stops before the next tree, as evict does.
        if cancelled() {
            report.cancelled = true;
            break;
        }
        match remove(&tree) {
            Ok(()) => report.removed += 1,
            Err(err) => report.failed.push((tree, err.to_string())),
        }
    }
    report
}

/// Whether `name` is an aside tree name: `.slot-<n>.evicting.<pid>.<nanos>`.
fn aside_pid(name: &str) -> Option<u32> {
    name.strip_prefix(".slot-")?
        .split_once(EVICTING_INFIX)?
        .1
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// The aside trees directly in `target` whose deleting process is gone.
///
/// What: aside names (what [`discard`] mints) whose pid is neither this
/// process nor a live one. Lists `target` only.
/// Test: `a_dead_deleters_aside_tree_is_reclaimed`,
/// `a_live_deleters_aside_tree_is_left_alone`.
#[must_use]
pub fn stale_aside(target: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(target) else {
        return Vec::new();
    };
    let mut trees: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(aside_pid)
                .is_some_and(|pid| {
                    pid != std::process::id() && !crate::core::process::is_process_alive(pid)
                })
        })
        .map(|entry| entry.path())
        .collect();
    trees.sort();
    trees
}

/// Log a [`Reclaim`]: failures at `warn`, removals at `info`.
pub fn log_reclaim(target: &Path, reclaim: &Reclaim) {
    for (tree, err) in &reclaim.failed {
        tracing::warn!(target_dir = %target.display(), tree = %tree.display(), "could not delete a rotated-aside cargo profile; the next pass retries (#10212): {err}");
    }
    if reclaim.removed > 0 {
        tracing::info!(target_dir = %target.display(), removed = reclaim.removed, "deleted rotated-aside cargo profiles (#10212)");
    }
}

/// Why a seed must not clone `src`, or `None` when it may.
///
/// Why: a slot is seeded by cloning the shared target, so one oversized shared
/// `deps/` was cloned into every new slot (#10212). The shared dir is not the
/// seed's to rotate (unleased builds use it, Q3a), so the seed starts cold.
/// What: `Some(detail)` when `<src>/debug/deps` is at or over `line`, or
/// cannot be counted — an unknown size is never cloned — or when `src` holds
/// an aside tree of any owner, which `cp -c -R` would clone whole.
/// Test: `seeding_does_not_clone_an_oversized_shared_deps`,
/// `an_uncountable_shared_deps_seeds_cold`,
/// `a_shared_target_holding_an_aside_tree_seeds_cold`.
#[must_use]
pub fn seed_refusal(src: &Path, line: usize) -> Option<String> {
    // #10212 round 1: a killed `tm doctor` leaves its aside tree in `src`.
    let aside = std::fs::read_dir(src).ok().and_then(|entries| {
        entries
            .flatten()
            .find(|e| e.file_name().to_str().and_then(aside_pid).is_some())
    });
    if let Some(entry) = aside {
        return Some(format!(
            "{} holds the rotated-aside tree {}, so it is not cloned (#10212); the slot \
             starts cold",
            src.display(),
            entry.path().display()
        ));
    }
    let deps = src.join(PROFILE).join("deps");
    match count_deps(&deps, line) {
        DepsCount::AtLeast(n) => Some(format!(
            "{} holds at least {n} entries, so it is not cloned (#10212); the slot starts cold",
            deps.display()
        )),
        DepsCount::Unreadable(err) => Some(format!(
            "{} could not be counted ({err}), so it is not cloned (#10212); the slot starts cold",
            deps.display()
        )),
        _ => None,
    }
}

#[cfg(test)]
#[path = "deps_rotate_tests.rs"]
pub(crate) mod tests;
