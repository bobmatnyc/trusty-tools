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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::agent_deployer::deploy_agents_filtered;
use crate::core::agent_manifest::{
    AgentManifest, MANIFEST_FILE as AGENT_MANIFEST_FILE, ManifestError, with_agent_manifest_lock,
};
use crate::core::manifest::HarnessPlan;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_drift::key_stem;
use crate::core::skill_manifest::{
    SKILL_MANIFEST_FILE, SkillManifest, SkillManifestSave, with_skill_manifest_lock,
};
use crate::core::skill_tiers::{deploy_all_skill_tiers, list_source_stems};
use crate::provisioner::GitBackend;

mod prune_guard;

use prune_guard::Verdict;

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

/// Lift a ledger failure into [`ApplyError`].
///
/// Why: `with_agent_manifest_lock` (#4409) and `with_skill_manifest_lock`
/// (#4881) report a lock-acquisition failure as the caller's own error type,
/// which requires this conversion. Prune is the only stage here that takes
/// either lock directly, so a ledger failure on this path is always a
/// prune-stage failure.
/// What: renders the ledger error into the `Prune` variant's message.
/// Test: exercised by `apply_prune_removes_deselected` (happy path).
impl From<ManifestError> for ApplyError {
    fn from(err: ManifestError) -> Self {
        Self::Prune(err.to_string())
    }
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
    /// Deselected agents a guard left in place, as `<filename>: <reason>` (#391).
    pub agents_prune_kept: Vec<String>,
    /// Deselected skills a guard left in place, as `<stem>: <reason>` (#391).
    pub skills_prune_kept: Vec<String>,
    /// Where the pruned content was copied before deletion, when anything was
    /// deleted (#391). `None` means nothing was removed, so nothing was backed up.
    pub prune_backup_dir: Option<PathBuf>,
}

/// One prune stage's result: what went, what stayed, and whether a backup exists.
///
/// Why (#391): a prune that silently declines to delete something is a guard the
/// operator cannot see working. Carrying the kept set alongside the removed set
/// is what lets `tm catalog apply --prune` say "left 2 alone, here is why".
/// What: removed names, kept names each with their reason, and whether this
/// stage wrote anything under the run's backup root.
/// Test: `apply_prune_skips_hand_edited_skill`, `apply_prune_removes_deselected`.
#[derive(Debug, Default)]
struct PruneOutcome {
    /// Names actually deleted this run.
    removed: Vec<String>,
    /// Names a guard declined to delete, formatted `<name>: <reason>`.
    kept: Vec<String>,
    /// True once this stage has copied something under the backup root.
    used_backup: bool,
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
/// [`ApplyReport`].
///
/// Pruning is opt-in, and #391 replaced the old summary ("only ever touches
/// MANAGED files; user-owned files are never removed") because it was true of
/// UNMANAGED files and silent on the two kinds of user content that were in fact
/// deletable. Precisely what prune will and will not remove:
///
/// - Only files the checksum manifest records as trusty-mpm-managed are eligible.
///   A file the user dropped in was never, and is still not, a candidate.
/// - An eligible asset is removed only when its bytes still match the checksum
///   the ledger recorded, so a hand-edited (frozen) agent or skill stays. A
///   skill directory is judged whole: any file under it that no ledger key
///   claims disqualifies the directory, because `remove_dir_all` would take that
///   file too.
/// - A skill stem the user-custom tier supplies is never pruned, whatever the
///   bundled include/exclude says — [`deploy_all_skill_tiers`] deploys that tier
///   in full and exempt from those rules, but its ledger entries look identical
///   to bundled ones.
/// - The agent equivalent of that rule reads the ledger instead of a source
///   directory, because the agent ledger records a tier and the skill one does
///   not: an entry whose `Origin` is not `Bundled` is user-owned and is never
///   pruned, matching how `agents::deployer` already treats it.
/// - Everything actually deleted is copied under
///   `<framework-root>/backup-catalog-prune-<timestamp>/` first;
///   [`ApplyReport::prune_backup_dir`] names it.
///
/// What this does NOT promise: a stem the user has since deleted from
/// `~/.trusty-mpm/skills/` is no longer user-tier and prunes normally, and
/// nothing here restores a backup — that stays a human decision.
/// Test: `apply_redeploys_and_clears_staleness`, `apply_prune_removes_deselected`,
/// `apply_prune_spares_user_owned`, `apply_prune_removes_deselected_skill`,
/// `apply_prune_skips_hand_edited_skill`, `apply_prune_spares_user_tier_skill`,
/// `apply_prune_spares_untracked_file_in_a_managed_skill`,
/// `apply_prune_backs_up_before_removing_a_skill`.
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
    let sources = crate::core::manifest::ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    let mut report = ApplyReport {
        fetched: sync_result.fetched,
        ..ApplyReport::default()
    };

    // 3. Redeploy the manifest-selected agents and skills, refreshing checksums.
    // #4409: the canonical bundled-agent tier is the tm-managed config dir, so
    // a catalog apply refreshes THAT, never the workspace's project tier.
    let agent_target = fw.agent_deploy_dir();
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
        // #391: ONE backup root per run, so an operator undoing a prune finds
        // everything it took in one place. Named here but created lazily — a
        // prune that removes nothing must not litter the framework root.
        let backup_root = prune_guard::prune_backup_root(&fw.root, chrono::Utc::now());
        let agents = prune_agents(&agent_target, &plan, &backup_root)?;
        let skills = prune_skills(
            &skill_target,
            &fw.user_skill_source_dir(),
            &plan,
            &backup_root,
        )?;
        if agents.used_backup || skills.used_backup {
            report.prune_backup_dir = Some(backup_root);
        }
        report.agents_pruned = agents.removed;
        report.agents_prune_kept = agents.kept;
        report.skills_pruned = skills.removed;
        report.skills_prune_kept = skills.kept;
    }

    Ok(report)
}

/// Remove managed agent files the plan no longer selects; return their names.
///
/// Why: HR-2 deferred removal of deselected agents; HR-3 supplies it behind the
/// explicit `--prune` flag. Only MANAGED files (present in the agent checksum
/// manifest) are eligible — a user-dropped file is never removed — and since
/// #391 an eligible file is deleted only when the ledger attributes it to the
/// framework (`Origin::Bundled`) AND its bytes still match the recorded
/// checksum. So a hand-edited agent survives being deselected, and so does a
/// pristine one the ledger records as user- or registry-owned.
/// What: for each manifest-managed agent filename whose stem the plan rejects
/// AND whose on-disk copy is framework-owned and unmodified, backs the file up
/// under `backup_root`,
/// deletes `<target>/<filename>` (ignoring an already-absent file) and drops the
/// manifest entry; saves the manifest if anything changed. A modified or
/// unreadable file keeps BOTH its content and its ledger entry — dropping the
/// entry would reclassify it as user-owned and freeze it against future deploys.
/// Returns the removed and kept names, sorted.
///
/// CONCURRENCY (#4409): the whole load-modify-save runs under the shared ledger
/// lock, because `target` is now the ONE machine-global agent tier rather than a
/// per-workspace directory — an unlocked prune racing a session launch's
/// `deploy_agents_locked` drops the launch's ledger entries, after which those
/// files are treated as untracked and frozen (#4408's shape, via a race).
///
/// The deploy (step 3) and this prune (step 4) are two SEPARATE locked sections,
/// not one span. A single span would require calling the deployer's unlocked
/// inner entry point, and exposing that is a worse hazard than the window it
/// closes: `flock` is not reentrant, so a caller that took the lock and then
/// reached for the ordinary `deploy_agents_filtered` would self-deadlock. The
/// residual window means a session launching between the two steps can deploy an
/// agent this prune then removes; that agent returns on the next launch or
/// sync-assets, since deploy is idempotent. No update is ever lost.
/// Test: `apply_prune_removes_deselected`, `apply_prune_spares_user_owned`,
/// `apply_prune_skips_hand_edited_agent`, `apply_prune_spares_a_user_origin_agent`.
fn prune_agents(
    target: &Path,
    plan: &HarnessPlan,
    backup_root: &Path,
) -> Result<PruneOutcome, ApplyError> {
    with_agent_manifest_lock(target, || prune_agents_locked(target, plan, backup_root))
}

/// The body of [`prune_agents`], run while holding the ledger lock.
///
/// Why/What: mirrors `deploy_agents_locked`/`retract_locked` — the critical
/// section is one expression so the lock's scope cannot be misread. Never call
/// it directly, and never from inside another `with_agent_manifest_lock` on the
/// same directory (`flock` on a second descriptor in one process deadlocks).
/// Test: `apply_prune_removes_deselected`, `apply_prune_spares_user_owned`,
/// `apply_prune_skips_hand_edited_agent`, `apply_prune_spares_a_user_origin_agent`.
fn prune_agents_locked(
    target: &Path,
    plan: &HarnessPlan,
    backup_root: &Path,
) -> Result<PruneOutcome, ApplyError> {
    let mut manifest = AgentManifest::load(target);
    let mut outcome = PruneOutcome::default();

    let candidates: Vec<String> = manifest.managed.keys().cloned().collect();
    for filename in candidates {
        let stem = filename.trim_end_matches(".md");
        if plan.agent_selected(stem) {
            continue;
        }
        // #391: deselected is not the same as disposable — check the ledger
        // still attributes the file to the framework, and that it is still the
        // one trusty-mpm wrote, before deleting it.
        if let Verdict::Kept(why) = prune_guard::agent_verdict(&manifest, target, &filename) {
            tracing::info!(agent = %filename, reason = %why, "not pruning a deselected agent");
            outcome.kept.push(format!("{filename}: {why}"));
            continue;
        }
        let path = target.join(&filename);
        if path.exists() {
            prune_guard::back_up_file(&path, backup_root, &Path::new("agents").join(&filename))?;
            outcome.used_backup = true;
        }
        remove_if_present(&path)?;
        manifest.managed.remove(&filename);
        outcome.removed.push(filename);
    }

    if !outcome.removed.is_empty() {
        manifest
            .save(target)
            .map_err(|e| ApplyError::Prune(e.to_string()))?;
        // The manifest file itself is never an agent; ensure it is left intact.
        debug_assert!(target.join(AGENT_MANIFEST_FILE).exists());
    }
    outcome.removed.sort_unstable();
    outcome.kept.sort_unstable();
    Ok(outcome)
}

/// Remove managed skill directories the plan no longer selects; return stems.
///
/// Why: the skill counterpart to [`prune_agents`]. Skills deploy as
/// `<target>/<stem>/SKILL.md`, so pruning removes the whole skill directory —
/// which is why #391 gave it TWO guards rather than one. A checksum gate alone
/// protects a hand-edited skill (its bytes no longer match) but waves through a
/// PRISTINE user-custom one, because [`deploy_all_skill_tiers`] deploys
/// `~/.trusty-mpm/skills/` in full, exempt from the bundled include/exclude, and
/// the ledger records no origin tier to tell the two apart afterwards.
///
/// The tier half is answered from the LIVE user source
/// ([`list_source_stems`]), not from a recorded origin field. A schema field
/// would have to decide what every pre-existing entry means, and the only
/// available default — "bundled" — is precisely this bug; deriving it from the
/// same directory `deploy_all_skill_tiers` reads makes the two structurally
/// unable to disagree, and needs no migration.
/// What: for each manifest-managed skill STEM the plan rejects, that the user
/// tier does not supply, and whose deployed directory holds nothing but
/// unmodified tm-deployed files, copies the directory under `backup_root`,
/// removes `<target>/<stem>/` (ignoring absence) and drops EVERY ledger key
/// belonging to that stem; saves the manifest if anything changed. Returns the
/// removed and kept stems, sorted.
///
/// Candidates are stems, not raw ledger keys: a key like
/// `<stem>/references/x.md` names a file the skill CARRIES, and judging it
/// independently let an `include` rule that matched the stem but not the nested
/// key drop that file's ledger entry while leaving the file on disk — after
/// which the deployer reads it as user-owned and never updates it again (#4881's
/// freeze shape, reached from this path).
///
/// CONCURRENCY (#4881): the whole load-modify-save runs under the skill ledger
/// lock, for the same reason [`prune_agents`] does — an unlocked prune racing a
/// session launch's skill deploy drops the launch's ledger entries, and the
/// files those entries described then read as user-edited and are skipped
/// forever. As with agents, the deploy (step 3) and this prune (step 4) are two
/// SEPARATE locked sections rather than one span: `flock` is not reentrant, so a
/// single span would have to call an unlocked inner deploy entry point, which is
/// a worse hazard than the residual window. A skill deployed between the two
/// steps that this prune then removes returns on the next deploy — deploy is
/// idempotent, and no ledger update is ever lost.
/// Test: `apply_prune_removes_deselected_skill`, `apply_prune_skips_hand_edited_skill`,
/// `apply_prune_spares_user_tier_skill`.
fn prune_skills(
    target: &Path,
    user_source: &Path,
    plan: &HarnessPlan,
    backup_root: &Path,
) -> Result<PruneOutcome, ApplyError> {
    // Read OUTSIDE the lock: this is the user's own source directory, not the
    // deploy target the lock serialises.
    let user_stems = list_source_stems(user_source)?;
    with_skill_manifest_lock(target, || {
        prune_skills_locked(target, plan, &user_stems, backup_root)
    })
}

/// The body of [`prune_skills`], run while holding the skill ledger lock.
///
/// Why/What: mirrors [`prune_agents_locked`] — the critical section is one
/// expression so the lock's scope cannot be misread. Never call it directly, and
/// never from inside another `with_skill_manifest_lock` on the same directory.
/// Test: `apply_prune_removes_deselected_skill`, `apply_prune_skips_hand_edited_skill`,
/// `apply_prune_spares_user_tier_skill`,
/// `apply_prune_spares_untracked_file_in_a_managed_skill`.
fn prune_skills_locked(
    target: &Path,
    plan: &HarnessPlan,
    user_stems: &BTreeSet<String>,
    backup_root: &Path,
) -> Result<PruneOutcome, ApplyError> {
    let mut manifest = SkillManifest::load(target);
    // #4881: the snapshot the merging save replays this run's delta against —
    // here the delta is REMOVALS, which `save_merging` applies to the current
    // on-disk document rather than reverting a concurrent writer's inserts.
    let base = manifest.clone();
    let mut outcome = PruneOutcome::default();

    // #391: candidates are STEMS. A ledger key may name a carried file
    // (`<stem>/references/x.md`); it is pruned with its skill, never on its own.
    let candidates: BTreeSet<String> = manifest
        .managed
        .keys()
        .map(|key| key_stem(key).to_string())
        .collect();
    for stem in candidates {
        if plan.skill_selected(&stem) {
            continue;
        }
        // #391: the bundled include/exclude does not govern the user-custom
        // tier, so it must not be able to select a user stem for deletion.
        if user_stems.contains(&stem) {
            tracing::info!(
                skill = %stem,
                "not pruning a deselected skill — the user-custom tier supplies it"
            );
            outcome
                .kept
                .push(format!("{stem}: supplied by the user-custom skill tier"));
            continue;
        }
        // #391: and the directory must still hold only unmodified tm-deployed
        // files — `remove_dir_all` takes everything else with it.
        if let Verdict::Kept(why) = prune_guard::skill_verdict(&manifest, target, &stem) {
            tracing::info!(skill = %stem, reason = %why, "not pruning a deselected skill");
            outcome.kept.push(format!("{stem}: {why}"));
            continue;
        }
        let skill_dir = target.join(&stem);
        if skill_dir.is_dir() {
            prune_guard::back_up_tree(&skill_dir, backup_root, &Path::new("skills").join(&stem))?;
            outcome.used_backup = true;
            std::fs::remove_dir_all(&skill_dir).map_err(|e| ApplyError::Prune(e.to_string()))?;
        }
        manifest.managed.retain(|key, _| key_stem(key) != stem);
        outcome.removed.push(stem);
    }

    if !outcome.removed.is_empty() {
        // #4881: the skill DIRECTORIES are already removed, so this save must
        // publish or the manifest keeps listing files that no longer exist.
        if manifest
            .save_merging(target, &base)
            .map_err(|e| ApplyError::Prune(e.to_string()))?
            == SkillManifestSave::Merged
        {
            tracing::warn!(
                target = %target.display(),
                "the skill manifest changed during prune — a writer bypassed the \
                 ledger lock; its entries were merged rather than dropped"
            );
        }
        debug_assert!(target.join(SKILL_MANIFEST_FILE).exists());
    }
    outcome.removed.sort_unstable();
    outcome.kept.sort_unstable();
    Ok(outcome)
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
