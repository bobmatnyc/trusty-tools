//! Removal of deployed skills no source ships any more (issue #5224).
//!
//! Why: `skills::deployer::deploy_skills_filtered` only ever writes — its own
//! doc says "Deselecting a skill does not remove a previously deployed copy".
//! When a skill is RETIRED from the bundle the deployed directory outlives it,
//! and two things go wrong at once. Claude Code keeps loading text the binary
//! deliberately stopped shipping, and
//! [`crate::core::skill_drift::audit_deployed_skills`] classifies the orphaned
//! ledger key [`SkillDrift::Unverifiable`](crate::core::skill_drift::SkillDrift)
//! — which folds the whole `skill_staleness` doctor check to
//! [`CheckStatus::Unknown`](crate::core::doctor::CheckStatus). A check reporting
//! `Unknown` has stopped protecting anything, so one intentional retirement
//! silently disables the probe on every machine that had the skill.
//!
//! [`crate::core::stale_skills`] solved this shape once for the #1905
//! `mpm-*`→`tm-*` rename behind a frozen allowlist, deliberately scoped to that
//! one migration. This module generalises it: retirement is DERIVED from the
//! live sources rather than listed by hand, so a future retirement needs no
//! second edit here.
//!
//! What: a deployed skill is RETIRED at a target when its stem is tracked by
//! that target's ownership ledger and appears in NO live source —
//! [`live_skill_stems`] unions every source that can legitimately feed a deploy
//! target. [`retire_orphaned_skills`] sweeps every
//! [`skill_deploy_tiers`](crate::core::skill_deploy_tiers::skill_deploy_tiers)
//! entry; [`retire_orphans_in`] is the injectable core one target at a time.
//!
//! TIER SAFETY — the hard constraint, held by three independent gates:
//! 1. Candidates come only from the ownership LEDGER. A skill the operator
//!    hand-placed in a project's or their own `.claude/skills/` has no ledger
//!    entry and is therefore never a candidate at all.
//! 2. The live set unions the user-custom tier (`~/.trusty-mpm/skills/`) and
//!    the project-custom stems already on disk, so a skill those tiers supply
//!    is never retired even when the bundle drops the same name. This is the
//!    tier oracle `update_check::apply::prune_skills` already uses for #391 —
//!    the live user source, not a recorded origin field, because the ledger
//!    records no tier.
//! 3. [`skill_removal_verdict`] refuses to delete a directory holding anything
//!    the ledger does not claim, or any claimed file whose bytes changed after
//!    deployment.
//!
//! DESELECTION IS NOT RETIREMENT. The live set is built from what the sources
//! CONTAIN, never from what a harness manifest's include/exclude selects, so an
//! excluded-but-still-shipped skill keeps its deployed copy exactly as before.
//! Deselection remains `tm catalog apply --prune`'s business.
//!
//! FAILING SAFE: every uncertainty widens the live set or aborts the sweep, so
//! the failure mode is leaving an orphan behind, never deleting something live.
//! An unreadable source aborts the sweep for that target entirely.
//!
//! Test: `skill_retire_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::agent_manifest::Result as ManifestResult;
use crate::core::bundle;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;
use crate::core::skill_drift::{deployed_path, key_stem};
use crate::core::skill_manifest::{SkillManifest, SkillManifestSave, with_skill_manifest_lock};
use crate::core::skill_tiers::{list_project_custom_stems, list_source_stems};

/// Whether a retired skill's deployed directory may be deleted.
///
/// Why: "the ledger says this skill is retired" and "nothing under its
/// directory belongs to the operator" are separate questions, and only the
/// second one authorises `remove_dir_all`. Keeping the reason with the refusal
/// is what makes a skipped removal visible instead of silent.
/// What: `Removable`, or `Kept` carrying an operator-facing reason.
/// Test: `verdict_keeps_a_hand_edited_skill`, `verdict_keeps_an_untracked_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRemoval {
    /// Every file under the directory is ledger-claimed and unmodified.
    Removable,
    /// Left on disk; the string says why, phrased for a log line.
    Kept(String),
}

/// One skill this sweep stopped tracking, and what happened to its files.
///
/// Why: callers report retirement to the operator, and "deleted" versus "left
/// on disk because you edited it" are different facts they must be able to
/// print separately.
/// What: the deploy tier's label, the stem, whether the directory was deleted,
/// and — when it was not — why.
/// Test: `retire_removes_a_pristine_orphan`,
/// `retire_keeps_a_hand_edited_orphan_but_releases_the_ledger`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredSkill {
    /// The deploy tier's label, e.g. `operator home`.
    pub tier: String,
    /// The retired skill stem.
    pub stem: String,
    /// Whether `<dest>/<stem>/` was deleted.
    pub removed: bool,
    /// Why the directory was kept, when `removed` is `false`.
    pub reason: Option<String>,
}

/// Every skill stem this binary itself embeds.
///
/// Why: the compiled-in table is the one source that cannot lag the binary
/// running it — the same reasoning [`crate::core::skill_drift::skill_reference`]
/// gives for never trusting the `~/.trusty-mpm/framework/skills/` extraction
/// cache as an authority.
/// What: the first path segment of every `skills/…` entry in [`bundle::ALL`],
/// with `.md` stripped — so both `skills/<stem>.md` and
/// `skills/<stem>/references/<file>.md` yield `<stem>`.
/// Test: `bundled_stems_covers_a_known_skill`.
pub fn bundled_skill_stems() -> BTreeSet<String> {
    bundle::ALL
        .iter()
        .filter_map(|a| a.rel_path.strip_prefix("skills/"))
        .filter_map(|rel| rel.split('/').next())
        .map(|first| first.strip_suffix(".md").unwrap_or(first).to_string())
        .collect()
}

/// Union every source that could legitimately supply a skill to `dest`.
///
/// Why: a stem is only safe to retire when NO source has it, so this set must
/// err large. It deliberately includes sources a given deploy is not currently
/// reading from — the catalog checkout, the extraction cache — because a target
/// like `~/.claude/skills` is shared across projects and one project's
/// `source = "catalog"` manifest must not make another project's bundled skills
/// look retired.
/// What: the compiled-in [`bundled_skill_stems`], plus the resolved bundled
/// source (the `agents/skills` submodule when checked out, else the extraction
/// cache), the user-custom tier, the synced catalog's skills, and the
/// project-custom stems already sitting in `dest`. Returns `None` when any of
/// those directories exists but cannot be enumerated — an incomplete live set
/// would misread live skills as retired, so the caller must skip the sweep.
/// Test: `live_stems_include_the_user_tier`,
/// `live_stems_are_none_when_a_source_cannot_be_read`.
pub fn live_skill_stems(paths: &FrameworkPaths, dest: &Path) -> Option<BTreeSet<String>> {
    let mut live = bundled_skill_stems();
    let catalog_skills = crate::content::catalog_root_for(&paths.root)
        .join("repo")
        .join(".claude")
        .join("skills");
    for dir in [
        paths.skill_source_dir(),
        paths.user_skill_source_dir(),
        catalog_skills,
    ] {
        match list_source_stems(&dir) {
            Ok(stems) => live.extend(stems),
            Err(error) => {
                tracing::warn!(
                    source = %dir.display(),
                    %error,
                    "skill source could not be enumerated — skipping retirement sweep (#5224)"
                );
                return None;
            }
        }
    }
    match list_project_custom_stems(dest) {
        Ok(stems) => live.extend(stems),
        Err(error) => {
            tracing::warn!(
                dest = %dest.display(),
                %error,
                "deploy target could not be scanned — skipping retirement sweep (#5224)"
            );
            return None;
        }
    }
    Some(live)
}

/// May a retired skill's whole directory be deleted?
///
/// Why (#391, extended by #5224): a skill is a DIRECTORY, so the question is
/// not "does one file match" but "is everything under it something trusty-mpm
/// deployed and the operator has not touched" — `remove_dir_all` takes the
/// untracked sibling with it. `update_check::apply::prune_guard::skill_verdict`
/// asked exactly this question first and now delegates here, so the deselection
/// prune and this retirement sweep cannot drift on what counts as safe.
/// What: `Removable` when the directory is absent, or when every file under it
/// is claimed by a ledger key belonging to `stem` AND still checksums to that
/// key's entry. `Kept` when the directory holds a file no ledger key claims,
/// when a claimed file's bytes differ, or when anything under it cannot be
/// read. A claimed file that is already gone is not disqualifying.
/// Test: `verdict_keeps_a_hand_edited_skill`, `verdict_keeps_an_untracked_file`,
/// `verdict_allows_a_pristine_skill`.
pub fn skill_removal_verdict(manifest: &SkillManifest, target: &Path, stem: &str) -> SkillRemoval {
    let dir = target.join(stem);
    if !dir.is_dir() {
        return SkillRemoval::Removable;
    }

    let claimed_keys: Vec<&String> = manifest
        .managed
        .keys()
        .filter(|key| key_stem(key) == stem)
        .collect();
    let claimed_paths: BTreeSet<std::path::PathBuf> = claimed_keys
        .iter()
        .map(|key| deployed_path(target, key))
        .collect();

    let mut on_disk = Vec::new();
    if let Err(e) = collect_files(&dir, &mut on_disk) {
        return SkillRemoval::Kept(format!(
            "{} could not be walked, so it cannot be verified: {e}",
            dir.display()
        ));
    }
    // A file the ledger does not claim is the operator's — and `remove_dir_all`
    // would take it along with everything else in the directory.
    if let Some(stray) = on_disk.iter().find(|path| !claimed_paths.contains(*path)) {
        return SkillRemoval::Kept(format!(
            "it holds {}, which trusty-mpm never deployed",
            stray.display()
        ));
    }

    for key in claimed_keys {
        let path = deployed_path(target, key);
        if !path.exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) if manifest.checksum_matches(key, &content) => {}
            Ok(_) => {
                return SkillRemoval::Kept(format!(
                    "{} was edited after it was deployed",
                    path.display()
                ));
            }
            Err(e) => {
                return SkillRemoval::Kept(format!(
                    "{} could not be read, so it cannot be verified: {e}",
                    path.display()
                ));
            }
        }
    }
    SkillRemoval::Removable
}

/// Collect every regular file under `dir`, recursively.
///
/// Why: [`skill_removal_verdict`] must see the whole subtree `remove_dir_all`
/// would take, not just the entry point — a reference file or a carried script
/// is operator content too.
/// What: appends each file path to `out`. Directory entries recurse; a symlink
/// is reported as a plain path (`DirEntry::file_type` does not follow it), which
/// makes it unclaimed and therefore disqualifying — the conservative answer.
/// Test: exercised through [`skill_removal_verdict`] by
/// `verdict_keeps_an_untracked_file`.
fn collect_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Retire every ledger-tracked stem at `dest` that `live` does not contain.
///
/// Why: this is the whole fix, and taking `live` as a parameter is what makes
/// it testable against a literal source set instead of the real embedded bundle
/// — the same split [`crate::daemon::doctor_skill_drift`] uses for its report.
/// What: under the ledger lock, for each retired stem —
/// - [`skill_removal_verdict`] `Removable` → delete `<dest>/<stem>/`;
/// - `Kept` → leave every file exactly where it is.
///
/// EITHER WAY the stem's ledger keys are dropped. A ledger entry is a claim
/// that tm deployed the file and can refresh it; once no source ships the
/// skill, that claim is false and keeping it is what pins `skill_staleness` to
/// `Unknown`. Releasing it reclassifies a kept file as operator-owned, which is
/// what a hand-edited copy has effectively become — and [`crate::core::
/// skill_drift`]'s #4605 unmanaged-skill detector already finds and adopts such
/// a file if the skill ever returns to the bundle.
///
/// CONCURRENCY (#4881): the whole load-modify-save runs under
/// [`with_skill_manifest_lock`], and as a SEPARATE locked section from the
/// deploy that precedes it — `flock` is not reentrant, so one span would have
/// to call an unlocked inner deploy entry point, a worse hazard than the
/// residual window. A skill deployed between the two is retired only if no
/// source has it, which cannot be true of one just deployed.
/// Test: `retire_removes_a_pristine_orphan`,
/// `retire_keeps_a_hand_edited_orphan_but_releases_the_ledger`,
/// `retire_spares_a_user_tier_skill`, `retire_spares_a_project_tier_skill`,
/// `retire_is_a_noop_when_nothing_is_orphaned`.
pub fn retire_orphans_in(
    tier: &str,
    dest: &Path,
    live: &BTreeSet<String>,
) -> ManifestResult<Vec<RetiredSkill>> {
    if !dest.is_dir() {
        return Ok(Vec::new());
    }
    with_skill_manifest_lock(dest, || retire_orphans_locked(tier, dest, live))
}

/// The body of [`retire_orphans_in`], run while holding the ledger lock.
///
/// Why/What: mirrors `deployer::deploy_skills_locked` — the critical section is
/// one expression so the lock's scope cannot be misread. Never call it directly.
/// Test: covered through [`retire_orphans_in`] by every `retire_*` test.
fn retire_orphans_locked(
    tier: &str,
    dest: &Path,
    live: &BTreeSet<String>,
) -> ManifestResult<Vec<RetiredSkill>> {
    let mut manifest = SkillManifest::load(dest);
    let base = manifest.clone();

    // #5224: candidates are STEMS, never raw ledger keys. A key like
    // `<stem>/references/x.md` names a file the skill CARRIES and is retired
    // with its skill or not at all.
    let retired: Vec<String> = manifest
        .managed
        .keys()
        .map(|key| key_stem(key).to_string())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .filter(|stem| !live.contains(stem))
        .collect();

    let mut outcome = Vec::new();
    for stem in retired {
        let reason = match skill_removal_verdict(&manifest, dest, &stem) {
            SkillRemoval::Removable => {
                let dir = dest.join(&stem);
                if dir.is_dir() {
                    std::fs::remove_dir_all(&dir)?;
                }
                None
            }
            SkillRemoval::Kept(why) => Some(why),
        };
        manifest.managed.retain(|key, _| key_stem(key) != stem);
        match &reason {
            None => tracing::info!(
                skill = %stem,
                tier,
                "removed a deployed skill no source ships any more (#5224)"
            ),
            Some(why) => tracing::warn!(
                skill = %stem,
                tier,
                reason = %why,
                "a deployed skill is no longer shipped by any source, but its files were \
                 kept — trusty-mpm no longer tracks them and they are now yours (#5224)"
            ),
        }
        outcome.push(RetiredSkill {
            tier: tier.to_string(),
            stem,
            removed: reason.is_none(),
            reason,
        });
    }

    if !outcome.is_empty() {
        // The directories are already gone, so this save must publish or the
        // ledger keeps listing files that no longer exist.
        let saved = manifest.save_merging(dest, &base)?;
        if saved == SkillManifestSave::Merged {
            tracing::warn!(
                dest = %dest.display(),
                "the skill manifest changed during retirement — a writer bypassed the \
                 ledger lock; its entries were merged rather than dropped"
            );
        }
    }
    Ok(outcome)
}

/// Sweep every skill deploy tier for skills no source ships any more.
///
/// Why: the orphan folds `skill_staleness` to `Unknown` independently at each
/// tier, so a sweep covering only the tier the caller happened to deploy into
/// leaves the others reporting `Unknown` forever — the same reasoning
/// [`skill_deploy_tiers`] exists for.
/// What: for each tier, builds [`live_skill_stems`] and calls
/// [`retire_orphans_in`]. A tier whose live set cannot be established, or whose
/// sweep fails, is logged and skipped — retirement is maintenance and must
/// never fail an install or a session launch.
/// Test: `retire_orphaned_skills_sweeps_every_tier`.
pub fn retire_orphaned_skills(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> Vec<RetiredSkill> {
    let mut all = Vec::new();
    for tier in skill_deploy_tiers(paths, project_dir) {
        let Some(live) = live_skill_stems(paths, &tier.dir) else {
            continue;
        };
        match retire_orphans_in(tier.label, &tier.dir, &live) {
            Ok(retired) => all.extend(retired),
            Err(error) => tracing::warn!(
                tier = tier.label,
                dest = %tier.dir.display(),
                %error,
                "retirement sweep failed for this deploy tier (#5224)"
            ),
        }
    }
    all
}

#[cfg(test)]
#[path = "skill_retire_tests.rs"]
mod tests;
