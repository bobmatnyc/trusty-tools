//! Build and spawn the attended `claude` session for a managed alias.
//!
//! Why: `tm run <alias>` is the claude-mpm replacement — it ensures the alias
//! is loaded, sets `CLAUDE_CONFIG_DIR` to the tm-global config dir (the
//! load-bearing isolation primitive, DOC-24 SPEC-STANDALONE-MPM-05c), and
//! launches `claude` in the project's `repo/` directory. Auth follows the
//! WI-10 two-path model: when `ANTHROPIC_API_KEY` is set, `claude` is launched
//! with `--bare` (bypasses keychain/OAuth); otherwise, the session relies on
//! the keychain entry created by `tm login`. This module is intentionally
//! unit-testable: `build_launch_command` and `build_login_command` return
//! configured `Command`s without spawning them.
//! What: four public functions — `build_launch_command` (pure, unit-testable),
//! `build_login_command` (pure, unit-testable), `check_credentials` (env check),
//! and `run_alias` (orchestrator that calls load, checks auth, builds and spawns
//! the command).
//! Test: `test_build_launch_command_sets_env_and_cwd`,
//! `test_build_launch_command_adds_bare_with_api_key`,
//! `test_build_launch_command_no_bare_without_api_key`,
//! `test_build_login_command_sets_env_and_invocation`,
//! `test_check_credentials_with_env_var`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use super::load::load_alias;
use super::registry::ManagedRegistry;

/// Build the `claude` launch `Command` for a managed repo directory.
///
/// Why: separating command construction from spawning makes the launch contract
/// unit-testable without actually starting a process. The caller (or a test)
/// can inspect program, current_dir, args, and env before spawning. The
/// WI-10 two-path auth model is encoded here: when `api_key` is `Some(_)`, the
/// `--bare` flag is added so Claude Code bypasses keychain/OAuth and uses the
/// API key directly; otherwise the keychain entry created by `tm login` is used.
/// `--dangerously-skip-permissions` is always added for fully unattended orchestration
/// (consistent with all other tm-initiated Claude launches).
/// Stdio is set to `Stdio::inherit()` explicitly so the attended interactive
/// session is always connected to the terminal, even if a future caller switches
/// from `.status()` to `.output()`.
/// What: returns a `Command` with program `"claude"`, cwd `repo_path`,
/// env `CLAUDE_CONFIG_DIR=<claude_config_dir>`, stdin/stdout/stderr set to
/// `Stdio::inherit()`, `--dangerously-skip-permissions`, and (when `api_key` is
/// `Some(s)` with a non-empty `s`) the `--bare` arg. Does NOT spawn.
/// Test: `test_build_launch_command_sets_env_and_cwd`,
/// `test_build_launch_command_adds_bare_with_api_key`,
/// `test_build_launch_command_no_bare_without_api_key`,
/// `test_build_launch_command_includes_bypass_permissions`.
pub fn build_launch_command(
    repo_path: &Path,
    claude_config_dir: &Path,
    api_key: Option<&str>,
) -> Command {
    let mut cmd = Command::new("claude");
    cmd.current_dir(repo_path);
    cmd.env("CLAUDE_CONFIG_DIR", claude_config_dir);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Always add bypass-permissions for fully automated orchestration (consistent with
    // all other tm launch paths that use PERMISSION_MODE_FLAG).
    cmd.arg(crate::core::model_inject::PERMISSION_MODE_FLAG);
    // WI-10: when ANTHROPIC_API_KEY is set, add --bare so Claude Code bypasses
    // keychain/OAuth reads and uses the API key directly. When the key is absent
    // the session relies on the keychain entry created by `tm login`.
    if let Some(key) = api_key
        && !key.trim().is_empty()
    {
        cmd.arg("--bare");
    }
    cmd
}

/// Build the `claude auth login` `Command` for the tm-global config dir.
///
/// Why: `tm login` needs to run the OAuth login flow under
/// `CLAUDE_CONFIG_DIR=<tm-global-config-dir>` so the resulting keychain entry
/// is keyed to that dir and `tm run` sessions can authenticate on the user's
/// Claude Max/Pro plan. Factoring out the command construction makes it
/// unit-testable without launching a browser. Stdio inheritance is set
/// explicitly (not left to the `.status()` default) so a future caller using
/// `.output()` cannot silently suppress the interactive OAuth prompt.
/// What: returns a `Command` with program `"claude"`, args `["auth", "login"]`,
/// env `CLAUDE_CONFIG_DIR=<claude_config_dir>`, and stdin/stdout/stderr all
/// set to `Stdio::inherit()`. Does NOT spawn.
/// Test: `test_build_login_command_sets_env_and_invocation`.
pub fn build_login_command(claude_config_dir: &Path) -> Command {
    let mut cmd = Command::new("claude");
    cmd.args(["auth", "login"]);
    cmd.env("CLAUDE_CONFIG_DIR", claude_config_dir);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd
}

/// Determine whether the managed session will have a usable auth path.
///
/// Why: CLAUDE_CONFIG_DIR (A9) relocates the entire `~/.claude/` tree including
/// `.credentials.json`; a clean tm-global config dir is therefore unauthenticated.
/// WI-10 defines two valid auth paths — `ANTHROPIC_API_KEY` (+`--bare`) and the
/// keychain login created by `tm login`. This function encodes the same two-path
/// check so `run_alias` can emit an informative warning when neither appears
/// available, without blocking the launch (a keychain entry cannot be probed
/// non-interactively).
/// Accepting `api_key` as a parameter (rather than reading the env directly)
/// removes env mutation from tests and keeps this pure under parallel test
/// runners (F4 fix).
/// What: returns `AuthState::ApiKey` when `api_key` is `Some(s)` with a
/// non-empty `s`; `AuthState::CredentialsFile` when
/// `<claude_config_dir>/.credentials.json` exists; `AuthState::Unknown`
/// otherwise (the keychain entry, if any, cannot be probed from Rust without
/// spawning `claude auth status`, so we issue a hint instead of a hard failure).
/// Test: `test_check_credentials_with_key_param`,
/// `test_check_credentials_with_file`.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthState {
    /// ANTHROPIC_API_KEY is set — `--bare` will be used.
    ApiKey,
    /// `.credentials.json` exists in the tm-global config dir (MCP OAuth tokens
    /// only, not primary session auth; kept for diagnostic purposes).
    CredentialsFile,
    /// Neither path is detectable non-interactively (keychain entry may still
    /// exist from a prior `tm login` — issue a hint, not a hard failure).
    Unknown,
}

/// Check which auth path is available for the managed session.
///
/// Why: see `AuthState` doc; encodes the WI-10 two-path check without probing
/// the macOS keychain (requires spawning `claude auth status`).
/// What: evaluates `api_key` then `.credentials.json` presence and returns the
/// matching `AuthState`.
/// Test: `test_check_credentials_with_key_param`, `test_check_credentials_with_file`.
pub fn check_credentials(claude_config_dir: &Path, api_key: Option<&str>) -> AuthState {
    if let Some(key) = api_key
        && !key.trim().is_empty()
    {
        return AuthState::ApiKey;
    }
    if claude_config_dir.join(".credentials.json").exists() {
        return AuthState::CredentialsFile;
    }
    AuthState::Unknown
}

/// Load and run an interactive `claude` session for the given alias.
///
/// Why: `tm run <alias>` is the primary daily-driver entry point. It calls
/// `load_alias` unconditionally (idempotent), then applies the WI-10 two-path
/// auth model: when `ANTHROPIC_API_KEY` is set the session launches with
/// `--bare` (API-key path); otherwise it relies on the keychain entry created
/// by `tm login` (keychain path). A best-effort stderr hint is emitted when
/// neither a seeded `.credentials.json` nor `ANTHROPIC_API_KEY` is detected,
/// pointing users to `tm login` — but we do not block, because a valid keychain
/// entry cannot be probed without spawning `claude auth status`.
/// What: (1) `load_alias` — idempotent, (2) reads `ANTHROPIC_API_KEY` and
/// calls `check_credentials` for a hint, (3) builds the command with
/// `build_launch_command` (which adds `--bare` when the key is set), (4)
/// spawns via `crate::core::spawn_disclaim::disclaimed_status` (macOS:
/// disclaims TCC responsibility for the child so consent prompts attribute to
/// `claude` rather than the signed `trusty-mpm`/`tm` binary, issue #2997;
/// elsewhere: plain `Command::status()`) and waits. The attended session's
/// non-zero exit code is not treated as a tm error (Ctrl-C → 130, typing
/// `exit` → 0/1, etc. are all normal).
/// Test: end-to-end requires a real `claude` binary; auth-path logic is covered
/// by `test_build_launch_command_adds_bare_with_api_key` and
/// `test_build_launch_command_no_bare_without_api_key`; the disclaim spawn
/// itself is covered by `disclaimed_status_*` in `crate::core::spawn_disclaim`.
pub fn run_alias(alias: &str, managed_root: &Path, claude_config_dir: &Path) -> anyhow::Result<()> {
    let repo_path = load_alias(alias, managed_root, claude_config_dir)
        .with_context(|| format!("failed to load alias '{alias}'"))?;

    // Read ANTHROPIC_API_KEY at the call site so `check_credentials` remains
    // pure and unit-testable without env mutation (F4 fix).
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok();

    match check_credentials(claude_config_dir, api_key.as_deref()) {
        AuthState::ApiKey => {
            // API key present → --bare will be added by build_launch_command.
        }
        AuthState::CredentialsFile => {
            // .credentials.json exists — likely MCP OAuth tokens; the primary
            // keychain session auth was established by `tm login`.
        }
        AuthState::Unknown => {
            // Cannot detect a keychain entry without spawning `claude auth
            // status`; emit a non-blocking hint so the user knows about tm login.
            eprintln!(
                "hint: no ANTHROPIC_API_KEY and no .credentials.json found in {}.\n\
                 If you haven't run `tm login` yet, do so once to authenticate\n\
                 managed sessions on your Claude Max/Pro plan.\n\
                 Alternatively, export ANTHROPIC_API_KEY to use the API-key path.",
                claude_config_dir.display()
            );
        }
    }

    let mut cmd = build_launch_command(&repo_path, claude_config_dir, api_key.as_deref());
    // Routed through the disclaim-aware spawn (issue #2997) rather than
    // `cmd.status()` directly: on macOS this disclaims TCC responsibility for
    // the child, so mis-attributed consent prompts (media library, App-Data,
    // and anything a `cargo build` the session runs touches) are attributed
    // to `claude`'s own stable code identity instead of the signed
    // `trusty-mpm`/`tm` binary. See `crate::core::spawn_disclaim` module docs.
    crate::core::spawn_disclaim::disclaimed_status(&mut cmd)
        .context("failed to spawn 'claude'; is it installed and on PATH?")?;

    // A non-zero exit from an attended interactive session (Ctrl-C → 130,
    // `/exit` → 0, etc.) is normal end-of-session, not a tm error.
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

        let cmd = build_launch_command(&repo, &cfg, None);

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

    // WI-10: `--bare` MUST be present when api_key is supplied (API-key path).
    #[test]
    fn test_build_launch_command_adds_bare_with_api_key() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let cfg = tmp.path().join("claude-config");

        let cmd = build_launch_command(&repo, &cfg, Some("sk-ant-test-key"));

        let args: Vec<_> = cmd.get_args().collect();
        assert!(
            args.iter().any(|a| a == &std::ffi::OsStr::new("--bare")),
            "expected --bare in args when api_key is set; got {args:?}"
        );
    }

    // WI-10: `--bare` MUST NOT be present when no api_key (keychain path).
    #[test]
    fn test_build_launch_command_no_bare_without_api_key() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let cfg = tmp.path().join("claude-config");

        // None key → no --bare
        let cmd = build_launch_command(&repo, &cfg, None);
        let args: Vec<_> = cmd.get_args().collect();
        assert!(
            !args.iter().any(|a| a == &std::ffi::OsStr::new("--bare")),
            "--bare must not be present when api_key is None; got {args:?}"
        );

        // Empty key → no --bare (treated same as None)
        let cmd2 = build_launch_command(&repo, &cfg, Some(""));
        let args2: Vec<_> = cmd2.get_args().collect();
        assert!(
            !args2.iter().any(|a| a == &std::ffi::OsStr::new("--bare")),
            "--bare must not be present for empty api_key; got {args2:?}"
        );

        // Whitespace-only key → no --bare
        let cmd3 = build_launch_command(&repo, &cfg, Some("   "));
        let args3: Vec<_> = cmd3.get_args().collect();
        assert!(
            !args3.iter().any(|a| a == &std::ffi::OsStr::new("--bare")),
            "--bare must not be present for whitespace-only key; got {args3:?}"
        );
    }

    // Every `tm run` launch must carry the bypass-permissions flag so unattended
    // orchestration never blocks on interactive prompts.
    #[test]
    fn test_build_launch_command_includes_bypass_permissions() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let cfg = tmp.path().join("claude-config");

        // Without API key.
        let cmd = build_launch_command(&repo, &cfg, None);
        let args: Vec<_> = cmd.get_args().collect();
        assert!(
            args.iter()
                .any(|a| *a == std::ffi::OsStr::new("--dangerously-skip-permissions")),
            "expected --dangerously-skip-permissions in args (no key); got {args:?}"
        );

        // With API key (--bare should co-exist with bypass flag).
        let cmd2 = build_launch_command(&repo, &cfg, Some("sk-ant-test-key"));
        let args2: Vec<_> = cmd2.get_args().collect();
        assert!(
            args2
                .iter()
                .any(|a| *a == std::ffi::OsStr::new("--dangerously-skip-permissions")),
            "expected --dangerously-skip-permissions with api key; got {args2:?}"
        );
        assert!(
            args2.iter().any(|a| *a == std::ffi::OsStr::new("--bare")),
            "expected --bare to co-exist with bypass flag; got {args2:?}"
        );
    }

    // WI-10: `tm login` command builder sets the correct program, args, and env.
    #[test]
    fn test_build_login_command_sets_env_and_invocation() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");

        let cmd = build_login_command(&cfg);

        assert_eq!(cmd.get_program(), "claude");

        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            vec![std::ffi::OsStr::new("auth"), std::ffi::OsStr::new("login")],
            "expected [auth, login], got {args:?}"
        );

        let env_vars: Vec<_> = cmd.get_envs().collect();
        let cfg_var = env_vars
            .iter()
            .find(|(k, _)| *k == std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"));
        assert!(
            cfg_var.is_some(),
            "CLAUDE_CONFIG_DIR must be set on the login command"
        );
        if let Some((_, val)) = cfg_var {
            assert_eq!(*val, Some(cfg.as_os_str()));
        }
    }

    // F4: check_credentials now accepts the key value as a parameter so tests
    // never need unsafe env mutation (safe under parallel test runners).
    #[test]
    fn test_check_credentials_with_key_param() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // Non-empty key → ApiKey.
        assert_eq!(
            check_credentials(&cfg, Some("sk-test-key")),
            AuthState::ApiKey
        );

        // Empty / whitespace-only key → falls through to file/unknown check.
        assert_eq!(check_credentials(&cfg, Some("")), AuthState::Unknown);
        assert_eq!(check_credentials(&cfg, Some("   ")), AuthState::Unknown);

        // None key, no file → Unknown.
        assert_eq!(check_credentials(&cfg, None), AuthState::Unknown);
    }

    #[test]
    fn test_check_credentials_with_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // No key, file exists → CredentialsFile.
        std::fs::write(cfg.join(".credentials.json"), r#"{"token":"t"}"#).unwrap();
        assert_eq!(check_credentials(&cfg, None), AuthState::CredentialsFile);

        // Both key and file → ApiKey (API key takes precedence).
        assert_eq!(
            check_credentials(&cfg, Some("sk-also-valid")),
            AuthState::ApiKey
        );
    }
}
