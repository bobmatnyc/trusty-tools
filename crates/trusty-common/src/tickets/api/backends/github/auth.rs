//! GitHub token resolution — PAT or `gh` CLI fallback.
//!
//! Why: The token-based REST client requires the user to mint and manage a
//! `GITHUB_TOKEN` PAT. Many users already have the official `gh` CLI installed
//! and authenticated (`gh auth login`); we should be able to obtain a bearer
//! token from it instead of demanding a second credential. This mirrors the
//! `gh`-CLI fallback that the `trusty-agents` `native_ticketing` stack has, and
//! finally wires the previously-vestigial `gh_cli_user` / `gh_cli_host` config
//! fields (`config::GithubConfig`).
//! What: `select_auth` decides — purely — whether an explicit token or the
//! `gh` CLI is the source; `gh_token_args` builds the `gh auth token` argv
//! (honouring host/user); `resolve_token` performs the async resolution the
//! backend constructor calls.
//! Test: `tests::*` cover the pure auth-selection + argv logic. The subprocess
//! shell-out itself is environment-dependent and not exercised in unit tests.

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::tickets::api::config::GithubConfig;

/// Which credential source the GitHub backend will use.
///
/// Why: Makes the token-vs-`gh`-CLI decision explicit and unit-testable
/// instead of buried inside the async constructor.
/// What: `Token` = an explicit PAT (config/env); `GhCli` = shell out to
/// `gh auth token`.
/// Test: `tests::select_auth_*`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AuthSource {
    /// Explicit PAT supplied via config or the `GITHUB_TOKEN` env var.
    Token,
    /// Fall back to the authenticated `gh` CLI (`gh auth token`).
    GhCli,
}

/// Decide the credential source from config (pure).
///
/// Why: A present, non-blank token always wins; otherwise we fall back to the
/// `gh` CLI so users authenticated via `gh auth login` need no PAT.
/// What: Returns `Token` when `cfg.token` is `Some` and non-whitespace, else
/// `GhCli`.
/// Test: `select_auth_prefers_token`, `select_auth_falls_back_to_gh_when_absent`,
/// `select_auth_falls_back_to_gh_when_blank`.
pub(super) fn select_auth(cfg: &GithubConfig) -> AuthSource {
    match cfg.token.as_deref() {
        Some(t) if !t.trim().is_empty() => AuthSource::Token,
        _ => AuthSource::GhCli,
    }
}

/// Build the `gh auth token` argv, honouring the vestigial host/user fields.
///
/// Why: `gh auth token` accepts `--hostname` (for GitHub Enterprise) and
/// `--user` (to pick among multiple logged-in accounts); the `gh_cli_host` /
/// `gh_cli_user` config fields exist precisely to drive these.
/// What: Always starts `["auth", "token"]`, appending `--hostname <h>` and
/// `--user <u>` only for non-blank values.
/// Test: `tests::gh_token_args_*`.
pub(super) fn gh_token_args(cfg: &GithubConfig) -> Vec<String> {
    let mut args = vec!["auth".to_string(), "token".to_string()];
    if let Some(host) = cfg.gh_cli_host.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("--hostname".to_string());
        args.push(host.to_string());
    }
    if let Some(user) = cfg.gh_cli_user.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("--user".to_string());
        args.push(user.to_string());
    }
    args
}

/// Resolve a bearer token: explicit PAT or `gh auth token`.
///
/// Why: The REST client only needs a bearer string; sourcing it from the `gh`
/// CLI keeps the whole existing transport unchanged.
/// What: Returns the trimmed config/env token when present, otherwise spawns
/// `gh auth token [...]` and returns its trimmed stdout.
/// Test: pure branches covered by `tests::select_auth_*`; the `gh` shell-out
/// path is environment-dependent and validated manually.
pub(super) async fn resolve_token(cfg: &GithubConfig) -> Result<String> {
    match select_auth(cfg) {
        AuthSource::Token => Ok(cfg.token.as_deref().unwrap_or_default().trim().to_string()),
        AuthSource::GhCli => gh_auth_token(cfg).await,
    }
}

/// Shell out to `gh auth token` and return the token string.
///
/// Why: Centralises the subprocess spawn + error mapping so the constructor
/// stays focused.
/// What: Runs `gh <gh_token_args>`; on non-zero exit or empty stdout, returns
/// an actionable error pointing at `GITHUB_TOKEN` / `gh auth login`.
/// Test: environment-dependent; not unit-tested.
async fn gh_auth_token(cfg: &GithubConfig) -> Result<String> {
    let args = gh_token_args(cfg);
    let output = Command::new("gh")
        .args(&args)
        .output()
        .await
        .context("failed to spawn 'gh auth token' — is the gh CLI installed and on PATH?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "gh auth token failed (exit {}): {} — set GITHUB_TOKEN or run 'gh auth login'",
            output.status,
            stderr.trim()
        );
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        bail!("gh auth token returned empty output — run 'gh auth login' to authenticate");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(token: Option<&str>, host: Option<&str>, user: Option<&str>) -> GithubConfig {
        GithubConfig {
            token: token.map(String::from),
            owner: Some("o".into()),
            repo: Some("r".into()),
            gh_cli_host: host.map(String::from),
            gh_cli_user: user.map(String::from),
        }
    }

    #[test]
    fn select_auth_prefers_token() {
        let cfg = cfg_with(Some("ghp_abc"), None, None);
        assert_eq!(select_auth(&cfg), AuthSource::Token);
    }

    #[test]
    fn select_auth_falls_back_to_gh_when_absent() {
        let cfg = cfg_with(None, None, None);
        assert_eq!(select_auth(&cfg), AuthSource::GhCli);
    }

    #[test]
    fn select_auth_falls_back_to_gh_when_blank() {
        let cfg = cfg_with(Some("   "), None, None);
        assert_eq!(select_auth(&cfg), AuthSource::GhCli);
    }

    #[test]
    fn gh_token_args_bare() {
        let cfg = cfg_with(None, None, None);
        assert_eq!(gh_token_args(&cfg), vec!["auth", "token"]);
    }

    #[test]
    fn gh_token_args_with_host_and_user() {
        let cfg = cfg_with(None, Some("ghe.example.com"), Some("octocat"));
        assert_eq!(
            gh_token_args(&cfg),
            vec![
                "auth",
                "token",
                "--hostname",
                "ghe.example.com",
                "--user",
                "octocat"
            ]
        );
    }

    #[test]
    fn gh_token_args_ignores_blank_host_and_user() {
        let cfg = cfg_with(None, Some("  "), Some(""));
        assert_eq!(gh_token_args(&cfg), vec!["auth", "token"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_token_returns_trimmed_explicit_token() {
        let cfg = cfg_with(Some("  ghp_padded  "), None, None);
        let tok = resolve_token(&cfg).await.expect("explicit token resolves");
        assert_eq!(tok, "ghp_padded");
    }
}
