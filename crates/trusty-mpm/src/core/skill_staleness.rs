//! Detect deployed skills that are stale relative to their bundled source.
//!
//! Why: trusty-mpm's per-project conventions (e.g. the commit/PR attribution
//! footer) ship inside bundled skills that `skill_deployer::deploy_skills`
//! copies into a project's `.claude/skills/`. A long-lived worktree — or any
//! workspace not re-provisioned since the bundled asset changed — keeps
//! serving the OLD skill text until the next deploy self-heals it. That silent
//! drift is exactly how the PR #2825 attribution bypass slipped through, so the
//! stack needs one honest, read-only signal that "the deployed skills differ
//! from the installed binary's bundled source" (issue #2876). The check is a
//! symmetric content-checksum diff — it makes no temporal claim about which
//! side is newer, only that they no longer match.
//! What: [`stale_skills`] compares each bundled source skill's CURRENT content
//! checksum against the checksum recorded in the deploy manifest at the target
//! directory. A managed skill whose source differs from what was last deployed
//! is returned as stale — precisely the set the next `deploy_skills_filtered`
//! run would refresh. It never reads the deployed file or touches disk beyond
//! loading the manifest, so it is safe to call from both the read-only `tm
//! doctor` path and the launch path.
//! Test: the `tests` module below covers the fresh, stale, unmanaged, and
//! missing-source cases.

use std::path::Path;

use crate::core::agent_manifest::Result as ManifestResult;
use crate::core::skill_manifest::SkillManifest;
use trusty_agents_common::skills::source_scan::scan_skill_sources;

/// Return the stems of managed skills whose bundled source content no longer
/// matches the checksum recorded when they were deployed to `dest_dir`.
///
/// Why: this is the deploy self-heal's own "safe to refresh" trigger surfaced
/// as a diagnostic — a non-empty result means `dest_dir` carries skill text
/// that differs from the current bundled source, and a relaunch / `tm install`
/// would reconcile it (issue #2876). Reporting it lets `tm doctor` and
/// `prepare_session` warn BEFORE a stale skill (e.g. an outdated attribution
/// footer) is acted on.
/// What: loads the [`SkillManifest`] from `dest_dir`, then scans `source_dir`
/// with the deployer's own [`scan_skill_sources`] (#10211: so every carried
/// file — `references/*`, `scripts/*`, any extension — is compared, keyed
/// `<stem>/<rel>` as the deployer records it, not only the `.md` entry point).
/// For every key the manifest manages, compares `checksum(source_content)`
/// against the stored checksum; the stem of any key that differs is returned,
/// deduplicated and sorted. A missing `source_dir` yields an empty vector
/// (nothing to compare); a source directory that exists but cannot be read is
/// an error, never "nothing is stale". Keys absent from the manifest (never
/// deployed, or user-owned) are ignored — those are a different probe's
/// concern. Read failures on an individual source file skip that file rather
/// than aborting.
///
/// Fallible since #5626: an empty ledger short-circuits to "nothing is stale",
/// which is right for a tier never deployed to and wrong for one whose ledger
/// could not be read — the caller then warns about no drift it never looked for.
/// Test: `stale_skills_flags_changed_source`, `stale_skills_empty_when_fresh`,
/// `stale_skills_ignores_unmanaged`, `stale_skills_missing_source_is_empty`,
/// `stale_skills_flags_a_changed_script`.
pub fn stale_skills(source_dir: &Path, dest_dir: &Path) -> ManifestResult<Vec<String>> {
    let manifest = SkillManifest::load(dest_dir)?;
    if manifest.managed.is_empty() {
        // Nothing has been deployed here yet — there is no prior deploy to be
        // stale relative to. Skip the directory read entirely.
        return Ok(Vec::new());
    }

    // #10211: one scan, shared with the deployer, so the files compared here
    // are exactly the files a deploy writes. A missing dir is an empty scan.
    let scan = scan_skill_sources(source_dir)?;

    let mut stale: Vec<String> = Vec::new();
    for skill in &scan.skills {
        let mut files: Vec<(String, &Path)> = vec![(skill.stem.clone(), skill.entry.as_path())];
        files.extend(skill.extras.iter().map(|extra| {
            (
                format!("{}/{}", skill.stem, extra.rel),
                extra.path.as_path(),
            )
        }));
        for (key, path) in files {
            if !manifest.is_managed(&key) {
                // Not a trusty-mpm-managed file in this target — either never
                // deployed or user-owned; staleness does not apply.
                continue;
            }
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            // `checksum_matches` returns true iff the manifest's recorded
            // checksum equals `checksum(content)`. A mismatch means the bundled
            // source has changed since this file was last deployed → a pending
            // refresh of its skill.
            if !manifest.checksum_matches(&key, &content) {
                stale.push(skill.stem.clone());
                break;
            }
        }
    }
    stale.sort_unstable();
    stale.dedup();
    Ok(stale)
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
        assert!(stale_skills(src.path(), dest.path()).unwrap().is_empty());
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
        let stale = stale_skills(src.path(), dest.path()).unwrap();
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
        let stale = stale_skills(src.path(), dest.path()).unwrap();
        assert!(!stale.contains(&"brand-new".to_string()));
        assert!(stale.is_empty());
    }

    /// #10211: a change to a carried script alone marks its skill stale. Fails
    /// on the pre-fix code, which compared the `.md` entry point only.
    #[test]
    fn stale_skills_flags_a_changed_script() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        fs::write(src.path().join("tm-doctor.md"), "body\n").unwrap();
        let scripts = src.path().join("tm-doctor").join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        deploy_skills(src.path(), dest.path()).unwrap();
        assert!(stale_skills(src.path(), dest.path()).unwrap().is_empty());

        fs::write(scripts.join("check.sh"), "#!/bin/sh\nexit 1\n").unwrap();

        let stale = stale_skills(src.path(), dest.path()).unwrap();
        assert_eq!(stale, vec!["tm-doctor".to_string()]);
    }

    #[test]
    fn stale_skills_missing_source_is_empty() {
        // A missing source directory yields no staleness (nothing to compare),
        // never an error.
        let dest = TempDir::new().unwrap();
        let stale = stale_skills(Path::new("/nonexistent/skill/source"), dest.path()).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn stale_skills_empty_dest_manifest_is_empty() {
        // With no prior deploy (empty manifest) nothing can be stale even if the
        // source is populated.
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        fs::write(src.path().join("tm-doctor.md"), "body").unwrap();
        assert!(stale_skills(src.path(), dest.path()).unwrap().is_empty());
    }
}
