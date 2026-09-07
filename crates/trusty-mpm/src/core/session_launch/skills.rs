//! The skill half of session provisioning, extracted from [`super`] (#5224).
//!
//! Why: `session_launch/mod.rs` sat exactly at the 500-SLOC production cap, so
//! the #5224 retirement sweep could not be added to it without a split — and
//! the skill block is the natural seam. Everything the launch does about skills
//! is one sequence: sweep the project tier's stray bundled copies, warn about
//! staleness, resolve the three tiers' stems, log what each agent declared,
//! deploy, then retire what no source ships any more. Splitting it out keeps
//! that sequence readable in one place instead of buried among agent, MCP,
//! hook, and instruction provisioning.
//!
//! What: [`deploy_session_skills`] runs that sequence and returns the
//! [`DeployStats`] the caller reports. Skill deployment is NON-FATAL to a
//! launch — a failure is logged, pushed onto `roster_errors`, and the session
//! starts without the skill set rather than not starting at all.
//! [`sweep_project_tier_strays`] (#6754) is the same non-fatal contract applied
//! to a REMOVAL, and `super::sync_assets` calls it too so a mid-session asset
//! refresh repairs the tier without a relaunch.
//!
//! Test: `skills_tests.rs` covers the stray sweep; the rest is exercised
//! end-to-end through `super::prepare_session` by `session_launch::tests`, and
//! the retirement sweep itself is tested in `crate::core::skill_retire`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::agent_skill_codeploy::log_declared_skills;
use crate::core::doctor_repair::{RepairMode, StepStatus};
use crate::core::manifest::HarnessPlan;
use crate::core::paths::FrameworkPaths;
use crate::core::project_tier_strays::remove_project_tier_strays;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::{
    deploy_all_skill_tiers, list_project_custom_stems, list_source_stems,
};

/// Backup root for one automatic session-start stray sweep.
///
/// Why: the sweep DELETES, so the copies must land where an operator already
/// looks — beside the `~/.trusty-mpm/backup-*-<timestamp>` directories
/// [`crate::core::skill_repair::backup_root_for`] and the catalog prune write.
/// The name says which run produced it, so a session-start sweep's copies are
/// never mistaken for a `tm doctor --fix-skills` run's, and the timestamp means
/// a second session cannot overwrite a first session's copies.
/// What: `<fw_root>/backup-session-stray-sweep-<YYYYmmdd-HHMMSS>`. The removal
/// creates it lazily, so a tier with nothing to sweep leaves no empty directory
/// behind.
/// Test: `the_backup_root_is_timestamped_under_the_framework_root`.
fn stray_backup_root(fw_root: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    fw_root.join(format!(
        "backup-session-stray-sweep-{}",
        now.format("%Y%m%d-%H%M%S")
    ))
}

/// Remove the project tier's stray copies of bundled skills, every session.
///
/// Why (#6754, owner ruling 2026-09-03): #6602 stopped the deploy paths WRITING
/// bundled skills into `<project>/.claude/skills`, and #6586 gave `tm doctor
/// --fix-skills --yes` the removal — but a copy already on disk stayed there
/// until an operator ran that command by hand, and twenty checkouts still held
/// a full tm-* set on 2026-09-03. Each frozen copy keeps the text that shipped
/// the day it landed while the user-tier copy moves on. Session start is where
/// every other asset's drift is repaired, so it is where this one is repaired.
/// What: runs [`remove_project_tier_strays`] in [`RepairMode::Apply`] against
/// the project's own skill tier and returns how many copies it removed. One
/// warn line names the count when it removed anything, and nothing is logged
/// when it removed nothing. A removal that FAILED — permission denied, an
/// unwritable backup root — is one warn line naming the path, and the launch
/// continues with that copy in place: a stray skill is drift, never a
/// precondition of starting. A REFUSAL is the evidence rule holding (see
/// [`crate::core::project_tier_strays`]) — a copy tm cannot prove it wrote, or
/// one somebody edited, is the operator's own work and stays — so it is logged
/// at debug, and `tm doctor`'s `skill_project_tier` check remains the
/// standalone audit that reports it.
///
/// The sweep runs against [`FrameworkPaths::from_root`], not the caller's `fw`:
/// a session's `fw` has `claude_skills` relocated onto
/// `<project>/.claude/skills` by [`FrameworkPaths::for_managed_project`], so the
/// sweep's reserved-tier guard would compare the project tier against itself
/// and refuse every sweep. `from_root` restores the operator-rooted layout the
/// doctor path sweeps under, keeping the same framework root, bundled source
/// and managed deploy dir.
/// Test: `skills_tests.rs`.
pub(super) fn sweep_project_tier_strays(fw: &FrameworkPaths, project_dir: &Path) -> usize {
    let operator = FrameworkPaths::from_root(&fw.root);
    let backup_root = stray_backup_root(&fw.root, chrono::Utc::now());
    let steps = remove_project_tier_strays(
        &operator,
        Some(project_dir),
        &backup_root,
        RepairMode::Apply,
    );

    let mut removed = 0usize;
    for step in &steps {
        match &step.status {
            StepStatus::Applied { .. } => removed += 1,
            // #6754: fail open. The session starts either way.
            StepStatus::Failed(why) => tracing::warn!(
                path = %step.path.display(),
                "could not remove a stray project-tier skill copy: {why} — the session \
                 starts with the copy still in place"
            ),
            StepStatus::Refused(why) => tracing::debug!(
                path = %step.path.display(),
                "left a project-tier skill copy alone: {why}"
            ),
            // Unreachable in `Apply` mode; a plan is what `DryRun` produces.
            StepStatus::Planned => {}
        }
    }
    if removed > 0 {
        let noun = if removed == 1 { "copy" } else { "copies" };
        tracing::warn!(
            project_dir = %project_dir.display(),
            removed,
            backup = %backup_root.display(),
            "removed {removed} stray project-tier {noun} of bundled skills (#6754) — \
             backed up first; `tm doctor` audits this tier"
        );
    }
    removed
}

/// Deploy the session's skill set and retire what the binary no longer ships.
///
/// Why: see the module doc — one sequence, one place. The pieces are ordered so
/// each feeds the next: the stem sets resolved for the co-deploy `select`
/// predicate are the same ones the declared-skill logging reports against, so
/// neither needs a second directory scan.
/// What: warns when the workspace's deployed skills lag the bundled source,
/// resolves the project / user / bundled stem sets, logs every agent-declared
/// skill's resolution (DOC-42, §SPEC-AGENTSKILLS-05), deploys the bundled roster
/// to the managed user tier and the user-custom tier to the project (#6586;
/// within each destination the precedence is still project-custom >
/// user-custom > bundled, #2816), then sweeps every deploy tier for skills no
/// source ships any more (#5224). A deploy failure at EITHER destination is
/// recorded in `roster_errors` and yields empty stats.
///
/// #6754: the sequence now OPENS with
/// [`sweep_project_tier_strays`] — removing before deploying, the same order
/// `tm doctor --fix-skills` uses, so no run can refresh a copy it is about to
/// delete.
/// Test: `session_launch::tests`, `skills_tests.rs`, plus
/// `crate::core::skill_retire`'s own suite.
pub(super) fn deploy_session_skills(
    fw: &FrameworkPaths,
    plan: &HarnessPlan,
    project_dir: &Path,
    declared_skills: &HashMap<String, Vec<String>>,
    roster_errors: &mut Vec<String>,
) -> DeployStats {
    // #6754: a bundled skill copy #6602 stopped writing, but which an older
    // binary already left in `<project>/.claude/skills`, is removed here rather
    // than waiting for an operator to run `tm doctor --fix-skills --yes`.
    sweep_project_tier_strays(fw, project_dir);

    // #4583/#4604: a long-lived worktree that never re-provisioned keeps the old
    // skill text (e.g. an outdated attribution footer) until this very deploy
    // self-heals it. The warn makes that drift auditable. Non-fatal, read-only.
    match crate::core::skill_staleness::stale_skills(&plan.skill_source, &fw.claude_skills_dir()) {
        Ok(stale) if !stale.is_empty() => tracing::warn!(
            project_dir = %project_dir.display(),
            stale_skills = %stale.join(", "),
            "deployed skills were stale relative to bundled assets — refreshing now \
             (run `tm doctor` to audit; long-lived worktrees drift until re-provisioned)"
        ),
        Ok(_) => {}
        // #5626: the probe could not run. It is advisory, so the launch
        // continues — but the deploy below reads the same ledger and will
        // refuse there, so this must not read as "nothing was stale".
        Err(e) => roster_errors.push(format!(
            "skill staleness undetermined — the ownership ledger at {} could not be read: {e}",
            fw.claude_skills_dir().display()
        )),
    }

    // DOC-42 (issue #2889): report how every deployed agent's declared `skills:`
    // resolves across the three tiers. #6586 removed the other half of this
    // block — the co-deploy `select` override — because the bundled tier is no
    // longer deployed here at all; §SPEC-AGENTSKILLS-02's guarantee is met by
    // the user-tier deploy, which takes the whole bundled roster unfiltered.
    let bundled_stems = list_source_stems(&plan.skill_source).unwrap_or_default();
    let user_stems = list_source_stems(&fw.user_skill_source_dir()).unwrap_or_default();
    let project_stems = list_project_custom_stems(&fw.claude_skills_dir()).unwrap_or_default();
    log_declared_skills(declared_skills, &project_stems, &user_stems, &bundled_stems);

    // Skills carry no inheritance, so each deploy below is a manifest-tracked
    // content copy. #6586 split the single deploy this used to be in two, by
    // destination: the managed user tier takes the bundled roster, the project's
    // own `.claude/skills/` takes the user-custom tier only. A project-custom
    // skill still outranks both and is never overwritten. See
    // `core::skill_tiers` and
    // `project_skill_tier::bundled_excluded_from_project_tier`.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::DeployingSkills,
    );
    // #6586: the bundled roster deploys here UNFILTERED, the same destination
    // and the same `|_| true` selection `managed_config::ensure_managed_config_dir`
    // uses on the daemon spawn path. Running it from session prep too is what
    // lets `deploy_validate::validate_and_repair` close a managed-tier gap: that
    // repair calls `prepare_session_with_repo_url`, i.e. this function, and its
    // probe reads `fw.skill_deploy_dir()`. Until this call existed the probe
    // read one tier and the repair wrote another, so a reported gap could never
    // be closed.
    let managed = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.skill_deploy_dir(),
        |_| true,
    );
    // #6586: bundled skills are user-tier only — see
    // `project_skill_tier::bundled_excluded_from_project_tier` for why declining
    // them here costs no coverage, DOC-42's co-deploy guarantee included.
    let project = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.claude_skills_dir(),
        crate::core::project_skill_tier::bundled_excluded_from_project_tier,
    );
    // #6586: either destination failing is ONE skill-deploy failure. The launch
    // continues without the skill set and the stats default out, so no caller
    // reads a half-deploy as a complete one — #2149's contract, unchanged.
    let stats = match (managed, project) {
        (Ok(managed), Ok(project)) => {
            let mut stats = managed.stats;
            stats.deployed.extend(project.stats.deployed);
            stats.skipped.extend(project.stats.skipped);
            stats.unchanged.extend(project.stats.unchanged);
            stats
        }
        (Err(err), _) | (Ok(_), Err(err)) => {
            tracing::error!(
                project_dir = %project_dir.display(),
                "skill deploy FAILED — session will launch WITHOUT the tm/mpm skill \
                 set: {err}. Identity/output-style provisioning continues regardless."
            );
            roster_errors.push(format!("skill deploy failed: {err}"));
            DeployStats::default()
        }
    };

    // #5224: the deploy above only WRITES, so a skill this binary stopped
    // shipping keeps its deployed directory and its ledger entry — and that
    // orphaned entry pins `tm doctor`'s `skill_staleness` check to Unknown.
    // Covers every deploy tier and logs per skill; never fails the launch,
    // because retirement is maintenance, not a precondition of starting.
    crate::core::skill_retire::retire_orphaned_skills(fw, Some(project_dir));

    stats
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
