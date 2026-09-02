//! `tm doctor --fix-skills`: remove bundled skill copies stranded at the
//! PROJECT tier (#6586).
//!
//! Why: the `skill_project_tier` probe reports a stray and, being read-only,
//! leaves it there. Every project provisioned before #6602 holds a full bundled
//! set under `.claude/skills/` that no deploy writes any more and no deploy
//! refreshes, so each copy freezes at the text that shipped the day it landed
//! while the user-tier copy moves on. Reporting it forever is not a remedy.
//!
//! WHAT LICENSES THE DELETION. Every other repair in this crate refuses to
//! remove a file, and the reason is always the same: tm cannot prove it wrote
//! the thing it is about to destroy. Here it can. The tier's own
//! `.trusty-mpm-skills-manifest.json` is tm's deployment ledger, and a stem
//! recorded there whose bytes still match the recorded checksum is a tm
//! deployment and nothing else. That evidence, and only that evidence, is what
//! this module acts on:
//!
//! 1. **Absent from the ledger → REFUSED.** A bundled-named directory tm never
//!    recorded may be a project-custom skill the operator wrote under a bundled
//!    name, and that is real work. It is reported, never removed.
//! 2. **Recorded but hand-edited → REFUSED** unless `include_frozen`. The same
//!    rule [`super::skill_repair`] applies to an overwrite applies to a removal,
//!    for the same reason: the edit was deliberate.
//! 3. **Every removal is backed up first** under the run's timestamped
//!    `~/.trusty-mpm/backup-doctor-remediation-<ts>/` root, whole directory
//!    including `references/`, and the removal is CONFIRMED by re-reading disk.
//!
//! And one hard boundary: this module only ever touches
//! `<project>/.claude/skills`. The managed tier
//! ([`FrameworkPaths::skill_deploy_dir`]) is where the bundled roster is
//! SUPPOSED to live — removing a skill there would be the #6586 defect running
//! backwards — so a project tier that resolves onto it, or onto the operator's
//! `~/.claude/skills`, is refused rather than swept.
//!
//! Test: `project_tier_strays_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::ManifestError;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_manifest::{SkillManifest, SkillManifestSave, with_skill_manifest_lock};
use crate::core::skill_tiers::list_source_stems;
use crate::core::skill_unmanaged::{SKILL_ENTRY_POINT, bundled_skill_dirs};

/// The doctor check these steps repair.
const CHECK: &str = "skill_project_tier";

/// Sweep the project tier of bundled skill copies tm's ledger proves it wrote.
///
/// Why: see the module doc — this is the action half of the `skill_project_tier`
/// probe, and the ledger is what makes the deletion provable rather than
/// guessed.
/// What: for each directory under `<project_dir>/.claude/skills` whose stem the
/// bundled roster carries, emits one [`RepairStep`] — [`StepStatus::Refused`]
/// when the ledger does not record it or records a checksum its bytes no longer
/// match (and `include_frozen` is unset), [`StepStatus::Planned`] in
/// [`RepairMode::DryRun`], otherwise [`StepStatus::Applied`] after the directory
/// has been copied under `backup_root`, removed, confirmed gone, and dropped
/// from the ledger. Returns no steps at all when there is no project in scope
/// and no tier on disk. The whole load-modify-save runs under the tier's ledger
/// lock, so a sweep cannot race a concurrent deploy into publishing a manifest
/// missing that deploy's entries.
/// Test: `project_tier_strays_tests.rs`.
pub fn remove_project_tier_strays(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    include_frozen: bool,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let Some(project_dir) = project_dir else {
        return Vec::new();
    };
    let dir = project_dir.join(".claude").join("skills");
    if !dir.is_dir() {
        return Vec::new();
    }

    // #6586: the managed tier is where bundled skills BELONG. Sweeping it would
    // run this fix backwards, so a project tier that resolves onto it (or onto
    // the operator's home tier) is refused loudly rather than silently skipped.
    for reserved in [paths.skill_deploy_dir(), paths.claude_skills_dir()] {
        if dir == reserved {
            return vec![RepairStep {
                check: CHECK,
                path: dir,
                what: "remove stray bundled skill copies from the project tier".to_string(),
                status: StepStatus::Refused(
                    "this project's skill tier IS a tier bundled skills are deployed to — \
                     removing them here would undo the deploy, not repair it"
                        .to_string(),
                ),
            }];
        }
    }

    let bundled = list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    if bundled.is_empty() {
        // Mirrors the probe: an empty roster classifies nothing, and treating it
        // as "nothing is bundled" would condemn every skill in the tier at once.
        return vec![RepairStep {
            check: CHECK,
            path: dir,
            what: "remove stray bundled skill copies from the project tier".to_string(),
            status: StepStatus::Refused(
                "no bundled skill source found — cannot tell which project-tier skills are \
                 bundled duplicates (run `tm install` to populate it)"
                    .to_string(),
            ),
        }];
    }

    let locked = with_skill_manifest_lock::<_, ManifestError, _>(&dir, || {
        Ok(sweep_locked(
            &dir,
            &bundled,
            include_frozen,
            backup_root,
            mode,
        ))
    });
    match locked {
        Ok(steps) => steps,
        Err(e) => vec![RepairStep {
            check: CHECK,
            path: dir,
            what: "remove stray bundled skill copies from the project tier".to_string(),
            status: StepStatus::Failed(format!(
                "could not lock the deploy manifest: {e} — refusing to sweep this tier \
                 unserialised"
            )),
        }],
    }
}

/// Sweep one project tier, holding that tier's skill ledger lock.
///
/// Why: split out so the locked critical section is one expression — every
/// manifest load, directory removal, and manifest save happens with the lock
/// held. Never call it directly.
/// What: the classify/remove/unrecord pipeline documented on
/// [`remove_project_tier_strays`], for one directory.
/// Test: exercised through [`remove_project_tier_strays`].
fn sweep_locked(
    dir: &Path,
    bundled: &std::collections::BTreeSet<String>,
    include_frozen: bool,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    // #5626: an unreadable ledger has no answer to "did tm write this?", and the
    // empty default answers "no" for every file — which would refuse the whole
    // sweep on a tier whose ledger is merely unreadable, and, worse, publish an
    // ownership document nobody read if the save path were ever reached.
    let mut manifest = match SkillManifest::load(dir) {
        Ok(m) => m,
        Err(e) => {
            return vec![RepairStep {
                check: CHECK,
                path: dir.to_path_buf(),
                what: "remove stray bundled skill copies from the project tier".to_string(),
                status: StepStatus::Refused(format!(
                    "{e} — refusing to touch this tier; its ownership ledger is what proves \
                     which copies tm wrote"
                )),
            }];
        }
    };
    // #4881: the snapshot the merging save replays this run's delta against.
    let base = manifest.clone();
    let mut dirty = false;
    let mut steps = Vec::new();

    for skill in bundled_skill_dirs(dir, bundled) {
        let what = format!(
            "remove the stray bundled copy of `{}` from the project tier",
            skill.stem
        );
        let status = if !manifest.is_managed(&skill.stem) {
            StepStatus::Refused(
                "absent from this tier's deploy ledger — tm cannot prove it wrote this copy, \
                 and a bundled-named directory it did not write may be a project-custom skill"
                    .to_string(),
            )
        } else {
            match hand_edited(&skill.dir, &skill.stem, &manifest) {
                Err(why) => StepStatus::Failed(why),
                Ok(true) if !include_frozen => StepStatus::Refused(
                    "hand-edited after deployment — pass `--include-frozen` to remove it \
                     anyway, backing it up first"
                        .to_string(),
                ),
                Ok(_) if mode == RepairMode::DryRun => StepStatus::Planned,
                Ok(_) => match back_up_and_remove(&skill.dir, &skill.stem, backup_root) {
                    Ok(backup) => {
                        unrecord(&mut manifest, &skill.stem);
                        dirty = true;
                        StepStatus::Applied {
                            backup: Some(backup),
                        }
                    }
                    Err(why) => StepStatus::Failed(why),
                },
            }
        };
        steps.push(RepairStep {
            check: CHECK,
            path: skill.dir,
            what,
            status,
        });
    }

    // The files are already gone, so this save must publish or the ledger keeps
    // claiming tm owns copies that no longer exist — which the next deploy would
    // read as skills it already deployed and never write again.
    if dirty {
        debug_assert_eq!(
            mode,
            RepairMode::Apply,
            "a dry run must never mark the ledger dirty — it removed nothing to record"
        );
        match manifest.save_merging(dir, &base) {
            Ok(SkillManifestSave::Written | SkillManifestSave::OverwroteUnreadable) => {}
            Ok(SkillManifestSave::Merged) => tracing::warn!(
                tier = %dir.display(),
                "the skill manifest changed during the project-tier sweep — a writer bypassed \
                 the ledger lock; its entries were merged rather than dropped"
            ),
            Err(e) => steps.push(RepairStep {
                check: CHECK,
                path: dir.to_path_buf(),
                what: "drop the removed strays from the deploy ledger".to_string(),
                status: StepStatus::Failed(format!("could not save the deploy manifest: {e}")),
            }),
        }
    }
    steps
}

/// Has this deployed copy been edited since tm wrote it?
///
/// Why: a hand-edit is the operator's work even under a bundled name, so it gets
/// the same `--include-frozen` gate an overwrite gets.
/// What: `Ok(true)` when the entry point's bytes no longer match the checksum
/// the ledger recorded. An unreadable entry point is `Err` — unverifiable is
/// never a licence to delete.
/// Test: `a_hand_edited_stray_is_refused_without_include_frozen`.
fn hand_edited(dir: &Path, stem: &str, manifest: &SkillManifest) -> Result<bool, String> {
    let entry_point = dir.join(SKILL_ENTRY_POINT);
    let content = std::fs::read_to_string(&entry_point)
        .map_err(|e| format!("could not read {}: {e}", entry_point.display()))?;
    Ok(!manifest.checksum_matches(stem, &content))
}

/// Copy the whole skill directory under `backup_root`, remove it, confirm.
///
/// Why: constraint 3 of [`super::skill_repair`], applied to a removal — a
/// deletion that reports success from `remove_dir_all`'s return value has not
/// verified anything. The backup is the whole subtree because a skill's
/// `references/*.md` are as much the operator's recoverable state as its entry
/// point.
/// What: copies `dir` to `<backup_root>/project/<stem>`, removes `dir`, then
/// re-checks that the path is gone. Returns the backup path.
/// Test: `a_removed_stray_is_backed_up_whole`.
fn back_up_and_remove(dir: &Path, stem: &str, backup_root: &Path) -> Result<PathBuf, String> {
    let dest = backup_root.join("project").join(stem);
    copy_tree(dir, &dest).map_err(|e| {
        format!(
            "could not back up {} to {}: {e}",
            dir.display(),
            dest.display()
        )
    })?;
    std::fs::remove_dir_all(dir).map_err(|e| format!("could not remove {}: {e}", dir.display()))?;
    if dir.exists() {
        return Err(format!(
            "removed {} but it is still present — the repair did NOT take",
            dir.display()
        ));
    }
    Ok(dest)
}

/// Recursively copy `src` into `dest`, creating parents as needed.
///
/// Why: `std::fs::rename` would be cheaper but fails across filesystems, and a
/// project on an external volume with `$HOME` on the internal disk is the
/// ordinary case on this machine. A copy works everywhere.
/// What: creates `dest`, then copies each entry — files with `std::fs::copy`,
/// directories by recursion. Symlinks are followed by `copy`, which is correct
/// for a backup: the bytes are what must survive.
/// Test: `a_removed_stray_is_backed_up_whole`.
fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Drop every ledger key belonging to `stem`.
///
/// Why: the deployer keys a skill's entry point by the bare stem and each
/// reference file by `<stem>/references/<file>`, so removing only the stem would
/// leave the ledger claiming tm owns files under a directory that no longer
/// exists.
/// What: retains every key that is neither `stem` nor prefixed `<stem>/`.
/// Test: `a_managed_stray_is_removed_and_a_custom_skill_is_kept`.
fn unrecord(manifest: &mut SkillManifest, stem: &str) {
    let prefix = format!("{stem}/");
    manifest
        .managed
        .retain(|key, _| key != stem && !key.starts_with(&prefix));
}

#[cfg(test)]
#[path = "project_tier_strays_tests.rs"]
mod tests;
