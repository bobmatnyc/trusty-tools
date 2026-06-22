//! Clone or refresh a managed project workspace for a registered alias.
//!
//! Why: `tm load <alias>` is the idempotent entry point that turns a registered
//! name into a ready-to-drive, isolated project directory (DOC-24
//! SPEC-STANDALONE-MPM-02). Making it idempotent (clone-once, refresh-many)
//! means `run` can call it unconditionally and it also serves as the
//! "bring this project up to date" verb.
//! What: [`load_alias`] resolves the alias from the registry, clones (or
//! fast-forward-pulls) the repo into
//! `<managed_root>/projects/<alias>/repo/`, runs `prepare_session` from the
//! session-launch core, writes `.trusty-mpm/managed.toml`, and returns the
//! absolute path to `repo/`.
//! Test: unit tests for the marker-file write logic; git operations are
//! integration-only (require network/git binary).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::registry::ManagedRegistry;

/// The marker file written into every managed project.
///
/// Why: the marker lets `path`, `ls`, `rm`, and `update` know a directory is
/// tm-managed and carries the metadata needed to replay a launch deterministically
/// (DOC-24 SPEC-STANDALONE-MPM-03d).
/// What: a TOML struct at `.trusty-mpm/managed.toml`.
/// Test: `test_marker_write_round_trip`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManagedMarker {
    /// The alias this project was cloned from.
    pub alias: String,
    /// The clone URL.
    pub url: String,
    /// The git ref checked out (default `"main"`).
    pub git_ref: String,
    /// Absolute path to the tm-global CLAUDE_CONFIG_DIR.
    pub claude_config_dir: String,
}

/// Load (clone or refresh) the managed workspace for `alias`.
///
/// Why: the idempotent load verb is the load-bearing primitive behind `run`;
/// separating it lets callers (`tm load`, `tm run`) both call it without
/// duplicating the clone/prepare logic.
/// What:
/// 1. Looks up the alias in the registry under `managed_root`.
/// 2. Derives `<managed_root>/projects/<alias>/repo/` as the checkout dir.
/// 3. If `repo/` doesn't exist: clones via `git clone --depth 1 <url> repo/`.
/// 4. If `repo/` exists: `git -C repo/ pull --ff-only` (best-effort, non-fatal).
/// 5. Runs `prepare_session` from `crate::core::session_launch` on `repo/`.
/// 6. Writes `.trusty-mpm/managed.toml`.
/// 7. Returns the absolute `PathBuf` to `repo/`.
///
/// Test: `test_marker_write_round_trip` (marker); git operations require network.
pub fn load_alias(
    alias: &str,
    managed_root: &Path,
    claude_config_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let registry = ManagedRegistry::load(managed_root)
        .with_context(|| format!("failed to load registry from {}", managed_root.display()))?;
    let entry = registry
        .get(alias)
        .with_context(|| format!("alias '{alias}' is not registered"))?;
    let url = entry.url.clone();
    let git_ref = entry.git_ref.clone();

    let project_dir = managed_root.join("projects").join(alias);
    let repo_dir = project_dir.join("repo");

    if !repo_dir.exists() {
        clone_repo(&url, &project_dir)?;
    } else {
        pull_ff_only(&repo_dir);
    }

    run_prepare_session(&repo_dir)?;
    write_marker(&repo_dir, alias, &url, &git_ref, claude_config_dir)?;

    Ok(repo_dir)
}

/// Clone the repository into `<project_dir>/repo/`.
///
/// Why: `git clone` is the authoritative way to get a fresh checkout;
/// shelling out avoids a heavy libgit2 dependency.
/// What: runs `git clone --depth 1 <url> repo/` in `project_dir`, creating
/// the directory first.
/// Test: integration-only (requires git binary + network).
fn clone_repo(url: &str, project_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(project_dir).with_context(|| {
        format!(
            "failed to create project directory {}",
            project_dir.display()
        )
    })?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url, "repo"])
        .current_dir(project_dir)
        .status()
        .context("failed to spawn git clone")?;
    if !status.success() {
        anyhow::bail!("git clone failed for '{url}'");
    }
    Ok(())
}

/// Attempt a fast-forward pull; non-fatal on any failure.
///
/// Why: idempotent `load` should refresh an existing checkout but must never
/// destroy user edits — fast-forward only is the safest pull strategy.
/// What: runs `git -C <repo_dir> pull --ff-only`; silently ignores failures
/// (dirty tree, network unavailable) so a load with local modifications still
/// succeeds.
/// Test: integration-only.
fn pull_ff_only(repo_dir: &Path) {
    let result = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo_dir)
        .status();
    if let Ok(status) = result
        && !status.success()
    {
        eprintln!(
            "warning: git pull --ff-only failed in {}; skipping refresh",
            repo_dir.display()
        );
    }
}

/// Run `prepare_session` on the given repo directory.
///
/// Why: `prepare_session` deploys composed agents, skills, and CLAUDE.md so
/// the project-local half of the managed configuration is complete.
/// What: resolves `FrameworkPaths` from the home directory and calls
/// `crate::core::session_launch::prepare_session`.
/// Test: `prepare_session` has its own unit tests in session_launch/tests.rs.
fn run_prepare_session(repo_dir: &Path) -> anyhow::Result<()> {
    let fw = crate::core::paths::FrameworkPaths::default();
    crate::core::session_launch::prepare_session(&fw, repo_dir)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("prepare_session failed: {e}"))
}

/// Write `.trusty-mpm/managed.toml` into the repo directory.
///
/// Why: the marker lets every other lifecycle verb (`path`, `ls`, `rm`) detect
/// a managed directory and read the metadata needed to replay a launch.
/// What: creates `.trusty-mpm/` if absent, serializes [`ManagedMarker`] to
/// TOML, and writes `managed.toml`.
/// Test: `test_marker_write_round_trip`.
fn write_marker(
    repo_dir: &Path,
    alias: &str,
    url: &str,
    git_ref: &str,
    claude_config_dir: &Path,
) -> anyhow::Result<()> {
    let dot_dir = repo_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&dot_dir)
        .with_context(|| format!("failed to create {}", dot_dir.display()))?;
    let marker = ManagedMarker {
        alias: alias.to_string(),
        url: url.to_string(),
        git_ref: git_ref.to_string(),
        claude_config_dir: claude_config_dir.to_string_lossy().to_string(),
    };
    let toml = toml::to_string_pretty(&marker).context("failed to serialize managed.toml")?;
    std::fs::write(dot_dir.join("managed.toml"), toml)
        .context("failed to write .trusty-mpm/managed.toml")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_marker_write_round_trip() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let cfg = tmp.path().join("claude-config");

        write_marker(&repo, "my-alias", "https://github.com/org/r", "main", &cfg).unwrap();

        let toml_path = repo.join(".trusty-mpm").join("managed.toml");
        assert!(toml_path.exists());
        let text = std::fs::read_to_string(&toml_path).unwrap();
        let marker: ManagedMarker = toml::from_str(&text).unwrap();
        assert_eq!(marker.alias, "my-alias");
        assert_eq!(marker.url, "https://github.com/org/r");
        assert_eq!(marker.git_ref, "main");
    }
}
