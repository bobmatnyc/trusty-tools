//! Detect deployed skills that are stale relative to their bundled source.
//!
//! Why: trusty-mpm's per-project conventions (e.g. the commit/PR attribution
//! footer) ship inside bundled skills that `skill_deployer::deploy_skills`
//! copies into a project's `.claude/skills/`. A long-lived worktree — or any
//! workspace not re-provisioned since the bundled asset last changed — keeps
//! serving the OLD skill text until the next deploy self-heals it. That silent
//! drift is exactly how the PR #2825 attribution bypass slipped through, so the
//! stack needs one honest, read-only signal that "the deployed skills predate
//! the installed binary's assets" (issue #2876).
//! What: [`stale_skills`] compares each bundled source skill's CURRENT content
//! checksum against the checksum recorded in the deploy manifest at the target
//! directory. A managed skill whose source no longer matches what was last
//! deployed is returned as stale — precisely the set the next
//! `deploy_skills_filtered` run would refresh. It never reads the deployed file
//! or touches disk beyond loading the manifest, so it is safe to call from both
//! the read-only `tm doctor` path and the launch path.
//! Test: the `tests` module below covers the fresh, stale, unmanaged, and
//! missing-source cases.

use std::path::Path;

use crate::core::skill_deployer::{is_skill_file, skill_stem};
use crate::core::skill_manifest::SkillManifest;

/// Return the stems of managed skills whose bundled source content no longer
/// matches the checksum recorded when they were deployed to `dest_dir`.
///
/// Why: this is the deploy self-heal's own "safe to refresh" trigger surfaced
/// as a diagnostic — a non-empty result means `dest_dir` carries skill text
/// older than the current bundled assets and a relaunch / `tm install` would
/// update it (issue #2876). Reporting it lets `tm doctor` and `prepare_session`
/// warn BEFORE a stale skill (e.g. an outdated attribution footer) is acted on.
/// What: loads the [`SkillManifest`] from `dest_dir`, then for every `.md`
/// skill file under `source_dir` whose stem the manifest already manages,
/// compares `checksum(source_content)` against the stored checksum. Stems that
/// differ are returned, sorted for stable output. A missing/unreadable
/// `source_dir` yields an empty vector (nothing to compare), and skills absent
/// from the manifest (never deployed, or user-owned) are ignored — those are a
/// different probe's concern. Read failures on an individual source file skip
/// that file rather than aborting.
/// Test: `stale_skills_flags_changed_source`, `stale_skills_empty_when_fresh`,
/// `stale_skills_ignores_unmanaged`, `stale_skills_missing_source_is_empty`.
pub fn stale_skills(source_dir: &Path, dest_dir: &Path) -> Vec<String> {
    let manifest = SkillManifest::load(dest_dir);
    if manifest.managed.is_empty() {
        // Nothing has been deployed here yet — there is no prior deploy to be
        // stale relative to. Skip the directory read entirely.
        return Vec::new();
    }

    let Ok(entries) = std::fs::read_dir(source_dir) else {
        return Vec::new();
    };

    let mut stale: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_skill_file(name) {
            continue;
        }
        let stem = skill_stem(name);
        if !manifest.is_managed(stem) {
            // Not a trusty-mpm-managed skill in this target — either never
            // deployed or user-owned; staleness does not apply.
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        // `checksum_matches` returns true iff the manifest's recorded checksum
        // equals `checksum(content)`. A mismatch means the bundled source has
        // changed since this skill was last deployed → a pending refresh.
        if !manifest.checksum_matches(stem, &content) {
            stale.push(stem.to_string());
        }
    }
    stale.sort_unstable();
    stale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::skill_deployer::deploy_skills;
    use std::fs;
    use tempfile::TempDir;

    /// Deploy a single skill and return (source_dir, dest_dir) temp dirs.
    fn deploy_one(source_body: &str) -> (TempDir, TempDir) {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        fs::write(src.path().join("tm-doctor.md"), source_body).unwrap();
        deploy_skills(src.path(), dest.path()).unwrap();
        (src, dest)
    }

    #[test]
    fn stale_skills_empty_when_fresh() {
        // A skill just deployed from an unchanged source is not stale.
        let (src, dest) = deploy_one("---\nname: tm-doctor\n---\n\nv1\n");
        assert!(stale_skills(src.path(), dest.path()).is_empty());
    }

    #[test]
    fn stale_skills_flags_changed_source() {
        // After deploy, mutating the bundled source (as a framework upgrade
        // does) must mark the managed skill stale — the exact drift that let
        // the PR #2825 attribution bypass persist in a long-lived worktree.
        let (src, dest) = deploy_one("---\nname: tm-doctor\n---\n\nv1\n");
        fs::write(
            src.path().join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\nv2 with new attribution footer\n",
        )
        .unwrap();
        let stale = stale_skills(src.path(), dest.path());
        assert_eq!(stale, vec!["tm-doctor".to_string()]);
    }

    #[test]
    fn stale_skills_ignores_unmanaged() {
        // A source skill that was never deployed to this target (absent from the
        // manifest) must not be reported — staleness applies only to skills tm
        // actually deployed here.
        let (src, dest) = deploy_one("---\nname: tm-doctor\n---\n\nv1\n");
        // Add a brand-new source skill that the earlier deploy never wrote.
        fs::write(
            src.path().join("brand-new.md"),
            "---\nname: brand-new\n---\n\nnever deployed\n",
        )
        .unwrap();
        let stale = stale_skills(src.path(), dest.path());
        assert!(!stale.contains(&"brand-new".to_string()));
        assert!(stale.is_empty());
    }

    #[test]
    fn stale_skills_missing_source_is_empty() {
        // A missing source directory yields no staleness (nothing to compare),
        // never an error.
        let dest = TempDir::new().unwrap();
        let stale = stale_skills(Path::new("/nonexistent/skill/source"), dest.path());
        assert!(stale.is_empty());
    }

    #[test]
    fn stale_skills_empty_dest_manifest_is_empty() {
        // With no prior deploy (empty manifest) nothing can be stale even if the
        // source is populated.
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        fs::write(src.path().join("tm-doctor.md"), "body").unwrap();
        assert!(stale_skills(src.path(), dest.path()).is_empty());
    }
}
