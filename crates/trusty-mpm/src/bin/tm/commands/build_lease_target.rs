//! Which target directory a leased build runs in (split from `build_lease.rs`
//! for the SLOC cap, #10215).
//!
//! Why: the lease's slot plan ([`TargetPlan`]) becomes a real directory only
//! once a slot is held; seeding it, guarding it against another checkout's
//! fingerprints and naming it to the agent belong together.
//! What: [`target_dir`] seeds and returns the slot directory, [`announce_slot`]
//! names it on stderr.
//! Test: `tests/tm_build_lease.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use trusty_mpm::core::build_lease::stale_guard::{
    Invalidation, checkout_root, invalidate_if_checkout_changed, workspace_packages,
};
use trusty_mpm::core::build_lease::target_dir::TargetPlan;
use trusty_mpm::core::builder_slot_pool::SlotPool;

/// Name the slot directory that replaced a shared `CARGO_TARGET_DIR`.
///
/// Why: the build gets the slot through its own environment, but the agent
/// that ran it still holds the shared `CARGO_TARGET_DIR` from `.envrc`, or a
/// slot path a brief named. Its next `$CARGO_TARGET_DIR/debug/<bin>` would run
/// another worktree's binary (#8261 round 5).
/// What: one stderr line, `slot N — CARGO_TARGET_DIR=<dir>`, only when the
/// plan replaced the directory; a kept or unset one prints nothing. Names the
/// directory as exclusive only while THIS lease holds it (critic MEDIUM,
/// round 6): another checkout can win the same slot once this build's flock
/// releases and rebuild `debug/<bin>` there. #8831: it restates the
/// `CLAUDE.md` rule's one exception, so the notice and the rule never read as
/// two contradictory instructions.
/// #10215: an unset `CARGO_TARGET_DIR` replaced in a linked worktree is named
/// the same way.
/// Test: `a_subagents_leased_build_runs_in_its_slot_directory`,
/// `an_unset_target_dir_in_a_linked_worktree_builds_in_a_slot_10215`.
pub(super) fn announce_slot(plan: &TargetPlan, slot: u32, target: Option<&str>) {
    let replaced = matches!(plan, TargetPlan::Slot { .. } | TargetPlan::UnsetSlot { .. });
    if let (true, Some(dir)) = (replaced, target) {
        eprintln!(
            "tm build-lease: slot {slot} — CARGO_TARGET_DIR={dir} for this build; its \
             binaries are under that directory, not the inherited one, and are exclusively \
             this build's only while the lease is held — once it releases, another checkout \
             can win the same slot and rebuild debug/<bin> there (#8261). Never override \
             CARGO_TARGET_DIR EXCEPT with this granted slot: the lease sets it for this build, \
             so the command needs no override of its own."
        );
    }
}

/// The `CARGO_TARGET_DIR` for this build, seeding the slot directory if new.
///
/// What: `Ok(None)` leaves cargo's own choice; `Ok(Some(dir))` sets `dir`.
/// #9239: the seed is bounded by `seed_wait` (the lease wait); past it the
/// slot starts cold. A slot is handed out only when its seed has committed.
/// `Err` when the build would otherwise run in a SHARED target directory
/// because its slot directory cannot be made or its stale fingerprints cannot
/// be cleared — the caller refuses instead (#8261, owner ruling 2026-09-21: an
/// unsafe target never admits). #10215: an [`TargetPlan::UnsetSlot`] whose
/// slot cannot be used falls back to cargo's own directory with a warning,
/// since nothing shared is at stake.
/// Test: `an_unusable_slot_directory_refuses_instead_of_sharing`,
/// `a_busy_orphan_slot_is_not_reused` in `tests/tm_build_lease.rs`,
/// `an_unset_target_dir_in_a_linked_worktree_builds_in_a_slot_10215`.
pub(super) fn target_dir(
    plan: &TargetPlan,
    slot: u32,
    cwd: &Path,
    seed_wait: Duration,
) -> Result<Option<String>, String> {
    let (pool, clone_from) = match plan {
        TargetPlan::Keep(dir) => return Ok(Some(dir.clone())),
        TargetPlan::Slot { pool, clone_from } => (pool, clone_from),
        TargetPlan::UnsetSlot { pool, clone_from } => {
            // #10215: nothing was inherited, so no shared-directory clause.
            return Ok(slot_dir(pool, clone_from, slot, cwd, seed_wait, "")
                .inspect_err(|why| {
                    eprintln!(
                        "tm build-lease: WARNING no CARGO_TARGET_DIR was set and slot {slot} is \
                         unusable, so this build runs in cargo's own target directory (#10215) \
                         — {why}"
                    );
                })
                .ok());
        }
        TargetPlan::Refuse(why) => return Err(why.clone()),
        _ => return Ok(None),
    };
    slot_dir(pool, clone_from, slot, cwd, seed_wait, SHARED_CLAUSE).map(Some)
}

/// Appended to a seed failure when the build inherited a SHARED directory.
const SHARED_CLAUSE: &str = ", and the inherited CARGO_TARGET_DIR is a shared target directory, \
                             which concurrent worktrees overwrite. Repair the slot pool root \
                             (`builders.slot_pool_root`, see `tm doctor`) or set a private \
                             CARGO_TARGET_DIR for this build";

/// Seed slot `slot` of `pool` and return its directory.
///
/// What: the seed and stale-fingerprint steps of [`target_dir`]; `Err` names
/// why the slot directory cannot be used, a seed failure followed by
/// `seed_clause`.
fn slot_dir(
    pool: &SlotPool,
    clone_from: &Option<PathBuf>,
    slot: u32,
    cwd: &Path,
    seed_wait: Duration,
    seed_clause: &str,
) -> Result<String, String> {
    let path = pool.slot_path(slot);
    if pool.committed(slot).is_none() {
        eprintln!(
            "tm build-lease: seeding slot {slot} from the shared target directory, bounded at \
             {}s; past that it starts cold (#9239). `tm build-lease status` shows the seed.",
            seed_wait.as_secs()
        );
    }
    let seeded =
        tokio::task::block_in_place(|| pool.seed_within(slot, clone_from.as_deref(), seed_wait));
    let dir = seeded.map(|(dir, _)| dir).map_err(|err| {
        format!(
            "slot directory {} is unusable ({err}){seed_clause}",
            path.display()
        )
    })?;
    // #9239: never a staging tree, never an uncommitted seed.
    if pool.committed(slot).as_ref() != Some(&dir) {
        return Err(format!(
            "slot directory {} has no committed seed, so no build runs in it (#9239); \
             `tm build-lease status` shows the slot",
            dir.display()
        ));
    }
    guard_against_stale_builds(&dir, cwd)?;
    Ok(dir.display().to_string())
}

/// Clear the slot's workspace fingerprints when it last built another checkout.
///
/// Why: see `trusty_mpm::core::build_lease::stale_guard` — without this a
/// slot shared in turn by two worktrees can report the second one "Fresh" and
/// hand back the first one's binary. A fingerprint that cannot be cleared is
/// an `Err`: the slot is not used (#8261 round 3).
fn guard_against_stale_builds(slot_dir: &Path, cwd: &Path) -> Result<(), String> {
    let root = checkout_root(cwd);
    let result = tokio::task::block_in_place(|| {
        invalidate_if_checkout_changed(slot_dir, &root, || workspace_packages(&root))
    });
    match result {
        Ok(Invalidation::Cleared {
            previous,
            removed,
            all,
        }) => eprintln!(
            "tm build-lease: slot {} last built {}; cleared {removed} {} fingerprint(s) so this \
             checkout's crates rebuild instead of reading as Fresh (#8261).",
            slot_dir.display(),
            previous.as_deref().unwrap_or("an unrecorded checkout"),
            if all {
                "(ALL — the package list was unreadable)"
            } else {
                "workspace-package"
            },
        ),
        Ok(_) => {}
        Err(err) => return Err(err),
    }
    Ok(())
}
