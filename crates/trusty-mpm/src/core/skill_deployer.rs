//! Skill deployment — writes skill directories into `~/.claude/skills/`.
//!
//! Why: Claude Code discovers skills from `~/.claude/skills/<name>/SKILL.md`
//! (directory per skill, entry-point file named `SKILL.md`). trusty-mpm must
//! keep that directory populated with up-to-date skills in that format, while
//! never destroying files the user owns or has hand-edited. Skills carry no
//! inheritance, so — unlike agents — deployment is a plain content copy, but
//! the manifest-based ownership tracking is identical.
//! What: [`deploy_skills`] reads every `*.md` file from a source directory,
//! derives the skill name by stripping the `.md` extension, and writes each
//! one as `~/.claude/skills/<name>/SKILL.md`. It consults the
//! [`SkillManifest`] to classify each target file and writes only the files it
//! safely may. It returns a [`DeployStats`] summarising what happened.
//! Test: `cargo test -p trusty-mpm skill_deployer` covers a new deploy, a
//! skipped user-modified file, an unchanged file, and a user-owned file.

use std::path::Path;

use crate::core::agent_manifest::{atomic_write, checksum};
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
/// and any manifest file or hidden file must be ignored.
/// What: returns `true` for `*.md` files that do not start with `.`.
/// Test: covered indirectly by `deploy_new_skill`.
fn is_skill_file(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".md")
}

/// Derive the skill name (manifest key and target directory name) from a
/// source filename.
///
/// Why: sources are flat `<name>.md` files but the deploy target is
/// `<dest>/<name>/SKILL.md`. Stripping `.md` gives the shared name.
/// What: returns the filename without its `.md` suffix.
/// Test: covered indirectly by every `deploy_*` test.
fn skill_stem(filename: &str) -> &str {
    filename.strip_suffix(".md").unwrap_or(filename)
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
        let stem = skill_stem(&filename).to_string();
        let source_path = source.join(&filename);
        let content = std::fs::read_to_string(&source_path)?;
        // Claude Code discovers skills from <dest>/<name>/SKILL.md.
        let skill_dir = dest.join(&stem);
        let target_path = skill_dir.join("SKILL.md");

        // Classify the existing target file, if any.
        if target_path.exists() {
            if !manifest.is_managed(&stem) {
                // User dropped their own file here — never touch it.
                stats.skipped.push(stem);
                continue;
            }
            let current = std::fs::read_to_string(&target_path)?;
            if manifest.checksum_matches(&stem, &current) {
                if checksum(&content) == checksum(&current) {
                    // Deployed copy is already the latest content.
                    stats.unchanged.push(stem);
                    continue;
                }
                // Managed and unmodified by the user → safe to refresh.
            } else {
                // Managed but the user edited it → preserve their changes.
                stats.skipped.push(stem);
                continue;
            }
        }

        // Write (new file, or safe refresh of a managed file) atomically.
        // Create <dest>/<name>/ if needed, then write SKILL.md via
        // write-temp-then-rename so a crash between the content write and the
        // subsequent manifest save leaves the old file intact.
        std::fs::create_dir_all(&skill_dir)?;
        atomic_write(&target_path, &content)?;
        manifest.managed.insert(
            stem.clone(),
            SkillManifestEntry {
                checksum: checksum(&content),
                deployed_at: now.clone(),
            },
        );
        stats.deployed.push(stem);
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
        // A first-ever deploy must write every skill as <dest>/<name>/SKILL.md
        // and record the skill name (stem) in the manifest.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert_eq!(stats.deployed.len(), 2);
        // Stats report stems, not filenames.
        assert!(stats.deployed.contains(&"tm-doctor".to_string()));
        assert!(stats.skipped.is_empty());
        assert!(stats.unchanged.is_empty());

        // Each skill lands at <dest>/<name>/SKILL.md — not a flat .md file.
        let doctor = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
        assert!(doctor.contains("Diagnostic skill."));

        let manifest = SkillManifest::load(tgt.path());
        assert!(manifest.is_managed("tm-doctor"));
    }

    #[test]
    fn deploy_skips_user_modified() {
        // A managed file the user edited must be skipped, not overwritten.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_skills(src.path(), tgt.path()).unwrap();

        // Simulate the user editing the deployed SKILL.md.
        fs::write(
            tgt.path().join("tm-doctor").join("SKILL.md"),
            "---\nname: tm-doctor\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor".to_string()));
        assert!(!stats.deployed.contains(&"tm-doctor".to_string()));

        let still = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
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
        let skill_md = tgt.path().join("tm-doctor").join("SKILL.md");
        let before = fs::metadata(&skill_md).unwrap().modified().unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.unchanged.contains(&"tm-doctor".to_string()));
        assert!(stats.deployed.is_empty());

        let after = fs::metadata(&skill_md).unwrap().modified().unwrap();
        assert_eq!(before, after, "unchanged file must not be rewritten");
    }

    #[test]
    fn deploy_user_owned_skipped() {
        // A SKILL.md in the target that trusty-mpm never deployed (absent from
        // the manifest) must be left completely untouched.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Pre-create a user-owned skill directory for tm-doctor.
        let user_dir = tgt.path().join("tm-doctor");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("SKILL.md"), "USER OWNED — not trusty-mpm's\n").unwrap();

        let stats = deploy_skills(src.path(), tgt.path()).unwrap();
        assert!(stats.skipped.contains(&"tm-doctor".to_string()));

        let content = fs::read_to_string(user_dir.join("SKILL.md")).unwrap();
        assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

        // example-skill had no conflict, so it deploys normally.
        assert!(stats.deployed.contains(&"example-skill".to_string()));
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
        assert!(stats.deployed.contains(&"tm-doctor".to_string()));
        let refreshed = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
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
