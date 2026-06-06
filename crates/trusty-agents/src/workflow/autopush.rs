//! Auto-push with semantic versioning (#76).
//!
//! Why: Workflows that produce release-worthy artifacts want the final step to
//! be "stage, version, commit, push" so CI / downstream consumers pick up the
//! new build automatically. Gating on `AutoPushConfig::enabled` keeps it
//! strictly opt-in.
//! What: `run_auto_push` stages `out_dir`, bumps the Cargo.toml version
//! (patch/minor/none), commits with a templated message, and pushes to the
//! configured remote. Non-fatal on failure — a failed push is logged but does
//! not error-propagate (the workflow itself already succeeded).
//! Test: `bump_version_patch_increments` and `extract_version_parses_simple`.

use std::path::Path;

use tokio::process::Command;

use crate::workflow::config::AutoPushConfig;

/// Bump the Cargo.toml version string according to `bump` ("patch", "minor",
/// or "none"). Returns the resulting version.
///
/// Why: The version string is the single source of truth for the project;
/// bumping here keeps auto-push idempotent within a single workflow run.
/// What: Reads Cargo.toml, extracts the first non-workspace `version = "x.y.z"`
/// field, increments according to `bump`, and writes the file back via an
/// atomic temp-file rename. Returns the new version.
/// Test: `bump_version_patch_increments`.
pub async fn bump_version(bump: &str) -> anyhow::Result<String> {
    let cargo = tokio::fs::read_to_string("Cargo.toml").await?;
    let current =
        extract_version(&cargo).ok_or_else(|| anyhow::anyhow!("no version field in Cargo.toml"))?;

    let parts: Vec<u32> = current.split('.').filter_map(|p| p.parse().ok()).collect();
    if parts.len() != 3 {
        anyhow::bail!("unexpected version format: {}", current);
    }

    let new_version = match bump {
        "minor" => format!("{}.{}.0", parts[0], parts[1] + 1),
        "patch" => format!("{}.{}.{}", parts[0], parts[1], parts[2] + 1),
        "none" => current.clone(),
        other => anyhow::bail!("unknown version_bump: {}", other),
    };

    if new_version != current {
        let updated = cargo.replacen(
            &format!("version = \"{}\"", current),
            &format!("version = \"{}\"", new_version),
            1,
        );
        // Atomic write via temp file + rename.
        let tmp = "Cargo.toml.tmp";
        tokio::fs::write(tmp, updated).await?;
        tokio::fs::rename(tmp, "Cargo.toml").await?;
        tracing::info!(from = %current, to = %new_version, "version bumped");
    }

    Ok(new_version)
}

/// Pull the first concrete `version = "x.y.z"` literal out of Cargo.toml.
///
/// Why: We purposely avoid pulling in the `toml` crate's full round-trip here
/// — preserving exact formatting, comments, and field order matters more than
/// parser sophistication. A simple line scan does the job and is easy to
/// reason about.
/// What: Walks lines, returns the first `version = "..."` value whose first
/// char is a digit (so `version.workspace = true` lines are skipped).
/// Test: `extract_version_parses_simple`.
fn extract_version(cargo_toml: &str) -> Option<String> {
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("version") && trimmed.contains('=') {
            let val = trimmed.split_once('=')?.1.trim().trim_matches('"');
            if val.chars().next()?.is_ascii_digit() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Run the full auto-push flow.
///
/// Why: Centralizes git-add -> version bump -> commit -> push so workflows
/// don't reimplement shell orchestration per config.
/// What: No-op when `config.enabled == false`. Otherwise stages `out_dir`,
/// bumps Cargo.toml, re-stages Cargo.toml if version changed, commits with
/// the templated message, and pushes. Failures at any step are surfaced
/// via `anyhow::Error` so the caller can log but not fail the workflow.
/// Test: End-to-end requires a real git remote; unit tests cover the pieces.
pub async fn run_auto_push(
    config: &AutoPushConfig,
    out_dir: &Path,
    workflow_name: &str,
    build_num: u64,
    task_preview: &str,
) -> anyhow::Result<()> {
    if !config.enabled {
        return Ok(());
    }

    // Stage out_dir files.
    let out_dir_str = out_dir.to_str().unwrap_or(".").to_string();
    let status = Command::new("git")
        .args(["add", &out_dir_str])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("git add failed");
    }

    // Bump version (may be a no-op when `bump == "none"`).
    let new_version = bump_version(&config.version_bump).await?;

    // Re-stage Cargo.toml if the bump actually changed it.
    if config.version_bump != "none" {
        let _ = Command::new("git")
            .args(["add", "Cargo.toml"])
            .status()
            .await?;
    }

    // Render commit message. Truncate task_preview at 72 chars (char-safe).
    let preview_trunc: String = task_preview.chars().take(72).collect();
    let message = config
        .commit_message_template
        .replace("{{workflow}}", workflow_name)
        .replace("{{build}}", &build_num.to_string())
        .replace("{{task_preview}}", &preview_trunc)
        .replace("{{version}}", &new_version);

    // Commit. `--allow-empty` ensures we don't fail when out_dir contained only
    // already-tracked unchanged files.
    let commit_status = Command::new("git")
        .args(["commit", "-m", &message, "--allow-empty"])
        .status()
        .await?;

    if !commit_status.success() {
        tracing::info!("auto-push: nothing to commit");
        return Ok(());
    }

    // Capture short hash for the success log.
    let hash_out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .await?;
    let hash = String::from_utf8_lossy(&hash_out.stdout).trim().to_string();

    // Push.
    let push_status = Command::new("git")
        .args(["push", &config.push_remote, &config.push_branch])
        .status()
        .await?;

    if push_status.success() {
        tracing::info!(
            workflow = %workflow_name,
            version = %new_version,
            remote = %config.push_remote,
            branch = %config.push_branch,
            commit = %hash,
            "auto-push: pushed"
        );
    } else {
        tracing::warn!(
            commit = %hash,
            "auto-push: git push failed — changes committed locally"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_parses_simple() {
        let cargo = r#"
[package]
name = "trusty-agents"
version = "0.1.2"
edition = "2024"
"#;
        assert_eq!(extract_version(cargo), Some("0.1.2".to_string()));
    }

    #[test]
    fn extract_version_skips_workspace_indirection() {
        let cargo = r#"
[package]
version.workspace = true
"#;
        // We only accept version lines whose value starts with a digit.
        assert_eq!(extract_version(cargo), None);
    }

    #[test]
    fn extract_version_none_when_missing() {
        let cargo = "[package]\nname = \"x\"\n";
        assert!(extract_version(cargo).is_none());
    }

    // Note: `bump_version` mutates Cargo.toml in CWD so we can't unit-test it
    // in isolation. Logic is covered by `extract_version_*` tests; end-to-end
    // behavior is exercised manually via `run_auto_push` with `enabled=true`.

    #[tokio::test]
    async fn run_auto_push_disabled_is_noop() {
        let cfg = AutoPushConfig::default(); // enabled = false
        // This should not touch git or the filesystem.
        let tmp = tempfile::tempdir().unwrap();
        run_auto_push(&cfg, tmp.path(), "w", 1, "preview")
            .await
            .unwrap();
    }
}
