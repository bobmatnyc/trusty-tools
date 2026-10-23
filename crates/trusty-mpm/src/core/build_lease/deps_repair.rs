//! The operator's `tm doctor --rotate-deps` pass and the daemon's reclaim
//! of rotated-aside cargo profiles (#10212).
//!
//! Why: builds outside the lease grow the shared target, and only an explicit
//! operator act may rotate it (Architect ruling Q3a); a rotated-aside tree a
//! dead process left must still be deleted.
//! What: [`rotate_all`] rotates every shared target and slot over the line,
//! holding a slot's build lease across the rename and waiting for the
//! deletion; [`reclaim_everywhere`] deletes dead deleters' aside trees, polling
//! a cancel. The per-target mechanics are `deps_rotate`.
//! Test: `deps_repair_tests.rs`.

use std::path::{Path, PathBuf};

use super::deps_rotate::{Reclaim, Rotation, reclaim_aside, rotate_with};
use super::evict::list_pool_slots;
use super::orphan::Leftover;
use super::slots::{SlotDir, SlotGuard};
use crate::core::builder_slot_pool::staging::discard;

/// The `<owner>/<repo>` target directories under the shared root.
///
/// What: two directory levels, the layout `default_cargo_target_dir_at`
/// writes. A missing root is no targets; an owner that cannot be listed is
/// an error, so a hidden target is never reported as healthy.
/// Test: `the_repair_rotates_an_over_line_target`.
///
/// # Errors
///
/// The root or an owner directory cannot be listed.
pub fn shared_targets(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let owners = match std::fs::read_dir(root) {
        Ok(owners) => owners,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut targets = Vec::new();
    for owner in owners {
        let owner = owner?.path();
        if !owner.is_dir() {
            continue;
        }
        for repo in std::fs::read_dir(&owner)? {
            let repo = repo?.path();
            if repo.is_dir() {
                targets.push(repo);
            }
        }
    }
    targets.sort();
    Ok(targets)
}

/// Take slot `index`'s build lease, or name what holds the slot.
///
/// Why (#10212 round 1): cargo drops `.cargo-lock` between the commands of one
/// leased run (`cargo build && cargo test`), so the lease, not the cargo lock,
/// says the slot is idle. The same three refusals as `evict::evict_one`.
/// What: `Err` for a held lease, an orphaned build still running from a dead
/// holder, or a lease or record whose state cannot be read.
/// Test: `the_repair_spares_a_slot_whose_lease_is_held`.
fn hold_slot_lease(store: Result<&SlotDir, &str>, index: u32) -> Result<SlotGuard, String> {
    let store =
        store.map_err(|err| format!("slot {index}'s build lease (store unusable: {err})"))?;
    let guard = match store.try_acquire(index) {
        Ok(Some(guard)) => guard,
        Ok(None) => return Err(format!("slot {index}'s build lease")),
        Err(err) => return Err(format!("slot {index}'s build lease (state unknown: {err})")),
    };
    match guard.leftover() {
        Leftover::Clear => Ok(guard),
        Leftover::Running(record) => {
            // The record is what keeps the slot held; it must survive the probe.
            guard.release_keeping_record();
            let pid = record.child_pid.unwrap_or(record.pid);
            Err(format!("an orphaned build of slot {index}, pid {pid}"))
        }
        Leftover::Unknown(err) => {
            guard.release_keeping_record();
            Err(format!(
                "slot {index}'s lease record (state unknown: {err})"
            ))
        }
    }
}

/// Rotate `target` and, when it moved, wait for the aside tree's deletion.
///
/// Why (#10212 round 1): `tm doctor` exits right after the pass, which kills a
/// detached deleting thread and leaves the tree for the next seed to clone.
/// What: [`rotate_with`] through [`discard`], then joins the remover; a failed
/// or missing remover is [`Rotation::DeleteFailed`] naming the tree.
fn rotate_and_wait<G>(
    target: &Path,
    line: usize,
    apply: bool,
    hold: impl FnOnce() -> Result<G, String>,
) -> Rotation {
    let mut remover = None;
    let rotation = rotate_with(target, line, apply, hold, |profile| {
        discard(profile).map(|mut moved| {
            remover = moved.remover.take();
            moved
        })
    });
    let Rotation::Rotated { aside, .. } = rotation else {
        return rotation;
    };
    let deleted = match remover.map(std::thread::JoinHandle::join) {
        Some(Ok(Ok(()))) => Ok(()),
        Some(Ok(Err(err))) => Err(err.to_string()),
        Some(Err(_)) => Err("the deleting thread panicked".to_string()),
        None => std::fs::remove_dir_all(&aside).map_err(|err| err.to_string()),
    };
    match deleted {
        Ok(()) => Rotation::Rotated {
            aside,
            deleted: true,
        },
        Err(error) => Rotation::DeleteFailed { tree: aside, error },
    }
}

/// The operator's repair: check and rotate every shared target and slot.
///
/// Why (#10212, Q3a): builds outside the lease grow the shared dir, and only an
/// explicit operator act may rotate it. The doctor row warns at `line`, so the
/// repair acts on exactly the directories the operator saw warned.
/// What: one rotation per target. A slot is rotated only while this pass holds
/// its build lease from `store`; a shared target has no lease, so only its
/// cargo locks guard it. A root that cannot be listed is one
/// [`Rotation::Unknown`] row. A dry run unless `apply`; an applied rotation
/// waits for its deletion. Then [`reclaim_aside`] on each target, whose
/// failures are returned too.
/// Test: `the_repair_rotates_an_over_line_target`,
/// `the_repair_refuses_a_target_whose_cargo_lock_is_held`,
/// `the_repair_spares_a_slot_whose_lease_is_held`,
/// `the_repair_reports_an_aside_tree_it_cannot_delete`.
#[must_use]
pub fn rotate_all(
    shared_root: &Path,
    pool_root: &Path,
    store: Result<&SlotDir, &str>,
    line: usize,
    apply: bool,
) -> Vec<(PathBuf, Rotation)> {
    let mut rows = Vec::new();
    let mut targets: Vec<(PathBuf, Option<u32>)> = Vec::new();
    match shared_targets(shared_root) {
        Ok(found) => targets.extend(found.into_iter().map(|t| (t, None))),
        Err(err) => rows.push((
            shared_root.to_path_buf(),
            Rotation::Unknown(err.to_string()),
        )),
    }
    match list_pool_slots(pool_root) {
        Ok(slots) => targets.extend(slots.into_iter().map(|s| (s.path, Some(s.index)))),
        Err(err) => rows.push((pool_root.to_path_buf(), Rotation::Unknown(err.to_string()))),
    }
    for (target, index) in targets {
        let rotation = match index {
            Some(index) => rotate_and_wait(&target, line, apply, || {
                hold_slot_lease(store, index).map(Some)
            }),
            None => rotate_and_wait(&target, line, apply, || Ok(None::<SlotGuard>)),
        };
        if apply {
            for (tree, error) in reclaim_aside(&target, &|| false).failed {
                rows.push((target.clone(), Rotation::DeleteFailed { tree, error }));
            }
        }
        rows.push((target, rotation));
    }
    rows
}

/// The daemon tick's half: delete dead deleters' aside trees in every slot
/// and shared target (#10212), stopping at a cancel before the next tree.
///
/// Test: `a_cancel_stops_the_reclaim_before_the_next_tree` covers the poll.
#[must_use]
pub fn reclaim_everywhere(
    shared_root: &Path,
    pool_root: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Reclaim {
    let mut all = Reclaim::default();
    let mut targets = shared_targets(shared_root).unwrap_or_default();
    targets.extend(
        list_pool_slots(pool_root)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.path),
    );
    for target in targets {
        let one = reclaim_aside(&target, cancelled);
        all.removed += one.removed;
        all.failed.extend(one.failed);
        if one.cancelled {
            all.cancelled = true;
            break;
        }
    }
    all
}

#[cfg(test)]
#[path = "deps_repair_tests.rs"]
mod tests;
