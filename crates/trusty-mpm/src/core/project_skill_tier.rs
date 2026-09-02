//! Deploy the PROJECT skill tier when the project manifest changes (#4880).
//!
//! Why: `<workspace>/.claude/skills` is the PROJECT tier. It was written by
//! exactly two callers — `session_launch::prepare_session` and `tm sessions
//! sync-assets` — and neither runs on a resume or on a bare-`tm` in-place
//! relaunch, so a skill an older binary deployed here stays stale for the life
//! of the workspace. The user tier, by contrast, is refreshed on every run
//! (`managed_config`, #4873).
//!
//! WHICH PRECEDENCE (#4958 — two different orders get called that; the earlier
//! text here conflated them and asserted the wrong one as fact).
//!
//! - DEPLOY-TIME SOURCE precedence — which SOURCE tm writes into ONE
//!   destination: `project-custom > user-custom > bundled`
//!   ([`crate::core::skill_tiers::plan_skill_tiers`]). This module's deploy
//!   obeys it, and it says nothing about what Claude Code loads.
//! - CLAUDE CODE RUNTIME resolution — which on-disk copy loads when the same
//!   name exists in two directories: `enterprise > personal > project >
//!   bundled`. For SKILLS personal beats project, so `$CLAUDE_CONFIG_DIR/skills`
//!   OUTRANKS this destination. AGENTS run the order the other way (project
//!   beats user), which is why #4408's project-tier stub beat the real 25 KB
//!   agent — that precedent does NOT transfer to skills.
//!
//! So a stale copy here never "beats" the current user-tier one; a same-named
//! skill under `$CLAUDE_CONFIG_DIR/skills` wins. Refreshing this tier still
//! matters because it is what loads for every name the managed roster does not
//! carry — the project-custom case above all.
//!
//! WHAT TRIGGERS A REDEPLOY (owner ruling 2026-08-05, refined on PR #4882).
//! Two things, and deliberately not a third.
//!
//! 1. **The binary VERSION changed.** "When version is updated, we re-run
//!    deployment." A version bump is the signal that the shipped skill set moved
//!    — the manifest does not need to see skill CONTENT, because the version
//!    already stands for it. The consequence, accepted rather than worked
//!    around: two builds carrying the same version but different skill text do
//!    not redeploy. That is a development-time condition, not a shipped one.
//! 2. **The manifest's SKILL SELECTION changed.** This is kept because it can
//!    genuinely move without a version bump: `HarnessPlan`'s `skill_source`,
//!    `skill_include`, and `skill_exclude` come straight from the resolved
//!    manifest's `[skills]` section (`manifest::apply`), and the highest-
//!    precedence layer feeding it — `<harness-root>/.trusty-mpm/framework/
//!    manifest.toml` — is the operator's own file, editable at any moment. An
//!    operator excluding a skill on a Tuesday must not wait for a release.
//!    Nothing else from the manifest enters the stamp: a `[style]` or `[mcp]`
//!    edit cannot change which skills this tier should hold.
//!
//! Two files that could also answer to "the project manifest" are NOT the
//! trigger. `.trusty-mpm-skills-manifest.json` is the deploy target's ownership
//! ledger, written BY `deployer::deploy_skills_filtered` at the end of every
//! deploy — keying on it never converges. The bundled `framework-manifest.toml`
//! is compiled in, so a change to it is a version change; it reaches the stamp
//! through component 1 already.
//!
//! What: [`ensure_project_skill_tier`] resolves the manifest, compares
//! [`project_tier_stamp`] against
//! `<project>/.claude/skills/.trusty-mpm-project-tier-stamp`, and returns
//! without touching a file when they agree — mirroring
//! [`crate::core::agent_source::ensure_agent_source_fresh`]'s sha256-stamp
//! no-op. When they differ it runs the ordinary [`deploy_all_skill_tiers`] and
//! rewrites the stamp.
//!
//! CUSTOM SKILLS SURVIVE A VERSION-TRIGGERED REDEPLOY. That is the point of the
//! model's third clause — local customizations are tracked in the local manifest
//! and outlive an upgrade. This path adds no new write rule to make that true:
//! a project-custom skill is dropped by the tier planner before any file I/O,
//! and a checksum-frozen hand edit is skipped by
//! `deployer::deploy_one_file`. Both are reused unchanged.
//! Test: `crates/trusty-mpm/src/core/project_skill_tier_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::error::Result;
use crate::core::manifest::{HarnessPlan, ManifestSources, resolve_manifest};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::{Shadow, deploy_all_skill_tiers, list_source_stems};

/// Marker file recording the project manifest stamp the deployed project tier
/// was last written for.
///
/// Why: named once so the writer here, the reader here, and any future doctor
/// check cannot drift. The leading `.` matters: `deployer::is_skill_file`
/// rejects dot-files and `tiers::list_project_custom_stems` only considers
/// directories holding a `SKILL.md`, so this file is invisible to both the
/// deployer and the tier planner. It sits beside the ownership ledger
/// (`.trusty-mpm-skills-manifest.json`) that is already there.
/// What: `.trusty-mpm-project-tier-stamp`.
/// Test: `stamp_file_is_ignored_by_the_tier_planner`.
pub const PROJECT_TIER_STAMP_FILE: &str = ".trusty-mpm-project-tier-stamp";

/// The bundled-skill selection for the PROJECT tier — nothing (#6586).
///
/// Why: owner ruling 2026-09-01, "these should be user level only", the same
/// principle #4448 settled for bundled AGENTS. Every bundled `tm-*` skill was
/// deployed BOTH to the managed user tier and to each project's
/// `.claude/skills/`, byte-identical, so an upgrade had two copies to keep in
/// step and a drifted project copy was indistinguishable from a deliberate
/// customization.
///
/// This costs no coverage, which is the fact that makes it safe rather than
/// merely tidier. `managed_config::ensure_managed_config_dir` deploys the
/// bundled roster to the user tier with `|_| true` — the WHOLE roster, not the
/// manifest-selected subset — so every stem this predicate now declines is
/// already on disk one tier up. And Claude Code resolves skills
/// `enterprise > personal > project > bundled`, so for skills the user tier
/// OUTRANKS the project tier anyway (see this module's WHICH PRECEDENCE note):
/// the project copy was never the one that loaded. DOC-42's co-deploy guarantee
/// (§SPEC-AGENTSKILLS-02) is met by that same user-tier deploy, which is why the
/// `co_deploy_skills` override is dropped at the project-tier call sites rather
/// than routed around this predicate.
///
/// What: a named function rather than an inline `|_| false` at three call sites,
/// so the ruling lives in one place and a reader who finds it at a deploy site
/// gets the reasoning. Only the BUNDLED tier is declined —
/// `deploy_all_skill_tiers` applies `select` to the bundled stem set alone, so
/// user-custom skills still deploy here and project-custom skills are still
/// never overwritten.
/// Test: `project_tier_receives_no_bundled_skill`,
/// `bundled_skill_reaches_the_user_tier_only`.
pub fn bundled_excluded_from_project_tier(_stem: &str) -> bool {
    false
}

/// What one [`ensure_project_skill_tier`] call did.
///
/// Why: the call site logs "the project tier was refreshed" only when a deploy
/// actually ran, and the tests assert the no-op path was taken rather than
/// inferring it from unchanged mtimes alone.
/// What: `deployed` is `false` for the stamp-matched no-op and for a refusal
/// (a missing or empty skill source); `stats` and `shadowed` are empty in both
/// those cases.
/// Test: `unchanged_version_is_a_noop`, `version_bump_redeploys`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectTierDeploy {
    /// Whether a deploy ran (and the stamp was rewritten) this call.
    pub deployed: bool,
    /// The deploy's own stats, empty when nothing ran.
    pub stats: DeployStats,
    /// Every tier collision the planner resolved this run.
    ///
    /// Why: a project-custom skill is preserved by TWO independent mechanisms,
    /// and `stats` only shows one of them. The planner drops a shadowed stem
    /// from `bundled_deploy` before any file I/O, so it never reaches
    /// `deploy_one_file`'s unmanaged-target skip at all and appears in no
    /// `stats` vector. Surfacing the shadow record is what makes that first,
    /// stronger guarantee assertable instead of inferred from an empty result.
    /// What: the planner's [`crate::core::skill_tiers::Shadow`] list verbatim.
    /// Test: `project_custom_skill_is_never_overwritten`.
    pub shadowed: Vec<Shadow>,
}

/// The project-tier deploy destination for a workspace.
///
/// Why: computing it here rather than reading `fw.claude_skills_dir()` removes
/// a footgun — a caller holding a home-scoped [`FrameworkPaths`] would
/// otherwise aim this deploy at the operator's real `~/.claude/skills`. The
/// SOURCE paths still come from `fw`, exactly as
/// `FrameworkPaths::for_managed_project` arranges them.
/// What: `<project_dir>/.claude/skills`.
/// Test: `deploys_into_the_workspace_not_the_framework_home`.
pub fn project_skill_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("skills")
}

/// Fingerprint the two things that trigger a redeploy: the binary version and
/// the manifest's skill selection.
///
/// Why: see the module doc for why those two and nothing else — in particular
/// why skill CONTENT is deliberately absent (the version stands for it) and why
/// the selection is deliberately present (the operator's project override can
/// move it between releases).
/// What: sha256 over `<version>\0<skill_source>\0<include…>\0<exclude…>`, with
/// the include/exclude lists joined by a separator no glob pattern contains.
/// Order within each list is the manifest's own and is treated as significant —
/// reordering is a manifest edit, and one spurious redeploy is cheaper than
/// sorting a list whose order the schema does not promise to be irrelevant.
/// Pure — no I/O.
/// Test: `stamp_is_stable_across_calls`, `stamp_changes_with_the_version`,
/// `stamp_changes_with_the_skill_selection`,
/// `stamp_ignores_manifest_changes_outside_the_skill_selection`.
pub fn project_tier_stamp(version: &str, plan: &HarnessPlan) -> String {
    checksum(&format!(
        "{version}\0{}\0{}\0{}",
        plan.skill_source.display(),
        plan.skill_include.join("\u{1}"),
        plan.skill_exclude.join("\u{1}"),
    ))
}

/// Deploy `<project_dir>/.claude/skills` when the project manifest stamp moved,
/// and do nothing at all when it did not.
///
/// Why: this is the trigger the owner's 2026-08-05 ruling asks for. It is safe
/// to call unconditionally on every spawn, resume, and in-place relaunch: the
/// steady-state cost is resolving the manifest (two small TOML reads) plus one
/// stamp-file read, and NOTHING is written when the stamp matches.
/// What: resolves the manifest and [`HarnessPlan`] for `project_dir` exactly as
/// `session_launch::prepare_session` does, compares [`project_tier_stamp`]
/// against the on-disk marker, and returns a no-op when they agree. Otherwise it
/// runs [`deploy_all_skill_tiers`] over the manifest-selected bundled tier plus
/// the user-custom tier, then writes the stamp.
///
/// `fw` supplies SOURCE paths only (`plan.skill_source`,
/// `fw.user_skill_source_dir()`); the destination is always
/// [`project_skill_dir`], so a home-scoped `fw` cannot redirect this into
/// `~/.claude/skills`.
///
/// A missing OR EMPTY skill source is a refusal, not an empty success: the stamp
/// stays unwritten so the next run tries again rather than recording "zero
/// skills" as current. Emptiness matters on its own because an unfetched
/// `agents/skills` submodule is an existing directory with nothing in it.
///
/// The DOC-42 co-deploy widening (`agent_skill_codeploy::co_deploy_skill_set`)
/// is deliberately NOT folded in here. It is derived from
/// `deploy_agents_filtered`'s `declared_skills`, which only a path that deploys
/// agents has; `prepare_session` and `sync-assets` own that and keep it. This
/// path refreshes what the manifest selects and never removes anything —
/// deselection has never deleted a deployed copy — so a co-deployed skill those
/// paths wrote survives untouched.
/// Test: `version_bump_redeploys`, `unchanged_version_is_a_noop`,
/// `skill_selection_change_redeploys`,
/// `project_custom_skill_is_never_overwritten`, `frozen_skill_is_still_skipped`,
/// `missing_skill_source_does_not_stamp`, `empty_skill_source_does_not_stamp`.
pub fn ensure_project_skill_tier(
    fw: &FrameworkPaths,
    project_dir: &Path,
) -> Result<ProjectTierDeploy> {
    ensure_project_skill_tier_for_version(fw, project_dir, env!("CARGO_PKG_VERSION"))
}

/// Hermetic core of [`ensure_project_skill_tier`], taking the version
/// explicitly.
///
/// Why: `env!("CARGO_PKG_VERSION")` is a compile-time constant, so "a version
/// bump redeploys" is not assertable without injecting it. Same split as
/// `managed_config::ensure_managed_config_dir` / `_with_root`.
/// What: as [`ensure_project_skill_tier`], with `version` as the stamp's first
/// component.
/// Test: `version_bump_redeploys`, `unchanged_version_is_a_noop`.
pub fn ensure_project_skill_tier_for_version(
    fw: &FrameworkPaths,
    project_dir: &Path,
    version: &str,
) -> Result<ProjectTierDeploy> {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources = ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    let dest = project_skill_dir(project_dir);
    let stamp_path = dest.join(PROJECT_TIER_STAMP_FILE);
    let current = project_tier_stamp(version, &plan);
    if std::fs::read_to_string(&stamp_path).ok().as_deref() == Some(current.as_str()) {
        return Ok(ProjectTierDeploy::default());
    }

    // PR #4882 review (MEDIUM): refuse on an EMPTY source, not merely a missing
    // one. `FrameworkPaths::skill_source_dir` prefers the `agents/skills`
    // submodule whenever that path EXISTS, and an unfetched or shallow submodule
    // is an existing-but-empty directory — so an `is_dir()`-only guard passes,
    // the stamp records success, and the project tier stays stale forever with
    // no signal. That is the silent-staleness class of #4840 / #4873: a guard
    // that reports success while doing nothing. Counting the stems the deployer
    // would actually act on is what closes it.
    if list_source_stems(&plan.skill_source)?.is_empty() {
        tracing::warn!(
            project_dir = %project_dir.display(),
            skill_source = %plan.skill_source.display(),
            "project skill tier NOT deployed — the skill source directory is missing or \
             holds no skill files (an unfetched `agents/skills` submodule looks exactly \
             like this). The stamp is left unwritten so the next run retries; the \
             deployed project tier may be stale until then."
        );
        return Ok(ProjectTierDeploy::default());
    }

    // #6586: bundled skills are user-tier only.
    let deploy = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &dest,
        bundled_excluded_from_project_tier,
    )?;

    std::fs::create_dir_all(&dest)?;
    atomic_write(&stamp_path, &current)?;

    tracing::info!(
        project_dir = %project_dir.display(),
        deployed = deploy.stats.deployed.len(),
        skipped = deploy.stats.skipped.len(),
        shadowed = deploy.shadowed.len(),
        "project skill tier refreshed — the binary version or the manifest's skill selection moved"
    );

    Ok(ProjectTierDeploy {
        deployed: true,
        stats: deploy.stats,
        shadowed: deploy.shadowed,
    })
}

#[cfg(test)]
#[path = "project_skill_tier_tests.rs"]
mod tests;
