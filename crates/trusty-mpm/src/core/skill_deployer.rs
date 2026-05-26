//! Skill deployment — copies skill files into `~/.claude/skills/`.
//!
//! Why: Claude Code reads skill files from `~/.claude/skills/`. trusty-mpm must
//! keep that directory populated with up-to-date skills, while never destroying
//! files the user owns or has hand-edited. Skills carry no inheritance, so —
//! unlike agents — deployment is a plain content copy, but the manifest-based
//! ownership tracking is identical.
//! What: [`deploy_skills`] reads every `*.md` file from a source directory,
//! consults the [`SkillManifest`] to classify each target file, and writes only
//! the files it safely may. It returns a [`DeployStats`] summarising what
//! happened.
//! Test: `cargo test -p trusty-mpm-core skill_deployer` covers a new deploy, a
//! skipped user-modified file, an unchanged file, and a user-owned file.

use std::path::Path;

use crate::core::agent_manifest::checksum;
use crate::core::error::Error;
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};

/// Summary of one [`deploy_skills`] run.
///
/// Why: callers print per-file status; they need the file lists split by
/// outcome to render that summary and to know whether any work was skipped.
/// What: filenames grouped into freshly written, skipped (user-owned or
/// user-modified), and unchanged (checksum already current).
/// Test: every `deploy_*` test asserts on these vectors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployStats {
    /// Filenames successfully (re)written this run.
    pub deployed: Vec<String>,
    /// Filenames skipped because the user owns or modified them.
    pub skipped: Vec<String>,
    /// Filenames left untouched because their checksum already matched.
    pub unchanged: Vec<String>,
}

/// Whether a source filename names a skill file to deploy.
///
/// Why: the source directory holds `.md` files; only those should be deployed,
/// and any manifest file must be ignored.
/// What: returns `true` for `*.md` files.
/// Test: covered indirectly by `deploy_new_skill`.
fn is_skill_file(name: &str) -> bool {
    name.ends_with(".md")
}

/// Deploy all skills from `source` to `dest`.
///
/// Why: ensures `~/.claude/skills/` has up-to-date skill files without
/// clobbering user-owned or user-modified files.
///
/// Rules:
///   - Not in manifest, file exists → user-owned → skip silently
///   - In manifest, checksum matches deployed copy → overwrite when stale
///   - In manifest, checksum differs → user-modified → skip
///   - New trusty-mpm skill → write + add to manifest
///
/// Test: `deploy_new_skill`, `deploy_skips_user_modified`,
/// `deploy_unchanged_no_write`, `deploy_user_owned_skipped`.
pub fn deploy_skills(source: &Path, dest: &Path) -> Result<DeployStats, Error> {
    let mut stats = DeployStats::default();

    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no skills still succeeds.
    if !source.is_dir() {
        return Ok(stats);
    }

    let mut manifest = SkillManifest::load(dest);
    let now = chrono::Utc::now().to_rfc3339();

    // Collect skill filenames deterministically so output and tests are stable.
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file() && is_skill_file(name) {
            names.push(name.to_string());
        }
    }
    names.sort_unstable();

    for filename in names {
        let source_path = source.join(&filename);
        let content = std::fs::read_to_string(&source_path)?;
        let target_path = dest.join(&filename);

        // Classify the existing target file, if any.
        if target_path.exists() {
            if !manifest.is_managed(&filename) {
                // User dropped their own file here — never touch it.
                stats.skipped.push(filename);
                continue;
            }
            let current = std::fs::read_to_string(&target_path)?;
            if manifest.checksum_matches(&filename, &current) {
                if checksum(&content) == checksum(&current) {
                    // Deployed copy is already the latest content.
                    stats.unchanged.push(filename);
                    continue;
                }
                // Managed and unmodified by the user → safe to refresh.
            } else {
                // Managed but the user edited it → preserve their changes.
                stats.skipped.push(filename);
                continue;
            }
        }

        // Write (new file, or safe refresh of a managed file).
        std::fs::create_dir_all(dest)?;
        std::fs::write(&target_path, &content)?;
        manifest.managed.insert(
            filename.clone(),
            SkillManifestEntry {
                checksum: checksum(&content),
                deployed_at: now.clone(),
            },
        );
        stats.deployed.push(filename);
    }

    manifest.save(dest)?;

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A two-file skill source set.
    fn write_sources(dir: &Path) {
        fs::write(
            dir.join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\n# Doctor\n\nDiagnostic skill.\n",
        )
        .unwrap();
        fs::write(
            dir.join("example-skill.md"),
            "---\nname: example-skill\n---\n\n# Example\n\nExample skill.\n",
        )
        .unwrap();
    }

    #[test]
    fn deploy_new_skill() {
        // A first-ever deploy must write every skill and record it in the
        // manifest.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert_eq!(stats.deployed.len(), 2);
        assert!(stats.deployed.contains(&"tm-doctor.md".to_string()));
        assert!(stats.skipped.is_empty());
        assert!(stats.unchanged.is_empty());

        let doctor = fs::read_to_string(tgt.path().join("tm-doctor.md")).unwrap();
        assert!(doctor.contains("Diagnostic skill."));

        let manifest = SkillManifest::load(tgt.path());
        assert!(manifest.is_managed("tm-doctor.md"));
    }

    #[test]
    fn deploy_skips_user_modified() {
        // A managed file the user edited must be skipped, not overwritten.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();

        fs::write(
            tgt.path().join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor.md".to_string()));
        assert!(!stats.deployed.contains(&"tm-doctor.md".to_string()));

        let still = fs::read_to_string(tgt.path().join("tm-doctor.md")).unwrap();
        assert!(still.contains("USER HAND-EDIT"));
    }

    #[test]
    fn deploy_unchanged_no_write() {
        // A second deploy with no source changes must report files unchanged
        // and not rewrite them.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();
        let before = fs::metadata(tgt.path().join("tm-doctor.md"))
            .unwrap()
            .modified()
            .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.unchanged.contains(&"tm-doctor.md".to_string()));
        assert!(stats.deployed.is_empty());

        let after = fs::metadata(tgt.path().join("tm-doctor.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "unchanged file must not be rewritten");
    }

    #[test]
    fn deploy_user_owned_skipped() {
        // A file in the target that trusty-mpm never deployed (absent from the
        // manifest) must be left completely untouched.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        fs::write(
            tgt.path().join("tm-doctor.md"),
            "USER OWNED — not trusty-mpm's\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor.md".to_string()));

        let content = fs::read_to_string(tgt.path().join("tm-doctor.md")).unwrap();
        assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

        // example-skill.md had no conflict, so it deploys normally.
        assert!(stats.deployed.contains(&"example-skill.md".to_string()));
    }

    #[test]
    fn deploy_refreshes_stale_managed_skill() {
        // A managed, unmodified file whose source changed must be refreshed.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();

        // The framework updates the source skill.
        fs::write(
            src.path().join("tm-doctor.md"),
            "---\nname: tm-doctor\n---\n\n# Doctor v2\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.deployed.contains(&"tm-doctor.md".to_string()));
        let refreshed = fs::read_to_string(tgt.path().join("tm-doctor.md")).unwrap();
        assert!(refreshed.contains("Doctor v2"));
    }

    #[test]
    fn deploy_missing_source_dir_is_empty_result() {
        // Deploying from a non-existent source directory is a no-op success.
        let tgt = TempDir::new().unwrap();
        let stats = deploy_skills(Path::new("/nonexistent/trusty-mpm/skills"), tgt.path()).unwrap();
        assert_eq!(stats, DeployStats::default());
    }
}
