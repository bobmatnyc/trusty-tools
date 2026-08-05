//! Deploy the PROJECT skill tier when the project manifest changes (#4880).
//!
//! Why: `<workspace>/.claude/skills` is the PROJECT tier, and project outranks
//! user. It was written by exactly two callers —
//! `session_launch::prepare_session` and `tm sessions sync-assets` — and neither
//! runs on a resume or on a bare-`tm` in-place relaunch. The user tier, by
//! contrast, is refreshed on every run (`managed_config`, #4873). So a
//! project-tier skill an older binary deployed keeps winning over the current
//! user-tier copy, silently, for the life of the workspace. That is the #4408
//! shadowing shape one tier down: there, a 32-byte project-tier stub beat the
//! real 25KB agent.
//!
//! WHAT "THE PROJECT MANIFEST" MEANS HERE (owner ruling 2026-08-05: "when the
//! project manifest is updated"). Three files could answer to that name; only
//! one can be the trigger.
//!
//! - `.trusty-mpm-skills-manifest.json` — the deploy target's own ownership
//!   ledger, written BY `skills::deployer::deploy_skills_filtered` at the end of
//!   every deploy. Triggering on it would be circular: each deploy updates it,
//!   so "deploy when it changes" never converges. It is deploy OUTPUT, not
//!   input.
//! - `framework-manifest.toml` — the bundled framework-tier declaration
//!   (`manifest::framework`), compiled into the binary. It is one INPUT LAYER to
//!   resolution, not the whole answer: a project override
//!   (`<harness-root>/.trusty-mpm/framework/manifest.toml`) sits above it and a
//!   synced catalog manifest between them.
//! - The RESOLVED manifest — `resolve_manifest(&ManifestSources::resolve(…))`,
//!   i.e. compiled default ⊕ framework tier ⊕ catalog ⊕ project override. This
//!   is the trigger, because it is the only value that decides what the project
//!   tier should contain, and it subsumes both operator-editable layers and the
//!   bundled `framework-manifest.toml`.
//!
//! [`project_tier_stamp`] fingerprints that resolved manifest AND
//! [`crate::core::skill_source::skill_bundle_stamp`]. The second component is
//! load-bearing, not belt-and-suspenders: the manifest declares WHICH skills
//! deploy, never their CONTENT, so a binary that merely edits a skill's text
//! leaves every layer byte-identical. Without the bundle stamp the project tier
//! would stay stale across exactly the upgrade this issue exists to fix.
//!
//! What: [`ensure_project_skill_tier`] resolves the manifest, compares the stamp
//! against `<project>/.claude/skills/.trusty-mpm-project-tier-stamp`, and
//! returns without touching a file when they agree — mirroring
//! [`crate::core::agent_source::ensure_agent_source_fresh`]'s sha256-stamp
//! no-op. When they differ it runs the ordinary
//! [`deploy_all_skill_tiers`] and rewrites the stamp. Custom skills keep every
//! protection they already had, because this path adds no new write rule: an
//! unmanaged (project-custom) skill and a checksum-frozen hand edit are both
//! skipped by `skills::deployer::deploy_one_file`, which this reuses unchanged.
//! Test: `crates/trusty-mpm/src/core/project_skill_tier_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::error::Result;
use crate::core::manifest::{HarnessManifest, HarnessPlan, ManifestSources, resolve_manifest};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::{Shadow, deploy_all_skill_tiers};

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

/// What one [`ensure_project_skill_tier`] call did.
///
/// Why: the call site logs "the project tier was refreshed" only when a deploy
/// actually ran, and the tests assert the no-op path was taken rather than
/// inferring it from unchanged mtimes alone.
/// What: `deployed` is `false` for the stamp-matched no-op and for a refusal
/// (missing skill source); `stats` and `shadowed` are empty in both those cases.
/// Test: `unchanged_manifest_is_a_noop`, `manifest_change_refreshes_a_stale_skill`.
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

/// Fingerprint the inputs that decide what the project skill tier should hold.
///
/// Why: see the module doc for why the RESOLVED manifest — not the deployed
/// `.trusty-mpm-skills-manifest.json`, and not `framework-manifest.toml` alone
/// — is "the project manifest", and why the bundled skill-content stamp joins
/// it.
/// What: sha256 over the resolved [`HarnessManifest`] serialized as JSON, a NUL
/// separator, and [`crate::core::skill_source::skill_bundle_stamp`]. JSON, not
/// TOML: serde emits struct fields in declaration order and `BTreeMap` keys
/// sorted, so the encoding is stable run to run, without TOML's
/// values-before-tables ordering constraints on nested optional sections. Pure —
/// no I/O beyond the compiled-in bundle already in memory.
/// Test: `stamp_is_stable_across_calls`, `stamp_changes_with_the_manifest`.
pub fn project_tier_stamp(manifest: &HarnessManifest) -> Result<String> {
    let selection = serde_json::to_string(manifest)?;
    Ok(checksum(&format!(
        "{selection}\0{}",
        crate::core::skill_source::skill_bundle_stamp()
    )))
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
/// A missing skill source is a refusal, not an empty success: the stamp stays
/// unwritten so the next run tries again rather than recording "zero skills" as
/// current.
///
/// The DOC-42 co-deploy widening (`agent_skill_codeploy::co_deploy_skill_set`)
/// is deliberately NOT folded in here. It is derived from
/// `deploy_agents_filtered`'s `declared_skills`, which only a path that deploys
/// agents has; `prepare_session` and `sync-assets` own that and keep it. This
/// path refreshes what the manifest selects and never removes anything —
/// deselection has never deleted a deployed copy — so a co-deployed skill those
/// paths wrote survives untouched.
/// Test: `manifest_change_refreshes_a_stale_skill`, `unchanged_manifest_is_a_noop`,
/// `project_custom_skill_is_never_overwritten`, `frozen_skill_is_still_skipped`,
/// `missing_skill_source_does_not_stamp`.
pub fn ensure_project_skill_tier(
    fw: &FrameworkPaths,
    project_dir: &Path,
) -> Result<ProjectTierDeploy> {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources = ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    let dest = project_skill_dir(project_dir);
    let stamp_path = dest.join(PROJECT_TIER_STAMP_FILE);
    let current = project_tier_stamp(&manifest)?;
    if std::fs::read_to_string(&stamp_path).ok().as_deref() == Some(current.as_str()) {
        return Ok(ProjectTierDeploy::default());
    }

    if !plan.skill_source.is_dir() {
        tracing::warn!(
            project_dir = %project_dir.display(),
            skill_source = %plan.skill_source.display(),
            "project skill tier not deployed — the skill source directory is missing; \
             leaving the stamp unwritten so the next run retries"
        );
        return Ok(ProjectTierDeploy::default());
    }

    let deploy = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &dest,
        |name| plan.skill_selected(name),
    )?;

    std::fs::create_dir_all(&dest)?;
    atomic_write(&stamp_path, &current)?;

    tracing::info!(
        project_dir = %project_dir.display(),
        deployed = deploy.stats.deployed.len(),
        skipped = deploy.stats.skipped.len(),
        shadowed = deploy.shadowed.len(),
        "project skill tier refreshed — the project manifest changed since the last deploy"
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
