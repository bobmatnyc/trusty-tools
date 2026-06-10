//! Agent deployment — writes composed agents into `~/.claude/agents/`.
//!
//! Why: Claude Code reads agent files from `~/.claude/agents/`. trusty-mpm must
//! keep that directory populated with up-to-date *composed* (inheritance-
//! flattened) agents, while never destroying files the user owns or has
//! hand-edited.
//! What: [`deploy_agents`] composes every source agent, consults the
//! [`AgentManifest`] to classify each target file, and writes only the files
//! it safely may. It uses atomic write-temp-then-rename for both content files
//! and the manifest. Corrupt manifests are detected and surfaced as errors
//! rather than silently reset to empty. Returns a [`DeployResult`] summarising
//! what happened.
//! Test: `cargo test -p trusty-mpm-core agent_deployer` covers a new deploy, a
//! skipped user-modified file, an unchanged file, a user-owned file, atomic
//! writes, and corrupt manifest detection.

use std::path::Path;

use crate::core::agent_builder::{AgentBuildError, compose_agent, source_chain};
use crate::core::agent_manifest::{
    AgentManifest, ManifestEntry, ManifestLoad, Origin, atomic_write, checksum,
};

/// Summary of one [`deploy_agents`] run.
///
/// Why: the CLI prints per-file status; callers need the file lists split by
/// outcome to render that summary and to know whether any work was skipped.
/// What: filenames grouped into freshly written, skipped (user-modified), and
/// unchanged (checksum already current).
/// Test: every `deploy_*` test asserts on these vectors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployResult {
    /// Filenames successfully (re)written this run.
    pub deployed: Vec<String>,
    /// Filenames skipped because the user modified them.
    pub skipped: Vec<String>,
    /// Filenames left untouched because their checksum already matched.
    pub unchanged: Vec<String>,
}

/// Whether a source filename names a trusty-mpm agent to compose.
///
/// Why: the source directory holds `.md` files; only those should be composed,
/// and the manifest file (if it ever appears there) must be ignored.
/// What: returns `true` for `*.md` files other than the manifest.
/// Test: covered indirectly by `deploy_new_agent`.
fn is_agent_file(name: &str) -> bool {
    name.ends_with(".md")
}

/// Deploy all agents from source_dir to target_dir.
///
/// Why: ensures ~/.claude/agents/ has up-to-date composed agent files
/// without clobbering user-owned or user-modified files.
///
/// Rules:
///   - Not in manifest → user-owned → skip silently
///   - In manifest, checksum matches → overwrite (safe)
///   - In manifest, checksum differs → user-modified → warn + skip
///   - New trusty-mpm agent → compose + write (atomic) + add to manifest
///   - Corrupt manifest → error (never silently reset, which would reclassify
///     managed files as user-owned and skip re-deploying them)
///
/// Atomic safety: every content file is written via write-temp-then-rename
/// so a crash between writes leaves the old file intact. The manifest is also
/// written atomically via [`AgentManifest::save`].
///
/// Test: `deploy_new_agent`, `deploy_skips_user_modified`, `deploy_unchanged_no_write`,
///       `deploy_aborts_on_corrupt_manifest`, `deploy_content_file_is_atomic`.
pub fn deploy_agents(
    source_dir: &Path,
    target_dir: &Path,
) -> Result<DeployResult, AgentBuildError> {
    let mut result = DeployResult::default();

    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no agents still succeeds.
    if !source_dir.is_dir() {
        return Ok(result);
    }

    // Detect manifest corruption before touching any file. A corrupt manifest
    // must surface as an error — resetting to empty would reclassify all
    // managed files as user-owned and silently skip the entire deploy.
    let mut manifest = match AgentManifest::load_checked(target_dir) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => {
            return Err(AgentBuildError::FrontmatterParse(format!(
                "agent manifest is corrupt and cannot be safely loaded; \
                 run `tm repair deploy` to recover. Detail: {detail}"
            )));
        }
    };
    let now = chrono::Utc::now().to_rfc3339();

    // Collect agent names deterministically so output and tests are stable.
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file() && is_agent_file(name) {
            names.push(name.trim_end_matches(".md").to_string());
        }
    }
    names.sort_unstable();

    for name in names {
        let filename = format!("{name}.md");
        let composed = compose_agent(&name, source_dir)?;
        let target_path = target_dir.join(&filename);

        // Classify the existing target file, if any.
        if target_path.exists() {
            if !manifest.is_managed(&filename) {
                // User dropped their own file here — never touch it.
                result.skipped.push(filename);
                continue;
            }
            let current = std::fs::read_to_string(&target_path)?;
            if manifest.checksum_matches(&filename, &current) {
                if checksum(&composed) == checksum(&current) {
                    // Deployed copy is already the latest composition.
                    result.unchanged.push(filename);
                    continue;
                }
                // Managed and unmodified by the user → safe to refresh.
            } else {
                // Managed but the user edited it → preserve their changes.
                result.skipped.push(filename);
                continue;
            }
        }

        // Write (new file, or safe refresh of a managed file) atomically.
        // Using write-temp-then-rename guarantees that a crash between the
        // content write and the subsequent manifest save leaves the old content
        // file intact — never a half-written one.
        std::fs::create_dir_all(target_dir)?;
        atomic_write(&target_path, &composed).map_err(|e| match e {
            crate::core::error::Error::Io(io) => AgentBuildError::Io(io),
            other => AgentBuildError::FrontmatterParse(other.to_string()),
        })?;
        manifest.managed.insert(
            filename.clone(),
            ManifestEntry {
                source_chain: source_chain(&name, source_dir)?,
                checksum: checksum(&composed),
                deployed_at: now.clone(),
                origin: Origin::Bundled,
            },
        );
        result.deployed.push(filename);
    }

    manifest.save(target_dir).map_err(|e| match e {
        crate::core::error::Error::Io(io) => AgentBuildError::Io(io),
        other => AgentBuildError::FrontmatterParse(other.to_string()),
    })?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A two-file source set: a base agent and a leaf that extends it.
    fn write_sources(dir: &Path) {
        fs::write(
            dir.join("base-agent.md"),
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBase content.\n",
        )
        .unwrap();
        fs::write(
            dir.join("engineer.md"),
            "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nEngineer content.\n",
        )
        .unwrap();
    }

    #[test]
    fn deploy_new_agent() {
        // A first-ever deploy must write every composed agent and record it
        // in the manifest.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert_eq!(result.deployed.len(), 2);
        assert!(result.deployed.contains(&"engineer.md".to_string()));
        assert!(result.skipped.is_empty());
        assert!(result.unchanged.is_empty());

        // Files exist and the composed engineer carries inherited content.
        let engineer = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(engineer.contains("Base content."));
        assert!(engineer.contains("Engineer content."));

        // The manifest records the resolved chain.
        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));
        assert_eq!(
            manifest.managed["engineer.md"].source_chain,
            vec!["base-agent", "engineer"]
        );
    }

    #[test]
    fn deploy_skips_user_modified() {
        // A managed file the user edited must be skipped, not overwritten.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // First deploy establishes the manifest.
        deploy_agents(src.path(), tgt.path()).unwrap();

        // User edits the deployed engineer file.
        fs::write(
            tgt.path().join("engineer.md"),
            "---\nname: engineer\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        // Second deploy must preserve the user's edit.
        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"engineer.md".to_string()));
        assert!(!result.deployed.contains(&"engineer.md".to_string()));

        let still = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(still.contains("USER HAND-EDIT"));
    }

    #[test]
    fn deploy_unchanged_no_write() {
        // A second deploy with no source changes must report files unchanged
        // and not rewrite them.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_agents(src.path(), tgt.path()).unwrap();
        let before = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.unchanged.contains(&"engineer.md".to_string()));
        assert!(result.deployed.is_empty());

        let after = fs::metadata(tgt.path().join("engineer.md"))
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

        // User pre-creates a file matching a source agent's name.
        fs::write(
            tgt.path().join("engineer.md"),
            "USER OWNED — not trusty-mpm's\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"engineer.md".to_string()));

        // The user's content survives untouched.
        let content = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

        // base-agent.md had no conflict, so it deploys normally.
        assert!(result.deployed.contains(&"base-agent.md".to_string()));
    }

    #[test]
    fn deploy_missing_source_dir_is_empty_result() {
        // Deploying from a non-existent source directory is a no-op success.
        let tgt = TempDir::new().unwrap();
        let result =
            deploy_agents(Path::new("/nonexistent/trusty-mpm/agents"), tgt.path()).unwrap();
        assert_eq!(result, DeployResult::default());
    }

    #[test]
    fn deploy_aborts_on_corrupt_manifest() {
        // A corrupt manifest file must cause deploy_agents to return an error
        // instead of silently resetting to empty and reclassifying managed
        // files as user-owned.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Write a malformed manifest to the target directory.
        fs::write(
            tgt.path().join(crate::core::agent_manifest::MANIFEST_FILE),
            b"not valid json{{{",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path());
        assert!(
            result.is_err(),
            "corrupt manifest must cause an error, not a silent reset to empty"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("corrupt") || err_msg.contains("repair"),
            "error message must mention corruption and repair: {err_msg}"
        );
    }

    #[test]
    fn deploy_content_file_is_atomic() {
        // After a successful deploy no stale .tmp file should remain in the
        // target directory — the atomic rename must have completed.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_agents(src.path(), tgt.path()).unwrap();

        for entry in fs::read_dir(tgt.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tmp"),
                "stale .tmp file found after deploy: {name_str}"
            );
        }
    }
}
