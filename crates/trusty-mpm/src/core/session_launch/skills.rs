//! The skill half of session provisioning, extracted from [`super`] (#5224).
//!
//! Why: `session_launch/mod.rs` sat exactly at the 500-SLOC production cap, so
//! the #5224 retirement sweep could not be added to it without a split — and
//! the skill block is the natural seam. Everything the launch does about skills
//! is one sequence: warn about staleness, resolve the three tiers' stems, log
//! what each agent declared, deploy, then retire what no source ships any more.
//! Splitting it out keeps that sequence readable in one place instead of buried
//! among agent, MCP, hook, and instruction provisioning.
//!
//! What: [`deploy_session_skills`] runs that sequence and returns the
//! [`DeployStats`] the caller reports. Skill deployment is NON-FATAL to a
//! launch — a failure is logged, pushed onto `roster_errors`, and the session
//! starts without the skill set rather than not starting at all.
//!
//! Test: exercised end-to-end through `super::prepare_session` by
//! `session_launch::tests`; the retirement sweep itself is tested in
//! `crate::core::skill_retire`.

use std::collections::HashMap;
use std::path::Path;

use crate::core::agent_skill_codeploy::log_declared_skills;
use crate::core::manifest::HarnessPlan;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::{
    deploy_all_skill_tiers, list_project_custom_stems, list_source_stems,
};

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
/// Test: `session_launch::tests`, plus `crate::core::skill_retire`'s own suite.
pub(super) fn deploy_session_skills(
    fw: &FrameworkPaths,
    plan: &HarnessPlan,
    project_dir: &Path,
    declared_skills: &HashMap<String, Vec<String>>,
    roster_errors: &mut Vec<String>,
) -> DeployStats {
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
