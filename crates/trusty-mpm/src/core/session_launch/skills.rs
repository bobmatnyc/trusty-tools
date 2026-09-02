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
/// skill's resolution (DOC-42, §SPEC-AGENTSKILLS-05), deploys the three tiers
/// with precedence project-custom > user-custom > bundled (#2816), then sweeps
/// every deploy tier for skills no source ships any more (#5224). A deploy
/// failure is recorded in `roster_errors` and yields empty stats.
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

    // Claude Code reads the project's `.claude/skills/` at startup. Skills carry
    // no inheritance, so this is a manifest-tracked content copy. Since #6586
    // only the user-custom tier deploys here; a skill hand-placed in the
    // project's `.claude/skills/` outranks it and is never overwritten, and
    // bundled skills reach the session through the user tier instead. See
    // `core::skill_tiers` and
    // `project_skill_tier::bundled_excluded_from_project_tier`.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::DeployingSkills,
    );
    // #6586: bundled skills are user-tier only — see
    // `project_skill_tier::bundled_excluded_from_project_tier` for why this
    // costs no coverage, including DOC-42's co-deploy guarantee.
    let stats = match deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.claude_skills_dir(),
        crate::core::project_skill_tier::bundled_excluded_from_project_tier,
    ) {
        Ok(result) => result.stats,
        Err(err) => {
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
