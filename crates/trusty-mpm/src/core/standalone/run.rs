//! Build and spawn the attended `claude` session for a managed alias.
//!
//! Why: `tm run <alias>` is the claude-mpm replacement — it ensures the alias
//! is loaded, sets `CLAUDE_CONFIG_DIR` to the tm-global config dir (the
//! load-bearing isolation primitive, DOC-24 SPEC-STANDALONE-MPM-05c), and
//! launches `claude` in the project's `repo/` directory. This module is
//! intentionally unit-testable: `build_launch_command` returns a configured
//! `Command` without spawning it.
//! What: three public functions — `build_launch_command` (pure, unit-testable),
//! `check_credentials` (env check), and `run_alias` (orchestrator that calls
//! load, checks credentials, builds and spawns the command).
//! Test: `test_build_launch_command_sets_env_and_cwd`,
//! `test_check_credentials_with_env_var`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use super::load::load_alias;
use super::registry::ManagedRegistry;

/// Build the `claude` launch `Command` for a managed repo directory.
///
/// Why: separating command construction from spawning makes the launch contract
/// unit-testable without actually starting a process. The caller (or a test)
/// can inspect program, current_dir, and env before spawning.
/// What: returns a `Command` with program `"claude"`, cwd `repo_path`, and
/// env `CLAUDE_CONFIG_DIR=<claude_config_dir>`. Does NOT spawn the process.
/// Test: `test_build_launch_command_sets_env_and_cwd`.
pub fn build_launch_command(repo_path: &Path, claude_config_dir: &Path) -> Command {
    let mut cmd = Command::new("claude");
    cmd.current_dir(repo_path);
    cmd.env("CLAUDE_CONFIG_DIR", claude_config_dir);
    cmd
}

/// Check whether valid credentials are available for the managed session.
///
/// Why: CLAUDE_CONFIG_DIR relocates `.credentials.json` away from `~/.claude/`
/// (validated 2026-06-22 / v2.1.185, A9); without credentials the session
/// launches but cannot make API calls. This guard lets `run_alias` warn the
/// user early.
/// What: returns `true` when `ANTHROPIC_API_KEY` is non-empty in the environment
/// OR `<claude_config_dir>/.credentials.json` exists.
/// Test: `test_check_credentials_with_env_var`.
pub fn check_credentials(claude_config_dir: &Path) -> bool {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.trim().is_empty()
    {
        return true;
    }
    claude_config_dir.join(".credentials.json").exists()
}

/// Load and run an interactive `claude` session for the given alias.
///
/// Why: `tm run <alias>` is the primary daily-driver entry point. It calls
/// `load_alias` unconditionally (idempotent), warns when credentials are
/// absent, then spawns `claude` with inherited stdio so the user drives the
/// session interactively.
/// What: (1) ensures `load_alias` succeeds, (2) warns to stderr when no
/// credential path is available (does NOT block — a session with
/// ANTHROPIC_API_KEY still works), (3) calls `build_launch_command` and
/// spawns it with `wait()`.
/// Test: end-to-end path requires a real `claude` binary; unit-level coverage
/// via `test_build_launch_command_sets_env_and_cwd`.
pub fn run_alias(alias: &str, managed_root: &Path, claude_config_dir: &Path) -> anyhow::Result<()> {
    let repo_path = load_alias(alias, managed_root, claude_config_dir)
        .with_context(|| format!("failed to load alias '{alias}'"))?;

    if !check_credentials(claude_config_dir) {
        eprintln!(
            "warning: no credentials found in {} and ANTHROPIC_API_KEY is not set.\n\
             The session will launch but cannot make API calls.\n\
             Seed credentials with `tm install` or export ANTHROPIC_API_KEY.",
            claude_config_dir.display()
        );
    }

    let mut cmd = build_launch_command(&repo_path, claude_config_dir);
    let status = cmd
        .status()
        .context("failed to spawn 'claude'; is it installed and on PATH?")?;

    if !status.success() {
        anyhow::bail!(
            "claude exited with non-zero status: {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }
    Ok(())
}

/// Resolve the repo path for an alias without launching it.
///
/// Why: `tm path <alias>` prints the stable `repo/` directory so an IDE or
/// script can open the project without running `tm run`.
/// What: resolves `<managed_root>/projects/<alias>/repo/`, checks the marker
/// exists, and returns the path.
/// Test: exercised by `path_cmd` in the CLI command layer.
pub fn resolve_repo_path(alias: &str, managed_root: &Path) -> anyhow::Result<PathBuf> {
    let registry = ManagedRegistry::load(managed_root)
        .with_context(|| format!("failed to load registry from {}", managed_root.display()))?;
    registry
        .get(alias)
        .with_context(|| format!("alias '{alias}' is not registered"))?;

    let repo = managed_root.join("projects").join(alias).join("repo");
    let marker = repo.join(".trusty-mpm").join("managed.toml");
    if !marker.exists() {
        anyhow::bail!("alias '{alias}' is registered but not loaded; run `tm load {alias}` first");
    }
    Ok(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_build_launch_command_sets_env_and_cwd() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let cfg = tmp.path().join("claude-config");

        let cmd = build_launch_command(&repo, &cfg);

        // Verify program is "claude".
        assert_eq!(cmd.get_program(), "claude");

        // Verify cwd is repo_path.
        assert_eq!(cmd.get_current_dir(), Some(repo.as_path()));

        // Verify CLAUDE_CONFIG_DIR env is set.
        let env_vars: Vec<_> = cmd.get_envs().collect();
        let claude_cfg_var = env_vars
            .iter()
            .find(|(k, _)| *k == std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"));
        assert!(
            claude_cfg_var.is_some(),
            "CLAUDE_CONFIG_DIR should be set in the command env"
        );
        if let Some((_, val)) = claude_cfg_var {
            assert_eq!(*val, Some(cfg.as_os_str()));
        }
    }

    #[test]
    fn test_check_credentials_with_env_var() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // No file, set env var.
        // SAFETY: single-threaded test; no other threads read this var.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-test-key") };
        assert!(check_credentials(&cfg));
        // SAFETY: same — undoing the set above.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        // No file, no env var.
        assert!(!check_credentials(&cfg));

        // File exists, no env var.
        std::fs::write(cfg.join(".credentials.json"), r#"{"token":"t"}"#).unwrap();
        assert!(check_credentials(&cfg));
    }
}
