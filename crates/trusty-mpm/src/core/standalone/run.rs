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
/// launches but cannot make API calls. This guard lets `run_alias` warn early.
/// Accepting `api_key` as a parameter (rather than reading the env directly)
/// removes the `unsafe set_var/remove_var` from tests and makes this unit-
/// testable without env mutation under parallel test runners (F4 fix).
/// What: returns `true` when `api_key` is `Some(s)` where `s` is non-empty
/// OR `<claude_config_dir>/.credentials.json` exists.
/// Test: `test_check_credentials_with_key_param`,
/// `test_check_credentials_with_file`.
pub fn check_credentials(claude_config_dir: &Path, api_key: Option<&str>) -> bool {
    if let Some(key) = api_key
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
/// spawns it with `wait()`. The attended session's exit code is NOT treated
/// as a tm error — the user ending an interactive session (e.g. typing `exit`
/// or pressing Ctrl-C, which yields exit code 130) is normal, not a failure.
/// Test: end-to-end path requires a real `claude` binary; unit-level coverage
/// via `test_build_launch_command_sets_env_and_cwd`.
pub fn run_alias(alias: &str, managed_root: &Path, claude_config_dir: &Path) -> anyhow::Result<()> {
    let repo_path = load_alias(alias, managed_root, claude_config_dir)
        .with_context(|| format!("failed to load alias '{alias}'"))?;

    // Read the env var here at the call site so `check_credentials` remains
    // pure and unit-testable without env mutation (F4 fix).
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    if !check_credentials(claude_config_dir, api_key.as_deref()) {
        eprintln!(
            "warning: no credentials found in {} and ANTHROPIC_API_KEY is not set.\n\
             The session will launch but cannot make API calls.\n\
             Seed credentials with `tm install` or export ANTHROPIC_API_KEY.",
            claude_config_dir.display()
        );
    }

    let mut cmd = build_launch_command(&repo_path, claude_config_dir);
    cmd.status()
        .context("failed to spawn 'claude'; is it installed and on PATH?")?;

    // The user ending an attended interactive session (e.g. Ctrl-C → exit 130
    // or typing `exit`) commonly results in a non-zero exit code. That is not a
    // tm error — we return Ok unconditionally here. If callers ever need to
    // propagate the child's exit code as the process exit code they should call
    // std::process::exit(code) directly; adding an error message here would be
    // misleading to the user.
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

    // F4: check_credentials now accepts the key value as a parameter so tests
    // never need unsafe env mutation (safe under parallel test runners).
    #[test]
    fn test_check_credentials_with_key_param() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // Non-empty key → true even with no credentials file.
        assert!(check_credentials(&cfg, Some("sk-test-key")));

        // Empty / whitespace-only key → falls through to file check.
        assert!(!check_credentials(&cfg, Some("")));
        assert!(!check_credentials(&cfg, Some("   ")));

        // None key, no file → false.
        assert!(!check_credentials(&cfg, None));
    }

    #[test]
    fn test_check_credentials_with_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // No key, file exists → true.
        std::fs::write(cfg.join(".credentials.json"), r#"{"token":"t"}"#).unwrap();
        assert!(check_credentials(&cfg, None));

        // Both key and file → true.
        assert!(check_credentials(&cfg, Some("sk-also-valid")));
    }
}
