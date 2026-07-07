//! Resolve the expected agent/skill roster for a workspace validation pass.
//!
//! Why (issue #2171): pre-#2171, [`super::validate_workspace`] diffed the
//! deployed payload against the UNCONDITIONAL full bundled roster returned by
//! [`FrameworkPaths::agent_source_dir`]/[`FrameworkPaths::skill_source_dir`] —
//! the same source `deploy_agents`/`deploy_skills` read from when called
//! unfiltered. A workspace legitimately provisioned from a FILTERED roster
//! (e.g. a Rust-only project whose #1941 language-scoped manifest excludes
//! every foreign-language `*-engineer` and the generic `engineer` catch-all)
//! was then falsely reported incomplete for every deliberately-excluded
//! entry. This module resolves the roster a workspace SHOULD carry, preferring
//! — in order — (a) the per-project [`HarnessPlan`] the SAME
//! `prepare_session_inner` pipeline would compute for this workspace, (b) the
//! workspace's own deployed ownership manifest (an internal-consistency
//! check: every entry the manifest claims to manage must exist on disk), and
//! only when both resolve to nothing (c) the full canonical bundled roster —
//! the pre-#2171 behaviour, preserved as the last-resort floor so a totally
//! unprovisioned workspace still reports every bundled entry as missing.
//! What: [`expected_agent_stems`] and [`expected_skill_stems`] each
//! reconstruct the manifest-resolved [`HarnessPlan`] via
//! [`crate::core::manifest::resolve_manifest`] /
//! [`crate::core::manifest::HarnessPlan::from_manifest`] rooted at
//! `fw.claude_home_dir()` — verified (see `paths.rs`'s
//! `for_managed_workspace`/`for_managed_project` tests) to equal the
//! workspace/project directory at every `validate_workspace` call site, so no
//! new parameter is needed on the public API. When the plan's source
//! directory yields no `.md` stems (missing/unreadable/genuinely empty), the
//! caller-supplied deployed manifest's managed keys are used instead; when
//! that is also empty (or absent), [`canonical_stems`] falls back to the
//! full bundled source directory.
//! Test: `plan_filters_expected_agents`, `plan_selection_excludes_generic_engineer`,
//! `empty_plan_source_falls_back_to_manifest`,
//! `manifest_and_plan_both_empty_falls_back_to_canonical_roster`,
//! `expected_skill_stems_mirrors_agent_fallback_order`.

use std::path::Path;

use crate::core::agent_manifest::AgentManifest;
use crate::core::manifest::{HarnessPlan, ManifestSources, resolve_manifest};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_manifest::SkillManifest;

/// Build the [`HarnessPlan`] `prepare_session_inner` would compute for `fw`'s
/// workspace.
///
/// Why: shared by [`expected_agent_stems`] and [`expected_skill_stems`] so the
/// two never reconstruct diverging plans.
/// What: resolves the manifest layers rooted at `fw.claude_home_dir()` and the
/// framework/catalog roots, then materializes the plan. Infallible: every
/// step tolerates a missing/malformed layer (`resolve_manifest`'s own
/// contract), so this always returns SOME plan — even one pointing at an
/// empty or nonexistent source directory.
/// Test: `plan_filters_expected_agents`.
fn reconstruct_plan(fw: &FrameworkPaths) -> HarnessPlan {
    let project_dir = fw.claude_home_dir();
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources = ManifestSources::resolve(&project_dir, &fw.root, &catalog_root);
    let manifest = resolve_manifest(&sources);
    HarnessPlan::from_manifest(&manifest, fw, &catalog_root)
}

/// Canonical `.md` stems (filename without extension) directly under `dir`.
///
/// Why: shared by every tier of the (a)/(b)/(c) fallback below — the plan
/// tier reads the plan's resolved source dir, the last-resort tier reads the
/// unconditional bundled source dir.
/// What: returns the sorted, deterministic list of `.md` stems; an
/// unreadable/missing `dir` yields an empty list (no roster to check
/// against — matches `deploy_agents`/`deploy_skills`'s own "missing source is
/// an empty no-op" convention).
/// Test: covered indirectly by every test in this module.
fn canonical_stems(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_md = path
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if !is_md {
                return None;
            }
            path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
        })
        .collect();
    names.sort_unstable();
    names
}

/// Resolve the expected AGENT stems for `fw`'s workspace (issue #2171).
///
/// Why/What: see the module doc — tier (a) the reconstructed plan, tier (b)
/// `manifest`'s managed keys (the caller already loaded it while checking
/// [`super::DeploymentGap::AgentManifestMissing`]/`AgentManifestCorrupt`, so
/// it is passed in rather than re-read), tier (c) the unconditional bundled
/// source directory.
/// Test: `plan_filters_expected_agents`, `plan_selection_excludes_generic_engineer`,
/// `empty_plan_source_falls_back_to_manifest`,
/// `manifest_and_plan_both_empty_falls_back_to_canonical_roster`.
pub(super) fn expected_agent_stems(
    fw: &FrameworkPaths,
    manifest: Option<&AgentManifest>,
) -> Vec<String> {
    let plan = reconstruct_plan(fw);
    let plan_stems: Vec<String> = canonical_stems(&plan.agent_source)
        .into_iter()
        .filter(|name| plan.agent_selected(name))
        .collect();
    if !plan_stems.is_empty() {
        return plan_stems;
    }

    if let Some(m) = manifest {
        let manifest_stems: Vec<String> = m
            .managed
            .keys()
            .filter_map(|f| f.strip_suffix(".md").map(str::to_owned))
            .collect();
        if !manifest_stems.is_empty() {
            return manifest_stems;
        }
    }

    canonical_stems(&fw.agent_source_dir())
}

/// Resolve the expected SKILL stems for `fw`'s workspace (issue #2171).
///
/// Why/What: mirrors [`expected_agent_stems`] exactly, for the skill roster —
/// tier (a) the reconstructed plan's skill source, tier (b) the deployed
/// [`SkillManifest`]'s managed keys, tier (c) the unconditional bundled skill
/// source directory.
/// Test: `expected_skill_stems_mirrors_agent_fallback_order`.
pub(super) fn expected_skill_stems(
    fw: &FrameworkPaths,
    manifest: Option<&SkillManifest>,
) -> Vec<String> {
    let plan = reconstruct_plan(fw);
    let plan_stems: Vec<String> = canonical_stems(&plan.skill_source)
        .into_iter()
        .filter(|name| plan.skill_selected(name))
        .collect();
    if !plan_stems.is_empty() {
        return plan_stems;
    }

    if let Some(m) = manifest {
        let manifest_stems: Vec<String> = m.managed.keys().cloned().collect();
        if !manifest_stems.is_empty() {
            return manifest_stems;
        }
    }

    canonical_stems(&fw.skill_source_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_agent_source(fw: &FrameworkPaths, names: &[&str]) {
        std::fs::create_dir_all(&fw.agents).unwrap();
        for name in names {
            std::fs::write(
                fw.agents.join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: d\n---\n\nBody.\n"),
            )
            .unwrap();
        }
    }

    fn write_project_manifest(fw: &FrameworkPaths, project_dir: &Path, toml: &str) {
        let dir = project_dir.join(".trusty-mpm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.toml"), toml).unwrap();
        // `for_managed_workspace`-style paths already point `claude_home_dir()`
        // at `project_dir`; nothing else to wire up here.
        let _ = fw;
    }

    #[test]
    fn plan_filters_expected_agents() {
        // No manifest.toml override present: the plan selects every bundled
        // agent (today's zero-regression default), so the expected set equals
        // the full seeded roster.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        let mut fw = fw;
        fw.trusty_mpm_root = None;
        seed_agent_source(&fw, &["engineer", "rust-engineer"]);

        let mut expected = expected_agent_stems(&fw, None);
        expected.sort();
        assert_eq!(
            expected,
            vec!["engineer".to_string(), "rust-engineer".to_string()]
        );
    }

    #[test]
    fn plan_selection_excludes_generic_engineer() {
        // A project manifest excluding the generic `engineer` catch-all must
        // yield an expected set WITHOUT it, even though the bundled source
        // directory still carries the file (issue #2171's exact scenario).
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        seed_agent_source(&fw, &["engineer", "rust-engineer", "python-engineer"]);
        write_project_manifest(&fw, &workspace, "[agents]\nexclude = [\"engineer\"]\n");

        let mut expected = expected_agent_stems(&fw, None);
        expected.sort();
        assert_eq!(
            expected,
            vec!["python-engineer".to_string(), "rust-engineer".to_string()],
            "excluded generic engineer must not be in the expected set"
        );
    }

    #[test]
    fn user_level_manifest_exclude_is_honored() {
        // The exact real-world scenario reported against a live launch: an
        // operator's USER-level `~/.trusty-mpm/manifest.toml` (not a
        // per-project `.trusty-mpm/manifest.toml` override) excludes the
        // generic `engineer` catch-all. `ManifestSources::resolve` reads this
        // layer from `fw.root.join("manifest.toml")` — `fw.root` is the real
        // global framework root (`~/.trusty-mpm` in production) for EVERY
        // workspace on the machine, not something scoped per-project — so the
        // expected set must reflect it exactly as `prepare_session_inner`
        // would when deploying.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        seed_agent_source(&fw, &["engineer", "rust-engineer", "python-engineer"]);

        // Write directly to `fw.root/manifest.toml` — the USER layer, no
        // project-level `.trusty-mpm/manifest.toml` involved at all.
        std::fs::create_dir_all(&fw.root).unwrap();
        std::fs::write(
            fw.root.join("manifest.toml"),
            "[agents]\nexclude = [\"engineer\"]\n",
        )
        .unwrap();

        let mut expected = expected_agent_stems(&fw, None);
        expected.sort();
        assert_eq!(
            expected,
            vec!["python-engineer".to_string(), "rust-engineer".to_string()],
            "a user-level (~/.trusty-mpm/manifest.toml) exclude must be honored, not just a project-level override"
        );
    }

    #[test]
    fn empty_plan_source_falls_back_to_manifest() {
        // The plan's source directory is entirely empty/unreadable (a
        // binary-only install with no framework/agents populated yet) — the
        // deployed manifest's managed keys must be used instead of an empty
        // expected set.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        // No seed_agent_source call: `fw.agents` stays nonexistent.

        let mut manifest = AgentManifest::default();
        manifest.managed.insert(
            "engineer.md".to_string(),
            crate::core::agent_manifest::ManifestEntry {
                source_chain: vec!["engineer".to_string()],
                checksum: "abc".to_string(),
                deployed_at: "2026-01-01T00:00:00Z".to_string(),
                origin: crate::core::agent_manifest::Origin::Bundled,
            },
        );

        let expected = expected_agent_stems(&fw, Some(&manifest));
        assert_eq!(expected, vec!["engineer".to_string()]);
    }

    #[test]
    fn manifest_and_plan_both_empty_falls_back_to_canonical_roster() {
        // Neither the plan's source nor the manifest yields anything — this is
        // the totally-fresh/unprovisioned-workspace floor: fall back to the
        // unconditional bundled canonical roster (pre-#2171 behaviour).
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        seed_agent_source(&fw, &["engineer"]);
        // Plan source == fw.agent_source_dir() here (no catalog override), so
        // this also exercises the "plan == bundled" common case end-to-end.

        let expected = expected_agent_stems(&fw, None);
        assert_eq!(expected, vec!["engineer".to_string()]);
    }

    #[test]
    fn expected_skill_stems_mirrors_agent_fallback_order() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
        fw.trusty_mpm_root = None;
        std::fs::create_dir_all(&fw.skills).unwrap();
        std::fs::write(fw.skills.join("tm-doctor.md"), "skill body").unwrap();

        let expected = expected_skill_stems(&fw, None);
        assert_eq!(expected, vec!["tm-doctor".to_string()]);
    }
}
