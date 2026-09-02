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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::agent_manifest::ManifestError;
use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::doctor_repair::RepairMode;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::{SkillDeployTier, project_skill_tier, skill_deploy_tiers};
use crate::core::skill_drift::{
    ManifestState, SkillDrift, SkillReference, audit_deployed_skills, deployed_path,
};
use crate::core::skill_manifest::{
    SkillManifest, SkillManifestEntry, SkillManifestSave, with_skill_manifest_lock,
};

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
    /// [`RepairMode::DryRun`] only: this file WOULD have been rewritten, and
    /// nothing was written. `existed` distinguishes an overwrite (which would
    /// be backed up first) from a file that is simply absent.
    WouldRepair { existed: bool },
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
    /// The exact file this outcome is about.
    ///
    /// Why (#4948): a preview that names only a tier label and a stem is not a
    /// preview — the operator cannot tell which of the three tiers' copies is
    /// about to be overwritten without re-deriving the path themselves. The
    /// repair already resolves it via [`deployed_path`]; carrying it out is
    /// what makes "print exactly what would change, with paths" true. For the
    /// synthetic `<manifest>` outcomes this is the tier directory.
    pub path: PathBuf,
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
    repair_skills_in_mode(
        reference,
        paths,
        project_dir,
        include_frozen,
        backup_root,
        RepairMode::Apply,
    )
}

/// [`repair_skills`], with the write suppressed in [`RepairMode::DryRun`].
///
/// Why (#4948): a repair that deletes and rewrites operator state must be able
/// to say what it is about to do BEFORE it does it, and the only preview worth
/// trusting is one produced by the code that performs the repair. A separate
/// planner would be a second implementation of the frozen/unverifiable
/// classification, and the two would disagree exactly when it mattered — the
/// preview would promise a rewrite the apply path then skips, or say nothing
/// about a file it is about to overwrite. So the mode is threaded through the
/// SAME audit → classify → act pipeline, and only the final write is gated.
/// What: identical to [`repair_skills`] except that in
/// [`RepairMode::DryRun`] nothing is written — no skill file, no backup, no
/// manifest — and every finding the apply path would have rewritten is
/// reported as [`RepairAction::WouldRepair`] instead. The tier ledger lock is
/// still taken, so a preview cannot race a concurrent deploy into reporting a
/// half-written ledger's contents.
/// Test: `dry_run_reports_the_same_set_it_would_repair`,
/// `dry_run_writes_absolutely_nothing`,
/// `dry_run_still_refuses_a_frozen_skill`.
pub fn repair_skills_in_mode(
    reference: &SkillReference,
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    include_frozen: bool,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairOutcome> {
    repair_skills_in_mode_deferring(
        reference,
        paths,
        project_dir,
        include_frozen,
        backup_root,
        mode,
        &BTreeSet::new(),
    )
}

/// The reason a stem the #6586 sweep is removing is skipped at the project tier.
///
/// Why (#6586 critic HIGH): a bare `tm doctor --fix-skills` used to run the
/// sweep as a DRY RUN and the redeploy as an APPLY. Those two halves met on the
/// same 51 project-tier stems: the sweep printed "would remove", then the
/// redeploy rewrote every one of them from the bundled asset and re-stamped its
/// ledger checksum — 51 files and 51 backups written immediately after the
/// command said nothing would be written. #6620 put both halves in one mode, so
/// that pairing no longer occurs; the deferral still holds, because under
/// `--yes` the sweep REMOVES the copy first and the redeploy would otherwise
/// see it missing and write it straight back. Refreshing a copy the sweep is
/// removing is work in the opposite direction whichever mode the run is in.
/// What: the `why` of the [`RepairAction::SkippedUnverifiable`] those stems get
/// instead. [`super::doctor_repair`] renders it verbatim.
/// Test: `a_deferred_project_stem_is_not_refreshed`.
pub const DEFERRED_TO_STRAY_SWEEP: &str = "the #6586 project-tier sweep is removing this stray copy in this run — refreshing it here \
     would write straight back what the sweep is taking out";

/// [`repair_skills_in_mode`], leaving the #6586 sweep's stems alone.
///
/// Why: see [`DEFERRED_TO_STRAY_SWEEP`]. The deferral is additive rather than a
/// signature change on [`repair_skills_in_mode`] so the two callers that run no
/// sweep — `tm doctor --fix`'s preview and [`repair_skills`] — keep the shorter
/// call.
/// What: identical to [`repair_skills_in_mode`] except that a finding whose stem
/// is in `deferred_project_stems` is reported
/// [`RepairAction::SkippedUnverifiable`] and never written. The deferral applies
/// ONLY at [`project_skill_tier`] — the same directory the sweep acts on — so a
/// stem that is also deployed at the user or managed tier is still repaired
/// there. With no project directory in scope it applies nowhere.
/// Test: `a_deferred_project_stem_is_not_refreshed`,
/// `a_deferred_stem_is_still_repaired_at_the_user_tier`.
pub fn repair_skills_in_mode_deferring(
    reference: &SkillReference,
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    include_frozen: bool,
    backup_root: &Path,
    mode: RepairMode,
    deferred_project_stems: &BTreeSet<String>,
) -> Vec<RepairOutcome> {
    let project_tier = project_dir.map(project_skill_tier);
    let nothing_deferred = BTreeSet::new();
    let mut outcomes = Vec::new();
    for tier in skill_deploy_tiers(paths, project_dir) {
        // #4881: a tier directory that does not exist has no ledger and so no
        // findings — repairing it was already a no-op. Skipped BEFORE the lock
        // so taking one never creates an empty `skills/` directory and a lock
        // sidecar in a project `tm doctor` was only inspecting.
        if !tier.dir.is_dir() {
            continue;
        }
        // #4881: audit → rewrite → manifest save is a load-modify-save of the
        // same ledger the deployer writes, so it runs under the tier's ledger
        // lock. Unlocked, a repair racing a session launch's deploy publishes a
        // manifest missing the launch's entries, and the skills those entries
        // described then read as hand-edited and freeze — the very state this
        // module exists to remedy.
        let label = tier.label;
        let dir = tier.dir.clone();
        // #6586: the sweep only ever touches the project tier, so the deferral
        // must not reach a copy at the user or managed tier that nothing is
        // removing.
        let deferred = if project_tier.as_ref() == Some(&tier.dir) {
            deferred_project_stems
        } else {
            &nothing_deferred
        };
        let locked = with_skill_manifest_lock::<_, ManifestError, _>(&tier.dir, || {
            Ok(repair_tier_locked(
                reference,
                &tier,
                include_frozen,
                backup_root,
                mode,
                deferred,
            ))
        });
        match locked {
            Ok(tier_outcomes) => outcomes.extend(tier_outcomes),
            Err(e) => outcomes.push(RepairOutcome {
                tier: label,
                stem: "<manifest>".to_string(),
                path: dir,
                action: RepairAction::Failed(format!(
                    "could not lock the deploy manifest: {e} — refusing to repair this tier \
                     unserialised"
                )),
            }),
        }
    }
    outcomes
}

/// Repair one tier, holding that tier's skill ledger lock.
///
/// Why (#4881): split out so the locked critical section is one expression and
/// its scope cannot be misread — every manifest load, file write, and manifest
/// save for this tier happens with the lock held. Never call it directly, and
/// never from inside another `with_skill_manifest_lock` on the same directory.
/// What: the audit/classify/rewrite/save pipeline documented on
/// [`repair_skills`], for a single tier.
/// Test: `skill_repair_tests.rs` exercises it through [`repair_skills`].
fn repair_tier_locked(
    reference: &SkillReference,
    tier: &SkillDeployTier,
    include_frozen: bool,
    backup_root: &Path,
    mode: RepairMode,
    deferred: &BTreeSet<String>,
) -> Vec<RepairOutcome> {
    let mut outcomes = Vec::new();
    let audit = audit_deployed_skills(reference, &tier.dir);
    // #4622 review: never write into a tier whose ownership ledger could not
    // be read. Saving a rebuilt manifest over an unparseable one would
    // silently reclassify every file there as tm-owned and make the next
    // deploy overwrite the operator's work — the frozen-skill protection
    // depends on that ledger being intact.
    if let ManifestState::Unreadable(why) = &audit.manifest {
        outcomes.push(RepairOutcome {
            tier: tier.label,
            stem: "<manifest>".to_string(),
            path: tier.dir.clone(),
            action: RepairAction::SkippedUnverifiable(format!(
                "{why} — refusing to touch this tier; repairing it would rewrite an                      ownership ledger that cannot be read"
            )),
        });
        return outcomes;
    }

    // #5626: `audit_deployed_skills` above already refuses an unreadable
    // ledger, so reaching this arm means the file changed under us between the
    // two reads. Report it as the same unverifiable skip rather than repairing
    // against an empty ledger — that would stamp every file in the tier as
    // tm-owned.
    let mut manifest = match SkillManifest::load(&tier.dir) {
        Ok(m) => m,
        Err(e) => {
            outcomes.push(RepairOutcome {
                tier: tier.label,
                stem: "<manifest>".to_string(),
                path: tier.dir.clone(),
                action: RepairAction::SkippedUnverifiable(format!(
                    "{e} — refusing to touch this tier; repairing it would rewrite an \
                     ownership ledger that cannot be read"
                )),
            });
            return outcomes;
        }
    };
    // #4881: the snapshot the merging save replays this run's delta against.
    let base = manifest.clone();
    let mut manifest_dirty = false;

    for finding in audit.findings {
        let target = deployed_path(&tier.dir, &finding.stem);
        let action = match finding.state {
            SkillDrift::Fresh => continue,
            // #6586 critic HIGH: checked BEFORE the write gate, so the deferral
            // holds in both modes — a preview must not promise a rewrite the
            // apply path skips.
            _ if deferred.contains(&finding.stem) => {
                RepairAction::SkippedUnverifiable(DEFERRED_TO_STRAY_SWEEP.to_string())
            }
            SkillDrift::Unverifiable(why) => RepairAction::SkippedUnverifiable(why),
            SkillDrift::DriftedFrozen if !include_frozen => RepairAction::SkippedFrozen,
            SkillDrift::Drifted | SkillDrift::DriftedFrozen | SkillDrift::Missing => {
                let Some(content) = reference.assets.get(&finding.stem) else {
                    // Unreachable: a non-`Unverifiable` state implies an
                    // asset exists. Reported rather than unwrapped.
                    outcomes.push(RepairOutcome {
                        tier: tier.label,
                        stem: finding.stem,
                        path: target,
                        action: RepairAction::Failed(
                            "no bundled asset available to write".to_string(),
                        ),
                    });
                    continue;
                };
                // #4948: the ONE gate between preview and apply. Everything
                // above — the audit, the frozen skip, the unverifiable skip —
                // has already run identically in both modes, so what a dry run
                // reports here is what an apply will act on.
                if mode == RepairMode::DryRun {
                    RepairAction::WouldRepair {
                        existed: target.exists(),
                    }
                } else {
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
            }
        };
        outcomes.push(RepairOutcome {
            tier: tier.label,
            stem: finding.stem,
            path: target,
            action,
        });
    }

    // #4881: `rewrite_and_verify` already wrote and re-read every repaired file,
    // so this save must publish or the repair leaves each one reading as
    // hand-edited — the exact state it was invoked to clear. `save_merging`
    // folds in a writer that bypassed the lock rather than dropping it.
    if manifest_dirty {
        debug_assert_eq!(
            mode,
            RepairMode::Apply,
            "a dry run must never mark the ledger dirty — it wrote nothing to record"
        );
        match manifest.save_merging(&tier.dir, &base) {
            // `OverwroteUnreadable` is already reported by `save_merging` at
            // error level — louder than this warn would be — so it needs no
            // second line here. The ledger was published either way.
            Ok(SkillManifestSave::Written | SkillManifestSave::OverwroteUnreadable) => {}
            Ok(SkillManifestSave::Merged) => tracing::warn!(
                tier = tier.label,
                "the skill manifest changed during repair — a writer bypassed the \
                 ledger lock; its entries were merged rather than dropped"
            ),
            Err(e) => outcomes.push(RepairOutcome {
                tier: tier.label,
                stem: "<manifest>".to_string(),
                path: tier.dir.clone(),
                action: RepairAction::Failed(format!("could not save the deploy manifest: {e}")),
            }),
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
    // #4622 review HIGH-1: a manifest key is either a bare stem (entry point at
    // `<stem>/SKILL.md`) or a nested `<stem>/references/<file>.md` that mirrors
    // the source layout. One shared resolver keeps the repair writing exactly
    // where the audit looked.
    let target = deployed_path(tier_dir, stem);
    let backup = if target.exists() {
        let rel = target
            .strip_prefix(tier_dir)
            .unwrap_or_else(|_| Path::new(stem));
        let dest = backup_root.join(tier_label.replace(' ', "-")).join(rel);
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
