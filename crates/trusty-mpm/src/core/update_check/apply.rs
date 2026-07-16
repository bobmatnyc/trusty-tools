//! Catalog apply: the HR-3 rebuild/redeploy offer made concrete (DOC-17).
//!
//! Why: detecting staleness is only half of HR-3 — the operator must be able to
//! ACCEPT the rebuild offer. `tm catalog apply` (and any future TUI keybind) calls
//! [`apply_catalog`] to sync the catalog, redeploy the manifest-selected agents
//! and skills from it (refreshing the checksum manifests so staleness clears), and
//! optionally PRUNE managed artifacts the manifest no longer selects (the removal
//! HR-2 deferred — kept behind an explicit opt-in, never silent).
//! What: [`apply_catalog`] threads a [`GitBackend`] (real git in production, a
//! fake in tests), syncs via [`CatalogSync`], resolves the harness manifest +
//! plan exactly as the launcher does, redeploys via the filtered deployers, and
//! when `prune` is set removes deselected managed files. It returns an
//! [`ApplyReport`] summarising the sync, the deploy counts, and the prune list.
//! Test: this module's `tests` cover redeploy-clears-staleness and prune-removes-
//! deselected against a `FakeGitBackend`-seeded catalog.

use std::path::Path;

use crate::core::agent_deployer::deploy_agents_filtered;
use crate::core::agent_manifest::{AgentManifest, MANIFEST_FILE as AGENT_MANIFEST_FILE};
use crate::core::manifest::HarnessPlan;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_manifest::{SKILL_MANIFEST_FILE, SkillManifest};
use crate::core::skill_tiers::deploy_all_skill_tiers;
use crate::provisioner::GitBackend;

/// A failure raised while applying a catalog update.
///
/// Why: `apply` performs a network-ish sync (via the backend), agent/skill
/// deploys, and filesystem pruning; the CLI needs one typed surface naming which
/// stage failed.
/// What: variants for the sync, the agent deploy, the skill deploy, and prune I/O.
/// Test: surfaced on a backend that fails to clone; the happy path is covered by
/// `apply_redeploys_and_clears_staleness`.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// Syncing the catalog checkout failed.
    #[error("catalog sync failed: {0}")]
    Sync(String),
    /// Redeploying agents failed.
    #[error("agent redeploy failed: {0}")]
    AgentDeploy(String),
    /// Redeploying skills failed.
    #[error("skill redeploy failed: {0}")]
    SkillDeploy(String),
    /// A filesystem operation during pruning failed.
    #[error("prune io error: {0}")]
    Prune(String),
}

/// Summary of one [`apply_catalog`] run.
///
/// Why: the CLI prints what the apply did; callers (and tests) need the counts
/// and lists split by outcome.
/// What: whether the sync actually fetched, the deployed/skipped agent filenames,
/// the deployed/skipped skill stems, and the pruned agent + skill names.
/// Test: `apply_redeploys_and_clears_staleness`, `apply_prune_removes_deselected`.
#[derive(Debug, Default)]
pub struct ApplyReport {
    /// True if the catalog sync performed a real fetch (vs a TTL-fresh skip).
    pub fetched: bool,
    /// Agent filenames (re)written this run.
    pub agents_deployed: Vec<String>,
    /// Agent filenames left as-is (user-modified / unchanged).
    pub agents_skipped: Vec<String>,
    /// Skill stems (re)written this run.
    pub skills_deployed: Vec<String>,
    /// Skill stems left as-is.
    pub skills_skipped: Vec<String>,
    /// Managed agent filenames removed because the manifest no longer selects them.
    pub agents_pruned: Vec<String>,
    /// Managed skill stems removed because the manifest no longer selects them.
    pub skills_pruned: Vec<String>,
}

/// Apply a catalog update: sync, redeploy the selected set, optionally prune.
///
/// Why: this is the single autonomous-rebuild entry point HR-3 names — accepting
/// the staleness offer. It deliberately mirrors the launcher's resolve→plan→deploy
/// sequence so the content `apply` lands is exactly what a fresh session would get,
/// and it refreshes the checksum manifests so a subsequent `detect_staleness`
/// reports fresh.
/// What: syncs the catalog via [`CatalogSync::sync`] (honouring the TTL unless
/// `force`), resolves the harness manifest for `project_dir`, materializes the
/// [`HarnessPlan`], redeploys the selected agents/skills via the filtered
/// deployers, and — when `prune` is set — removes managed agents/skills the plan
/// no longer selects (and drops their manifest entries). Returns an
/// [`ApplyReport`]. Pruning is opt-in and only ever touches MANAGED files (those
/// in the checksum manifest); user-owned files are never removed.
/// Test: `apply_redeploys_and_clears_staleness`, `apply_prune_removes_deselected`,
/// `apply_prune_spares_user_owned`.
pub fn apply_catalog<G: GitBackend>(
    git: G,
    fw: &FrameworkPaths,
    project_dir: &Path,
    force: bool,
    prune: bool,
) -> Result<ApplyReport, ApplyError> {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let config = crate::core::config::MpmConfig::load(&fw.root);

    // 1. Sync the catalog checkout (TTL-gated unless forced).
    let sync =
        crate::content::CatalogSync::with_config(git, catalog_root.clone(), Some(&config.manifest));
    let sync_result = sync
        .sync(force)
        .map_err(|e| ApplyError::Sync(e.to_string()))?;

    // 2. Resolve manifest + plan exactly as the launcher does.
    let sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    let mut report = ApplyReport {
        fetched: sync_result.fetched,
        ..ApplyReport::default()
    };

    // 3. Redeploy the manifest-selected agents and skills, refreshing checksums.
    let agent_target = fw.claude_agents_dir();
    let deploy = deploy_agents_filtered(&plan.agent_source, &agent_target, |name| {
        plan.agent_selected(name)
    })
    .map_err(|e| ApplyError::AgentDeploy(e.to_string()))?;
    report.agents_deployed = deploy.deployed;
    report.agents_skipped = deploy.skipped;

    // Route through the SAME multi-tier orchestrator `session_launch` uses
    // (issue #2818 review fix): a single-tier `deploy_skills_filtered` call
    // here would see a previously user-tier-deployed skill as "managed,
    // checksum matches" (the manifest only ever recorded the LAST writer, not
    // which tier wrote it) and silently refresh it back to bundled content —
    // clobbering the user's override on every `tm catalog apply`. Threading
    // the user tier through `deploy_all_skill_tiers` here keeps `apply`'s
    // precedence identical to a fresh session launch.
    let skill_target = fw.claude_skills_dir();
    let skill_deploy = deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &skill_target,
        |name| plan.skill_selected(name),
    )
    .map_err(|e| ApplyError::SkillDeploy(e.to_string()))?;
    report.skills_deployed = skill_deploy.stats.deployed;
    report.skills_skipped = skill_deploy.stats.skipped;

    // 4. Optionally prune managed artifacts the manifest no longer selects.
    if prune {
        report.agents_pruned = prune_agents(&agent_target, &plan)?;
        report.skills_pruned = prune_skills(&skill_target, &plan)?;
    }

    Ok(report)
}

/// Remove managed agent files the plan no longer selects; return their names.
///
/// Why: HR-2 deferred removal of deselected agents; HR-3 supplies it behind the
/// explicit `--prune` flag. Only MANAGED files (present in the agent checksum
/// manifest) are eligible — a user-dropped file is never removed.
/// What: for each manifest-managed agent filename whose stem the plan rejects,
/// deletes `<target>/<filename>` (ignoring an already-absent file) and drops the
/// manifest entry; saves the manifest if anything changed. Returns the removed
/// filenames, sorted.
/// Test: `apply_prune_removes_deselected`, `apply_prune_spares_user_owned`.
fn prune_agents(target: &Path, plan: &HarnessPlan) -> Result<Vec<String>, ApplyError> {
    let mut manifest = AgentManifest::load(target);
    let mut pruned: Vec<String> = Vec::new();

    let candidates: Vec<String> = manifest.managed.keys().cloned().collect();
    for filename in candidates {
        let stem = filename.trim_end_matches(".md");
        if plan.agent_selected(stem) {
            continue;
        }
        remove_if_present(&target.join(&filename))?;
        manifest.managed.remove(&filename);
        pruned.push(filename);
    }

    if !pruned.is_empty() {
        manifest
            .save(target)
            .map_err(|e| ApplyError::Prune(e.to_string()))?;
        // The manifest file itself is never an agent; ensure it is left intact.
        debug_assert!(target.join(AGENT_MANIFEST_FILE).exists());
    }
    pruned.sort_unstable();
    Ok(pruned)
}

/// Remove managed skill directories the plan no longer selects; return stems.
///
/// Why: the skill counterpart to [`prune_agents`]. Skills deploy as
/// `<target>/<stem>/SKILL.md`, so pruning removes the whole skill directory.
/// What: for each manifest-managed skill stem the plan rejects, removes
/// `<target>/<stem>/` (ignoring absence) and drops the manifest entry; saves the
/// manifest if anything changed. The [`SkillManifest`] is keyed by stem, matching
/// the skill deployer. Returns the removed stems, sorted.
/// Test: `apply_prune_removes_deselected`.
fn prune_skills(target: &Path, plan: &HarnessPlan) -> Result<Vec<String>, ApplyError> {
    let mut manifest = SkillManifest::load(target);
    let mut pruned: Vec<String> = Vec::new();

    let candidates: Vec<String> = manifest.managed.keys().cloned().collect();
    for stem in candidates {
        if plan.skill_selected(&stem) {
            continue;
        }
        let skill_dir = target.join(&stem);
        if skill_dir.is_dir() {
            std::fs::remove_dir_all(&skill_dir).map_err(|e| ApplyError::Prune(e.to_string()))?;
        }
        manifest.managed.remove(&stem);
        pruned.push(stem);
    }

    if !pruned.is_empty() {
        manifest
            .save(target)
            .map_err(|e| ApplyError::Prune(e.to_string()))?;
        debug_assert!(target.join(SKILL_MANIFEST_FILE).exists());
    }
    pruned.sort_unstable();
    Ok(pruned)
}

/// Delete a file if it exists, treating "already gone" as success.
///
/// Why: pruning is idempotent — a file the manifest lists may already be absent
/// (the user deleted it); that is not an error.
/// What: removes `path`; maps `NotFound` to `Ok(())`, propagates other I/O errors.
/// Test: exercised by `apply_prune_removes_deselected`.
fn remove_if_present(path: &Path) -> Result<(), ApplyError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApplyError::Prune(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
