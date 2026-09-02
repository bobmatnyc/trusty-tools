//! `tm doctor --fix-agents [--yes]`: remove bundled agent copies stranded at
//! the PROJECT tier (#6649).
//!
//! Why: `asset_tier` (#4442) reports a tm-owned agent in a project's own
//! `.claude/agents/` and, being read-only, leaves it there — and for agents the
//! project tier OUTRANKS the canonical one, so that copy is not merely stale,
//! it is the copy that loads (#4408). Two repairs already reach part of the
//! set: `retract_framework_agents` runs at every launch and deletes what the
//! ledger names, and the #4448 quarantine moves untracked shadows aside. What
//! neither offers is what `--fix-skills` gives the other kind — an operator-
//! driven, previewable sweep the doctor row can name. This is that half.
//!
//! WHAT LICENSES THE DELETION — identical to
//! [`crate::core::project_tier_strays`], because the evidence rule must not
//! differ by asset kind. The tier's own `.trusty-mpm-manifest.json` is tm's
//! deployment ledger, and a file recorded there, with a framework-owned origin,
//! whose bytes still match the recorded checksum, is a tm deployment and
//! nothing else. Everything else is REFUSED and reported:
//!
//! 1. **Not a plain agent file → REFUSED.** A bundled-named DIRECTORY is the
//!    shape an operator creates by hand; tm cannot tell what is in it and never
//!    removes it. This is the case #6649's acceptance test pins.
//! 2. **Absent from the ledger → REFUSED.** A hand-placed agent under a bundled
//!    name is real work. The #4448 quarantine reaches those, non-destructively;
//!    this sweep does not.
//! 3. **Ledger says the OPERATOR owns it → REFUSED.** `Origin::is_framework_
//!    owned` is the same predicate `retract_framework_agents` uses, so the two
//!    cannot disagree about whose file it is.
//! 4. **Deployed by tm and since edited → REFUSED.** A checksum mismatch is a
//!    hand edit, and the operator's edit is not tm's to delete.
//!
//! Every removal is backed up first under the run's timestamped
//! `~/.trusty-mpm/backup-doctor-remediation-<ts>/` root and CONFIRMED by
//! re-reading disk.
//!
//! And the same hard boundary: this module only ever touches
//! `<project>/.claude/agents`. A project tier that RESOLVES onto the canonical
//! [`FrameworkPaths::agent_deploy_dir`] or the operator's `~/.claude/agents` is
//! refused rather than swept — removing the roster from the tier it is SUPPOSED
//! to live in would run #4409 backwards.
//!
//! Test: `project_tier_agent_strays_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::deployer::is_agent_file;
use trusty_agents_common::agents::manifest::{
    AgentManifest, ManifestLoad, with_agent_manifest_lock,
};

use crate::core::agent_manifest::ManifestError;
use crate::core::bundled_roster::bundled_roster;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::paths::FrameworkPaths;

/// The doctor check these steps repair.
const CHECK: &str = "asset_tier";

/// The one-line summary every tier-wide refusal carries.
const SWEEP_WHAT: &str = "remove stray bundled agent copies from the project tier";

/// The workspace agent tier this sweep may touch.
///
/// Why: spelled out from `project_dir` rather than taken from
/// `paths.claude_agents_dir()`, for the reason
/// `core::session_launch::quarantine_shadows` states at length — those
/// are the same directory for a managed layout and the operator's own
/// `~/.claude/agents` for a home-tier one. (Named here, not linked: that module
/// is private, so a link to it breaks the rustdoc gate.)
/// Test: `a_tier_bundled_agents_deploy_to_is_never_swept`.
pub fn project_agent_tier(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("agents")
}

/// Sweep the project tier of bundled agent copies tm's ledger proves it wrote.
///
/// Why: see the module doc — the action half of the `asset_tier` probe, and the
/// ledger is what makes the deletion provable rather than guessed.
/// What: for each entry under `<project_dir>/.claude/agents` whose stem the
/// bundled roster carries, emits one [`RepairStep`] — [`StepStatus::Refused`]
/// for each of the four refusals above, [`StepStatus::Planned`] in
/// [`RepairMode::DryRun`], otherwise [`StepStatus::Applied`] after the file has
/// been copied under `backup_root`, removed, confirmed gone, and dropped from
/// the ledger. Returns no steps at all when there is no project in scope and no
/// tier on disk. The whole load-modify-save runs under that tier's ledger lock,
/// the same one `deploy_agents_filtered` and `retract_framework_agents` take.
/// Test: `project_tier_agent_strays_tests.rs`.
pub fn remove_project_tier_agent_strays(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let Some(project_dir) = project_dir else {
        return Vec::new();
    };
    let dir = project_agent_tier(project_dir);
    match tier_shape(&dir) {
        TierShape::Nothing => return Vec::new(),
        TierShape::Refuse(why) => return vec![refusal(dir, why)],
        TierShape::Sweepable => {}
    }
    if let Some(why) = resolves_onto_a_reserved_tier(paths, &dir) {
        return vec![refusal(dir, why)];
    }

    // #6649 fail-open deliverable: an empty roster classifies nothing. Treating
    // it as "nothing is bundled" would sweep nothing and report a clean tier —
    // the #4605 shape. It is reported as a refusal instead.
    let roster = bundled_roster(paths);
    if roster.is_empty() {
        return vec![refusal(
            dir,
            "no bundled agent roster could be built — cannot tell which project-tier agents \
             are bundled duplicates (run `tm install` to populate the agent source)"
                .to_string(),
        )];
    }

    let locked = with_agent_manifest_lock::<_, ManifestError, _>(&dir, || {
        Ok(sweep_locked(&dir, &roster, backup_root, mode))
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

/// Classify `<project>/.claude/agents` before the sweep touches it.
///
/// Why: `Path::is_dir` FOLLOWS symlinks, so a project whose `.claude/agents` is
/// a symlink to the operator's `~/.claude/agents` would pass it and the sweep
/// would delete the operator's live roster through the link — the hazard #6586
/// found on the skill side, which applies here identically.
/// What: [`TierShape::Nothing`] when absent or not a directory,
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
            "{} is a symlink — sweeping it would delete whatever it points at, which is not \
             this project's tier",
            dir.display()
        )),
        Ok(meta) if !meta.is_dir() => TierShape::Nothing,
        Ok(_) => TierShape::Sweepable,
    }
}

/// Does this project tier resolve onto a tier bundled agents DEPLOY to?
///
/// Why: removing an agent from the canonical deploy dir or the operator's
/// `~/.claude/agents` would run #4409 backwards. A lexical comparison answers a
/// different question — whether the paths are spelled the same — so an ancestor
/// symlink walks straight past it.
/// What: `Some(reason)` when `dir` canonicalises onto
/// [`FrameworkPaths::agent_deploy_dir`] or [`FrameworkPaths::claude_agents_dir`],
/// and also when `dir` itself cannot be canonicalised — an unresolvable path is
/// never a licence to delete.
/// Test: `a_tier_bundled_agents_deploy_to_is_never_swept`.
fn resolves_onto_a_reserved_tier(paths: &FrameworkPaths, dir: &Path) -> Option<String> {
    let real = match std::fs::canonicalize(dir) {
        Ok(p) => p,
        Err(e) => {
            return Some(format!(
                "{} could not be resolved to a real path, so tm cannot prove it is not a tier \
                 bundled agents deploy to: {e}",
                dir.display()
            ));
        }
    };
    for reserved in [paths.agent_deploy_dir(), paths.claude_agents_dir()] {
        let real_reserved = std::fs::canonicalize(&reserved).unwrap_or(reserved);
        if real == real_reserved {
            return Some(format!(
                "this project's agent tier resolves onto {} — a tier bundled agents are \
                 deployed to; removing them there would undo the deploy, not repair it",
                real_reserved.display()
            ));
        }
    }
    None
}

/// Sweep one project tier, holding that tier's agent ledger lock.
///
/// Why: split out so the locked critical section is one expression. Never call
/// it directly.
/// What: the classify/remove/unrecord pipeline documented on
/// [`remove_project_tier_agent_strays`], for one directory.
/// Test: exercised through [`remove_project_tier_agent_strays`].
fn sweep_locked(
    dir: &Path,
    roster: &BTreeSet<String>,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    // #6649 fail-open deliverable: a CORRUPT ledger has no answer to "did tm
    // write this?", and `AgentManifest::default()` answers "no" for every file.
    // That would refuse every removal, which is safe, and report the tier as
    // examined, which is not — the operator would read a clean-looking sweep
    // over a tier nothing was actually established about.
    let mut manifest = match AgentManifest::load_checked(dir) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => {
            return vec![refusal(
                dir.to_path_buf(),
                format!(
                    "this tier's ownership ledger is unreadable ({detail}) — it is what proves \
                     which copies tm wrote, so nothing here can be removed until it is \
                     repaired or deleted by hand"
                ),
            )];
        }
    };
    // A tier that exists, permits the ledger lock, and refuses `read_dir` would
    // otherwise produce no steps at all and read as a clean tier.
    if let Err(e) = std::fs::read_dir(dir) {
        return vec![refusal(
            dir.to_path_buf(),
            format!(
                "{} could not be listed, so tm cannot tell which copies are in it: {e}",
                dir.display()
            ),
        )];
    }

    let mut dirty = false;
    let mut steps: Vec<RepairStep> = Vec::new();
    for candidate in bundled_named_entries(dir, roster) {
        let what = format!(
            "remove the stray bundled copy of `{}` from the project tier",
            candidate.stem
        );
        let status = verdict_for(&manifest, &candidate, backup_root, mode, &mut dirty);
        // The file is gone at this point, so the ledger entry claiming it must
        // go with it, in the same critical section.
        if matches!(status, StepStatus::Applied { .. }) {
            unrecord(&mut manifest, &candidate.file_name);
        }
        steps.push(RepairStep {
            check: CHECK,
            path: candidate.path.clone(),
            what,
            status,
        });
    }
    steps.sort_by(|a, b| a.path.cmp(&b.path));

    if dirty {
        debug_assert_eq!(
            mode,
            RepairMode::Apply,
            "a dry run must never mark the ledger dirty — it removed nothing to record"
        );
        if let Err(e) = manifest.save(dir) {
            steps.push(RepairStep {
                check: CHECK,
                path: dir.to_path_buf(),
                what: "drop the removed strays from the deploy ledger".to_string(),
                status: StepStatus::Failed(format!("could not save the deploy manifest: {e}")),
            });
        }
    }
    steps
}

/// One bundled-named entry of the project tier.
struct Candidate {
    /// Full path of the entry.
    path: PathBuf,
    /// The entry's file name, the key the ledger uses.
    file_name: String,
    /// The roster name it collides with.
    stem: String,
    /// Whether the entry is a plain `.md` agent file.
    is_agent_file: bool,
}

/// Every entry of `dir` whose stem the bundled roster carries.
///
/// Why: a bundled-named DIRECTORY must reach the report as a refusal, not be
/// dropped from the scan — the #6586 lesson, where an unclassifiable entry
/// silently vanished and the tier read clean. Shape is recorded, never used to
/// filter.
/// What: entries whose `.md`-stripped file name is in `roster`, sorted by path.
/// Dot-entries are skipped: the ledger and its lock sidecar are not assets.
/// Test: `an_operator_authored_directory_is_refused_and_survives`,
/// `a_ledger_tracked_copy_is_removed`.
fn bundled_named_entries(dir: &Path, roster: &BTreeSet<String>) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_str()?.to_owned();
            if file_name.starts_with('.') {
                return None;
            }
            let stem = file_name
                .strip_suffix(".md")
                .unwrap_or(&file_name)
                .to_owned();
            if !roster.contains(&stem) {
                return None;
            }
            let is_agent_file =
                is_agent_file(&file_name) && entry.file_type().is_ok_and(|t| t.is_file());
            Some(Candidate {
                path: entry.path(),
                file_name,
                stem,
                is_agent_file,
            })
        })
        .collect();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// The four refusals, then the removal.
///
/// Why: stated once, in order, so the evidence rule reads as the module doc
/// writes it.
/// What: see the module doc's numbered list. On [`RepairMode::Apply`] a
/// removable file is backed up, removed, and confirmed; `dirty` is set so the
/// caller republishes the ledger.
/// Test: `an_operator_authored_directory_is_refused_and_survives`,
/// `an_untracked_copy_is_refused`, `a_user_owned_ledger_entry_is_refused`,
/// `a_hand_edited_managed_copy_is_refused`, `a_ledger_tracked_copy_is_removed`,
/// `a_dry_run_writes_nothing`.
fn verdict_for(
    manifest: &AgentManifest,
    candidate: &Candidate,
    backup_root: &Path,
    mode: RepairMode,
    dirty: &mut bool,
) -> StepStatus {
    if !candidate.is_agent_file {
        return StepStatus::Refused(
            "a bundled-named entry that is not an agent file — a directory, a symlink, or not \
             `.md` at all — so tm cannot tell what it is and leaves it alone"
                .to_string(),
        );
    }
    let Some(entry) = manifest.managed.get(&candidate.file_name) else {
        return StepStatus::Refused(
            "absent from this tier's deploy ledger — tm cannot prove it wrote this copy, and a \
             bundled-named file it did not write may be the operator's own agent"
                .to_string(),
        );
    };
    if !entry.origin.is_framework_owned() {
        return StepStatus::Refused(
            "the deploy ledger records the OPERATOR as its owner, not tm — the same seed-once \
             entry `retract_framework_agents` preserves"
                .to_string(),
        );
    }
    let Ok(content) = std::fs::read_to_string(&candidate.path) else {
        return StepStatus::Refused(
            "could not be read, so tm cannot check it against the checksum its ledger records"
                .to_string(),
        );
    };
    if !manifest.checksum_matches(&candidate.file_name, &content) {
        return StepStatus::Refused(
            "hand-edited after tm deployed it — the bytes no longer match the ledger checksum, \
             and the edit is not tm's to delete"
                .to_string(),
        );
    }
    if mode == RepairMode::DryRun {
        return StepStatus::Planned;
    }
    match back_up_and_remove(&candidate.path, &candidate.file_name, backup_root) {
        Ok(backup) => {
            *dirty = true;
            StepStatus::Applied {
                backup: Some(backup),
            }
        }
        Err(why) => StepStatus::Failed(why),
    }
}

/// Copy the agent file under `backup_root`, remove it, confirm.
///
/// Why: a deletion that reports success from `remove_file`'s return value has
/// verified nothing.
/// What: copies `path` to `<backup_root>/project-agents/<file_name>`, removes
/// it, then re-checks that the path is gone. A failure AFTER the copy names the
/// backup, because at that point the operator's copy exists in two places and
/// the message is the only thing that says where.
/// Test: `a_removed_stray_is_backed_up`.
fn back_up_and_remove(path: &Path, file_name: &str, backup_root: &Path) -> Result<PathBuf, String> {
    let dest_dir = backup_root.join("project-agents");
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("could not create {}: {e}", dest_dir.display()))?;
    let dest = dest_dir.join(file_name);
    std::fs::copy(path, &dest).map_err(|e| {
        format!(
            "could not back up {} to {}: {e}",
            path.display(),
            dest.display()
        )
    })?;
    std::fs::remove_file(path).map_err(|e| {
        format!(
            "could not remove {}: {e} — it was backed up to {} first, so nothing is lost",
            path.display(),
            dest.display()
        )
    })?;
    if path.exists() {
        return Err(format!(
            "removed {} but it is still present — the repair did NOT take; the backup at {} \
             holds the copy either way",
            path.display(),
            dest.display()
        ));
    }
    Ok(dest)
}

/// Drop `file_name` from the ledger.
///
/// Why: leaving the entry would have the ledger claim tm owns a file that no
/// longer exists, which the next deploy reads as one it already wrote.
/// Test: `a_ledger_tracked_copy_is_removed`.
fn unrecord(manifest: &mut AgentManifest, file_name: &str) {
    manifest.managed.remove(file_name);
}

#[cfg(test)]
#[path = "project_tier_agent_strays_tests.rs"]
mod tests;
