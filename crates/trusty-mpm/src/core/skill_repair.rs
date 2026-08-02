//! `tm doctor --fix-skills`: redeploy drifted skills and VERIFY the result by
//! re-reading disk (issue #4604).
//!
//! Why: detection alone left the operator holding a report and no remedy for
//! the one state `tm install` structurally cannot fix — a FROZEN (hand-edited)
//! skill, which the deployer's "checksum differs → user-modified → skip" rule
//! deliberately never touches, so it can stay stale forever. The owner's
//! constraints on that remedy are explicit and are encoded here rather than
//! left to reviewer memory:
//!
//! 1. **A frozen skill is NEVER silently overwritten.** The freeze exists
//!    because a human edited the file; that is deliberate customization, not
//!    rot. Repairing one requires the explicit `include_frozen` opt-in.
//! 2. **Every overwrite is backed up first**, under
//!    `~/.trusty-mpm/backup-doctor-remediation-<timestamp>/`, following the
//!    existing remediation-backup convention.
//! 3. **The fix VERIFIES ITS OWN WORK by re-reading from disk.** Reporting
//!    success from a write's return value is the exact failure that produced
//!    #4604 — PR #4583 merged, the binary installed, every gate went green, and
//!    three deployed copies were untouched. A repair that cannot be confirmed
//!    from disk is reported as [`RepairAction::Failed`].
//!
//! And one hard boundary, stated because a fix mode is where it would be
//! violated: **this module NEVER deletes anything.** No `rm -rf`, no directory
//! removal, and nothing whatsoever to do with worktrees — `tm doctor`'s
//! worktree and `worktree_disk` checks are REPORT-ONLY and stay that way. On
//! 2026-07-21 an `rm -rf .base/*` wiped a shared bare clone's git internals and
//! orphaned ~70 worktrees across concurrent sessions; a fix mode that deletes
//! what it thinks is orphaned is that incident with a scheduler. Deletion stays
//! a human decision.
//!
//! What: [`repair_skills`] walks the same audit [`super::skill_drift`] produces
//! and acts only on the states it is allowed to act on. It writes exactly two
//! kinds of file: a skill's `SKILL.md`, and the tier's deploy manifest (so a
//! repaired skill returns to tm ownership rather than immediately re-reading as
//! frozen).
//!
//! Test: `skill_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;
use crate::core::skill_drift::{SkillDrift, SkillReference, audit_deployed_skills};
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};

/// Entry-point filename Claude Code reads inside a deployed skill directory.
const SKILL_ENTRY_FILE: &str = "SKILL.md";

/// What the repair did — or deliberately did not do — to one skill.
///
/// Why: "skipped because frozen" and "failed" must never be reported the same
/// way; the first is the safety rule working, the second is the fix not
/// working. And `Repaired` names the backup so the operator can undo it.
/// What: four outcomes. `Repaired` is only ever produced after the written
/// content has been READ BACK from disk and confirmed.
/// Test: `skill_repair_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    /// Rewritten from the bundled asset and CONFIRMED by re-reading the file.
    /// Carries the backup path, or `None` when the file had been absent (there
    /// was nothing to back up).
    Repaired { backup: Option<PathBuf> },
    /// Hand-edited after deployment and `include_frozen` was not set. Untouched.
    SkippedFrozen,
    /// Could not be verified in the first place, so it is not repairable — the
    /// binary ships no asset to write. Untouched.
    SkippedUnverifiable(String),
    /// The write or the post-write verification failed; the reason is carried.
    Failed(String),
}

/// One skill's repair outcome at one deploy tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOutcome {
    /// Human label of the deploy tier (`managed config`, `operator home`, …).
    pub tier: &'static str,
    /// Skill stem.
    pub stem: String,
    /// What happened.
    pub action: RepairAction,
}

impl RepairOutcome {
    /// Did this outcome actually change a file?
    ///
    /// Why: the caller reports "N repaired" and must not count skips.
    /// What: `true` only for [`RepairAction::Repaired`].
    /// Test: `repair_rewrites_drifted_and_verifies`.
    pub fn changed(&self) -> bool {
        matches!(self.action, RepairAction::Repaired { .. })
    }
}

/// Repair drifted skills across every deploy tier, verifying each from disk.
///
/// Why: see the module doc — the three owner constraints are enforced here.
/// What: audits each [`skill_deploy_tiers`] entry against `reference`, then:
/// - [`SkillDrift::Fresh`] → nothing, and no outcome is reported;
/// - [`SkillDrift::Drifted`] → backed up, rewritten, re-read, manifest updated;
/// - [`SkillDrift::Missing`] → written (nothing to back up), re-read;
/// - [`SkillDrift::DriftedFrozen`] → [`RepairAction::SkippedFrozen`] unless
///   `include_frozen`, in which case treated like `Drifted` (still backed up);
/// - [`SkillDrift::Unverifiable`] → never touched.
///
/// `backup_root` is created lazily, only when something is actually about to be
/// overwritten. Nothing is ever deleted.
/// Test: `skill_repair_tests.rs`.
pub fn repair_skills(
    reference: &SkillReference,
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    include_frozen: bool,
    backup_root: &Path,
) -> Vec<RepairOutcome> {
    let mut outcomes = Vec::new();
    for tier in skill_deploy_tiers(paths, project_dir) {
        let mut manifest = SkillManifest::load(&tier.dir);
        let mut manifest_dirty = false;

        for finding in audit_deployed_skills(reference, &tier.dir) {
            let action = match finding.state {
                SkillDrift::Fresh => continue,
                SkillDrift::Unverifiable(why) => RepairAction::SkippedUnverifiable(why),
                SkillDrift::DriftedFrozen if !include_frozen => RepairAction::SkippedFrozen,
                SkillDrift::Drifted | SkillDrift::DriftedFrozen | SkillDrift::Missing => {
                    let Some(content) = reference.assets.get(&finding.stem) else {
                        // Unreachable: a non-`Unverifiable` state implies an
                        // asset exists. Reported rather than unwrapped.
                        outcomes.push(RepairOutcome {
                            tier: tier.label,
                            stem: finding.stem,
                            action: RepairAction::Failed(
                                "no bundled asset available to write".to_string(),
                            ),
                        });
                        continue;
                    };
                    match rewrite_and_verify(
                        &tier.dir,
                        tier.label,
                        &finding.stem,
                        content,
                        backup_root,
                    ) {
                        Ok(backup) => {
                            manifest.managed.insert(
                                finding.stem.clone(),
                                SkillManifestEntry {
                                    checksum: checksum(content),
                                    deployed_at: chrono::Utc::now().to_rfc3339(),
                                },
                            );
                            manifest_dirty = true;
                            RepairAction::Repaired { backup }
                        }
                        Err(e) => RepairAction::Failed(e),
                    }
                }
            };
            outcomes.push(RepairOutcome {
                tier: tier.label,
                stem: finding.stem,
                action,
            });
        }

        if manifest_dirty && let Err(e) = manifest.save(&tier.dir) {
            outcomes.push(RepairOutcome {
                tier: tier.label,
                stem: "<manifest>".to_string(),
                action: RepairAction::Failed(format!("could not save the deploy manifest: {e}")),
            });
        }
    }
    outcomes
}

/// Back up, write, then RE-READ to confirm. Returns the backup path, if any.
///
/// Why: constraint 3 — a remediation that reports success from a write's return
/// value, without re-reading the file, is exactly what produced #4604. The
/// verification here is a fresh `read_to_string` compared byte-for-byte against
/// what was supposed to land.
/// What: copies any existing file under `backup_root/<tier>/<stem>/SKILL.md`
/// (never overwriting an earlier backup in the same run — the root is
/// timestamped per run), atomically writes `content`, then reads the file back
/// and compares. `Err` carries the reason on any step's failure, including a
/// verification mismatch. Never deletes.
/// Test: `repair_rewrites_drifted_and_verifies`,
/// `repair_backs_up_before_overwriting`, `repair_reports_verification_failure`.
fn rewrite_and_verify(
    tier_dir: &Path,
    tier_label: &str,
    stem: &str,
    content: &str,
    backup_root: &Path,
) -> Result<Option<PathBuf>, String> {
    let target = tier_dir.join(stem).join(SKILL_ENTRY_FILE);

    let backup = if target.exists() {
        let dest = backup_root
            .join(tier_label.replace(' ', "-"))
            .join(stem)
            .join(SKILL_ENTRY_FILE);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create backup dir {}: {e}", parent.display()))?;
        }
        std::fs::copy(&target, &dest).map_err(|e| {
            format!(
                "could not back up {} to {}: {e}",
                target.display(),
                dest.display()
            )
        })?;
        Some(dest)
    } else {
        None
    };

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    atomic_write(&target, content)
        .map_err(|e| format!("could not write {}: {e}", target.display()))?;

    // VERIFY FROM DISK. The write returning Ok is not evidence the file now
    // holds the intended bytes.
    let observed = std::fs::read_to_string(&target)
        .map_err(|e| format!("wrote {} but could not read it back: {e}", target.display()))?;
    if observed != content {
        return Err(format!(
            "wrote {} but re-reading it returned different content — the repair did NOT take",
            target.display()
        ));
    }
    Ok(backup)
}

/// Timestamped backup root for one `--fix-skills` run.
///
/// Why: follows the existing `~/.trusty-mpm/backup-doctor-remediation-<date>/`
/// convention so an operator finds remediation backups where they already look,
/// and a per-run directory means a second run never overwrites the first run's
/// copies.
/// What: `<home>/.trusty-mpm/backup-doctor-remediation-<YYYYmmdd-HHMMSS>`.
/// Test: `backup_root_follows_the_remediation_convention`.
pub fn backup_root_for(home: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    home.join(".trusty-mpm").join(format!(
        "backup-doctor-remediation-{}",
        now.format("%Y%m%d-%H%M%S")
    ))
}

#[cfg(test)]
#[path = "skill_repair_tests.rs"]
mod tests;
