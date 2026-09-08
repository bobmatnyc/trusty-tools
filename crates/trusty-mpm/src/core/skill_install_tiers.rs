//! `tm install`'s skill deploy, split by destination (#7102).
//!
//! Why: `tm install` wrote the WHOLE skill set — every bundled `tm-*` skill
//! included — into `~/.claude/skills`, while #6586 had already made
//! [`FrameworkPaths::skill_deploy_dir`] the one directory bundled skills deploy
//! into. `tm doctor`'s `legacy_sources` check counts `~/.claude/skills/tm-*` as
//! a leftover pre-#6586 global deploy, so the installer refilled the exact
//! directory that check flags: removing the copies and running `tm install` —
//! the remediation the check printed — reproduced the warning on the next
//! doctor run (#7102). The agents side had the same breach and #4409 closed it
//! by moving `tm install`'s agent deploy onto
//! [`FrameworkPaths::agent_deploy_dir`]; this is that fix for skills.
//!
//! What: [`deploy_install_skill_tiers`] runs the two-destination deploy
//! `session_launch::skills::deploy_session_skills` already runs — the bundled
//! roster unfiltered into [`FrameworkPaths::skill_deploy_dir`], the user-custom
//! tier alone into [`FrameworkPaths::claude_skills_dir`] — and merges both
//! results into one [`TierDeploy`] for the installer's report. It lives in the
//! library rather than in `bin/tm` so the regression test can drive the
//! installer's own deploy and then read `tm doctor`'s verdict on the same temp
//! home; nothing else about the installer's reporting, retirement sweep, or
//! reconcile path changes.
//! Test: `bundled_skills_land_only_in_the_managed_tier`,
//! `user_custom_skills_reach_the_operator_home`, plus
//! `crate::daemon::doctor_staleness`'s
//! `legacy_sources_ok_after_the_install_deploy`.

use crate::core::error::Result;
use crate::core::paths::FrameworkPaths;
use crate::core::project_skill_tier::bundled_excluded_from_project_tier;
use crate::core::skill_tiers::{TierDeploy, deploy_all_skill_tiers};

/// Deploy the installed skill set to the two destinations `tm install` owns.
///
/// Why: see the module doc — one call site holding the destination split keeps
/// the installer from re-deriving "where does a bundled skill go?" a fourth
/// time, which is how it drifted from #6586 in the first place.
/// What: the bundled roster deploys unfiltered into `skill_deploy_dir()` (the
/// tier `deploy_validate::validate_skills` probes, so #386's "a fresh install
/// must not leave the skill tier empty" still holds); the operator's own
/// `claude_skills_dir()` takes the user-custom tier only, declined for bundled
/// stems by [`bundled_excluded_from_project_tier`] — the same #6586 ruling, at
/// a destination that is likewise not `skill_deploy_dir()`. Returns both
/// deploys' stats and shadow records merged, so the installer's existing report
/// lines cover every file written. A failure at EITHER destination is returned
/// as-is and no partial result is reported.
/// Test: `bundled_skills_land_only_in_the_managed_tier`,
/// `user_custom_skills_reach_the_operator_home`.
pub fn deploy_install_skill_tiers(paths: &FrameworkPaths) -> Result<TierDeploy> {
    // #7102: bundled skills go to the ONE managed tier #6586 named, matching
    // `session_launch::skills::deploy_session_skills`'s first deploy.
    let mut deploy = deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.skill_deploy_dir(),
        |_| true,
    )?;

    // #7102: the operator's `~/.claude/skills` keeps receiving USER-custom
    // skills — a plain `claude` run outside tm reads that directory and nothing
    // else — but never a bundled one, which is what `legacy_sources` flags.
    let home = deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.claude_skills_dir(),
        bundled_excluded_from_project_tier,
    )?;

    deploy.stats.deployed.extend(home.stats.deployed);
    deploy.stats.skipped.extend(home.stats.skipped);
    deploy.stats.unchanged.extend(home.stats.unchanged);
    deploy.shadowed.extend(home.shadowed);
    Ok(deploy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one flat `<stem>.md` skill source into `dir`.
    fn write_source(dir: &std::path::Path, stem: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{stem}.md")), body).unwrap();
    }

    #[test]
    fn bundled_skills_land_only_in_the_managed_tier() {
        // #7102: the installer must not refill `~/.claude/skills` with the
        // bundled roster — that is the directory `legacy_sources` flags.
        let base = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(base.path());
        write_source(&paths.skill_source_dir(), "tm-workflow", "bundled body");

        let deploy = deploy_install_skill_tiers(&paths).unwrap();

        assert!(
            paths
                .skill_deploy_dir()
                .join("tm-workflow")
                .join("SKILL.md")
                .is_file(),
            "the bundled skill must land in the managed tier"
        );
        assert!(
            !paths.claude_skills_dir().join("tm-workflow").exists(),
            "the bundled skill must NOT land in the operator's ~/.claude/skills"
        );
        assert!(deploy.stats.deployed.contains(&"tm-workflow".to_string()));
    }

    #[test]
    fn user_custom_skills_reach_the_operator_home() {
        // A user-custom skill is the operator's own opt-in and still deploys to
        // both tiers — only the BUNDLED tier is declined at the home tier.
        let base = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(base.path());
        write_source(&paths.skill_source_dir(), "tm-workflow", "bundled body");
        write_source(&paths.user_skill_source_dir(), "my-skill", "user body");

        deploy_install_skill_tiers(&paths).unwrap();

        assert!(
            paths
                .claude_skills_dir()
                .join("my-skill")
                .join("SKILL.md")
                .is_file(),
            "a user-custom skill must still reach ~/.claude/skills"
        );
        assert!(
            paths
                .skill_deploy_dir()
                .join("my-skill")
                .join("SKILL.md")
                .is_file(),
            "a user-custom skill must also reach the managed tier"
        );
    }
}
