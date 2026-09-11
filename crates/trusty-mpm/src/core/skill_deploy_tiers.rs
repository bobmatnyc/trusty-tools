//! Every directory a bundled skill can be deployed into (issue #4605).
//!
//! Why: skills, unlike agents (#4409), have no single canonical destination.
//! `tm install` writes the operator's `~/.claude/skills`,
//! `standalone::global_config::ensure_global_config_dir` writes the tm-managed
//! `$CLAUDE_CONFIG_DIR/skills`, and a managed session writes its workspace's
//! `<project>/.claude/skills`. The #4605 defect — a bundled skill absent from
//! its target's manifest is classified project-custom and silently dropped
//! from every deploy — occurs independently at each one, and it was measured
//! at two of them simultaneously. A reporter or reconcile that covered only
//! the tier it happened to be invoked from would leave the others permanently
//! stale, which is the defect again with a smaller blast radius.
//!
//! What: [`skill_deploy_tiers`] enumerates those directories, deduplicated and
//! labelled, from a resolved [`FrameworkPaths`] plus an optional project
//! directory. Pure path arithmetic — it never reads or creates anything, and a
//! tier that does not exist on disk is still listed (its scan simply finds
//! nothing).
//!
//! Test: the `tests` module below.

use std::path::{Path, PathBuf};

use crate::core::paths::FrameworkPaths;

/// One directory bundled skills deploy into.
///
/// Why: every #4605 report is per-tier — "unmanaged at `~/.claude/skills`" and
/// "unmanaged at `$CLAUDE_CONFIG_DIR/skills`" are different facts needing
/// different remediation scope — so the human label travels with the path.
/// What: a short stable label for output and the directory itself.
/// Test: `tiers_include_the_managed_config_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDeployTier {
    /// Short label for operator-facing output (`managed config`, `operator
    /// home`, `project`).
    pub label: &'static str,
    /// The `skills/` directory itself.
    pub dir: PathBuf,
    /// Whether the WHOLE bundled roster is expected here (#7423).
    ///
    /// Why: only [`FrameworkPaths::skill_deploy_dir`] receives every bundled
    /// skill — the 2026-09-01 ruling (#6586) made the operator home and the
    /// project tier user-custom-only, and `tm install` declines bundled stems
    /// at both. So "a bundled skill is absent here" is a finding at exactly one
    /// tier and normal everywhere else; carrying the answer on the tier keeps
    /// the audit from asserting a roster the deploy would never write.
    /// What: `true` for the managed-config tier alone.
    /// Test: `only_the_managed_tier_expects_the_bundled_roster`.
    pub receives_bundled_roster: bool,
}

/// The project tier's skills directory: `<project_dir>/.claude/skills`.
///
/// Why (#6586): three call sites spelled this join by hand — the tier
/// enumeration below, the stray sweep, and the `skill_project_tier` probe — and
/// the sweep's deferral has to name the SAME directory the enumeration yields
/// or the redeploy skips nothing. One spelling, one answer.
/// What: `<project_dir>/.claude/skills`. Pure path arithmetic; touches no disk.
/// Test: `tiers_add_the_project_tier`.
pub fn project_skill_tier(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("skills")
}

/// Enumerate every skill deploy target, deduplicated, highest-reach first.
///
/// Why: see the module doc — one call site listing the tiers keeps `tm
/// install`'s reporter, `tm install --reconcile-skills`, and `tm doctor` from
/// each covering a different subset and disagreeing about which tiers are
/// clean.
/// What: `$CLAUDE_CONFIG_DIR/skills` (the tm-global roster EVERY managed
/// session reads — derived as the `skills` sibling of
/// [`FrameworkPaths::agent_deploy_dir`], which #4409 pins to the managed
/// `claude-config` dir and which `for_managed_project`/`for_managed_workspace`
/// deliberately never rewrite), then [`FrameworkPaths::claude_skills_dir`]
/// (the operator's `~/.claude/skills`, or the workspace's own when `paths` was
/// resolved for a managed workspace), then `<project_dir>/.claude/skills` when
/// a project is supplied. Later duplicates are dropped, so a managed-workspace
/// `paths` whose `claude_skills_dir` already IS the project tier yields one
/// entry, not two.
/// Test: `tiers_include_the_managed_config_dir`, `tiers_add_the_project_tier`,
/// `tiers_deduplicate_identical_paths`.
pub fn skill_deploy_tiers(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> Vec<SkillDeployTier> {
    // #6586: one derivation, on `FrameworkPaths`, shared with the session
    // launch's bundled deploy and `deploy_validate`'s probe.
    let managed = paths.skill_deploy_dir();

    // #7423: the third element says whether the whole bundled roster belongs
    // here. Only the managed tier — see `SkillDeployTier::receives_bundled_roster`.
    let candidates = [
        Some(("managed config", managed, true)),
        Some(("operator home", paths.claude_skills_dir(), false)),
        project_dir.map(|dir| ("project", project_skill_tier(dir), false)),
    ];

    let mut tiers: Vec<SkillDeployTier> = Vec::new();
    for (label, dir, receives_bundled_roster) in candidates.into_iter().flatten() {
        if tiers.iter().any(|t| t.dir == dir) {
            continue;
        }
        tiers.push(SkillDeployTier {
            label,
            dir,
            receives_bundled_roster,
        });
    }
    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tiers_include_the_managed_config_dir() {
        // The tm-global roster every managed session reads must always be
        // covered — it is the tier #4605 was measured stale at.
        let base = TempDir::new().unwrap();
        let paths = FrameworkPaths::under(base.path());
        let tiers = skill_deploy_tiers(&paths, None);

        assert_eq!(tiers[0].label, "managed config");
        assert_eq!(tiers[0].dir, paths.skill_deploy_dir());
        assert!(tiers.iter().any(|t| t.dir == paths.claude_skills_dir()));
    }

    // #7423: a bundled skill missing from the managed tier is a finding; the
    // same skill missing from the operator home or the project tier is the
    // 2026-09-01 ruling working as intended (#6586).
    #[test]
    fn only_the_managed_tier_expects_the_bundled_roster() {
        let base = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let paths = FrameworkPaths::under(base.path());
        let tiers = skill_deploy_tiers(&paths, Some(project.path()));

        let expecting: Vec<&str> = tiers
            .iter()
            .filter(|t| t.receives_bundled_roster)
            .map(|t| t.label)
            .collect();
        assert_eq!(expecting, vec!["managed config"], "tiers: {tiers:?}");
    }

    #[test]
    fn tiers_add_the_project_tier() {
        let base = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let paths = FrameworkPaths::under(base.path());
        let tiers = skill_deploy_tiers(&paths, Some(project.path()));

        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[2].label, "project");
        assert_eq!(tiers[2].dir, project.path().join(".claude").join("skills"));
    }

    #[test]
    fn tiers_deduplicate_identical_paths() {
        // A managed-workspace `paths` resolves `claude_skills_dir` to the
        // project's own `.claude/skills`; listing it twice would double every
        // finding reported against it.
        let base = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let paths =
            FrameworkPaths::for_managed_project(base.path().join(".trusty-mpm"), project.path());
        let tiers = skill_deploy_tiers(&paths, Some(project.path()));

        assert_eq!(tiers.len(), 2, "expected dedup, got {tiers:?}");
        assert_eq!(tiers[1].dir, project.path().join(".claude").join("skills"));
    }
}
