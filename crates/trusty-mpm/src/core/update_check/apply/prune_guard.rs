//! The safety gate `tm catalog apply --prune` applies before it deletes (#391).
//!
//! Why: pruning was the ONLY asset-mutating path in the codebase with none of
//! the guards its neighbours have. `skills::deployer::deploy_one_file` refuses
//! to overwrite a file whose bytes no longer match the recorded checksum, and
//! [`crate::core::skill_repair`] backs up before every overwrite and deletes
//! nothing at all — while `prune_skills_locked` called `remove_dir_all` on any
//! managed stem the current include/exclude rule rejected. Two distinct kinds of
//! user content sat behind that call:
//!
//! 1. **A hand-edited asset.** Its checksum no longer matches the ledger, which
//!    is exactly what every other path reads as "the user owns this now".
//! 2. **A pristine user-custom skill.** `skills::tiers::deploy_all_skill_tiers`
//!    deploys `~/.trusty-mpm/skills/` in full and explicitly exempt from the
//!    harness manifest's bundled include/exclude — but once deployed that stem
//!    sits in the ledger with a perfectly matching checksum, indistinguishable
//!    from a bundled one. A checksum gate alone waves it straight through, so
//!    tier awareness is a SECOND, independent requirement, handled by the
//!    caller (see `prune_skills`).
//!
//! What: [`agent_verdict`] and [`skill_verdict`] answer "may prune delete this?"
//! and carry the operator-facing reason when the answer is no; [`back_up_file`]
//! and [`back_up_tree`] copy what is about to go under a timestamped root named
//! by [`prune_backup_root`].
//!
//! Neither verdict ever mutates the ledger. A kept asset stays MANAGED — dropping
//! its entry would reclassify it as user-owned and freeze it against every future
//! deploy, which is the #4881 shape.
//! Test: `crate::core::update_check::apply::tests` —
//! `apply_prune_skips_hand_edited_skill`, `apply_prune_spares_user_tier_skill`,
//! `apply_prune_spares_untracked_file_in_a_managed_skill`,
//! `apply_prune_backs_up_before_removing_a_skill`,
//! `apply_prune_skips_hand_edited_agent`,
//! `apply_prune_spares_a_user_origin_agent`.

use std::path::{Path, PathBuf};

use super::ApplyError;
use crate::core::agent_manifest::AgentManifest;
use crate::core::skill_manifest::SkillManifest;
use crate::core::skill_retire::{SkillRemoval, skill_removal_verdict};

/// Whether one managed asset may be deleted, and why not when it may not.
///
/// Why: "left in place" and "deleted" must be reportable separately — a silent
/// skip is how a guard becomes invisible to the operator who asked for a prune.
/// What: `Prunable`, or `Kept` carrying a reason phrased for a log line.
/// Test: every `apply_prune_*` test asserts on one branch or the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Nothing of the user's is in this asset; prune may remove it.
    Prunable,
    /// Left in place; the string says why, in operator-facing terms.
    Kept(String),
}

/// Timestamped backup root for one `--prune` run.
///
/// Why: mirrors [`crate::core::skill_repair::backup_root_for`]'s convention so
/// an operator finds prune backups next to the remediation ones, and a per-run
/// directory means a second prune never overwrites the first run's copies.
/// What: `<framework-root>/backup-catalog-prune-<YYYYmmdd-HHMMSS>`. The
/// directory is NOT created here — only when something is actually backed up.
/// Test: `prune_backup_root_is_timestamped_under_the_framework_root`.
pub(super) fn prune_backup_root(fw_root: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    fw_root.join(format!(
        "backup-catalog-prune-{}",
        now.format("%Y%m%d-%H%M%S")
    ))
}

/// May prune delete this managed agent file?
///
/// Why (#391): the same rule `agents::deployer` already applies before an
/// overwrite. Deleting a file whose bytes the user changed destroys work no
/// backup was asked for.
/// A checksum gate alone is not the whole rule, for the same reason it was not
/// on the skill side: it protects an EDITED file, never a pristine one the user
/// owns. Skills had to derive that from the live user source because their
/// ledger records no tier. The agent ledger already carries it —
/// `ManifestEntry::origin`, which `agents::deployer` consults through
/// `Origin::is_framework_owned` to decide refresh-vs-preserve — so this guard
/// reads the same field rather than inventing a second answer.
///
/// Nothing writes a non-`Bundled` origin today, so no live prune reaches that
/// branch. It is closed now because the day something does, it becomes a data
/// -loss path with no test and no record of why it mattered.
/// What: `Prunable` when the file is absent (prune is idempotent — there is
/// nothing to lose), or when the ledger records it framework-owned AND its
/// content still checksums to that entry. `Kept` when the ledger has no entry
/// for it, when the entry is user-owned (`User`/`Registry` — the seed-once
/// tier), when the content differs, or when it cannot be read as text.
/// Test: `apply_prune_skips_hand_edited_agent`,
/// `apply_prune_spares_a_user_origin_agent`, `apply_prune_removes_deselected`.
pub(super) fn agent_verdict(manifest: &AgentManifest, target: &Path, filename: &str) -> Verdict {
    let path = target.join(filename);
    if !path.exists() {
        return Verdict::Prunable;
    }
    // #391: an agent the ledger attributes to the user is theirs whether or not
    // they have edited it — the same case a matching checksum waved through on
    // the skill side. Callers only pass managed filenames, so a missing entry is
    // unreachable; treated as user-owned rather than unwrapped.
    match manifest.managed.get(filename) {
        Some(entry) if entry.origin.is_framework_owned() => {}
        Some(entry) => {
            return Verdict::Kept(format!(
                "the ledger records it as {:?}-owned, not framework-owned",
                entry.origin
            ));
        }
        None => {
            return Verdict::Kept(format!(
                "{} has no ledger entry, so trusty-mpm does not own it",
                path.display()
            ));
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(content) if manifest.checksum_matches(filename, &content) => Verdict::Prunable,
        Ok(_) => Verdict::Kept(format!(
            "{} was edited after it was deployed",
            path.display()
        )),
        Err(e) => Verdict::Kept(format!(
            "{} could not be read, so it cannot be verified: {e}",
            path.display()
        )),
    }
}

/// May prune delete this managed skill's whole directory?
///
/// Why (#391): a skill is a DIRECTORY, so the question is not "does one file
/// match" but "is everything under it something trusty-mpm deployed and the user
/// has not touched". `remove_dir_all` takes the untracked sibling with it, which
/// is why an unclaimed file is disqualifying on its own.
/// #5224 asks the IDENTICAL question before removing a retired skill, so the
/// implementation moved to [`crate::core::skill_retire::skill_removal_verdict`]
/// and this is the thin adapter onto prune's own [`Verdict`]. Two copies of a
/// deletion gate is how the two would come to disagree about what is safe.
/// What: `Prunable` when the directory is absent, or when every file under it is
/// claimed by a ledger key belonging to `stem` AND still checksums to that key's
/// entry. `Kept` when the directory holds a file no ledger key claims, when a
/// claimed file's bytes differ, or when anything under it cannot be read. A
/// claimed file that is already gone is not disqualifying — there is nothing
/// left to lose.
/// Test: `apply_prune_skips_hand_edited_skill`,
/// `apply_prune_spares_untracked_file_in_a_managed_skill`,
/// `apply_prune_removes_deselected_skill`.
pub(super) fn skill_verdict(manifest: &SkillManifest, target: &Path, stem: &str) -> Verdict {
    match skill_removal_verdict(manifest, target, stem) {
        SkillRemoval::Removable => Verdict::Prunable,
        SkillRemoval::Kept(why) => Verdict::Kept(why),
    }
}

/// Copy one file under the prune backup root before it is deleted.
///
/// Why (#391): the guards above decide what is safe to delete; the backup is
/// what makes a WRONG decision recoverable. `skill_repair` already treats a
/// backup as a precondition of touching operator state, and deletion is the
/// stronger operation.
/// What: copies `src` to `<backup_root>/<rel>`, creating parents. Returns the
/// backup path.
/// Test: `apply_prune_removes_deselected`.
pub(super) fn back_up_file(
    src: &Path,
    backup_root: &Path,
    rel: &Path,
) -> Result<PathBuf, ApplyError> {
    let dest = backup_root.join(rel);
    create_parents(&dest)?;
    std::fs::copy(src, &dest).map_err(|e| {
        ApplyError::Prune(format!(
            "could not back up {} to {}: {e}",
            src.display(),
            dest.display()
        ))
    })?;
    Ok(dest)
}

/// Copy a whole directory tree under the prune backup root before it is deleted.
///
/// Why: the skill counterpart to [`back_up_file`] — a skill's reference files
/// and carried assets go with the directory, so they must come back with it.
/// What: recursively copies `src` to `<backup_root>/<rel>`. Returns that path.
/// Test: `apply_prune_backs_up_before_removing_a_skill`.
pub(super) fn back_up_tree(
    src: &Path,
    backup_root: &Path,
    rel: &Path,
) -> Result<PathBuf, ApplyError> {
    let dest = backup_root.join(rel);
    copy_tree(src, &dest)?;
    Ok(dest)
}

/// Recursively copy `src` onto `dest`.
///
/// Why: `std::fs` has no recursive copy, and pulling a crate in for one call
/// site is not worth the dependency.
/// What: creates `dest`, then copies each entry, recursing into directories.
/// Test: through [`back_up_tree`] in `apply_prune_backs_up_before_removing_a_skill`.
fn copy_tree(src: &Path, dest: &Path) -> Result<(), ApplyError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| ApplyError::Prune(format!("could not create {}: {e}", dest.display())))?;
    let entries = std::fs::read_dir(src)
        .map_err(|e| ApplyError::Prune(format!("could not read {}: {e}", src.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| ApplyError::Prune(format!("could not read {}: {e}", src.display())))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let is_dir = entry
            .file_type()
            .map_err(|e| ApplyError::Prune(format!("could not stat {}: {e}", from.display())))?
            .is_dir();
        if is_dir {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| {
                ApplyError::Prune(format!(
                    "could not back up {} to {}: {e}",
                    from.display(),
                    to.display()
                ))
            })?;
        }
    }
    Ok(())
}

/// Create `path`'s parent directory chain.
fn create_parents(path: &Path) -> Result<(), ApplyError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ApplyError::Prune(format!("could not create {}: {e}", parent.display()))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_backup_root_is_timestamped_under_the_framework_root() {
        // The root must live beside the other `~/.trusty-mpm/backup-*` dirs and
        // carry the run's timestamp, so a second prune cannot overwrite a first
        // prune's copies.
        let now = chrono::DateTime::from_timestamp(1_754_000_000, 0).unwrap();
        let root = prune_backup_root(Path::new("/home/.trusty-mpm"), now);
        assert_eq!(
            root,
            PathBuf::from("/home/.trusty-mpm/backup-catalog-prune-20250731-221320")
        );
    }
}
