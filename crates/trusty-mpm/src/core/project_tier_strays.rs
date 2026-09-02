//! `tm doctor --fix-skills --yes`: remove bundled skill copies stranded at the
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
//! 2. **Anything in the subtree tm did not deploy, or deployed and someone
//!    changed → REFUSED.** The unit of removal is `remove_dir_all` on a
//!    DIRECTORY, so the question is never "does `SKILL.md` still match" but "is
//!    every file under here one tm wrote and nobody has touched".
//!    [`skill_removal_verdict`] is that question, shared with the #5224
//!    retirement sweep and `update_check::apply::prune_guard`, so the three
//!    cannot drift on what counts as safe to delete. `--include-frozen` does
//!    NOT override it WITHIN A RUN: that flag promotes an OVERWRITE of one file,
//!    which is recoverable from the backup of that same file, and a
//!    whole-directory deletion is a different act. Across runs the protection
//!    is weaker and the operator should know it — a `--fix-skills
//!    --include-frozen` overwrites the hand-edited file and re-stamps its ledger
//!    checksum, so the subtree it was protecting becomes removable on the NEXT
//!    sweep. The backup of that overwrite is what the edit survives in.
//! 3. **Every removal is backed up first** under the run's timestamped
//!    `~/.trusty-mpm/backup-doctor-remediation-<ts>/` root, whole directory
//!    including `references/`, and the removal is CONFIRMED by re-reading disk.
//!
//! And one hard boundary: this module only ever touches
//! `<project>/.claude/skills`. The managed tier
//! ([`FrameworkPaths::skill_deploy_dir`]) is where the bundled roster is
//! SUPPOSED to live — removing a skill there would be the #6586 defect running
//! backwards — so a project tier that RESOLVES onto it, or onto the operator's
//! `~/.claude/skills`, is refused rather than swept. The comparison is made on
//! canonicalised paths and a symlinked tier is refused outright, because a
//! lexical `PathBuf ==` sees `<project>/.claude/skills` and `~/.claude/skills`
//! as different paths even when the first is a symlink to the second.
//!
//! Test: `project_tier_strays_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::ManifestError;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::project_skill_tier;
use crate::core::skill_manifest::{SkillManifest, SkillManifestSave, with_skill_manifest_lock};
use crate::core::skill_retire::{SkillRemoval, skill_removal_verdict};
use crate::core::skill_tiers::list_source_stems;
use crate::core::skill_unmanaged::bundled_skill_dirs;

/// The doctor check these steps repair.
const CHECK: &str = "skill_project_tier";

/// The one-line summary every tier-wide refusal carries.
const SWEEP_WHAT: &str = "remove stray bundled skill copies from the project tier";

/// Sweep the project tier of bundled skill copies tm's ledger proves it wrote.
///
/// Why: see the module doc — this is the action half of the `skill_project_tier`
/// probe, and the ledger is what makes the deletion provable rather than
/// guessed.
/// What: for each directory under `<project_dir>/.claude/skills` whose stem the
/// bundled roster carries, emits one [`RepairStep`] — [`StepStatus::Refused`]
/// when the ledger does not record it or [`skill_removal_verdict`] keeps it,
/// [`StepStatus::Planned`] in [`RepairMode::DryRun`], otherwise
/// [`StepStatus::Applied`] after the directory has been copied under
/// `backup_root`, removed, confirmed gone, and dropped from the ledger. A
/// bundled-named entry that is not a skill directory at all is refused rather
/// than skipped silently. Returns no steps at all when there is no project in
/// scope and no tier on disk. The whole load-modify-save runs under the tier's
/// ledger lock, so a sweep cannot race a concurrent deploy into publishing a
/// manifest missing that deploy's entries.
/// Test: `project_tier_strays_tests.rs`.
pub fn remove_project_tier_strays(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let Some(project_dir) = project_dir else {
        return Vec::new();
    };
    let dir = project_skill_tier(project_dir);
    match tier_shape(&dir) {
        TierShape::Nothing => return Vec::new(),
        TierShape::Refuse(why) => return vec![refusal(dir, why)],
        TierShape::Sweepable => {}
    }
    if let Some(why) = resolves_onto_a_reserved_tier(paths, &dir) {
        return vec![refusal(dir, why)];
    }

    let bundled = list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    if bundled.is_empty() {
        // Mirrors the probe: an empty roster classifies nothing, and treating it
        // as "nothing is bundled" would condemn every skill in the tier at once.
        return vec![refusal(
            dir,
            "no bundled skill source found — cannot tell which project-tier skills are \
             bundled duplicates (run `tm install` to populate it)"
                .to_string(),
        )];
    }

    let locked = with_skill_manifest_lock::<_, ManifestError, _>(&dir, || {
        Ok(sweep_locked(&dir, &bundled, backup_root, mode))
    });
    match locked {
        Ok(steps) => steps,
        Err(e) => vec![RepairStep {
            check: CHECK,
            path: dir,
            what: SWEEP_WHAT.to_string(),
            status: StepStatus::Failed(format!(
                "could not lock the deploy manifest: {e} — refusing to sweep this tier \
                 unserialised"
            )),
        }],
    }
}

/// One tier-wide [`StepStatus::Refused`] step.
fn refusal(dir: PathBuf, why: String) -> RepairStep {
    RepairStep {
        check: CHECK,
        path: dir,
        what: SWEEP_WHAT.to_string(),
        status: StepStatus::Refused(why),
    }
}

/// What the tier path on disk is, before anything is swept.
enum TierShape {
    /// Absent, or not a directory — there is no stray here to remove.
    Nothing,
    /// Present but must not be swept; the string says why.
    Refuse(String),
    /// A real directory this module may walk.
    Sweepable,
}

/// Classify `<project>/.claude/skills` before the sweep touches it.
///
/// Why (#6586 critic HIGH): the guard used `Path::is_dir`, which FOLLOWS
/// symlinks — a project whose `.claude/skills` is a symlink to the operator's
/// `~/.claude/skills` passed it, and the sweep would then have removed the
/// operator's live home-tier skills through the link. `remove_dir_all` on a
/// path reached through a symlinked tier is not a repair of that project.
/// What: [`TierShape::Nothing`] when the path is absent or is not a directory,
/// [`TierShape::Refuse`] when it is a symlink or cannot be stat-ed, otherwise
/// [`TierShape::Sweepable`]. Uses `symlink_metadata`, which does not follow.
/// Test: `a_symlinked_project_tier_is_refused`.
fn tier_shape(dir: &Path) -> TierShape {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TierShape::Nothing,
        Err(e) => TierShape::Refuse(format!(
            "{} could not be inspected, so tm cannot tell what it would be deleting: {e}",
            dir.display()
        )),
        Ok(meta) if meta.is_symlink() => TierShape::Refuse(format!(
            "{} is a symlink — sweeping it would delete whatever it points at, which is \
             not this project's tier",
            dir.display()
        )),
        Ok(meta) if !meta.is_dir() => TierShape::Nothing,
        Ok(_) => TierShape::Sweepable,
    }
}

/// Does this project tier resolve onto a tier bundled skills DEPLOY to?
///
/// Why: removing a skill from the managed deploy dir or the operator's
/// `~/.claude/skills` would run the #6586 fix backwards. Comparing the two
/// lexically answered a different question — whether the paths are spelled the
/// same — so an ancestor symlink walked straight past it.
/// What: `Some(reason)` when `dir` canonicalises onto
/// [`FrameworkPaths::skill_deploy_dir`] or
/// [`FrameworkPaths::claude_skills_dir`], and also when `dir` itself cannot be
/// canonicalised — an unresolvable path is never a licence to delete. A
/// reserved tier that does not exist falls back to its lexical path, which still
/// catches the spelled-identical case.
/// Test: `a_tier_bundled_skills_deploy_to_is_never_swept`,
/// `a_tier_resolving_onto_the_managed_deploy_dir_is_refused`.
fn resolves_onto_a_reserved_tier(paths: &FrameworkPaths, dir: &Path) -> Option<String> {
    let real = match std::fs::canonicalize(dir) {
        Ok(p) => p,
        Err(e) => {
            return Some(format!(
                "{} could not be resolved to a real path, so tm cannot prove it is not a tier \
                 bundled skills deploy to: {e}",
                dir.display()
            ));
        }
    };
    for reserved in [paths.skill_deploy_dir(), paths.claude_skills_dir()] {
        let real_reserved = std::fs::canonicalize(&reserved).unwrap_or(reserved);
        if real == real_reserved {
            return Some(format!(
                "this project's skill tier resolves onto {} — a tier bundled skills are \
                 deployed to; removing them there would undo the deploy, not repair it",
                real_reserved.display()
            ));
        }
    }
    None
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
            return vec![refusal(
                dir.to_path_buf(),
                format!(
                    "{e} — refusing to touch this tier; its ownership ledger is what proves \
                     which copies tm wrote"
                ),
            )];
        }
    };
    // #6586 critic MEDIUM: `bundled_skill_dirs` and `unclassifiable_entries`
    // both treat an unreadable directory as an EMPTY one, so a tier that exists,
    // permits the ledger lock, and refuses `read_dir` — mode `0o300`, say —
    // produced no steps at all and read as a clean tier. The probe reports the
    // same undetermined state (`an_unreadable_project_tier_is_unknown_not_ok`),
    // so the repair must not contradict it.
    if let Err(e) = std::fs::read_dir(dir) {
        return vec![refusal(
            dir.to_path_buf(),
            format!(
                "{} could not be listed, so tm cannot tell which copies are in it: {e}",
                dir.display()
            ),
        )];
    }

    // #4881: the snapshot the merging save replays this run's delta against.
    let base = manifest.clone();
    let mut dirty = false;

    let skills = bundled_skill_dirs(dir, bundled);
    let classified: std::collections::BTreeSet<&str> =
        skills.iter().map(|s| s.stem.as_str()).collect();
    let mut steps = unclassifiable_entries(dir, bundled, &classified);

    for skill in &skills {
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
            // #6586 critic HIGH: the whole DIRECTORY is deleted, so the whole
            // subtree must be verified — checksumming `SKILL.md` alone would
            // take an operator's `references/our-notes.md` with it.
            match skill_removal_verdict(&manifest, dir, &skill.stem) {
                SkillRemoval::Kept(why) => StepStatus::Refused(format!(
                    "{why} — the whole directory would be deleted, so tm removes one only when \
                     every file under it is one tm deployed and nobody has changed"
                )),
                SkillRemoval::Removable if mode == RepairMode::DryRun => StepStatus::Planned,
                SkillRemoval::Removable => {
                    match back_up_and_remove(&skill.dir, &skill.stem, backup_root) {
                        Ok(backup) => {
                            unrecord(&mut manifest, &skill.stem);
                            dirty = true;
                            StepStatus::Applied {
                                backup: Some(backup),
                            }
                        }
                        Err(why) => StepStatus::Failed(why),
                    }
                }
            }
        };
        steps.push(RepairStep {
            check: CHECK,
            path: skill.dir.clone(),
            what,
            status,
        });
    }
    steps.sort_by(|a, b| a.path.cmp(&b.path));

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

/// Every bundled-named entry of `dir` that [`bundled_skill_dirs`] cannot
/// classify, by path.
///
/// Why (#6586 critic): `bundled_skill_dirs` silently drops an entry that is a
/// symlink, a plain file, or a directory with no `SKILL.md`. Dropping it from
/// the SCAN is right — none of those is a deployed skill the sweep can verify —
/// but dropping it from the REPORT told the operator the tier was clean when
/// something bundled-named was sitting in it. The `skill_project_tier` PROBE
/// counted the classified set only, so it said "holds no bundled skill" about
/// the same tier the repair then reported a refusal for. One finder, so the
/// check and the repair count the same entries.
/// What: the path of each entry of `dir` whose file name the bundled roster
/// carries and which `classified` does not already cover, sorted. An unreadable
/// `dir` yields none — every caller probes `read_dir` itself first and reports
/// that state rather than an empty tier.
/// Test: `a_bundled_named_entry_that_is_not_a_skill_directory_is_refused`,
/// `an_unclassifiable_bundled_entry_is_counted_by_the_check`.
pub fn unclassifiable_bundled_entries(
    dir: &Path,
    bundled: &std::collections::BTreeSet<String>,
    classified: &std::collections::BTreeSet<&str>,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (bundled.contains(&name) && !classified.contains(name.as_str())).then(|| entry.path())
        })
        .collect();
    found.sort();
    found
}

/// [`unclassifiable_bundled_entries`], as refusal steps.
///
/// Why: the sweep reports one line per entry it declined to act on; the probe
/// only needs the count. The shared finder answers both.
/// What: one [`StepStatus::Refused`] step per entry.
/// Test: `a_bundled_named_entry_that_is_not_a_skill_directory_is_refused`.
fn unclassifiable_entries(
    dir: &Path,
    bundled: &std::collections::BTreeSet<String>,
    classified: &std::collections::BTreeSet<&str>,
) -> Vec<RepairStep> {
    unclassifiable_bundled_entries(dir, bundled, classified)
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            RepairStep {
                check: CHECK,
                path,
                what: format!("remove the stray bundled copy of `{name}` from the project tier"),
                status: StepStatus::Refused(
                    "a bundled-named entry that is not a skill directory — no `SKILL.md`, or \
                     not a directory at all — so tm cannot tell what it is and leaves it alone"
                        .to_string(),
                ),
            }
        })
        .collect()
}

/// The stems this sweep run is removing from the project tier.
///
/// Why (#6586 critic HIGH): the `--fix-skills` redeploy runs straight after the
/// sweep and would otherwise rewrite the very copies the sweep just removed, or
/// just said it would remove. It needs the stems, and deriving them from the
/// step's `what` string would be parsing English.
/// What: the file name of every [`StepStatus::Planned`] or
/// [`StepStatus::Applied`] step. Tier-wide refusals and failures carry the tier
/// directory rather than a skill directory and are excluded by the status
/// filter, so no tier name can leak into the set.
/// Test: `swept_stems_are_the_planned_and_applied_ones`.
pub fn stems_being_removed(steps: &[RepairStep]) -> std::collections::BTreeSet<String> {
    steps
        .iter()
        .filter(|step| {
            matches!(
                step.status,
                StepStatus::Planned | StepStatus::Applied { .. }
            )
        })
        .filter_map(|step| Some(step.path.file_name()?.to_string_lossy().into_owned()))
        .collect()
}

/// Copy the whole skill directory under `backup_root`, remove it, confirm.
///
/// Why: constraint 3 of [`super::skill_repair`], applied to a removal — a
/// deletion that reports success from `remove_dir_all`'s return value has not
/// verified anything. The backup is the whole subtree because a skill's
/// `references/*.md` are as much the operator's recoverable state as its entry
/// point.
/// What: copies `dir` to `<backup_root>/project/<stem>`, removes `dir`, then
/// re-checks that the path is gone. Returns the backup path. A failure AFTER
/// the copy names the backup, because at that point the operator's copy exists
/// in two places and the message is the only thing that says where.
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
    std::fs::remove_dir_all(dir).map_err(|e| {
        format!(
            "could not remove {}: {e} — it was backed up to {} first, so nothing is lost",
            dir.display(),
            dest.display()
        )
    })?;
    if dir.exists() {
        return Err(format!(
            "removed {} but it is still present — the repair did NOT take; the backup at {} \
             holds the copy either way",
            dir.display(),
            dest.display()
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
/// directories by recursion. A SYMLINK anywhere in the subtree aborts the copy,
/// which aborts the removal: `fs::copy` follows a link and writes the target's
/// bytes as a plain file, so the backup of a linked entry is not a copy of what
/// `remove_dir_all` would then unlink, and the operator would have no way back
/// to the link. `skill_removal_verdict` reads through a link too, so a link
/// whose target happens to match the ledger checksum reaches here (#6586 critic).
/// Test: `a_symlink_inside_a_stray_stops_the_removal`.
fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(std::io::Error::other(format!(
                "{} is a symlink — tm backs up bytes, not links, so it cannot restore this \
                 entry and refuses to remove the directory holding it",
                entry.path().display()
            )));
        }
        if kind.is_dir() {
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
